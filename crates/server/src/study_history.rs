//! Read-only revision metadata and report identity; never use the requesting actor as performer.
use crate::api::{self, App, Error};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};
use std::sync::Arc;
use uuid::Uuid;
type Result<T> = std::result::Result<T, Error>;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextQuery {
    revision_id: Option<Uuid>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    before_revision: Option<i32>,
}
fn revision_sql(body: bool, snapshots: bool) -> String {
    format!(
        r#"SELECT r.id,r.revision,r.author_id,r.approved_by,to_jsonb(to_char(r.approved_at AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"')) AS approved_time,to_jsonb(to_char(app.protocol_revision_created_at(r.id,(SELECT p.organization_id FROM app.studies s JOIN app.patients p ON p.id=s.patient_id WHERE s.id=r.study_id)) AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"')) AS created_time{}{} FROM app.protocol_revisions r{}{} WHERE r.study_id=$1"#,
        if body {
            ",r.form,coalesce(a.data,'[]'::jsonb) AS annotations"
        } else {
            ""
        },
        if snapshots {
            ",i.patient_snapshot,i.author_snapshot,i.approver_snapshot,i.selected_photo_ids"
        } else {
            ""
        },
        if body {
            " LEFT JOIN app.protocol_annotations a ON a.revision_id=r.id"
        } else {
            ""
        },
        if snapshots {
            " LEFT JOIN app.revision_identity_snapshots i ON i.revision_id=r.id"
        } else {
            ""
        }
    )
}
fn metadata(row: &PgRow) -> Value {
    json!({"revision_id":row.get::<Uuid,_>("id"),"revision_number":row.get::<i32,_>("revision"),"created_at":row.get::<Option<Value>,_>("created_time"),"created_at_source":if row.get::<Option<Value>,_>("created_time").is_some(){"revision_insert_audit"}else{"unknown"},"approved_at":row.get::<Option<Value>,_>("approved_time"),"status":if row.get::<Option<Value>,_>("approved_time").is_some(){"approved"}else{"draft"},"readonly":true,"editor_user_id":row.get::<Uuid,_>("author_id"),"approver_user_id":row.get::<Option<Uuid>,_>("approved_by")})
}
// Reuse immutable creation audit when it proves the study's original assigned performer.
pub(crate) async fn performer(
    tx: &mut Transaction<'_, Postgres>,
    study: Uuid,
    org: Uuid,
    frozen: Option<&Value>,
    approved: bool,
) -> Result<Value> {
    if let Some(value) = frozen
        .and_then(|s| s.get("performer"))
        .filter(|v| v.is_object())
    {
        return Ok(value.clone());
    }
    let row=sqlx::query("SELECT s.author_id,s.assigned_doctor_id,coalesce(d.display_name,u.display_name) AS current_name,e.actor_name,e.doctor_name,e.doctor_id AS initial_doctor FROM app.studies s JOIN app.patients p ON p.id=s.patient_id JOIN app.users u ON u.id=s.author_id LEFT JOIN app.doctors d ON d.id=s.assigned_doctor_id LEFT JOIN LATERAL app.study_performer_creation(s.id,$2) e ON true WHERE s.id=$1 AND p.organization_id=$2")
  .bind(study).bind(org).fetch_one(&mut **tx).await?;
    let assigned: Option<Uuid> = row.get("assigned_doctor_id");
    let initial: Option<Uuid> = row.get("initial_doctor");
    let name = row
        .get::<Option<String>, _>("doctor_name")
        .or_else(|| row.get::<Option<String>, _>("actor_name"));
    if assigned == initial && name.as_ref().is_some_and(|v| !v.trim().is_empty()) {
        return Ok(
            json!({"user_id":row.get::<Uuid,_>("author_id"),"doctor_id":assigned,"display_name":name,"source":"study_creation_audit","frozen":true}),
        );
    }
    if approved {
        return Ok(Value::Null);
    }
    Ok(
        json!({"user_id":row.get::<Uuid,_>("author_id"),"doctor_id":assigned,"display_name":row.get::<String,_>("current_name"),"source":"study_assignment_current_record","frozen":false}),
    )
}
async fn scoped(tx: &mut Transaction<'_, Postgres>, id: Uuid, org: Uuid) -> Result<PgRow> {
    sqlx::query("SELECT s.* FROM app.studies s JOIN app.patients p ON p.id=s.patient_id WHERE s.id=$1 AND p.organization_id=$2 AND (s.organization_id IS NULL OR s.organization_id=$2)")
 .bind(id).bind(org).fetch_optional(&mut **tx).await?.ok_or(Error(StatusCode::NOT_FOUND,"study_not_found"))
}
async fn patient(
    tx: &mut Transaction<'_, Postgres>,
    study: &PgRow,
    revision: Option<&PgRow>,
) -> Result<(Value, &'static str)> {
    if let Some(snapshot) = revision.and_then(|r| r.get::<Option<Value>, _>("patient_snapshot")) {
        let mut value = snapshot;
        value["id"] = value["patient_id"].clone();
        // Old snapshots did not record a display external_id: do not infer one from live data.
        if value.get("external_id").is_none() {
            value["external_id"] = Value::Null;
        }
        return Ok((value, "approved_snapshot"));
    }
    if revision.is_some_and(|r| r.get::<Option<Value>, _>("approved_time").is_some()) {
        return Ok((Value::Null, "unknown_legacy"));
    }
    let row=sqlx::query("SELECT p.*,p.birth_date::text AS birth_date_text,(SELECT e.external_id FROM app.patient_external_ids e WHERE e.patient_id=p.id ORDER BY e.source LIMIT 1) AS external_id FROM app.patients p WHERE p.id=$1")
  .bind(study.get::<Uuid,_>("patient_id")).fetch_one(&mut **tx).await?;
    Ok((json!(api::patient_row(&row)), "current_record"))
}
async fn context_value(
    tx: &mut Transaction<'_, Postgres>,
    study: &PgRow,
    revision: Option<&PgRow>,
    org: Uuid,
) -> Result<Value> {
    let id: Uuid = study.get("id");
    let approved = revision.is_some_and(|r| r.get::<Option<Value>, _>("approved_time").is_some());
    let frozen = revision.and_then(|r| r.get::<Option<Value>, _>("author_snapshot"));
    let physician = performer(tx, id, org, frozen.as_ref(), approved).await?;
    let (patient, source) = patient(tx, study, revision).await?;
    let mut value=revision.map(metadata).unwrap_or_else(||json!({"revision_id":null,"revision_number":null,"created_at":null,"approved_at":null,"status":"draft","readonly":false}));
    value["study_id"] = json!(id);
    value["study_row_version"] = json!(study.get::<i64, _>("row_version"));
    value["study_created_at"]=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(to_char(created_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')) FROM app.studies WHERE id=$1").bind(id).fetch_one(&mut **tx).await?;
    value["encounter_date"] = sqlx::query_scalar::<_, Option<String>>(
        "SELECT encounter_date::text FROM app.studies WHERE id=$1",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await?
    .map(Value::String)
    .unwrap_or(Value::Null);
    let current: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM app.protocol_revisions WHERE study_id=$1 ORDER BY revision DESC LIMIT 1",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    value["is_current"] = json!(revision.map(|r| r.get::<Uuid, _>("id")) == current);
    value["colposcope_model"] = json!(study.get::<Option<String>,_>("colposcope_model"));
    if let Some(row)=revision.filter(|_| approved) {
        let selected:Option<Vec<Uuid>>=row.get("selected_photo_ids");
        let (assets,source)=if let Some(ids)=selected {
            let rows=sqlx::query("SELECT a.* FROM unnest($2::uuid[]) WITH ORDINALITY chosen(id,position) JOIN app.assets a ON a.id=chosen.id WHERE a.study_id=$1 AND a.kind='original' ORDER BY chosen.position")
                .bind(id).bind(ids.as_slice()).fetch_all(&mut **tx).await?;
            if rows.len()!=ids.len() {return Err(Error(StatusCode::CONFLICT,"report_assets_unavailable"));}
            (rows,"approval_selection")
        } else {
            let rows=sqlx::query("SELECT a.* FROM app.assets a JOIN app.protocol_revisions r ON r.study_id=a.study_id WHERE a.study_id=$1 AND r.id=$2 AND a.kind='original' AND a.imported_at<=r.approved_at ORDER BY a.imported_at,a.id")
                .bind(id).bind(row.get::<Uuid,_>("id")).fetch_all(&mut **tx).await?;
            (rows,"legacy_before_approval")
        };
        value["report_assets"]=json!(assets.iter().map(api::asset_row).collect::<Vec<_>>());
        value["report_assets_source"]=json!(source);
    }
    value["performer"] = physician;
    value["patient"] = patient;
    value["patient_source"] = json!(source);
    value["editor"] = revision
        .map(|r| json!({"user_id":r.get::<Uuid,_>("author_id")}))
        .unwrap_or(Value::Null);
    value["approver"] = revision
        .and_then(|r| r.get::<Option<Value>, _>("approver_snapshot"))
        .unwrap_or(Value::Null);
    Ok(value)
}
pub async fn report_context(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<ContextQuery>,
) -> Result<Json<Value>> {
    let a = api::actor(&app, &h).await?;
    let mut tx = app.db.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let study = scoped(&mut tx, id, a.org).await?;
    let row = if let Some(revision) = q.revision_id {
        sqlx::query(&(revision_sql(false, true) + " AND r.id=$2"))
            .bind(id)
            .bind(revision)
            .fetch_optional(&mut *tx)
            .await?
    } else {
        sqlx::query(&(revision_sql(false, true) + " ORDER BY r.revision DESC LIMIT 1"))
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
    };
    if q.revision_id.is_some() && row.is_none() {
        return Err(Error(StatusCode::NOT_FOUND, "revision_not_found"));
    }
    let mut value = context_value(&mut tx, &study, row.as_ref(), a.org).await?;
    value["readonly"] = json!(value["is_current"] != true || value["status"] == "approved");
    tx.commit().await?;
    Ok(Json(value))
}
pub async fn list(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<Page>,
) -> Result<Json<Value>> {
    let a = api::actor(&app, &h).await?;
    if q.before_revision.is_some_and(|v| v < 1) {
        return Err(Error(StatusCode::BAD_REQUEST, "invalid_revision_cursor"));
    }
    let mut tx = app.db.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    scoped(&mut tx, id, a.org).await?;
    let rows = sqlx::query(
        &(revision_sql(false, false)
            + " AND ($2::int IS NULL OR r.revision<$2) ORDER BY r.revision DESC LIMIT 501"),
    )
    .bind(id)
    .bind(q.before_revision)
    .fetch_all(&mut *tx)
    .await?;
    let more = rows.len() > 500;
    let page = &rows[..rows.len().min(500)];
    let next = if more {
        page.last().map(|r| r.get::<i32, _>("revision"))
    } else {
        None
    };
    let value = json!({"study_id":id,"revisions":page.iter().map(metadata).collect::<Vec<_>>(),"next_before_revision":next});
    tx.commit().await?;
    Ok(Json(value))
}
pub async fn revision(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path((id, revision)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>> {
    let a = api::actor(&app, &h).await?;
    let mut tx = app.db.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let study = scoped(&mut tx, id, a.org).await?;
    let row = sqlx::query(&(revision_sql(true, true) + " AND r.id=$2"))
        .bind(id)
        .bind(revision)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(Error(StatusCode::NOT_FOUND, "revision_not_found"))?;
    let form: Value = row.get("form");
    let supported = h
        .get("x-m53-protocol-version")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(1);
    if form
        .get("schema_version")
        .and_then(Value::as_u64)
        .is_some_and(|version| version > u64::from(supported))
    {
        return Err(Error(StatusCode::CONFLICT, "client_update_required"));
    }
    let mut value = context_value(&mut tx, &study, Some(&row), a.org).await?;
    value["readonly"] = json!(true);
    value["protocol"] = form;
    value["annotations"] = row.get("annotations");
    tx.commit().await?;
    Ok(Json(value))
}
