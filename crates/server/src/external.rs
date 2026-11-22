use crate::api::{App, Error, hash};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use m53_contracts::workflow::*;
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use std::sync::Arc;
use uuid::Uuid;
type Result<T> = std::result::Result<T, Error>;
struct Integration {
    org: Uuid,
    source: String,
}
async fn authenticate(app: &App, h: &HeaderMap) -> Result<Integration> {
    let key = h
        .get("x-integration-key")
        .and_then(|v| v.to_str().ok())
        .ok_or(Error(StatusCode::UNAUTHORIZED, "integration_key_required"))?;
    let row=sqlx::query("SELECT w.organization_id,w.id FROM app.api_credentials c JOIN app.workstations w ON w.id=c.workstation_id WHERE c.verifier=$1 AND c.purpose='1c' AND c.revoked_at IS NULL AND (c.expires_at IS NULL OR c.expires_at>now()) AND 'orders'=ANY(c.permissions)").bind(hash(key)).fetch_optional(&app.db).await?.ok_or(Error(StatusCode::UNAUTHORIZED,"invalid_integration_key"))?;
    Ok(Integration {
        org: row.get("organization_id"),
        source: format!("1c:{}", row.get::<Uuid, _>("id")),
    })
}
pub(crate) async fn authorize_body(app:&App,h:&HeaderMap)->Result<()> {
    authenticate(app,h).await.map(|_|())
}

async fn request(
    tx: &mut Transaction<'_, Postgres>,
    h: &HeaderMap,
    source: &str,
    body: &Value,
) -> Result<(String, String, Option<Value>)> {
    let key = h
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .ok_or(Error(StatusCode::BAD_REQUEST, "idempotency_key_required"))?
        .to_owned();
    let digest = hash(serde_json::to_vec(body).unwrap());
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("{source}:{key}"))
        .execute(&mut **tx)
        .await?;
    let row=sqlx::query("SELECT request_sha256,result FROM app.integration_requests WHERE source=$1 AND idempotency_key=$2").bind(source).bind(&key).fetch_optional(&mut **tx).await?;
    if let Some(row) = row {
        if row.get::<String, _>("request_sha256") != digest {
            return Err(Error(StatusCode::CONFLICT, "idempotency_conflict"));
        }
        return Ok((key, digest, row.get("result")));
    }
    Ok((key, digest, None))
}
async fn result(
    tx: &mut Transaction<'_, Postgres>,
    source: &str,
    key: &str,
    digest: &str,
    value: &Value,
) -> Result<()> {
    sqlx::query("INSERT INTO app.integration_requests(source,idempotency_key,request_sha256,state,result) VALUES($1,$2,$3,'complete',$4)").bind(source).bind(key).bind(digest).bind(value).execute(&mut **tx).await?;
    Ok(())
}
pub async fn patient(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Json(input): Json<ExternalPatient>,
) -> Result<Json<Value>> {
    let integration = authenticate(&app, &h).await?;
    if input.source_revision < 1
        || input.external_id.trim().is_empty()
        || input.external_id.len() > 256
        || input.patient.family_name.trim().is_empty()
        || input.patient.given_name.trim().is_empty()
    {
        return Err(Error(StatusCode::BAD_REQUEST, "invalid_external_patient"));
    }
    let source = format!("{}:patients", integration.source);
    let body = json!(input);
    let mut tx = app.db.begin().await?;
    let (key, digest, cached) = request(&mut tx, &h, &source, &body).await?;
    if let Some(value) = cached {
        return Ok(Json(value));
    }
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("{}:{}", integration.source, input.external_id))
        .execute(&mut *tx)
        .await?;
    let old = sqlx::query(
        "SELECT * FROM app.patient_external_ids WHERE source=$1 AND external_id=$2 FOR UPDATE",
    )
    .bind(&integration.source)
    .bind(&input.external_id)
    .fetch_optional(&mut *tx)
    .await?;
    let id = if let Some(old) = old {
        let revision: i64 = old.get("source_revision");
        if revision > input.source_revision
            || (revision == input.source_revision && old.get::<String, _>("source_hash") != digest)
        {
            return Err(Error(StatusCode::CONFLICT, "source_version_conflict"));
        }
        old.get("patient_id")
    } else {
        Uuid::new_v4()
    };
    let p = &input.patient;
    sqlx::query("INSERT INTO app.patients(id,organization_id,family_name,given_name,patronymic,birth_date) VALUES($1,$2,$3,$4,$5,$6::text::date) ON CONFLICT(id) DO UPDATE SET family_name=excluded.family_name,given_name=excluded.given_name,patronymic=excluded.patronymic,birth_date=excluded.birth_date").bind(id).bind(integration.org).bind(&p.family_name).bind(&p.given_name).bind(&p.patronymic).bind(&p.birth_date).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO app.patient_external_ids(patient_id,source,external_id,dicom_issuer,source_revision,source_hash) VALUES($1,$2,$3,$2,$4,$5) ON CONFLICT(source,external_id) DO UPDATE SET source_revision=excluded.source_revision,source_hash=excluded.source_hash").bind(id).bind(&integration.source).bind(&input.external_id).bind(input.source_revision).bind(&digest).execute(&mut *tx).await?;
    let value = json!({"patient_id":id,"source_revision":input.source_revision});
    result(&mut tx, &source, &key, &digest, &value).await?;
    tx.commit().await?;
    Ok(Json(value))
}
pub async fn order(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Json(input): Json<ExternalOrder>,
) -> Result<Json<Value>> {
    let integration = authenticate(&app, &h).await?;
    if input.source_revision < 1
        || input.external_order_id.trim().is_empty()
        || !matches!(input.status.as_str(), "active" | "cancelled" | "completed")
    {
        return Err(Error(StatusCode::BAD_REQUEST, "invalid_order"));
    }
    let source = format!("{}:orders", integration.source);
    let mut tx = app.db.begin().await?;
    let (key, digest, cached) = request(&mut tx, &h, &source, &json!(input)).await?;
    if let Some(value) = cached {
        return Ok(Json(value));
    }
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("{}:{}", source, input.external_order_id))
        .execute(&mut *tx)
        .await?;
    let patient_id:Uuid=sqlx::query_scalar("SELECT e.patient_id FROM app.patient_external_ids e JOIN app.patients p ON p.id=e.patient_id WHERE e.source=$1 AND e.external_id=$2 AND p.organization_id=$3").bind(&integration.source).bind(&input.external_patient_id).bind(integration.org).fetch_one(&mut *tx).await?;
    let old=sqlx::query("SELECT id,source_revision,source_hash FROM app.orders WHERE source=$1 AND external_order_id=$2 FOR UPDATE").bind(&integration.source).bind(&input.external_order_id).fetch_optional(&mut *tx).await?;
    let id = if let Some(old) = old {
        let revision: i64 = old.get("source_revision");
        if revision > input.source_revision
            || (revision == input.source_revision && old.get::<String, _>("source_hash") != digest)
        {
            return Err(Error(StatusCode::CONFLICT, "source_version_conflict"));
        }
        old.get("id")
    } else {
        Uuid::new_v4()
    };
    sqlx::query("INSERT INTO app.orders(id,patient_id,source,external_order_id,source_version,status,source_revision,source_hash) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(id) DO UPDATE SET patient_id=excluded.patient_id,source_version=excluded.source_version,status=excluded.status,source_revision=excluded.source_revision,source_hash=excluded.source_hash").bind(id).bind(patient_id).bind(&integration.source).bind(input.external_order_id).bind(input.source_revision.to_string()).bind(input.status).bind(input.source_revision).bind(digest.clone()).execute(&mut *tx).await?;
    let value = json!({"order_id":id,"source_revision":input.source_revision});
    result(&mut tx, &source, &key, &digest, &value).await?;
    tx.commit().await?;
    Ok(Json(value))
}
pub async fn publications(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    let integration = authenticate(&app, &h).await?;
    let rows=sqlx::query("SELECT pub.id,pub.version,pub.state,pub.manifest FROM app.study_publications pub JOIN app.studies s ON s.id=pub.study_id JOIN app.patients p ON p.id=s.patient_id WHERE pub.study_id=$1 AND p.organization_id=$2 ORDER BY pub.version DESC").bind(id).bind(integration.org).fetch_all(&app.db).await?;
    Ok(Json(
        json!({"publications":rows.iter().map(|r|json!({"id":r.get::<Uuid,_>("id"),"version":r.get::<i32,_>("version"),"state":r.get::<String,_>("state"),"manifest":if r.get::<String,_>("state")=="complete"{r.get::<Value,_>("manifest")}else{Value::Null}})).collect::<Vec<_>>()}),
    ))
}

/// Integration can retrieve only files included in a completed publication for its organization.
pub async fn asset(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<axum::response::Response> {
    use axum::response::IntoResponse;
    let integration = authenticate(&app, &h).await?;
    let row=sqlx::query("SELECT a.storage_key,a.sha256,a.mime_type FROM app.assets a JOIN app.studies s ON s.id=a.study_id JOIN app.patients p ON p.id=s.patient_id WHERE a.id=$1 AND p.organization_id=$2 AND EXISTS(SELECT 1 FROM app.study_publications pub WHERE pub.study_id=s.id AND pub.state='complete' AND pub.manifest->'files' @> jsonb_build_array(jsonb_build_object('id',a.id::text)))").bind(id).bind(integration.org).fetch_one(&app.db).await?;
    let key: String = row.get("storage_key");
    if key != id.simple().to_string() {
        return Err(Error(StatusCode::BAD_REQUEST, "invalid_storage_key"));
    }
    let bytes = tokio::fs::read(app.storage.join(key)).await?;
    if hash(&bytes) != row.get::<String, _>("sha256") {
        return Err(Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "asset_integrity_error",
        ));
    }
    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                row.get::<String, _>("mime_type"),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{id}\""),
            ),
            (
                axum::http::header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; sandbox".into(),
            ),
        ],
        bytes,
    )
        .into_response())
}
