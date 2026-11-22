//! Direct TLS listeners. HTTP integrations retain their existing authentication.
//! Certificate issuance is a separate OS principal; runtime only reads a generation.
use crate::{
    api::{self, App},
    tls_files::{TlsFiles, TlsSnapshot},
};
use axum::{
    Json, Router,
    body::Body,
    extract::{MatchedPath, State},
    http::{Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use axum_server::{Handle, tls_rustls::RustlsConfig};
use serde_json::{Value, json};
use std::{
    env,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, task::JoinSet};
type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone)]
struct Routing {
    redirect: bool,
    origin: String,
}
#[derive(Clone)]
struct Public {
    origin: String,
    legacy: Vec<String>,
    redirect: bool,
    roots: Vec<String>,
    status: Arc<Mutex<TlsSnapshot>>,
}
pub(crate) struct Prepared {
    http: std::net::TcpListener,
    tls: Option<(std::net::TcpListener, RustlsConfig, TlsFiles, Public)>,
}
impl Prepared {
    pub(crate) async fn from_env(http: TcpListener) -> Result<Self, Error> {
        let listen = env::var("M53_HTTPS_LISTEN").ok();
        let redirect = match env::var("M53_HTTP_CLIENT_MODE").as_deref() {
            Ok("redirect") => true,
            Ok("serve") | Err(_) => false,
            _ => return Err("invalid_http_client_mode".into()),
        };
        let tls = if let Some(listen) = listen {
            let addr: SocketAddr = listen.parse()?;
            let origin = canonical_origin(&env::var("M53_HTTPS_PUBLIC_ORIGIN")?, "https")?;
            let legacy = env::var("M53_LEGACY_HTTP_ORIGINS")?
                .split(',')
                .map(|x| canonical_origin(x.trim(), "http"))
                .collect::<Result<Vec<_>, _>>()?;
            if legacy.is_empty() || legacy.len() > 8 {
                return Err("invalid_legacy_origins".into());
            }
            let files = TlsFiles::from_env(&origin)?;
            let (config, snapshot, roots) = files.load().await?;
            let public = Public {
                origin,
                legacy,
                redirect,
                roots,
                status: Arc::new(Mutex::new(snapshot)),
            };
            let socket = TcpListener::bind(addr).await?.into_std()?;
            Some((
                socket,
                RustlsConfig::from_config(Arc::new(config)),
                files,
                public,
            ))
        } else {
            if redirect
                || [
                    "M53_TLS_DIRECTORY",
                    "M53_TLS_ROOT_CA_FILE",
                    "M53_HTTPS_PUBLIC_ORIGIN",
                ]
                .iter()
                .any(|k| env::var_os(k).is_some())
            {
                return Err("incomplete_https_configuration".into());
            }
            None
        };
        Ok(Self {
            http: http.into_std()?,
            tls,
        })
    }
    pub(crate) async fn run(self, app: App) -> Result<(), Error> {
        let runtime = crate::database::Runtime {
            db: app.db.clone(),
            policy: Arc::new(crate::database::Policy::from_env()?),
        };
        let api = api::router(app).layer(middleware::from_fn_with_state(runtime.clone(), crate::database::deadline));
        let mut jobs = JoinSet::new();
        let mut handles = Vec::new();
        let mut reload = None;
        let http = if let Some((listener, config, files, public)) = self.tls {
            let readiness = readiness_router(runtime.clone(), Some(public.status.clone()));
            let https = api.clone().merge(public_router(public.clone())).merge(readiness.clone());
            let handle = Handle::new();
            handles.push(handle.clone());
            let listener_config = config.clone();
            jobs.spawn(async move {
                axum_server::from_tcp_rustls(listener, listener_config)?
                    .handle(handle)
                    .serve(https.into_make_service())
                    .await
            });
            reload = Some((files, config, public.clone()));
            api.layer(middleware::from_fn_with_state(
                Routing {
                    redirect: public.redirect,
                    origin: public.origin.clone(),
                },
                http_gate,
            ))
            .merge(public_router(public))
            .merge(readiness)
        } else {
            api.merge(readiness_router(runtime, None))
        };
        let handle = Handle::new();
        handles.push(handle.clone());
        jobs.spawn(async move {
            axum_server::from_tcp(self.http)?
                .handle(handle)
                .serve(http.into_make_service())
                .await
        });
        let reload_task = reload.map(|(files, config, public)| {
            tokio::spawn(async move { files.watch(config, public.status, public.roots).await })
        });
        let first = tokio::select! {
         _=shutdown()=>None,
         result=jobs.join_next()=>result,
        };
        for handle in &handles {
            handle.graceful_shutdown(Some(Duration::from_secs(10)));
        }
        let mut failure = first.and_then(|r| match r {
            Ok(Ok(())) => Some("listener_stopped".to_string()),
            Ok(Err(_)) => Some("listener_failed".to_string()),
            Err(_) => Some("listener_task_failed".to_string()),
        });
        while let Some(result) = jobs.join_next().await {
            if !matches!(result, Ok(Ok(()))) {
                failure = Some("listener_shutdown_failed".into());
            }
        }
        if let Some(task) = reload_task {
            task.abort();
        }
        match failure {
            Some(code) => Err(code.into()),
            None => Ok(()),
        }
    }
}
fn readiness_router(runtime: crate::database::Runtime, tls: Option<Arc<Mutex<TlsSnapshot>>>) -> Router {
    Router::new().route("/ready", get(move || {
        let runtime = runtime.clone();
        let tls = tls.clone();
        async move {
            if tls.is_some_and(|status| !status.lock().unwrap_or_else(|e| e.into_inner()).ready) {
                return StatusCode::SERVICE_UNAVAILABLE;
            }
            if crate::database::ready(&runtime.db, &runtime.policy).await { StatusCode::OK }
            else { StatusCode::SERVICE_UNAVAILABLE }
        }
    }))
}
fn public_router(public: Public) -> Router {
    Router::new()
        .route("/.well-known/m53-transport", get(public_info))
        .with_state(public)
}
async fn public_info(State(p): State<Public>) -> Json<Value> {
    let status = p.status.lock().unwrap_or_else(|e| e.into_inner()).clone();
    Json(
        json!({"schema_version":1,"https_origin":p.origin,"legacy_http_origins":p.legacy,"root_ca_sha256":p.roots,"http_client_mode":if p.redirect{"redirect"}else{"serve"},"tls_ready":status.ready,"tls":status}),
    )
}
fn integration_path(path: &str) -> bool {
    matches!(
        path,
        "/api/colposcopy/tasks"
            | "/api/v1/patients/external"
            | "/api/v1/orders/external"
            | "/api/v1/external/studies/:id/publications"
            | "/api/v1/external/assets/:id"
    )
}
async fn http_gate(State(p): State<Routing>, request: Request<Body>, next: Next) -> Response {
    let matched = request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str);
    if !p.redirect
        || matched.is_none()
        || matched == Some("/health")
        || matched.is_some_and(integration_path)
    {
        return next.run(request).await;
    }
    let Some(path) = request.uri().path_and_query() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let target = format!("{}{}", p.origin, path.as_str());
    match target.parse() {
        Ok(location) => {
            let mut response = StatusCode::PERMANENT_REDIRECT.into_response();
            response.headers_mut().insert(header::LOCATION, location);
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
            response
        }
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}
fn canonical_origin(raw: &str, scheme: &str) -> Result<String, Error> {
    let url = reqwest::Url::parse(raw)?;
    if url.scheme() != scheme
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err("invalid_transport_origin".into());
    }
    Ok(url.origin().ascii_serialization())
}
async fn shutdown() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn origin_rejects_untrusted_url_components() {
        for bad in [
            "http://clinic.example.org:8791",
            "https://user@host/",
            "https://host/path",
            "https://host/?x=1",
            "https://host/#x",
        ] {
            assert!(canonical_origin(bad, "https").is_err())
        }
        assert_eq!(
            canonical_origin("https://clinic.example.org:8791/", "https").unwrap(),
            "https://clinic.example.org:8791"
        );
    }
    #[test]
    fn only_exact_integration_paths_remain_http() {
        assert!(integration_path("/api/colposcopy/tasks"));
        assert!(!integration_path("/api/v1/auth/1c/login"));
        assert!(!integration_path("/api/v1/studies/:id/results/1c/pdf"));
        assert!(!integration_path("/api/v1/patients/:id/refresh-from-1c"));
    }
}
