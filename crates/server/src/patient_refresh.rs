//! Explicit background client refresh by the established source GUID, never by name.
use crate::{
    api::{self, App, Error},
    odata_core::CoreClient,
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use serde_json::{Value, json};
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

pub async fn refresh(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, Error> {
    let actor = api::actor(&app, &headers).await?;
    let source = std::env::var("M53_ODATA_CORE_SOURCE_ID")
        .ok()
        .and_then(|s| s.parse::<Uuid>().ok())
        .filter(|id| !id.is_nil())
        .ok_or(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "patient_refresh_not_configured",
        ))?;
    let row = sqlx::query("SELECT p.row_version,l.external_guid FROM app.patients p LEFT JOIN app.patient_source_links l ON l.patient_id=p.id AND l.organization_id=p.organization_id AND l.source_id=$3 WHERE p.id=$1 AND p.organization_id=$2")
        .bind(id).bind(actor.org).bind(source).fetch_optional(&app.db).await?
        .ok_or(Error(StatusCode::NOT_FOUND,"patient_not_found"))?;
    let guid: Uuid = row
        .get::<Option<Uuid>, _>("external_guid")
        .ok_or(Error(StatusCode::CONFLICT, "patient_1c_guid_required"))?;
    let expected: i64 = row.get("row_version");
    let client = CoreClient::from_env()
        .map_err(|_| {
            Error(
                StatusCode::SERVICE_UNAVAILABLE,
                "patient_refresh_not_configured",
            )
        })?
        .ok_or(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "patient_refresh_not_configured",
        ))?;
    // No transaction or patient lock is held while Core is unavailable/slow.
    let verified = client
        .patient(guid)
        .await
        .map_err(|e| Error(StatusCode::SERVICE_UNAVAILABLE, e.code()))?
        .ok_or(Error(StatusCode::NOT_FOUND, "patient_not_found_in_1c"))?;
    if !verified.active {
        return Err(Error(StatusCode::CONFLICT, "patient_inactive_in_1c"));
    }
    // Recheck access after network I/O; never use an expired/revoked session to write.
    let actor = api::actor(&app, &headers).await?;
    let mut tx = app.db.begin().await?;
    sqlx::query("SET LOCAL lock_timeout='4s'")
        .execute(&mut *tx)
        .await?;
    let current: Option<i64> = sqlx::query_scalar(
        "SELECT row_version FROM app.patients WHERE id=$1 AND organization_id=$2 FOR UPDATE",
    )
    .bind(id)
    .bind(actor.org)
    .fetch_optional(&mut *tx)
    .await?;
    if current != Some(expected) {
        return Err(Error(
            StatusCode::CONFLICT,
            "patient_changed_during_refresh",
        ));
    }
    let linked: Option<Uuid> = sqlx::query_scalar("SELECT l.external_guid FROM app.patient_source_links l JOIN app.integration_sources s ON s.id=l.source_id AND s.organization_id=l.organization_id WHERE l.patient_id=$1 AND l.organization_id=$2 AND l.source_id=$3 AND s.enabled FOR SHARE OF l")
        .bind(id).bind(actor.org).bind(source).fetch_optional(&mut *tx).await?;
    if linked != Some(guid) {
        return Err(Error(StatusCode::CONFLICT, "patient_binding_changed"));
    }
    let patient = sqlx::query("UPDATE app.patients SET family_name=$3,given_name=$4,patronymic=$5,birth_date=$6::date,sex=coalesce($7,sex),origin='1c',identity_state='confirmed' WHERE id=$1 AND organization_id=$2 RETURNING *,birth_date::text AS birth_date_text,clock_timestamp()::text AS checked_at")
        .bind(id).bind(actor.org).bind(&verified.family_name).bind(&verified.given_name)
        .bind(&verified.patronymic).bind(&verified.birth_date).bind(&verified.sex).fetch_one(&mut *tx).await?;
    let version: i64 = patient.get("row_version");
    sqlx::query("UPDATE app.patient_source_links SET source_version=$4,last_seen_at=now() WHERE patient_id=$1 AND organization_id=$2 AND source_id=$3")
        .bind(id).bind(actor.org).bind(source).bind(&verified.source_version).execute(&mut *tx).await?;
    api::audit(&mut tx, &actor, id, "patient.refreshed_from_1c", version).await?;
    let response = json!({"patient":api::patient_row(&patient),"row_version":version,
        "source_id":source,"patient_key":guid,"changed":version!=expected,
        "checked_at":patient.get::<String,_>("checked_at")});
    tx.commit().await?;
    Ok(Json(response))
}
