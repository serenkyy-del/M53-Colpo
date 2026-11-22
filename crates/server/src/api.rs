use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use m53_contracts::{SavedProtocol, workflow::*};
use m53_domain::{
    protocol::*,
    validation::{self, Dictionary, Term, ValidationMode},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row, postgres::PgRow};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};
use uuid::Uuid;

#[derive(Clone)]
pub struct App {
    pub db: PgPool,
    pub storage: PathBuf,
}
#[derive(Debug)]
pub struct Error(pub StatusCode, pub &'static str);
#[derive(Clone)]
pub(crate) struct SafeErrorCode(pub &'static str);
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let mut response = (self.0, Json(json!({"code":self.1}))).into_response();
        response.extensions_mut().insert(SafeErrorCode(self.1));
        response
    }
}
impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        if let sqlx::Error::Database(d) = &e {
            eprintln!(
                "DB error code={:?} constraint={:?}",
                d.code(),
                d.constraint()
            );
        }
        if let sqlx::Error::Database(d) = &e
            && matches!(d.code().as_deref(), Some("22007" | "22008" | "22P02"))
        {
            return Self(StatusCode::BAD_REQUEST, "invalid_date_or_value");
        }
        // A lost COMMIT response is not proof of rollback. Return a retryable
        // outage while the client retains its frozen operation and checks its
        // receipt. Constraint/business failures keep their existing responses.
        let unavailable = match &e {
            sqlx::Error::Io(_) | sqlx::Error::Tls(_) | sqlx::Error::Protocol(_)
                | sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => true,
            sqlx::Error::Database(database) => database.code().is_some_and(|code| {
                code.starts_with("08") || matches!(code.as_ref(),
                    "57P01" | "57P02" | "57P03" | "57014" | "55P03" | "25006")
            }),
            _ => false,
        };
        if unavailable {
            return Self(StatusCode::SERVICE_UNAVAILABLE, "database_unavailable");
        }
        match &e {
            sqlx::Error::RowNotFound => Self(StatusCode::NOT_FOUND, "not_found"),
            sqlx::Error::Database(d) if d.is_unique_violation() => {
                Self(StatusCode::CONFLICT, "duplicate")
            }
            sqlx::Error::Database(d) if d.is_foreign_key_violation() || d.is_check_violation() => {
                Self(StatusCode::BAD_REQUEST, "invalid_reference_or_value")
            }
            _ => Self(StatusCode::INTERNAL_SERVER_ERROR, "database_error"),
        }
    }
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, "storage_error")
    }
}
type Result<T> = std::result::Result<T, Error>;
fn bad(code: &'static str) -> Error {
    Error(StatusCode::BAD_REQUEST, code)
}
pub fn hash(s: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(s.as_ref()))
}
#[derive(Clone)]
pub(crate) struct Actor {
    pub(crate) session_hash: String,
    pub(crate) user: Uuid,
    pub(crate) org: Uuid,
    pub(crate) credential: Uuid,
    pub(crate) request_id: Option<Uuid>,
    pub(crate) operation_id: Option<Uuid>,
}
pub(crate) async fn credential(app: &App, h: &HeaderMap) -> Result<(Uuid, Uuid)> {
    let key = h
        .get("x-workstation-key")
        .and_then(|v| v.to_str().ok())
        .ok_or(Error(StatusCode::UNAUTHORIZED, "workstation_required"))?;
    let r=sqlx::query("SELECT c.id,w.organization_id FROM app.api_credentials c JOIN app.workstations w ON w.id=c.workstation_id WHERE c.verifier=$1 AND c.purpose='client' AND c.revoked_at IS NULL AND (c.expires_at IS NULL OR c.expires_at>now()) AND 'clinical'=ANY(c.permissions) AND w.disabled_at IS NULL").bind(hash(key)).fetch_optional(&app.db).await?.ok_or(Error(StatusCode::UNAUTHORIZED,"invalid_workstation"))?;
    Ok((r.get("id"), r.get("organization_id")))
}
pub(crate) async fn actor(app: &App, h: &HeaderMap) -> Result<Actor> {
    let key = h
        .get("x-workstation-key")
        .and_then(|v| v.to_str().ok())
        .ok_or(Error(StatusCode::UNAUTHORIZED, "workstation_required"))?;
    let token = h
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    // One fresh statement, not a cross-request rights cache. Keep the valid-key
    // row even when its user is absent to preserve the existing safe error codes.
    let row=sqlx::query("SELECT c.id AS credential,w.organization_id,(SELECT u.id FROM app.user_sessions s JOIN app.users u ON u.id=s.user_id WHERE s.token_hash=$2 AND s.credential_id=c.id AND s.expires_at>now() AND s.revoked_at IS NULL AND NOT u.disabled AND u.organization_id=w.organization_id AND (u.doctor_id IS NULL OR EXISTS(SELECT 1 FROM app.doctors d WHERE d.id=u.doctor_id AND d.organization_id=u.organization_id AND d.active)) AND EXISTS(SELECT 1 FROM app.user_roles r WHERE r.user_id=u.id AND r.role_code='doctor')) AS user_id FROM app.api_credentials c JOIN app.workstations w ON w.id=c.workstation_id WHERE c.verifier=$1 AND c.purpose='client' AND c.revoked_at IS NULL AND (c.expires_at IS NULL OR c.expires_at>now()) AND 'clinical'=ANY(c.permissions) AND w.disabled_at IS NULL")
        .bind(hash(key)).bind(token.map(hash)).fetch_optional(&app.db).await?.ok_or(Error(StatusCode::UNAUTHORIZED,"invalid_workstation"))?;
    if token.is_none() {
        return Err(Error(StatusCode::UNAUTHORIZED, "user_session_required"));
    }
    let user = row
        .get::<Option<Uuid>, _>("user_id")
        .ok_or(Error(StatusCode::UNAUTHORIZED, "invalid_user_session"))?;
    let credential = row.get("credential");
    let org = row.get("organization_id");
    Ok(Actor {
        session_hash: token.map(hash).unwrap_or_default(),
        user,
        org,
        credential,
        request_id: h
            .get("x-m53-request-id")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| Uuid::parse_str(v).ok()),
        operation_id: h
            .get("x-operation-id")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| Uuid::parse_str(v).ok()),
    })
}
pub(crate) async fn study(app: &App, a: &Actor, id: Uuid) -> Result<Study> {
    let r=sqlx::query("SELECT s.* FROM app.studies s JOIN app.patients p ON p.id=s.patient_id WHERE s.id=$1 AND p.organization_id=$2 AND (s.organization_id IS NULL OR s.organization_id=$2)").bind(id).bind(a.org).fetch_one(&app.db).await?;
    Ok(study_row(&r))
}
fn study_row(r: &PgRow) -> Study {
    Study {
        id: r.get("id"),
        patient_id: r.get("patient_id"),
        status: r.get("status"),
        row_version: r.get("row_version"),
        study_instance_uid: r.get("study_instance_uid"),
    }
}
pub(crate) fn patient_row(r: &PgRow) -> Patient {
    Patient {
        card_number: r.get("card_number"),
        external_id: r.try_get("external_id").unwrap_or(None),
        id: r.get("id"),
        family_name: r.get("family_name"),
        given_name: r.get("given_name"),
        patronymic: r.get("patronymic"),
        birth_date: r.get("birth_date_text"),
        sex: r.get::<Option<String>, _>("sex").map(|s| match s.as_str() {
            "F" => m53_contracts::colposcopy_1c::Sex::F,
            "M" => m53_contracts::colposcopy_1c::Sex::M,
            _ => m53_contracts::colposcopy_1c::Sex::O,
        }),
    }
}
pub(crate) fn asset_row(r: &PgRow) -> Asset {
    Asset {
        id: r.get("id"),
        study_id: r.get("study_id"),
        kind: r.get("kind"),
        sha256: r.get("sha256"),
        byte_size: r.get("byte_size"),
        mime_type: r.get("mime_type"),
    }
}
pub(crate) async fn audit(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    a: &Actor,
    id: Uuid,
    action: &str,
    version: i64,
) -> Result<()> {
    sqlx::query("INSERT INTO app.audit_events(id,actor_id,action,entity_id,entity_version,safe_description,credential_id,request_id,operation_id) VALUES($1,$2,$3,$4,$5,'',$6,$7,$8)").bind(Uuid::new_v4()).bind(a.user).bind(action).bind(id).bind(version).bind(a.credential).bind(a.request_id).bind(a.operation_id).execute(&mut **tx).await?;
    Ok(())
}
pub fn router(app: App) -> Router {
    let app = Arc::new(app);
    let router = Router::new()
        .route("/install", get(crate::public_downloads::installer))
        .route(
            "/downloads/M53Colpo-Setup.exe",
            get(crate::public_downloads::installer),
        )
        .route(
            "/downloads/latest.json",
            get(crate::public_downloads::latest),
        )
        .route(
            "/downloads/releases/:id/files/:name",
            get(crate::public_downloads::file),
        )
        .route(
            "/api/v1/workstation/activity-events",
            post(crate::activity::workstation_events).layer(DefaultBodyLimit::max(128 * 1024)),
        )
        .route("/api/v1/workstation", get(crate::activity::workstation))
        .route("/api/v1/activity-actions", get(crate::activity::catalog))
        .route("/api/v1/diagnoses/priority",get(crate::diagnosis_priority::get).put(crate::diagnosis_priority::put).layer(DefaultBodyLimit::max(crate::body_access::PRIORITY_JSON)))
        .route("/api/v1/protocol-templates", get(crate::protocol_templates::list).post(crate::protocol_templates::create).layer(DefaultBodyLimit::max(5*1024*1024)))
        .route("/api/v1/protocol-templates/:id", get(crate::protocol_templates::get).put(crate::protocol_templates::update).layer(DefaultBodyLimit::max(5*1024*1024)))
        .route("/api/v1/diagnoses/catalog",get(crate::diagnoses::metadata))
        .route("/api/v1/diagnoses/catalog/page",get(crate::diagnoses::page))
        .route(
            "/api/v1/activity-events",
            post(crate::activity::events).layer(DefaultBodyLimit::max(128 * 1024)),
        )
        .route(
            "/api/v1/workstation-events",
            get(crate::client_events::workstation_subscribe),
        )
        .route(
            "/api/v1/client-updates/check",
            get(crate::client_updates::check),
        )
        .route(
            "/api/v1/client-updates/:id/files/:name",
            get(crate::client_updates::file),
        )
        .route(
            "/api/v1/client-updates/status",
            post(crate::client_updates::status).layer(DefaultBodyLimit::max(8192)),
        )
        .route("/api/v1/events", get(crate::client_events::subscribe))
        .route("/api/v1/operations/:id", get(operation_receipt))
        .route("/health", get(|| async { Json(json!({"status":"ok"})) }))
        .route("/api/v1/me", get(crate::auth_access::me))
        .route(
            "/api/v1/auth/offline-key",
            get(crate::auth_access::offline_key),
        )
        .route(
            "/api/v1/auth/offline-grant",
            post(crate::auth_access::offline_grant),
        )
        .route("/api/v1/doctors", get(crate::catalog::doctors))
        .route("/api/v1/capabilities", get(crate::catalog::capabilities))
        .route("/api/v1/patients/:id/edit-context", get(crate::patient_edit::context))
        .route("/api/v1/patients/:id", put(crate::patient_edit::update).layer(DefaultBodyLimit::max(8192)))
        .route("/api/v1/studies/:id/report-context", get(crate::study_history::report_context))
        .route("/api/v1/studies/:id/revisions", get(crate::study_history::list))
        .route("/api/v1/studies/:id/revisions/:revision", get(crate::study_history::revision))
        .route("/api/v1/studies/:id/context", get(crate::catalog::context))
        .route("/api/v1/studies/:id/manual-identity-verification", post(crate::continuity::manual_verification).layer(DefaultBodyLimit::max(32*1024)))
        .route("/api/v1/login", post(login).layer(DefaultBodyLimit::max(crate::body_access::AUTH_JSON)))
        .route("/api/v1/logout", post(logout))
        .route("/api/v1/patients", get(patients).post(new_patient))
        .route("/api/v1/patients/page", get(crate::worklists::patients))
        .route("/api/v1/studies", get(studies).post(new_study))
        .route("/api/v1/studies/summaries", get(study_summaries))
        .route("/api/v1/orders", get(orders))
        .route("/api/v1/studies/:id", get(detail))
        .route("/api/v1/studies/:id/protocol", put(save_draft).layer(DefaultBodyLimit::max(crate::body_access::LARGE_JSON)))
        .route("/api/v1/studies/:id/approve", post(approve).layer(DefaultBodyLimit::max(crate::body_access::APPROVAL_JSON)))
        .route("/api/v1/studies/:id/reopen", post(reopen))
        .route("/api/v1/studies/:id/assets", post(upload).layer(DefaultBodyLimit::max(crate::body_access::ORIGINAL_BYTES)))
        .route("/api/v1/assets/:id", get(download))
        .route("/api/v1/assets/:id/thumbnail", get(thumbnail))
        .route("/api/v1/terminology", get(dictionary_route))
        .route("/api/v1/terminology/versions", get(dictionary_versions))
        .route(
            "/api/v1/phrase-template-fields",
            get(crate::phrase_templates::fields),
        )
        .route(
            "/api/v1/phrase-templates",
            get(crate::phrase_templates::list)
                .post(crate::phrase_templates::create)
                .layer(DefaultBodyLimit::max(128 * 1024)),
        )
        .route(
            "/api/v1/phrase-templates/:id",
            get(crate::phrase_templates::get)
                .put(crate::phrase_templates::update)
                .layer(DefaultBodyLimit::max(128 * 1024)),
        )
        .route(
            "/api/v1/me/phrase-template-preferences",
            get(crate::phrase_templates::preferences)
                .put(crate::phrase_templates::save_preferences)
                .layer(DefaultBodyLimit::max(128 * 1024)),
        )
        .route("/api/v1/templates", get(templates).post(save_template).layer(DefaultBodyLimit::max(crate::body_access::LARGE_JSON)))
        .route("/api/v1/clinic-branches", get(crate::clinic::list))
        .route("/api/v1/studies/:id/reports", post(save_report).layer(DefaultBodyLimit::max(crate::body_access::LARGE_JSON)))
        .route(
            "/api/v1/studies/:id/publications",
            get(publications).post(publish),
        )
;
    #[cfg(not(feature="standalone"))]
    let router = router
        .route(
            "/api/colposcopy/tasks",
            post(crate::colposcopy_tasks::receive).layer(DefaultBodyLimit::max(128 * 1024)),
        )
        .route("/api/v1/tasks", get(crate::colposcopy_tasks::worklist))
        .route(
            "/api/v1/tasks/:id/claim",
            post(crate::colposcopy_tasks::claim),
        )
        .route(
            "/api/v1/studies/:id/results/1c",
            get(list_1c_results).post(queue_1c_result),
        )
        .route(
            "/api/v1/studies/:id/results/1c/pdf",
            post(queue_client_pdf).layer(DefaultBodyLimit::max(
                m53_contracts::colposcopy_1c::MAX_PDF_BYTES,
            )),
        )
        .route("/api/v1/external/assets/:id", get(crate::external::asset))
        .route("/api/v1/patients/external", post(crate::external::patient))
        .route("/api/v1/orders/external", post(crate::external::order))
        .route(
            "/api/v1/external/studies/:id/publications",
            get(crate::external::publications),
        )
        .route("/api/v1/auth/1c/login", post(crate::one_c_login::login).layer(DefaultBodyLimit::max(crate::body_access::AUTH_JSON)))
        .route(
            "/api/v1/studies/:id/identity-link/preview",
            post(crate::continuity::preview),
        )
        .route(
            "/api/v1/studies/:id/identity-link",
            post(crate::continuity::identity_link),
        )
        .route(
            "/api/v1/auth/profiles",
            get(crate::login_profiles::profiles),
        )
        .route("/api/v1/tasks/page", get(crate::worklists::tasks))
        .route(
            "/api/v1/patients/:id/refresh-from-1c",
            post(crate::patient_refresh::refresh),
        );
    router
        .route_layer(axum::middleware::from_fn_with_state(
            app.clone(),crate::body_access::authorize_body,
        ))
        .layer(DefaultBodyLimit::max(crate::body_access::SMALL_JSON))
        .layer(axum::middleware::from_fn_with_state(
            app.clone(),
            crate::activity::request_audit,
        ))
        .with_state(app)
        .layer(axum::middleware::from_fn(standalone_boundary))
}
async fn standalone_boundary(request: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    #[cfg(feature="standalone")]
    if crate::standalone::integration_path(request.uri().path()) {
        use axum::response::IntoResponse;
        return StatusCode::NOT_FOUND.into_response();
    }
    next.run(request).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendResult {
    report_id: Uuid,
}
async fn list_1c_results(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<Value>>> {
    let a = actor(&app, &h).await?;
    study(&app, &a, id).await?;
    let rows=sqlx::query("SELECT id,sequence,state,pdf_sha256,safe_error_code,receipt,payload_kind,cache_state,protocol_revision_id,template_version_id,(state='sent' AND sent_at IS NOT NULL AND coalesce(receipt->>'success','false')='true') AS delivered,CASE WHEN state='sent' THEN to_jsonb(sent_at) ELSE NULL END AS delivered_at FROM app.result_delivery_jobs WHERE study_id=$1 AND organization_id=$2 ORDER BY sequence")
        .bind(id).bind(a.org).fetch_all(&app.db).await?;
    Ok(Json(rows.iter().map(|r|json!({"job_id":r.get::<Uuid,_>("id"),"sequence":r.get::<i64,_>("sequence"),"state":r.get::<String,_>("state"),"delivered":r.get::<bool,_>("delivered"),"delivered_at":r.get::<Option<Value>,_>("delivered_at"),"pdf_sha256":r.get::<String,_>("pdf_sha256"),"safe_error_code":r.get::<Option<String>,_>("safe_error_code"),"receipt":r.get::<Option<Value>,_>("receipt"),"payload_kind":r.get::<String,_>("payload_kind"),"cache_state":r.get::<Option<String>,_>("cache_state"),"revision_id":r.get::<Option<Uuid>,_>("protocol_revision_id"),"template_version_id":r.get::<Option<Uuid>,_>("template_version_id")})).collect()))
}
async fn queue_client_pdf(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(metadata): Query<crate::result_payloads::ClientPdfMetadata>,
    bytes: Bytes,
) -> Result<Json<Value>> {
    let a = actor(&app, &h).await?;
    study(&app, &a, id).await?;
    if h.get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_none_or(|v| !v.trim().eq_ignore_ascii_case("application/pdf"))
    {
        return Err(Error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "pdf_required"));
    }
    let sha = h
        .get("x-content-sha256")
        .and_then(|v| v.to_str().ok())
        .ok_or(bad("pdf_sha256_required"))?;
    let mut tx = app.db.begin().await?;
    let queued = crate::result_payloads::enqueue(
        &app,
        &mut tx,
        a.org,
        a.credential,
        id,
        &metadata,
        sha,
        &bytes,
    )
    .await?;
    sqlx::query("UPDATE app.colposcopy_tasks SET state='completed',row_version=row_version+1,updated_at=now() WHERE study_id=$1 AND organization_id=$2 AND state='in_progress'").bind(id).bind(a.org).execute(&mut *tx).await?;
    audit(&mut tx, &a, id, "result.delivery.queued", 1).await?;
    revalidate_write_access(&mut tx, &a).await?;
    tx.commit().await?;
    Ok(Json(queued))
}
async fn queue_1c_result(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<SendResult>,
) -> Result<Json<Value>> {
    let a = actor(&app, &h).await?;
    study(&app, &a, id).await?;
    if std::env::var("M53_LEGACY_REPORT_ASSETS").as_deref() != Ok("1") {
        return Err(Error(StatusCode::CONFLICT, "client_pdf_upload_required"));
    }
    let mut tx = app.db.begin().await?;
    let queued =
        crate::colposcopy_results::enqueue(&app, &mut tx, a.org, id, input.report_id).await?;
    sqlx::query("UPDATE app.colposcopy_tasks SET state='completed',row_version=row_version+1,updated_at=now() WHERE study_id=$1 AND organization_id=$2 AND state='in_progress'").bind(id).bind(a.org).execute(&mut *tx).await?;
    audit(&mut tx, &a, id, "result.delivery.queued", 1).await?;
    revalidate_write_access(&mut tx, &a).await?;
    tx.commit().await?;
    Ok(Json(queued))
}
async fn login(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Json(input): Json<Login>,
) -> Result<Json<Session>> {
    if !cfg!(feature="standalone") && std::env::var("M53_AUTH_PROVIDER").as_deref() == Ok("1c_web") {
        return Err(Error(StatusCode::CONFLICT, "use_1c_web_login"));
    }
    let (credential, org) = credential(&app, &h).await?;
    if input.password.len() > 1024 || input.login.len() > 256 {
        return Err(bad("invalid_login"));
    }
    let login_hash = hash(&input.login);
    let mut tx = app.db.begin().await?;
    sqlx::query("INSERT INTO app.login_attempts(credential_id,login_hash) VALUES($1,$2) ON CONFLICT DO NOTHING").bind(credential).bind(&login_hash).execute(&mut *tx).await?;
    let blocked:bool=sqlx::query_scalar("SELECT coalesce(blocked_until>now(),false) FROM app.login_attempts WHERE credential_id=$1 AND login_hash=$2 FOR UPDATE").bind(credential).bind(&login_hash).fetch_one(&mut *tx).await?;
    if blocked {
        return Err(Error(
            StatusCode::TOO_MANY_REQUESTS,
            "login_temporarily_blocked",
        ));
    }
    let row=sqlx::query("SELECT u.id,u.display_name,u.auth_version,a.password_hash FROM app.user_auth a JOIN app.users u ON u.id=a.user_id WHERE a.login=$1 AND u.organization_id=$2 AND NOT u.disabled AND (u.doctor_id IS NULL OR EXISTS(SELECT 1 FROM app.doctors d WHERE d.id=u.doctor_id AND d.organization_id=u.organization_id AND d.active)) AND EXISTS(SELECT 1 FROM app.user_roles ur WHERE ur.user_id=u.id AND ur.role_code='doctor')").bind(&input.login).bind(org).fetch_optional(&mut *tx).await?;
    let encoded: Option<String> = row.as_ref().map(|r| r.get("password_hash"));
    let verified_hash = encoded.clone();
    let password = input.password;
    let valid = tokio::task::spawn_blocking(move || {
        encoded.as_ref().is_some_and(|encoded| {
            PasswordHash::new(encoded).is_ok_and(|p| {
                Argon2::default()
                    .verify_password(password.as_bytes(), &p)
                    .is_ok()
            })
        })
    })
    .await
    .unwrap_or(false);
    if !valid {
        sqlx::query("UPDATE app.login_attempts SET failures=failures+1,blocked_until=CASE WHEN failures>=4 THEN now()+interval '5 minutes' ELSE NULL END WHERE credential_id=$1 AND login_hash=$2").bind(credential).bind(&login_hash).execute(&mut *tx).await?;
        tx.commit().await?;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        return Err(Error(StatusCode::UNAUTHORIZED, "invalid_login"));
    }
    let row = row.ok_or(Error(StatusCode::UNAUTHORIZED, "invalid_login"))?;
    // A reset during Argon2 verification must not create a new session with the
    // old password after the reset transaction has revoked existing sessions.
    sqlx::query("SELECT id FROM app.api_credentials WHERE id=$1 AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at>clock_timestamp()) AND purpose='client' AND 'clinical'=ANY(permissions) FOR SHARE")
        .bind(credential).fetch_optional(&mut *tx).await?
        .ok_or(Error(StatusCode::UNAUTHORIZED,"invalid_workstation"))?;
    let generation:i64=sqlx::query_scalar("SELECT auth_version FROM app.users WHERE id=$1 AND organization_id=$2 AND NOT disabled FOR UPDATE")
        .bind(row.get::<Uuid,_>("id")).bind(org).fetch_optional(&mut *tx).await?
        .ok_or(Error(StatusCode::UNAUTHORIZED,"invalid_login"))?;
    let current_hash: Option<String> =
        sqlx::query_scalar("SELECT password_hash FROM app.user_auth WHERE user_id=$1 AND login=$2")
            .bind(row.get::<Uuid, _>("id"))
            .bind(&input.login)
            .fetch_optional(&mut *tx)
            .await?;
    if generation != row.get::<i64, _>("auth_version") || current_hash != verified_hash {
        return Err(Error(StatusCode::UNAUTHORIZED, "invalid_login"));
    }
    sqlx::query("UPDATE app.login_attempts SET failures=0,blocked_until=NULL WHERE credential_id=$1 AND login_hash=$2").bind(credential).bind(&login_hash).execute(&mut *tx).await?;
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    sqlx::query("INSERT INTO app.user_sessions(token_hash,user_id,credential_id,expires_at) VALUES($1,$2,$3,now()+interval '8 hours')").bind(hash(&token)).bind(row.get::<Uuid,_>("id")).bind(credential).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(Session {
        user_id: row.get("id"),
        token,
        display_name: row.get("display_name"),
    }))
}
async fn logout(State(app): State<Arc<App>>, h: HeaderMap) -> Result<Json<Value>> {
    let a = actor(&app, &h).await?;
    let token = h
        .get(header::AUTHORIZATION)
        .unwrap()
        .to_str()
        .unwrap()
        .strip_prefix("Bearer ")
        .unwrap();
    let mut tx = app.db.begin().await?;
    sqlx::query("UPDATE app.user_sessions SET revoked_at=now() WHERE token_hash=$1 AND user_id=$2 AND credential_id=$3").bind(hash(token)).bind(a.user).bind(a.credential).execute(&mut *tx).await?;
    audit(&mut tx, &a, a.user, "user.logout", 1).await?;
    tx.commit().await?;
    Ok(Json(json!({"ok":true})))
}
async fn patients(State(app): State<Arc<App>>, h: HeaderMap) -> Result<Json<Vec<Patient>>> {
    let a = actor(&app, &h).await?;
    let rows=sqlx::query("SELECT p.*,p.birth_date::text AS birth_date_text,(SELECT e.external_id FROM app.patient_external_ids e WHERE e.patient_id=p.id ORDER BY e.source LIMIT 1) AS external_id FROM app.patients p WHERE p.organization_id=$1 ORDER BY p.family_name,p.given_name LIMIT 500").bind(a.org).fetch_all(&app.db).await?;
    Ok(Json(rows.iter().map(patient_row).collect()))
}
#[derive(Default)]
enum PatientSexInput {
    #[default]
    Missing,
    Present(Option<m53_contracts::colposcopy_1c::Sex>),
}
impl PatientSexInput {
    fn is_missing(&self)->bool { matches!(self,Self::Missing) }
    fn value(&self)->Option<m53_contracts::colposcopy_1c::Sex> {match self {Self::Missing=>None,Self::Present(value)=>*value}}
}
impl serde::Serialize for PatientSexInput {
    fn serialize<S:serde::Serializer>(&self,serializer:S)->std::result::Result<S::Ok,S::Error> {
        serde::Serialize::serialize(&self.value(),serializer)
    }
}
impl<'de> serde::Deserialize<'de> for PatientSexInput {
    fn deserialize<D:serde::Deserializer<'de>>(deserializer:D)->std::result::Result<Self,D::Error> {
        Ok(Self::Present(Option::<m53_contracts::colposcopy_1c::Sex>::deserialize(deserializer)?))
    }
}
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct NewPatientRequest {
    family_name: String,
    given_name: String,
    patronymic: Option<String>,
    birth_date: Option<String>,
    #[serde(default, skip_serializing_if="PatientSexInput::is_missing")]
    sex: PatientSexInput,
}
#[cfg(test)]
#[test]
fn manual_patient_without_sex_keeps_old_receipt_body() {
    let old=NewPatient {family_name:"Synthetic".into(),given_name:"Patient".into(),patronymic:None,birth_date:None};
    let body=json!(old);let current:NewPatientRequest=serde_json::from_value(body.clone()).unwrap();
    assert_eq!(json!(current),body);
}

#[cfg(test)]
#[test]
fn manual_patient_explicit_null_keeps_its_receipt_body() {
    let body=json!({"family_name":"Synthetic","given_name":"Patient","patronymic":null,"birth_date":null,"sex":null});
    let current:NewPatientRequest=serde_json::from_value(body.clone()).unwrap();
    assert_eq!(json!(current),body);
}


async fn new_patient(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Json(p): Json<NewPatientRequest>,
) -> Result<Json<Patient>> {
    let a = actor(&app, &h).await?;
    if p.family_name.trim().is_empty()
        || p.given_name.trim().is_empty()
        || p.family_name.len() > 256
        || p.given_name.len() > 256
        || p.patronymic.as_ref().is_some_and(|s| s.len() > 256)
    {
        return Err(bad("invalid_patient"));
    }
    let id = entity_id(&h, "x-entity-id")?;
    let mut tx = app.db.begin().await?;
    let request_body = json!(p);
    let (operation, cached) = operation_start(
        &mut tx,
        &a,
        &h,
        "/api/v1/patients".to_string(),
        &request_body,
    )
    .await?;
    if let Some(cached) = cached {
        return Ok(Json(
            serde_json::from_value::<Patient>(cached)
                .map_err(|_| bad("invalid_operation_receipt"))?,
        ));
    }
    let sex=p.sex.value().map(|value|match value {m53_contracts::colposcopy_1c::Sex::F=>"F",m53_contracts::colposcopy_1c::Sex::M=>"M",m53_contracts::colposcopy_1c::Sex::O=>"O"});
    let r=sqlx::query("INSERT INTO app.patients(id,organization_id,family_name,given_name,patronymic,birth_date,sex,origin,identity_state) VALUES($1,$2,$3,$4,$5,$6::text::date,$7,'manual','provisional') RETURNING *,birth_date::text AS birth_date_text")
        .bind(id).bind(a.org).bind(p.family_name.trim()).bind(p.given_name.trim()).bind(p.patronymic).bind(p.birth_date).bind(sex).fetch_one(&mut *tx).await?;
    audit(&mut tx, &a, id, "patient.created", 1).await?;
    operation_finish(tx, &a, operation, patient_row(&r)).await
}
#[derive(Default, Deserialize)]
struct StudyFilter {
    patient_id: Option<Uuid>,
}
async fn studies(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Query(filter): Query<StudyFilter>,
) -> Result<Json<Vec<Study>>> {
    let a = actor(&app, &h).await?;
    let rows=sqlx::query("SELECT s.* FROM app.studies s JOIN app.patients p ON p.id=s.patient_id WHERE p.organization_id=$1 AND (s.organization_id IS NULL OR s.organization_id=$1) AND ($2::uuid IS NULL OR s.patient_id=$2) ORDER BY s.created_at DESC LIMIT 500").bind(a.org).bind(filter.patient_id).fetch_all(&app.db).await?;
    Ok(Json(rows.iter().map(study_row).collect()))
}
/// Lightweight list projection. Keep the same visibility predicate and limit as studies.
async fn study_summaries(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Query(filter): Query<StudyFilter>,
) -> Result<Json<Vec<Value>>> {
    let a = actor(&app, &h).await?;
    let rows = sqlx::query(r#"
SELECT s.id, s.row_version, coalesce(nullif(r.form #>> '{colpo,fields,context.exam_date,value}',''),s.encounter_date::text) AS exam_date, s.assigned_doctor_id AS doctor_id,
       coalesce(d.display_name,u.display_name) AS doctor_name,
       CASE WHEN r.form #>> '{conclusion,value,state}' = 'assessed'
            THEN r.form #>> '{conclusion,value,value}' END AS conclusion,
       r.id AS revision_id,
       CASE WHEN s.status='approved' AND r.approved_at IS NOT NULL AND task.binding_count=1 AND task.state<>'cancelled' AND delivery.id IS NOT NULL
            THEN jsonb_build_object('job_id',delivery.id,'revision_id',r.id,
                'template_version_id',delivery.template_version_id,'pdf_sha256',delivery.pdf_sha256,
                'state',delivery.state,'delivered',coalesce(delivery.state='sent' AND delivery.sent_at IS NOT NULL
                    AND delivery.receipt->>'success'='true'
                    AND delivery.receipt->>'application_key'=task.application_key::text,false))
       END AS mis_delivery
FROM app.studies s JOIN app.patients p ON p.id=s.patient_id
LEFT JOIN app.doctors d ON d.id=s.assigned_doctor_id
LEFT JOIN app.users u ON u.id=s.author_id
LEFT JOIN LATERAL (SELECT id,form,approved_at FROM app.protocol_revisions
                  WHERE study_id=s.id ORDER BY revision DESC LIMIT 1) r ON true
LEFT JOIN LATERAL (SELECT t.id,t.application_key,t.state,count(*) OVER() AS binding_count
                  FROM app.colposcopy_tasks t WHERE t.study_id=s.id AND t.organization_id=p.organization_id
                  ORDER BY t.id LIMIT 1) task ON true
LEFT JOIN LATERAL (SELECT j.id,j.state,j.pdf_sha256,j.sent_at,j.receipt,coalesce(j.template_version_id,report.template_version_id) AS template_version_id
                  FROM app.result_delivery_jobs j LEFT JOIN app.reports report ON report.id=j.report_id
                  WHERE j.task_id=task.id AND coalesce(j.protocol_revision_id,report.protocol_revision_id)=r.id
                  ORDER BY j.sequence DESC LIMIT 1) delivery ON true
WHERE p.organization_id=$1 AND (s.organization_id IS NULL OR s.organization_id=$1) AND ($2::uuid IS NULL OR s.patient_id=$2)
ORDER BY s.created_at DESC LIMIT 500
"#).bind(a.org).bind(filter.patient_id).fetch_all(&app.db).await?;
    Ok(Json(
        rows.iter()
            .map(|r| {
                json!({
                    "study_id":r.get::<Uuid,_>("id"),
                    "row_version":r.get::<i64,_>("row_version"),
                    "exam_date":r.get::<Option<String>,_>("exam_date"),
                    "doctor_id":r.get::<Option<Uuid>,_>("doctor_id"),
                    "doctor_name":r.get::<Option<String>,_>("doctor_name"),
                    "conclusion":r.get::<Option<String>,_>("conclusion"),
                    "revision_id":r.get::<Option<Uuid>,_>("revision_id"),
                    "mis_delivery":r.get::<Option<Value>,_>("mis_delivery")
                })
            })
            .collect(),
    ))
}
// Keep the previous serialization for receipts created before equipment capture.
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct NewStudyRequest {
    patient_id: Uuid,
    order_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    encounter_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    colposcope_model: Option<String>,
}

#[cfg(test)]
#[test]
fn equipment_omitted_keeps_old_receipt_body() {
    for encounter_date in [None, Some("2031-04-05".to_owned())] {
        let previous = m53_contracts::continuity::NewEncounter {
            patient_id: Uuid::new_v4(), order_id: None, encounter_date,
        };
        let body = json!(previous);
        let current: NewStudyRequest = serde_json::from_value(body.clone()).unwrap();
        assert_eq!(json!(current), body);
    }
}

pub(crate) fn validate_colposcope_model(value: Option<&str>) -> Result<()> {
    if value.is_some_and(|model| model.len() > 256 || model.trim().is_empty() || model.chars().any(char::is_control)) {
        return Err(bad("invalid_colposcope_model"));
    }
    Ok(())
}

async fn new_study(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Json(input): Json<NewStudyRequest>,
) -> Result<Json<Study>> {
    let a = actor(&app, &h).await?;
    validate_colposcope_model(input.colposcope_model.as_deref())?;
    sqlx::query("SELECT id FROM app.patients WHERE id=$1 AND organization_id=$2")
        .bind(input.patient_id)
        .bind(a.org)
        .fetch_one(&app.db)
        .await?;
    if let Some(order) = input.order_id {
        sqlx::query("SELECT id FROM app.orders WHERE id=$1 AND patient_id=$2 AND status='active'")
            .bind(order)
            .bind(input.patient_id)
            .fetch_one(&app.db)
            .await?;
    }
    let id = entity_id(&h, "x-entity-id")?;
    let mut tx = app.db.begin().await?;
    let request_body = json!(input);
    let (operation, cached) = operation_start(
        &mut tx,
        &a,
        &h,
        "/api/v1/studies".to_string(),
        &request_body,
    )
    .await?;
    if let Some(cached) = cached {
        return Ok(Json(
            serde_json::from_value::<Study>(cached)
                .map_err(|_| bad("invalid_operation_receipt"))?,
        ));
    }
    let doctor: Option<Uuid> = sqlx::query_scalar("SELECT doctor_id FROM app.users WHERE id=$1")
        .bind(a.user)
        .fetch_one(&mut *tx)
        .await?;
    let legacy =
        doctor.is_none() && std::env::var("M53_LEGACY_UNLINKED_DOCTORS").as_deref() == Ok("1");
    if !legacy && (doctor.is_none() || input.encounter_date.is_none()) {
        return Err(bad("doctor_and_encounter_date_required"));
    }
    if input
        .encounter_date
        .as_ref()
        .is_some_and(|d| m53_contracts::colposcopy_1c::CalendarDate::parse(d).is_err())
    {
        return Err(bad("invalid_encounter_date"));
    }
    let origin = if legacy {
        "legacy"
    } else {
        "manual_continuity"
    };
    let r=sqlx::query("INSERT INTO app.studies(id,patient_id,author_id,study_instance_uid,order_id,organization_id,assigned_doctor_id,origin,encounter_date,colposcope_model) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9::text::date,$10) RETURNING *").bind(id).bind(input.patient_id).bind(a.user).bind(format!("2.25.{}",id.as_u128())).bind(input.order_id).bind(a.org).bind(doctor).bind(origin).bind(input.encounter_date).bind(input.colposcope_model).fetch_one(&mut *tx).await?;
    audit(&mut tx, &a, id, "study.created", 1).await?;
    operation_finish(tx, &a, operation, study_row(&r)).await
}
async fn detail(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<StudyDetail>> {
    let a = actor(&app, &h).await?;
    // Status/version, protocol, annotations and assets must describe one point
    // in time. A concurrent reopen must never pair approved with draft text.
    let mut tx = app.db.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx).await?;
    let row = sqlx::query("SELECT s.*,(SELECT count(*)=1 AND coalesce(bool_and(t.state<>'cancelled'),false) FROM app.colposcopy_tasks t WHERE t.study_id=s.id AND t.organization_id=p.organization_id) AS mis_task_bound FROM app.studies s JOIN app.patients p ON p.id=s.patient_id WHERE s.id=$1 AND p.organization_id=$2 AND (s.organization_id IS NULL OR s.organization_id=$2)")
        .bind(id).bind(a.org).fetch_one(&mut *tx).await?;
    let study = study_row(&row);
    let p =
        sqlx::query("SELECT p.*,p.birth_date::text AS birth_date_text,(SELECT e.external_id FROM app.patient_external_ids e WHERE e.patient_id=p.id ORDER BY e.source LIMIT 1) AS external_id FROM app.patients p WHERE p.id=$1")
            .bind(study.patient_id)
            .fetch_one(&mut *tx)
            .await?;
    let r=sqlx::query("SELECT r.id,r.form,coalesce(a.data,'[]'::jsonb) AS annotations FROM app.protocol_revisions r LEFT JOIN app.protocol_annotations a ON a.revision_id=r.id WHERE r.study_id=$1 ORDER BY r.revision DESC LIMIT 1").bind(id).fetch_optional(&mut *tx).await?;
    let (revision_id, protocol, annotations): (
        Option<Uuid>,
        Option<m53_domain::protocol::Protocol>,
        Vec<Annotation>,
    ) = if let Some(r) = r {
        (
            Some(r.get("id")),
            Some(
                serde_json::from_value(r.get("form"))
                    .map_err(|_| bad("stored_protocol_invalid"))?,
            ),
            serde_json::from_value(r.get("annotations"))
                .map_err(|_| bad("stored_annotations_invalid"))?,
        )
    } else {
        (None, None, vec![])
    };
    let supported = h
        .get("x-m53-protocol-version")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(1);
    if protocol
        .as_ref()
        .is_some_and(|p| p.schema_version > supported)
    {
        return Err(Error(StatusCode::CONFLICT, "client_update_required"));
    }
    let assets = sqlx::query("SELECT * FROM app.assets WHERE study_id=$1 ORDER BY imported_at,id")
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(StudyDetail {
        mis_task_bound: row.get("mis_task_bound"),
        study,
        patient: patient_row(&p),
        revision_id,
        protocol,
        annotations,
        assets: assets.iter().map(asset_row).collect(),
    }))
}
pub async fn dictionary(db: &PgPool, id: Option<Uuid>) -> Result<Dictionary> {
    let mut connection = db.acquire().await?;
    dictionary_connection(&mut connection, id).await
}
async fn dictionary_connection(connection: &mut sqlx::PgConnection, id: Option<Uuid>) -> Result<Dictionary> {
    let r=sqlx::query("SELECT id,clinically_approved FROM app.terminology_versions WHERE ($1::uuid IS NULL OR id=$1) ORDER BY code DESC LIMIT 1").bind(id).fetch_one(&mut *connection).await?;
    let version_id = r.get("id");
    let terms =
        sqlx::query("SELECT code,region,kind,coalesce(russian,english) AS label FROM app.terminology_terms WHERE version_id=$1")
            .bind(version_id)
            .fetch_all(&mut *connection)
            .await?
            .iter()
            .map(|r| {
                let region = serde_json::from_value(json!(r.get::<String, _>("region")))
                    .map_err(|_| bad("invalid_dictionary_region"))?;
                let kind = serde_json::from_value(json!(r.get::<String, _>("kind")))
                    .map_err(|_| bad("invalid_dictionary_kind"))?;
                Ok(Term {label:r.get("label"),
                    code: r.get("code"),
                    region,
                    kind,
                })
            })
            .collect::<Result<Vec<_>>>()?;
    Ok(Dictionary {
        version_id,
        clinically_approved: r.get("clinically_approved"),
        terms,
    })
}
#[derive(Deserialize)]
struct DictionaryQuery {
    version_id: Option<Uuid>,
}
async fn dictionary_route(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Query(query): Query<DictionaryQuery>,
) -> Result<Json<Dictionary>> {
    actor(&app, &h).await?;
    Ok(Json(dictionary(&app.db, query.version_id).await?))
}
fn protocol_validation_response(mut errors: Vec<validation::ValidationError>) -> Option<Response> {
    if errors.is_empty() { return None; }
    // The domain's catalog validator embeds submitted map keys in its diagnostic
    // strings. Do not echo arbitrary keys (or clinical text disguised as keys).
    for error in &mut errors {
        if error.path == "colpo" && error.code.starts_with("fields.") && error.code.ends_with(": invalid_value") {
            let id = error.code.trim_start_matches("fields.").trim_end_matches(": invalid_value");
            if m53_domain::colpo::FIELDS.iter().any(|field| field.parent.is_empty() && field.id == id) {
                error.path = format!("/colpo/fields/{id}");
            }
            error.code = "invalid_colpo_value".into();
        } else if error.path == "colpo" && error.code.starts_with("marks.") && error.code.ends_with(": invalid_property") {
            error.code = "invalid_colpo_property".into();
        }
    }
    // Only validator paths/codes: never include rejected clinical values.
    let mut response = (StatusCode::BAD_REQUEST,
        Json(json!({"code":"protocol_validation_failed","errors":errors}))).into_response();
    response.extensions_mut().insert(SafeErrorCode("protocol_validation_failed"));
    Some(response)
}
async fn lock_study(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
    expected: i64,
    allow_approved: bool,
) -> Result<i64> {
    let r = sqlx::query("SELECT row_version,status FROM app.studies WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    if r.get::<i64, _>("row_version") != expected {
        return Err(Error(StatusCode::CONFLICT, "version_conflict"));
    }
    if expected <= 0 || expected == i64::MAX {
        return Err(bad("invalid_version"));
    }
    if !allow_approved && r.get::<String, _>("status") == "approved" {
        return Err(Error(StatusCode::CONFLICT, "revision_immutable"));
    }
    Ok(expected + 1)
}
async fn save_draft(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(wire): Json<Value>,
) -> Result<Response> {
    let diagnoses_wire = wire.get("protocol").and_then(|p| p.get("diagnoses")).cloned();
    let diagnoses_omitted = diagnoses_wire.as_ref().is_none_or(Value::is_null);
    let note_wire = wire.get("protocol").and_then(|p| p.get("diagnosis_note")).cloned();
    let note_omitted = note_wire.as_ref().is_none_or(Value::is_null);
    let history_omitted = wire.get("protocol").and_then(|p| p.get("history")).is_none();
    let mut input: SaveDraft = serde_json::from_value(wire)
        .map_err(|_| Error(StatusCode::UNPROCESSABLE_ENTITY, "invalid_protocol_request"))?;
    let a = actor(&app, &h).await?;
    study(&app, &a, id).await?;
    let d = dictionary(&app.db, Some(input.protocol.terminology_version_id)).await?;
    if let Some(response) = protocol_validation_response(validation::validate(&input.protocol, &d, ValidationMode::Draft)) { return Ok(response); }
    if input.annotations.len() > 1000 {
        return Err(bad("too_many_annotations"));
    }
    let mut tx = app.db.begin().await?;
    let mut request_body = json!(input);
    if history_omitted {
        request_body["protocol"].as_object_mut().unwrap().remove("history");
    }
    // None is a legacy writer, including explicit null. Preserve the exact
    // null/omission distinction in receipt hashing before merging stored data.
    if diagnoses_wire.as_ref().is_some_and(Value::is_null) {
        request_body["protocol"]["diagnoses"] = Value::Null;
    }
    if note_wire.as_ref().is_some_and(Value::is_null) {
        request_body["protocol"]["diagnosis_note"] = Value::Null;
    }
    let (operation, cached) = operation_start(
        &mut tx,
        &a,
        &h,
        format!("/api/v1/studies/{id}/protocol"),
        &request_body,
    )
    .await?;
    if let Some(cached) = cached {
        return Ok(Json(
            serde_json::from_value::<SavedProtocol>(cached)
                .map_err(|_| bad("invalid_operation_receipt"))?,
        ).into_response());
    }
    let version = lock_study(&mut tx, id, input.expected_version, false).await?;
    if history_omitted || diagnoses_omitted || note_omitted {
        // Legacy writers must not erase newer optional content. Explicit []
        // clears diagnoses; explicit NotAssessed clears anamnesis.
        let previous: Option<Value> = sqlx::query_scalar("SELECT form FROM app.protocol_revisions WHERE study_id=$1 ORDER BY revision DESC LIMIT 1")
            .bind(id).fetch_optional(&mut *tx).await?;
        if history_omitted && let Some(history) = previous.as_ref().and_then(|p| p.get("history")) {
            input.protocol.history = serde_json::from_value(history.clone())
                .map_err(|_| bad("stored_protocol_invalid"))?;
        }
        if diagnoses_omitted && let Some(diagnoses) = previous.as_ref().and_then(|p| p.get("diagnoses")) {
            input.protocol.diagnoses = serde_json::from_value(diagnoses.clone())
                .map_err(|_| bad("stored_protocol_invalid"))?;
        }
        if note_omitted && let Some(note) = previous.as_ref().and_then(|p| p.get("diagnosis_note")) {
            input.protocol.diagnosis_note = serde_json::from_value(note.clone())
                .map_err(|_| bad("stored_protocol_invalid"))?;
        }
        if let Some(response) = protocol_validation_response(validation::validate(&input.protocol, &d, ValidationMode::Draft)) { return Ok(response); }
    }
    let lesion_ids: BTreeSet<_> = input.protocol.sections.iter()
        .flat_map(|section| section.lesions.iter().map(|lesion| lesion.id)).collect();
    let finding_codes: BTreeMap<_, _> = input.protocol.sections.iter()
        .flat_map(|section| section.findings.iter().chain(section.lesions.iter().flat_map(|lesion| lesion.findings.iter())))
        .map(|finding| (finding.id, finding.term_code.as_str())).collect();
    let mut asset_ids = BTreeSet::new();
    for annotation in &input.annotations {
        if !annotation.valid_layer()
            || annotation.id.is_nil()
            || !annotation.x.is_finite()
            || !annotation.y.is_finite()
            || !annotation.radius.is_finite()
            || !(0.0..=1.0).contains(&annotation.x)
            || !(0.0..=1.0).contains(&annotation.y)
            || !(0.001..=1.0).contains(&annotation.radius)
            || annotation.label.len() > 512
        {
            return Err(bad("invalid_annotation"));
        }
        asset_ids.insert(annotation.asset_id);
        if annotation.lesion_id.is_some_and(|id| !lesion_ids.contains(&id))
            || annotation.finding_id.is_some_and(|id| !finding_codes.contains_key(&id))
        {
            return Err(bad("invalid_annotation_reference"));
        }
    }
    if !asset_ids.is_empty() {
        let asset_ids: Vec<_> = asset_ids.into_iter().collect();
        let present: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM app.assets WHERE study_id=$1 AND kind='original' AND id=ANY($2)")
            .bind(id).bind(&asset_ids).fetch_all(&mut *tx).await?;
        if present.len() != asset_ids.len() {
            return Err(sqlx::Error::RowNotFound.into());
        }
    }

    for a in &input.annotations {
        if let AnnotationLayer::Callout {
            term_code: Some(ref code),
            ..
        } = a.layer
            && a.finding_id.and_then(|id| finding_codes.get(&id)).copied() != Some(code.as_str())
        {
            return Err(bad("invalid_callout_finding"));
        }
    }
    m53_contracts::workflow::validate_colpo_links(&input.protocol, &input.annotations)
        .map_err(bad)?;
    let mut layer_ids = std::collections::BTreeSet::new();
    for a in &input.annotations {
        let kind = match a.layer {
            AnnotationLayer::Mark
            | AnnotationLayer::Arrow { .. }
            | AnnotationLayer::Callout { .. } => continue,
            AnnotationLayer::Target { .. } => "target",
            AnnotationLayer::OutputRegion { .. } => "output_region",
        };
        if !layer_ids.insert((a.asset_id, kind)) {
            return Err(bad("duplicate_image_layer"));
        }
    }
    let mut annotation_ids = std::collections::BTreeSet::new();
    for annotation in &input.annotations {
        if !annotation_ids.insert(annotation.id) {
            return Err(bad("duplicate_annotation_id"));
        }
    }
    fn referenced_annotation_ids(value: &Value, ids: &mut Vec<Uuid>) {
        match value {
            Value::Object(map) => {
                if map.get("kind").and_then(Value::as_str) == Some("annotation")
                    && let Some(id) = map
                        .get("annotation_id")
                        .and_then(Value::as_str)
                        .and_then(|id| Uuid::parse_str(id).ok())
                {
                    ids.push(id);
                }
                for child in map.values() {
                    referenced_annotation_ids(child, ids);
                }
            }
            Value::Array(values) => {
                for child in values {
                    referenced_annotation_ids(child, ids);
                }
            }
            _ => {}
        }
    }
    let mut references = vec![];
    referenced_annotation_ids(&json!(input.protocol), &mut references);
    if references.iter().any(|id| !annotation_ids.contains(id)) {
        return Err(bad("dangling_annotation_origin"));
    }
    let revision_id = entity_id(&h, "x-entity-id")?;
    let form = serde_json::to_value(&input.protocol).unwrap();
    sqlx::query("INSERT INTO app.protocol_revisions(id,study_id,revision,terminology_version_id,schema_version,form,author_id,conclusion) SELECT $1,$2,coalesce(max(revision),0)+1,$3,$4,$5,$6,$7 FROM app.protocol_revisions WHERE study_id=$2").bind(revision_id).bind(id).bind(d.version_id).bind(input.protocol.schema_version as i32).bind(form).bind(a.user).bind(match &input.protocol.conclusion.value {Assessment::Assessed(s)=>s.as_str(),_=>""}).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO app.protocol_annotations(revision_id,data) VALUES($1,$2)")
        .bind(revision_id)
        .bind(json!(input.annotations))
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE app.studies SET row_version=$2 WHERE id=$1")
        .bind(id)
        .bind(version)
        .execute(&mut *tx)
        .await?;
    audit(&mut tx, &a, id, "protocol.saved", version).await?;
    operation_finish(
        tx,
        &a,
        operation,
        SavedProtocol {
            study_id: id,
            revision_id,
            row_version: version,
        },
    )
    .await.map(IntoResponse::into_response)
}
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ApproveRequest {
    study_id: Uuid,
    revision_id: Uuid,
    expected_version: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selected_photo_ids: Option<Vec<Uuid>>,
}

#[cfg(test)]
#[test]
fn approval_without_photo_selection_keeps_old_receipt_body() {
    let old = m53_contracts::ApproveProtocolRequest {
        study_id: Uuid::new_v4(), revision_id: Uuid::new_v4(), expected_version: 2,
    };
    let body = json!(old);
    let current: ApproveRequest = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(json!(current), body);
}

async fn approve(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<ApproveRequest>,
) -> Result<Response> {
    let a = actor(&app, &h).await?;
    study(&app, &a, id).await?;
    let previous = m53_contracts::ApproveProtocolRequest {
        study_id: input.study_id, revision_id: input.revision_id, expected_version: input.expected_version,
    };
    if input.study_id != id || !previous.validate().is_empty() {
        return Err(bad("invalid_request"));
    }
    let mut tx = app.db.begin().await?;
    let request_body = json!(input);
    let (operation, cached) = operation_start(
        &mut tx,
        &a,
        &h,
        format!("/api/v1/studies/{id}/approve"),
        &request_body,
    )
    .await?;
    if let Some(cached) = cached {
        return Ok(Json(
            serde_json::from_value::<SavedProtocol>(cached)
                .map_err(|_| bad("invalid_operation_receipt"))?,
        )
        .into_response());
    }
    let version = lock_study(&mut tx, id, input.expected_version, false).await?;
    let r=sqlx::query("SELECT id,form,approved_at IS NOT NULL AS already_approved FROM app.protocol_revisions WHERE study_id=$1 ORDER BY revision DESC LIMIT 1 FOR UPDATE").bind(id).fetch_one(&mut *tx).await?;
    if r.get::<Uuid, _>("id") != input.revision_id {
        return Err(Error(StatusCode::CONFLICT, "revision_conflict"));
    }
    if let Some(ids) = &input.selected_photo_ids {
        if ids.len() > 1024 || ids.iter().any(Uuid::is_nil) || ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
            return Err(bad("invalid_report_photo_selection"));
        }
        // The locked study serializes this with uploads; row locks also fence deletion.
        let originals: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM app.assets WHERE study_id=$1 AND kind='original' AND id=ANY($2) ORDER BY id FOR SHARE")
            .bind(id).bind(ids.as_slice()).fetch_all(&mut *tx).await?;
        if originals.len()!=ids.len() {return Err(bad("invalid_report_photo_selection"));}
    }
    let p: Protocol = serde_json::from_value(r.get("form")).map_err(|_| bad("invalid_protocol"))?;
    // Approval already owns a pool connection and study locks. Acquiring a
    // second slot here deadlocks when parallel approvals fill the pool.
    let d = dictionary_connection(&mut tx, Some(p.terminology_version_id)).await?;
    if let Some(response) = protocol_validation_response(validation::validate(&p, &d, ValidationMode::Approval)) { return Ok(response); }
    crate::continuity::can_approve(&mut tx, id, &a).await?;
    // Compatibility with drafts reopened by older servers: preserve the old
    // approved snapshot and approve a fresh copy instead.
    let revision_id = if r.get::<bool, _>("already_approved") {
        let fresh = Uuid::new_v4();
        sqlx::query("INSERT INTO app.protocol_revisions(id,study_id,revision,terminology_version_id,schema_version,form,author_id,conclusion) SELECT $1,study_id,revision+1,terminology_version_id,schema_version,form,$3,conclusion FROM app.protocol_revisions WHERE id=$2")
            .bind(fresh).bind(input.revision_id).bind(a.user).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO app.protocol_annotations(revision_id,data) SELECT $1,data FROM app.protocol_annotations WHERE revision_id=$2")
            .bind(fresh).bind(input.revision_id).execute(&mut *tx).await?;
        fresh
    } else {
        input.revision_id
    };
    sqlx::query("UPDATE app.protocol_revisions SET approved_by=$2,approved_at=now() WHERE id=$1")
        .bind(revision_id)
        .bind(a.user)
        .execute(&mut *tx)
        .await?;
    crate::continuity::snapshot_approval(&mut tx, revision_id, id, &a, input.selected_photo_ids.as_deref()).await?;
    sqlx::query("UPDATE app.studies SET status='approved',row_version=$2 WHERE id=$1")
        .bind(id)
        .bind(version)
        .execute(&mut *tx)
        .await?;
    audit(&mut tx, &a, id, "protocol.approved", version).await?;
    Ok(operation_finish(
        tx,
        &a,
        operation,
        SavedProtocol {
            study_id: id,
            revision_id,
            row_version: version,
        },
    )
    .await?
    .into_response())
}
async fn reopen(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<ExpectedVersion>,
) -> Result<Json<Value>> {
    let a = actor(&app, &h).await?;
    study(&app, &a, id).await?;
    let mut tx = app.db.begin().await?;
    let request_body = json!(input);
    let (operation, cached) = operation_start(
        &mut tx,
        &a,
        &h,
        format!("/api/v1/studies/{id}/reopen"),
        &request_body,
    )
    .await?;
    if let Some(cached) = cached {
        return Ok(Json(
            serde_json::from_value::<Value>(cached)
                .map_err(|_| bad("invalid_operation_receipt"))?,
        ));
    }
    let version = lock_study(&mut tx, id, input.expected_version, true).await?;
    // Reopening must create an editable revision even if the clinician makes no
    // changes before approving again. Never re-approve an immutable snapshot.
    let revision_id = Uuid::new_v4();
    let copied = sqlx::query("INSERT INTO app.protocol_revisions(id,study_id,revision,terminology_version_id,schema_version,form,author_id,conclusion) SELECT $1,study_id,revision+1,terminology_version_id,schema_version,form,$3,conclusion FROM app.protocol_revisions WHERE study_id=$2 ORDER BY revision DESC LIMIT 1 RETURNING id")
        .bind(revision_id)
        .bind(id)
        .bind(a.user)
        .fetch_optional(&mut *tx)
        .await?;
    if copied.is_some() {
        sqlx::query("INSERT INTO app.protocol_annotations(revision_id,data) SELECT $1,data FROM app.protocol_annotations WHERE revision_id=(SELECT id FROM app.protocol_revisions WHERE study_id=$2 AND id<>$1 ORDER BY revision DESC LIMIT 1)")
            .bind(revision_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE app.studies SET status='draft',row_version=$2 WHERE id=$1")
        .bind(id)
        .bind(version)
        .execute(&mut *tx)
        .await?;
    audit(&mut tx, &a, id, "study.reopened", version).await?;
    operation_finish(tx, &a, operation, json!({"row_version":version,"revision_id":if copied.is_some(){Some(revision_id)}else{None}})).await
}
#[derive(Deserialize)]
struct UploadQuery {
    kind: String,
    sha256: String,
    parent: Option<Uuid>,
}
async fn upload(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<UploadQuery>,
    bytes: Bytes,
) -> Result<Json<Asset>> {
    let a = actor(&app, &h).await?;
    study(&app, &a, id).await?;
    if matches!(q.kind.as_str(), "html" | "pdf")
        && std::env::var("M53_LEGACY_REPORT_ASSETS").as_deref() != Ok("1")
    {
        return Err(Error(StatusCode::CONFLICT, "report_assets_disabled"));
    }
    if hash(&bytes) != q.sha256 {
        return Err(bad("hash_mismatch"));
    }
    let mime = if bytes.starts_with(b"M53RGB01") {
        crate::rgb::rgb_dimensions(&bytes).map_err(|_| bad("invalid_rgb"))?;
        "application/x-m53-rgb8"
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else if bytes.starts_with(b"BM") {
        "image/bmp"
    } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        "image/tiff"
    } else if bytes.starts_with(b"%PDF-") {
        "application/pdf"
    } else if bytes.starts_with(b"<!doctype html>") {
        "text/html"
    } else {
        return Err(bad("unsupported_content"));
    };
    if !matches!(
        (q.kind.as_str(), mime),
        ("derived", "application/x-m53-rgb8")
            | (
                "original",
                "image/png" | "image/jpeg" | "image/tiff" | "image/bmp"
            )
            | ("pdf", "application/pdf")
            | ("html", "text/html")
    ) {
        return Err(bad("kind_mime_mismatch"));
    }
    let mut tx = app.db.begin().await?;
    let request_body = json!({"kind":q.kind,"sha256":q.sha256,"parent":q.parent});
    let (operation, cached) = operation_start(
        &mut tx,
        &a,
        &h,
        format!("/api/v1/studies/{id}/assets"),
        &request_body,
    )
    .await?;
    if let Some(cached) = cached {
        return Ok(Json(
            serde_json::from_value::<Asset>(cached)
                .map_err(|_| bad("invalid_operation_receipt"))?,
        ));
    }
    let r = sqlx::query("SELECT status FROM app.studies WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    if r.get::<String, _>("status") == "approved" && q.kind == "original" {
        return Err(Error(StatusCode::CONFLICT, "revision_immutable"));
    }
    let asset_id = entity_id(&h, "x-entity-id")?;
    // Serialize UUID ownership across studies as well as operation retries. A
    // filesystem orphan is adoptable only when no committed conflicting asset
    // exists; it must never become a route to reassign an original to a study.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("asset:{asset_id}"))
        .execute(&mut *tx)
        .await?;
    if let Some(row) = sqlx::query("SELECT * FROM app.assets WHERE id=$1")
        .bind(asset_id)
        .fetch_optional(&mut *tx)
        .await?
    {
        if row.get::<Uuid, _>("study_id") != id
            || row.get::<String, _>("kind") != q.kind
            || row.get::<String, _>("sha256") != q.sha256
            || row.get::<Option<Uuid>, _>("parent_asset_id") != q.parent
            || row.get::<i64, _>("byte_size") != bytes.len() as i64
            || row.get::<String, _>("mime_type") != mime
        {
            return Err(Error(StatusCode::CONFLICT, "asset_id_conflict"));
        }
        return operation_finish(tx, &a, operation, asset_row(&row)).await;
    }
    if q.kind == "derived" {
        let parent = q.parent.ok_or(bad("parent_required"))?;
        sqlx::query("SELECT id FROM app.assets WHERE id=$1 AND study_id=$2 AND kind='original'")
            .bind(parent)
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    }
    if let Some(row) =
        sqlx::query("SELECT * FROM app.assets WHERE study_id=$1 AND sha256=$2 AND kind=$3 AND ($3<>'derived' OR parent_asset_id=$4)")
            .bind(id)
            .bind(&q.sha256)
            .bind(&q.kind)
            .bind(q.parent)
            .fetch_optional(&mut *tx)
            .await?
    {
        return operation_finish(tx, &a, operation, asset_row(&row)).await;
    }
    let key = asset_id.simple().to_string();
    crate::storage::publish_recoverable(&app.storage, &key, &bytes)
        .await
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                Error(StatusCode::CONFLICT, "asset_id_conflict")
            } else {
                error.into()
            }
        })?;
    let result=sqlx::query("INSERT INTO app.assets(id,study_id,kind,storage_key,sha256,byte_size,mime_type,import_source,parent_asset_id) VALUES($1,$2,$3,$4,$5,$6,$7,'client_import',$8) RETURNING *").bind(asset_id).bind(id).bind(&q.kind).bind(&key).bind(&q.sha256).bind(bytes.len() as i64).bind(mime).bind(q.parent).fetch_one(&mut *tx).await;
    let row = match result {
        Ok(r) => r,
        // Never delete final asset paths from an HTTP failure branch. An
        // uncommitted file remains available for the recovery inventory.
        Err(e) => return Err(e.into()),
    };
    audit(&mut tx, &a, asset_id, "asset.uploaded", 1).await?;
    operation_finish(tx, &a, operation, asset_row(&row)).await
}
async fn download(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Response> {
    let a = actor(&app, &h).await?;
    let r=sqlx::query("SELECT a.* FROM app.assets a JOIN app.studies s ON s.id=a.study_id JOIN app.patients p ON p.id=s.patient_id WHERE a.id=$1 AND p.organization_id=$2").bind(id).bind(a.org).fetch_one(&app.db).await?;
    study(&app, &a, r.get("study_id")).await?;
    let key: String = r.get("storage_key");
    if key != id.simple().to_string() {
        return Err(bad("invalid_storage_key"));
    }
    crate::asset_download::response(
        app.storage.join(key),
        id,
        r.get("sha256"),
        r.get("byte_size"),
        r.get("mime_type"),
        &h,
    )
    .await
}
async fn thumbnail(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Response> {
    let a = actor(&app, &h).await?;
    let r = sqlx::query("SELECT a.* FROM app.assets a JOIN app.studies s ON s.id=a.study_id JOIN app.patients p ON p.id=s.patient_id WHERE a.id=$1 AND p.organization_id=$2").bind(id).bind(a.org).fetch_one(&app.db).await?;
    study(&app, &a, r.get("study_id")).await?;
    if r.get::<String, _>("kind") != "original" {
        return Err(bad("thumbnail_requires_original"));
    }
    let key: String = r.get("storage_key");
    if key != id.simple().to_string() {
        return Err(bad("invalid_storage_key"));
    }
    crate::thumbnails::response(
        app.storage.clone(),
        key,
        r.get("sha256"),
        r.get("byte_size"),
        &h,
    )
    .await
}
async fn templates(State(app): State<Arc<App>>, h: HeaderMap) -> Result<Json<Vec<Template>>> {
    let a = actor(&app, &h).await?;
    let modern =
        h.get("x-m53-report-format").and_then(|v| v.to_str().ok()) == Some("letterhead-v1");
    let rows=sqlx::query("SELECT t.id,v.id AS version_id,v.version,v.layout FROM app.report_templates t JOIN app.report_template_versions v ON v.template_id=t.id WHERE v.published_at IS NOT NULL AND t.organization_id=$1 AND (v.clinic_branch_guid IS NULL OR ($2 AND v.clinic_branch_guid=(SELECT x.branch_guid FROM app.organization_clinic_defaults x JOIN app.clinic_branches cb ON cb.organization_id=x.organization_id AND cb.guid=x.branch_guid AND cb.active WHERE x.organization_id=$1))) ORDER BY v.version DESC,t.id,v.id").bind(a.org).bind(modern).fetch_all(&app.db).await?;
    let mut values = rows
        .iter()
        .map(|r| {
            Ok(Template {
                id: r.get("id"),
                version_id: r.get("version_id"),
                version: r.get("version"),
                layout: serde_json::from_value(r.get("layout"))
                    .map_err(|_| bad("invalid_template"))?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if !modern {
        for t in &mut values {
            t.layout.letterhead = None;
        }
    }
    Ok(Json(values))
}
async fn save_template(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Json(mut layout): Json<ReportLayout>,
) -> Result<Json<Template>> {
    let a = actor(&app, &h).await?;
    if let Some(h) = layout.letterhead.as_mut() {
        h.clinic = None;
    }
    if layout.title.trim().is_empty()
        || layout.title.len() > 256
        || !(1..=3).contains(&layout.photo_columns)
        || layout.blocks.is_empty()
        || layout.blocks.len() > 20
        || !crate::clinic::valid_letterhead(&layout)
    {
        return Err(bad("invalid_layout"));
    }
    let id = entity_id(&h, "x-entity-id")?;
    let version_id = entity_id(&h, "x-version-id")?;
    let mut tx = app.db.begin().await?;
    crate::clinic::org_lock(&mut tx, a.org).await?;
    let request_body = json!(layout);
    let (operation, cached) = operation_start(
        &mut tx,
        &a,
        &h,
        "/api/v1/templates".to_string(),
        &request_body,
    )
    .await?;
    if let Some(cached) = cached {
        return Ok(Json(
            serde_json::from_value::<Template>(cached)
                .map_err(|_| bad("invalid_operation_receipt"))?,
        ));
    }
    sqlx::query("INSERT INTO app.report_templates(id,name,organization_id) VALUES($1,$2,$3)")
        .bind(id)
        .bind(&layout.title)
        .bind(a.org)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO app.report_template_versions(id,template_id,version,schema_version,layout,styles,published_at) VALUES($1,$2,1,1,$3,'',now())").bind(version_id).bind(id).bind(json!(layout)).execute(&mut *tx).await?;
    crate::clinic::publish_branch_versions(&mut tx, a.org).await?;
    audit(&mut tx, &a, id, "template.published", 1).await?;
    operation_finish(
        tx,
        &a,
        operation,
        Template {
            id,
            version_id,
            version: 1,
            layout,
        },
    )
    .await
}
async fn save_report(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<SaveReport>,
) -> Result<Json<Value>> {
    let a = actor(&app, &h).await?;
    study(&app, &a, id).await?;
    if std::env::var("M53_LEGACY_REPORT_ASSETS").as_deref() != Ok("1") {
        return Err(Error(StatusCode::CONFLICT, "client_pdf_upload_required"));
    }
    let mut tx = app.db.begin().await?;
    let request_body = json!(input);
    let (operation, cached) = operation_start(
        &mut tx,
        &a,
        &h,
        format!("/api/v1/studies/{id}/reports"),
        &request_body,
    )
    .await?;
    if let Some(cached) = cached {
        return Ok(Json(
            serde_json::from_value::<Value>(cached)
                .map_err(|_| bad("invalid_operation_receipt"))?,
        ));
    }
    crate::clinic::check_template_scope(&mut tx, a.org, a.credential, input.template_version_id)
        .await?;
    let revision = sqlx::query(
        "SELECT form FROM app.protocol_revisions WHERE id=$1 AND study_id=$2 FOR UPDATE",
    )
    .bind(input.revision_id)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    let template=sqlx::query("SELECT v.layout FROM app.report_template_versions v JOIN app.report_templates t ON t.id=v.template_id WHERE v.id=$1 AND v.published_at IS NOT NULL AND t.organization_id=$2").bind(input.template_version_id).bind(a.org).fetch_one(&mut *tx).await?;
    for (asset, kind) in [(input.html_asset_id, "html"), (input.pdf_asset_id, "pdf")] {
        let row = sqlx::query("SELECT * FROM app.assets WHERE id=$1 AND study_id=$2 AND kind=$3")
            .bind(asset)
            .bind(id)
            .bind(kind)
            .fetch_one(&mut *tx)
            .await?;
        if !input.manifest.iter().any(|m| {
            m.id == asset
                && m.sha256 == row.get::<String, _>("sha256")
                && m.byte_size == row.get::<i64, _>("byte_size")
        }) {
            return Err(bad("invalid_manifest"));
        }
    }

    for item in &input.manifest {
        let row = sqlx::query(
            "SELECT sha256,byte_size,mime_type,kind FROM app.assets WHERE id=$1 AND study_id=$2",
        )
        .bind(item.id)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if item.study_id != id
            || item.sha256 != row.get::<String, _>("sha256")
            || item.byte_size != row.get::<i64, _>("byte_size")
            || item.mime_type != row.get::<String, _>("mime_type")
            || item.kind != row.get::<String, _>("kind")
        {
            return Err(bad("invalid_manifest"));
        }
    }
    if input.client_version.is_empty()
        || input.renderer_version.is_empty()
        || input.client_version.len() > 128
        || input.renderer_version.len() > 128
    {
        return Err(bad("invalid_renderer_version"));
    }
    let snapshot = &input.snapshot;
    let current_row = sqlx::query("SELECT s.* FROM app.studies s JOIN app.patients p ON p.id=s.patient_id WHERE s.id=$1 AND p.organization_id=$2 AND (s.organization_id IS NULL OR s.organization_id=$2)")
        .bind(id).bind(a.org).fetch_one(&mut *tx).await?;
    let current = study_row(&current_row);
    let patient =
        sqlx::query("SELECT p.*,p.birth_date::text AS birth_date_text,(SELECT e.external_id FROM app.patient_external_ids e WHERE e.patient_id=p.id ORDER BY e.source LIMIT 1) AS external_id FROM app.patients p WHERE p.id=$1")
            .bind(current.patient_id)
            .fetch_one(&mut *tx)
            .await?;
    let annotations: Value =
        sqlx::query_scalar("SELECT data FROM app.protocol_annotations WHERE revision_id=$1")
            .bind(input.revision_id)
            .fetch_one(&mut *tx)
            .await?;
    if snapshot.study.id != id
        || snapshot.revision_id != Some(input.revision_id)
        || snapshot.study.row_version != current.row_version
        || !crate::anamnesis::snapshot_matches(serde_json::to_value(&snapshot.protocol).unwrap(), &revision.get::<Value, _>("form"))
        || serde_json::to_value(&snapshot.patient).unwrap() != json!(patient_row(&patient))
        || json!(snapshot.annotations) != annotations
    {
        return Err(Error(StatusCode::CONFLICT, "report_snapshot_conflict"));
    }
    let report_id = entity_id(&h, "x-entity-id")?;
    sqlx::query("INSERT INTO app.reports(id,study_id,protocol_revision_id,template_version_id,html_asset_id,pdf_asset_id,client_version,renderer_version,schema_version,data_snapshot,template_snapshot,manifest) VALUES($1,$2,$3,$4,$5,$6,$7,$8,1,$9,$10,$11)").bind(report_id).bind(id).bind(input.revision_id).bind(input.template_version_id).bind(input.html_asset_id).bind(input.pdf_asset_id).bind(input.client_version).bind(input.renderer_version).bind(json!(input.snapshot)).bind(template.get::<Value,_>("layout")).bind(json!({"files":input.manifest})).execute(&mut *tx).await?;
    audit(&mut tx, &a, report_id, "report.created", 1).await?;
    operation_finish(tx, &a, operation, json!({"id":report_id})).await
}
async fn publications(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<Publication>>> {
    let a = actor(&app, &h).await?;
    study(&app, &a, id).await?;
    let rows=sqlx::query("SELECT p.id,p.study_id,p.state,n.state AS notification_state FROM app.study_publications p LEFT JOIN app.notification_jobs n ON n.publication_id=p.id WHERE p.study_id=$1 ORDER BY p.version DESC").bind(id).fetch_all(&app.db).await?;
    Ok(Json(
        rows.iter()
            .map(|r| Publication {
                notification_state: r.get("notification_state"),
                id: r.get("id"),
                study_id: r.get("study_id"),
                state: r.get("state"),
            })
            .collect(),
    ))
}
async fn publish(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    let actor = actor(&app, &h).await?;
    study(&app, &actor, id).await?;
    Err(Error(StatusCode::GONE, "dicom_publication_retired"))
}

async fn dictionary_versions(
    State(app): State<Arc<App>>,
    h: HeaderMap,
) -> Result<Json<Vec<TerminologyVersion>>> {
    actor(&app, &h).await?;
    let rows = sqlx::query("SELECT * FROM app.terminology_versions ORDER BY code")
        .fetch_all(&app.db)
        .await?;
    Ok(Json(
        rows.iter()
            .map(|r| TerminologyVersion {
                id: r.get("id"),
                code: r.get("code"),
                source: r.get("source"),
                clinically_approved: r.get("clinically_approved"),
            })
            .collect(),
    ))
}

async fn orders(State(app): State<Arc<App>>, h: HeaderMap) -> Result<Json<Vec<Order>>> {
    let a = actor(&app, &h).await?;
    let rows=sqlx::query("SELECT o.id,o.patient_id,o.external_order_id,o.status FROM app.orders o JOIN app.patients p ON p.id=o.patient_id WHERE p.organization_id=$1 ORDER BY o.external_order_id LIMIT 500").bind(a.org).fetch_all(&app.db).await?;
    Ok(Json(
        rows.iter()
            .map(|r| Order {
                id: r.get("id"),
                patient_id: r.get("patient_id"),
                external_order_id: r.get("external_order_id"),
                status: r.get("status"),
            })
            .collect(),
    ))
}

/// Authorize at the transaction's commit boundary, after any business-lock wait.
/// Short SHARE locks serialize the final check with revocation triggers. Keep
/// credential -> user -> workstation -> session order, shared with grant issuance.
/// Do not add role/doctor row locks after the user: their revocation triggers
/// update that user and revoke sessions/grants, and would invert login's order.
async fn lock_and_revalidate_authority(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, a: &Actor, grant: bool) -> Result<()> {
    sqlx::query("SELECT id FROM app.api_credentials WHERE id=$1 FOR SHARE")
        .bind(a.credential).fetch_optional(&mut **tx).await?.ok_or(Error(StatusCode::UNAUTHORIZED,"invalid_workstation"))?;
    sqlx::query(if grant {"SELECT id FROM app.users WHERE id=$1 FOR UPDATE"} else {"SELECT id FROM app.users WHERE id=$1 FOR SHARE"})
        .bind(a.user).fetch_optional(&mut **tx).await?.ok_or(Error(StatusCode::UNAUTHORIZED,"invalid_user_session"))?;
    sqlx::query("SELECT w.id FROM app.workstations w JOIN app.api_credentials c ON c.workstation_id=w.id WHERE c.id=$1 FOR SHARE OF w")
        .bind(a.credential).fetch_optional(&mut **tx).await?.ok_or(Error(StatusCode::UNAUTHORIZED,"invalid_workstation"))?;
    sqlx::query("SELECT token_hash FROM app.user_sessions WHERE token_hash=$1 FOR SHARE")
        .bind(&a.session_hash).fetch_optional(&mut **tx).await?.ok_or(Error(StatusCode::UNAUTHORIZED,"invalid_user_session"))?;
    let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app.user_sessions s JOIN app.users u ON u.id=s.user_id JOIN app.api_credentials c ON c.id=s.credential_id JOIN app.workstations w ON w.id=c.workstation_id WHERE s.token_hash=$1 AND u.id=$2 AND c.id=$3 AND u.organization_id=$4 AND w.organization_id=$4 AND s.expires_at>clock_timestamp() AND s.revoked_at IS NULL AND NOT u.disabled AND c.purpose='client' AND c.revoked_at IS NULL AND (c.expires_at IS NULL OR c.expires_at>clock_timestamp()) AND 'clinical'=ANY(c.permissions) AND w.disabled_at IS NULL AND (u.doctor_id IS NULL OR EXISTS(SELECT 1 FROM app.doctors d WHERE d.id=u.doctor_id AND d.organization_id=u.organization_id AND d.active)) AND EXISTS(SELECT 1 FROM app.user_roles r WHERE r.user_id=u.id AND r.role_code='doctor'))")
        .bind(&a.session_hash).bind(a.user).bind(a.credential).bind(a.org).fetch_one(&mut **tx).await?;
    if !valid { return Err(Error(StatusCode::UNAUTHORIZED,"invalid_user_session")); }
    Ok(())
}


pub(crate) async fn revalidate_write_access(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, actor: &Actor) -> Result<()> {
    lock_and_revalidate_authority(tx, actor, false).await
}

pub(crate) async fn revalidate_grant_access(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, actor: &Actor) -> Result<()> {
    lock_and_revalidate_authority(tx, actor, true).await
}

pub(crate) struct OperationToken {
    id: Option<Uuid>,
    scope: String,
    digest: String,
}
fn entity_id(headers: &HeaderMap, name: &str) -> Result<Uuid> {
    match headers.get(name) {
        None => Ok(Uuid::new_v4()),
        Some(value) => value
            .to_str()
            .ok()
            .and_then(|v| Uuid::parse_str(v).ok())
            .filter(|id| !id.is_nil())
            .ok_or(bad("invalid_client_entity_id")),
    }
}
pub(crate) async fn operation_start(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    actor: &Actor,
    headers: &HeaderMap,
    scope: String,
    body: &Value,
) -> Result<(OperationToken, Option<Value>)> {
    let id = match headers.get("x-operation-id") {
        None => None,
        Some(value) => Some(
            value
                .to_str()
                .ok()
                .and_then(|v| Uuid::parse_str(v).ok())
                .filter(|id| !id.is_nil())
                .ok_or(bad("invalid_operation_id"))?,
        ),
    };
    let digest=hash(m53_contracts::canonical_json::to_vec(&json!({"scope":scope,"body":body,"entity_id":headers.get("x-entity-id").and_then(|v|v.to_str().ok()),"version_id":headers.get("x-version-id").and_then(|v|v.to_str().ok())})).unwrap());
    let mut cached = None;
    if let Some(id) = id {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("{}:{}", actor.user, id))
            .execute(&mut **tx)
            .await?;
        if let Some(row)=sqlx::query("SELECT scope,request_sha256,response FROM app.client_operations WHERE user_id=$2 AND operation_id=$3 AND $1::uuid IS NOT NULL").bind(actor.credential).bind(actor.user).bind(id).fetch_optional(&mut **tx).await?{
   if row.get::<String,_>("scope")!=scope || row.get::<String,_>("request_sha256")!=digest{return Err(Error(StatusCode::CONFLICT,"operation_id_conflict"));}cached=Some(row.get("response"));
  }
    }
    if cached.is_some() {
        revalidate_write_access(tx, actor).await?;
    }
    Ok((OperationToken { id, scope, digest }, cached))
}
pub(crate) async fn operation_finish<T: serde::Serialize>(
    mut tx: sqlx::Transaction<'_, sqlx::Postgres>,
    actor: &Actor,
    operation: OperationToken,
    response: T,
) -> Result<Json<T>> {
    revalidate_write_access(&mut tx, actor).await?;
    if let Some(id) = operation.id {
        sqlx::query("INSERT INTO app.client_operations(credential_id,user_id,operation_id,scope,request_sha256,response) VALUES($1,$2,$3,$4,$5,$6)").bind(actor.credential).bind(actor.user).bind(id).bind(operation.scope).bind(operation.digest).bind(json!(response)).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(Json(response))
}
async fn operation_receipt(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    let a = actor(&app, &h).await?;
    let row=sqlx::query("SELECT scope,request_sha256,response FROM app.client_operations WHERE user_id=$2 AND operation_id=$3 AND $1::uuid IS NOT NULL").bind(a.credential).bind(a.user).bind(id).fetch_one(&app.db).await?;
    Ok(Json(
        json!({"operation_id":id,"scope":row.get::<String,_>("scope"),"request_sha256":row.get::<String,_>("request_sha256"),"response":row.get::<Value,_>("response")}),
    ))
}
