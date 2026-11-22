//! Manual-only demographic editing; source identities and approved snapshots are untouched.
use crate::api::{self, App, Error};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Row, postgres::PgRow};
use std::sync::Arc;
use uuid::Uuid;
type Result<T> = std::result::Result<T, Error>;
const SELECT: &str = "SELECT p.*,p.birth_date::text AS birth_date_text,EXISTS(SELECT 1 FROM app.patient_source_links l WHERE l.patient_id=p.id) OR EXISTS(SELECT 1 FROM app.patient_external_ids e WHERE e.patient_id=p.id) AS source_linked FROM app.patients p WHERE p.id=$1 AND p.organization_id=$2";
fn allowed(row: &PgRow) -> bool {
    row.get::<String, _>("origin") == "manual"
        && row.get::<String, _>("identity_state") == "provisional"
        && !row.get::<bool, _>("source_linked")
}
fn view(row: &PgRow) -> Value {
    json!({"id":row.get::<Uuid,_>("id"),"row_version":row.get::<i64,_>("row_version"),"manual_edit_allowed":allowed(row),"origin":row.get::<String,_>("origin"),"identity_state":row.get::<String,_>("identity_state"),"family_name":row.get::<String,_>("family_name"),"given_name":row.get::<String,_>("given_name"),"patronymic":row.get::<Option<String>,_>("patronymic"),"birth_date":row.get::<Option<String>,_>("birth_date_text"),"sex":row.get::<Option<String>,_>("sex")})
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    expected_version: i64,
    family_name: String,
    given_name: String,
    patronymic: Option<String>,
    birth_date: Option<String>,
    sex: Option<m53_contracts::colposcopy_1c::Sex>,
}
fn validate(v: &Update) -> Result<()> {
    let valid_name = |s: &str, required: bool| {
        (!required || !s.trim().is_empty()) && s.len() <= 256 && !s.contains('\0')
    };
    if v.expected_version < 1
        || v.expected_version == i64::MAX
        || !valid_name(&v.family_name, true)
        || !valid_name(&v.given_name, true)
        || v.patronymic.as_ref().is_some_and(|s| !valid_name(s, false))
        || v.birth_date
            .as_ref()
            .is_some_and(|s| m53_contracts::colposcopy_1c::CalendarDate::parse(s).is_err())
    {
        return Err(Error(StatusCode::BAD_REQUEST, "invalid_patient"));
    }
    Ok(())
}
pub async fn context(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    let a = api::actor(&app, &h).await?;
    let row = sqlx::query(SELECT)
        .bind(id)
        .bind(a.org)
        .fetch_optional(&app.db)
        .await?
        .ok_or(Error(StatusCode::NOT_FOUND, "patient_not_found"))?;
    Ok(Json(view(&row)))
}
pub async fn update(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(v): Json<Update>,
) -> Result<Json<Value>> {
    let a = api::actor(&app, &h).await?;
    validate(&v)?;
    if a.operation_id.is_none_or(|id| id.is_nil()) {
        return Err(Error(StatusCode::BAD_REQUEST, "operation_id_required"));
    }
    let mut tx = app.db.begin().await?;
    let (operation, cached) =
        api::operation_start(&mut tx, &a, &h, format!("/api/v1/patients/{id}"), &json!(v)).await?;
    if let Some(cached) = cached {
        return Ok(Json(cached));
    }
    let row = sqlx::query(&(SELECT.to_owned() + " FOR UPDATE OF p"))
        .bind(id)
        .bind(a.org)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(Error(StatusCode::NOT_FOUND, "patient_not_found"))?;
    if !allowed(&row) {
        return Err(Error(
            StatusCode::FORBIDDEN,
            "manual_patient_edit_forbidden",
        ));
    }
    if row.get::<i64, _>("row_version") != v.expected_version {
        return Err(Error(StatusCode::CONFLICT, "patient_version_conflict"));
    }
    let sex = v.sex.as_ref().map(|s| {
        serde_json::to_value(s)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned()
    });
    sqlx::query("UPDATE app.patients SET family_name=$2,given_name=$3,patronymic=$4,birth_date=$5::text::date,sex=$6 WHERE id=$1")
  .bind(id).bind(v.family_name.trim()).bind(v.given_name.trim()).bind(v.patronymic.as_deref().map(str::trim)).bind(v.birth_date).bind(sex).execute(&mut *tx).await?;
    let row = sqlx::query(SELECT)
        .bind(id)
        .bind(a.org)
        .fetch_one(&mut *tx)
        .await?;
    api::audit(
        &mut tx,
        &a,
        id,
        "patient.manual_updated",
        row.get("row_version"),
    )
    .await?;
    api::operation_finish(tx, &a, operation, view(&row)).await
}
