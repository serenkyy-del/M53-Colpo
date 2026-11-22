//! Server transaction attribution and bounded, explicitly client-reported UI events.
use crate::api::{self, Actor, App, Error, actor, credential, hash};
use axum::{
    Json,
    extract::{MatchedPath, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;
type Result<T> = std::result::Result<T, Error>;
fn uuid_header(h: &HeaderMap, name: &str) -> Option<Uuid> {
    h.get(name)
        .and_then(|s| s.to_str().ok())
        .and_then(|s| Uuid::parse_str(s).ok())
        .filter(|s| !s.is_nil())
}
fn action(method: &str, route: &str) -> &'static str {
    if route.contains("phrase-template") {
        return if method == "GET" {
            "api.phrases.read"
        } else {
            "api.phrases.write"
        };
    }
    if route.contains("activity-events") {
        return "api.activity.submit";
    }
    if route == "/api/v1/workstation" {
        return "api.workstation.read";
    }
    if route.ends_with("/login") {
        return "api.auth.login";
    }
    if route.ends_with("/logout") {
        return "api.auth.logout";
    }
    if route.ends_with("/check") {
        return "api.updates.check";
    }
    if route.contains("client-updates") && route.contains("/files/") {
        return "api.updates.download";
    }
    if route.contains("client-updates") {
        return "api.updates.status";
    }
    if route.ends_with("events") {
        return "api.events.subscribe";
    }
    if route == "unmatched" {
        return "api.request";
    }
    if method != "GET" && method != "HEAD" {
        if route == "/api/colposcopy/tasks" {
            return "api.tasks.receive";
        }
        if route.ends_with("/approve") {
            return "api.protocol.approve";
        }
        if route.ends_with("/reopen") {
            return "api.protocol.reopen";
        }
        if route.ends_with("/protocol") {
            return "api.protocol.save";
        }
        if route.ends_with("/claim") {
            return "api.task.claim";
        }
        if route.ends_with("/refresh-from-1c") {
            return "api.patient.refresh";
        }
        if route == "/api/v1/patients" {
            return "api.patient.create";
        }
        if route == "/api/v1/studies" {
            return "api.study.create";
        }
        if route.ends_with("/assets") {
            return "api.image.upload";
        }
        if route.contains("/results/1c") {
            return "api.result.send";
        }
    }
    if method != "GET" && method != "HEAD" {
        return "api.clinical.write";
    }
    if route.contains("/patients") {
        return "api.patients.read";
    }
    if route.contains("/tasks") {
        return "api.tasks.read";
    }
    if route.contains("/assets") {
        return "api.assets.read";
    }
    if route.contains("/results") || route.contains("/reports") {
        return "api.report.read";
    }
    if route.contains("/studies") {
        return "api.studies.read";
    }
    "api.catalog.read"
}
pub async fn request_audit(
    State(app): State<Arc<App>>,
    mut request: Request,
    next: Next,
) -> Response {
    if request.uri().path() == "/health" {
        return next.run(request).await;
    }
    let id = Uuid::new_v4();
    let started = Instant::now();
    request.headers_mut().insert(
        "x-m53-request-id",
        id.to_string().parse().expect("UUID header"),
    );
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|s| s.as_str())
        .unwrap_or("unmatched")
        .to_owned();
    let method = request.method().as_str().to_owned();
    let requested = request
        .uri()
        .path()
        .split('/')
        .find_map(|s| Uuid::parse_str(s).ok());
    let operation = uuid_header(request.headers(), "x-operation-id");
    let key_hash = request
        .headers()
        .get("x-workstation-key")
        .and_then(|v| v.to_str().ok())
        .map(hash);
    let token_hash = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(hash);
    let integration_hash = request
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(hash);
    // Attribution and durable request-start are one database statement. This
    // does not authorize the handler: it independently rechecks current rights.
    let save=sqlx::query(r#"
WITH workstation_key AS (
 SELECT c.id,w.organization_id,w.id AS workstation_id,
  (c.revoked_at IS NULL AND (c.expires_at IS NULL OR c.expires_at>now()) AND 'clinical'=ANY(c.permissions) AND w.disabled_at IS NULL) AS active
 FROM app.api_credentials c JOIN app.workstations w ON w.id=c.workstation_id
 WHERE c.verifier=$1 AND c.purpose='client'
), physician AS (
 SELECT u.id FROM workstation_key k JOIN app.user_sessions s ON s.credential_id=k.id JOIN app.users u ON u.id=s.user_id
 WHERE k.active AND s.token_hash=$2 AND s.expires_at>now() AND s.revoked_at IS NULL AND NOT u.disabled AND u.organization_id=k.organization_id
 AND (u.doctor_id IS NULL OR EXISTS(SELECT 1 FROM app.doctors d WHERE d.id=u.doctor_id AND d.organization_id=u.organization_id AND d.active))
 AND EXISTS(SELECT 1 FROM app.user_roles r WHERE r.user_id=u.id AND r.role_code='doctor')
), integration_key AS (
 SELECT c.id,c.organization_id FROM app.integration_task_credentials c JOIN app.integration_sources s ON s.id=c.source_id
 WHERE NOT EXISTS(SELECT 1 FROM workstation_key) AND c.key_sha256=$3 AND c.revoked_at IS NULL AND (c.expires_at IS NULL OR c.expires_at>now()) AND s.enabled
)
INSERT INTO app.api_request_log(request_id,organization_id,credential_id,integration_credential_id,workstation_id,user_id,doctor_id,workstation_name,key_name,user_name,doctor_name,action_code,action_name,method,route,operation_id,requested_entity_id,credential_active)
SELECT $4,coalesce(k.organization_id,ik.organization_id),k.id,ik.id,w.id,u.id,u.doctor_id,w.name,c.name,u.display_name,d.display_name,a.code,a.name,$6,$7,$8,$9,coalesce(k.active,false) OR ik.id IS NOT NULL
FROM app.activity_actions a LEFT JOIN workstation_key k ON true LEFT JOIN integration_key ik ON true LEFT JOIN physician p ON true
LEFT JOIN app.api_credentials c ON c.id=k.id LEFT JOIN app.workstations w ON w.id=k.workstation_id LEFT JOIN app.users u ON u.id=p.id LEFT JOIN app.doctors d ON d.id=u.doctor_id WHERE a.code=$5
"#).bind(key_hash).bind(token_hash).bind(integration_hash).bind(id).bind(action(&method,&route)).bind(&method).bind(&route).bind(operation).bind(requested).execute(&app.db);
    if !matches!(tokio::time::timeout(Duration::from_secs(2),save).await,Ok(Ok(r)) if r.rows_affected()==1)
    {
        eprintln!("request_audit_unavailable");
        return Error(StatusCode::SERVICE_UNAVAILABLE, "request_audit_unavailable").into_response();
    }
    let mut response = next.run(request).await;
    let error_code = response
        .extensions()
        .get::<api::SafeErrorCode>()
        .map(|e| e.0)
        .or_else(|| {
            if response.status().is_client_error() {
                Some("http_client_error")
            } else if response.status().is_server_error() {
                Some("http_server_error")
            } else {
                None
            }
        });
    let finish=sqlx::query("UPDATE app.api_request_log SET finished_at=clock_timestamp(),http_status=$2,duration_ms=$3,safe_error_code=$4 WHERE request_id=$1").bind(id).bind(response.status().as_u16() as i16).bind(started.elapsed().as_millis().min(i64::MAX as u128) as i64).bind(error_code).execute(&app.db);
    if !matches!(
        tokio::time::timeout(Duration::from_secs(2), finish).await,
        Ok(Ok(_))
    ) {
        eprintln!("request_audit_completion_pending");
        response.headers_mut().insert(
            "x-audit-completion",
            "pending".parse().expect("static header"),
        );
    }
    response
        .headers_mut()
        .insert("x-request-id", id.to_string().parse().expect("UUID header"));
    response
}
pub async fn workstation(State(app): State<Arc<App>>, headers: HeaderMap) -> Result<Json<Value>> {
    let (key, _) = credential(&app, &headers).await?;
    let r=sqlx::query("SELECT w.id,w.organization_id,w.name,w.row_version,c.id AS credential,c.name AS key_name,left(c.verifier,12) AS fingerprint,c.expires_at::text AS expires_at FROM app.workstations w JOIN app.api_credentials c ON c.workstation_id=w.id WHERE c.id=$1").bind(key).fetch_one(&app.db).await?;
    Ok(Json(
        json!({"workstation_id":r.get::<Uuid,_>("id"),"organization_id":r.get::<Uuid,_>("organization_id"),"name":r.get::<String,_>("name"),"row_version":r.get::<i64,_>("row_version"),"credential_id":r.get::<Uuid,_>("credential"),"key_name":r.get::<String,_>("key_name"),"key_fingerprint":r.get::<String,_>("fingerprint"),"expires_at":r.get::<Option<String>,_>("expires_at")}),
    ))
}
pub async fn catalog(State(app): State<Arc<App>>, headers: HeaderMap) -> Result<Json<Value>> {
    credential(&app, &headers).await?;
    let rows = sqlx::query(
        "SELECT code,name FROM app.activity_actions WHERE source='client' ORDER BY code",
    )
    .fetch_all(&app.db)
    .await?;
    Ok(Json(
        json!({"version":1,"actions":rows.iter().map(|r|json!({"code":r.get::<String,_>("code"),"name":r.get::<String,_>("name")})).collect::<Vec<_>>()}),
    ))
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClientEvent {
    event_id: Uuid,
    action: String,
    reported_at: i64,
    client_sequence: i64,
    study_id: Option<Uuid>,
    entity_id: Option<Uuid>,
    operation_id: Option<Uuid>,
    field_code: Option<String>,
    reported_credential_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phrase_template_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phrase_template_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    apply_mode: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Batch {
    events: Vec<ClientEvent>,
}
pub async fn events(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(batch): Json<Batch>,
) -> Result<Json<Value>> {
    let a = actor(&app, &headers).await?;
    receive(&app, a.credential, Some(a), batch).await
}
pub async fn workstation_events(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(batch): Json<Batch>,
) -> Result<Json<Value>> {
    let (key, _) = credential(&app, &headers).await?;
    receive(&app, key, None, batch).await
}
async fn receive(app: &App, key: Uuid, a: Option<Actor>, batch: Batch) -> Result<Json<Value>> {
    if batch.events.is_empty() || batch.events.len() > 100 {
        return Err(Error(StatusCode::BAD_REQUEST, "invalid_activity_batch"));
    }
    let mut ids = std::collections::BTreeSet::new();
    for e in &batch.events {
        if e.event_id.is_nil()
            || e.reported_at <= 0
            || e.client_sequence <= 0
            || !ids.insert(e.event_id)
            || e.field_code.as_ref().is_some_and(|s| {
                s.is_empty()
                    || s.len() > 128
                    || !s
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-[]".contains(&b))
            })
        {
            return Err(Error(StatusCode::BAD_REQUEST, "invalid_activity_event"));
        }
        if a.is_none()
            && (!matches!(
                e.action.as_str(),
                "client.app.started" | "client.app.closed"
            ) || e.study_id.is_some()
                || e.entity_id.is_some()
                || e.field_code.is_some()
                || e.phrase_template_id.is_some()
                || e.phrase_template_version.is_some()
                || e.apply_mode.is_some())
        {
            return Err(Error(StatusCode::FORBIDDEN, "activity_requires_physician"));
        }
    }
    let user = a.as_ref().map(|a| a.user);
    let context=sqlx::query("SELECT w.id,w.organization_id,w.name,c.name AS key_name,u.display_name,u.doctor_id,d.display_name AS doctor_name FROM app.api_credentials c JOIN app.workstations w ON w.id=c.workstation_id LEFT JOIN app.users u ON u.id=$2 LEFT JOIN app.doctors d ON d.id=u.doctor_id WHERE c.id=$1").bind(key).bind(user).fetch_one(&app.db).await?;
    let ws: Uuid = context.get("id");
    let org: Uuid = context.get("organization_id");
    let mut checked = std::collections::BTreeSet::new();
    for e in &batch.events {
        if let Some(id) = e.study_id
            && checked.insert(id)
        {
            api::study(
                app,
                a.as_ref()
                    .ok_or(Error(StatusCode::FORBIDDEN, "activity_requires_physician"))?,
                id,
            )
            .await?;
        }
    }
    let mut tx = app.db.begin().await?;
    for e in &batch.events {
        let digest = hash(
            serde_json::to_vec(&json!({"event":e,"user":user})).expect("serializable activity"),
        );
        if let Some(previous) = sqlx::query_scalar::<_,String>("SELECT request_hash FROM app.client_activity_events WHERE workstation_id=$1 AND event_id=$2").bind(ws).bind(e.event_id).fetch_optional(&mut *tx).await? {
            if previous != digest {return Err(Error(StatusCode::CONFLICT,"activity_event_conflict"));}
            continue;
        }
        let phrase_metadata = e.phrase_template_id.is_some()
            || e.phrase_template_version.is_some()
            || e.apply_mode.is_some();
        if e.action == "client.phrase.applied" {
            if e.study_id.is_none()
                || e.field_code.is_none()
                || e.phrase_template_id.is_none_or(|id| id.is_nil())
                || e.phrase_template_version.is_none_or(|v| v < 1)
                || !matches!(e.apply_mode.as_deref(), Some("append" | "replace"))
            {
                return Err(Error(StatusCode::BAD_REQUEST, "invalid_phrase_activity"));
            }
            let allowed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app.phrase_templates t JOIN app.phrase_template_fields f USING(field_code) WHERE t.id=$1 AND t.organization_id=$2 AND (t.scope='shared' OR t.owner_user_id=$3) AND t.field_code=$4 AND t.row_version >= $5 AND f.enabled)")
                .bind(e.phrase_template_id).bind(org).bind(user).bind(&e.field_code).bind(e.phrase_template_version).fetch_one(&mut *tx).await?;
            if !allowed {
                return Err(Error(
                    StatusCode::BAD_REQUEST,
                    "phrase_activity_reference_unavailable",
                ));
            }
        } else if phrase_metadata {
            return Err(Error(
                StatusCode::BAD_REQUEST,
                "unexpected_phrase_activity_metadata",
            ));
        }
        if e.action == "client.phrase.local_applied" {
            if e.study_id.is_none()
                || e.entity_id.is_none_or(|id| id.is_nil())
                || e.field_code.is_none()
            {
                return Err(Error(
                    StatusCode::BAD_REQUEST,
                    "invalid_local_phrase_activity",
                ));
            }
            let enabled:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app.phrase_template_fields WHERE field_code=$1 AND enabled)").bind(&e.field_code).fetch_one(&mut *tx).await?;
            if !enabled {
                return Err(Error(StatusCode::BAD_REQUEST, "unknown_phrase_field"));
            }
        }
        let label: Option<String> = sqlx::query_scalar(
            "SELECT name FROM app.activity_actions WHERE code=$1 AND source='client'",
        )
        .bind(&e.action)
        .fetch_optional(&mut *tx)
        .await?;
        let label = label.ok_or(Error(StatusCode::BAD_REQUEST, "unknown_activity_action"))?;
        if let Some(reported) = e.reported_credential_id {
            let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app.api_credentials WHERE id=$1 AND workstation_id=$2 AND purpose='client')").bind(reported).bind(ws).fetch_one(&mut *tx).await?;
            if !valid {
                return Err(Error(StatusCode::BAD_REQUEST, "activity_key_mismatch"));
            }
        }
        let inserted=sqlx::query("INSERT INTO app.client_activity_events(event_id,workstation_id,credential_id,reported_credential_id,organization_id,user_id,doctor_id,workstation_name,key_name,user_name,doctor_name,action_code,action_name,reported_at,client_sequence,study_id,entity_id,operation_id,field_code,request_hash,phrase_template_id,phrase_template_version,apply_mode) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23) ON CONFLICT(workstation_id,event_id) DO NOTHING")
   .bind(e.event_id).bind(ws).bind(key).bind(e.reported_credential_id).bind(org).bind(user).bind(context.get::<Option<Uuid>,_>("doctor_id")).bind(context.get::<String,_>("name")).bind(context.get::<String,_>("key_name")).bind(context.get::<Option<String>,_>("display_name")).bind(context.get::<Option<String>,_>("doctor_name")).bind(&e.action).bind(label).bind(e.reported_at).bind(e.client_sequence).bind(e.study_id).bind(e.entity_id).bind(e.operation_id).bind(&e.field_code).bind(&digest).bind(e.phrase_template_id).bind(e.phrase_template_version).bind(&e.apply_mode).execute(&mut *tx).await?;
        if inserted.rows_affected() == 0 {
            let previous:String=sqlx::query_scalar("SELECT request_hash FROM app.client_activity_events WHERE workstation_id=$1 AND event_id=$2").bind(ws).bind(e.event_id).fetch_one(&mut *tx).await?;
            if previous != digest {
                return Err(Error(StatusCode::CONFLICT, "activity_event_conflict"));
            }
        }
    }
    tx.commit().await?;
    Ok(Json(
        json!({"accepted_event_ids":batch.events.iter().map(|e|e.event_id).collect::<Vec<_>>()}),
    ))
}
