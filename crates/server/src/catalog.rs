use crate::api::{self, App, Error};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use m53_contracts::{
    access::{DoctorIdentity, ExternalDoctorId},
    catalog::DoctorPage,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorQuery {
    after: Option<Uuid>,
    limit: Option<i64>,
}

pub async fn doctors(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Query(q): Query<DoctorQuery>,
) -> Result<Json<DoctorPage>, Error> {
    let actor = api::actor(&app, &h).await?;
    let limit = q.limit.unwrap_or(100);
    if !(1..=200).contains(&limit) {
        return Err(Error(StatusCode::BAD_REQUEST, "invalid_page_size"));
    }
    let mut tx = app.db.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let mut rows=sqlx::query("SELECT id,display_name,active,origin FROM app.doctors WHERE organization_id=$1 AND ($2::uuid IS NULL OR id>$2) ORDER BY id LIMIT $3")
        .bind(actor.org).bind(q.after).bind(limit+1).fetch_all(&mut *tx).await?;
    let has_next = rows.len() > limit as usize;
    if has_next {
        rows.pop();
    }
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let id: Uuid = row.get("id");
        let refs=sqlx::query("SELECT source_id,external_guid FROM app.doctor_external_ids WHERE doctor_id=$1 AND organization_id=$2 ORDER BY source_id")
            .bind(id).bind(actor.org).fetch_all(&mut *tx).await?;
        items.push(DoctorIdentity {
            id,
            display_name: row.get("display_name"),
            active: row.get("active"),
            origin: row.get("origin"),
            external_ids: refs
                .iter()
                .map(|r| ExternalDoctorId {
                    source_id: r.get("source_id"),
                    doctor_guid: r.get("external_guid"),
                })
                .collect(),
        });
    }
    let next_after = if has_next {
        items.last().map(|d| d.id)
    } else {
        None
    };
    tx.commit().await?;
    Ok(Json(DoctorPage { items, next_after }))
}

pub async fn capabilities(State(app): State<Arc<App>>, h: HeaderMap) -> Result<Json<Value>, Error> {
    api::actor(&app, &h).await?;
    Ok(Json(json!({"api_version":"v1","protocol_schema_version":2,
        "auth_provider":if cfg!(feature="standalone") {"m53".into()} else {std::env::var("M53_AUTH_PROVIDER").unwrap_or_else(|_|"m53".into())},"auth_method":if !cfg!(feature="standalone") && std::env::var("M53_AUTH_PROVIDER").as_deref()==Ok("1c_web"){"odata_basic"}else{"m53_password"},"own_auth":cfg!(feature="standalone") || std::env::var("M53_AUTH_PROVIDER").as_deref()!=Ok("1c_web"),"offline_grants":true,"offline_grant_days":30,
        "manual_patient_edit_v1":true,"revision_history_v1":true,"report_context_v1":true,"diagnoses_v1":true,"manual_encounters":true,"explicit_identity_link":!cfg!(feature="standalone"),"client_generated_pdf":true,
        "integration_1c":!cfg!(feature="standalone"),"temporary_pdf_delivery_cache":!cfg!(feature="standalone"),"permanent_report_assets":false,
        "max_original_bytes":64*1024*1024,"max_pdf_bytes":m53_contracts::colposcopy_1c::MAX_PDF_BYTES})))
}

pub async fn context(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, Error> {
    let a = api::actor(&app, &h).await?;
    api::study(&app, &a, id).await?;
    let r=sqlx::query("SELECT s.origin,s.assigned_doctor_id,s.encounter_date::text AS encounter_date,p.identity_state,p.sex,p.origin AS patient_origin,p.row_version AS patient_version,p.id AS patient_id FROM app.studies s JOIN app.patients p ON p.id=s.patient_id WHERE s.id=$1 AND s.organization_id=$2")
        .bind(id).bind(a.org).fetch_one(&app.db).await?;
    let links=sqlx::query("SELECT source_id,external_guid FROM app.patient_source_links WHERE patient_id=$1 AND organization_id=$2 ORDER BY source_id")
        .bind(r.get::<Uuid,_>("patient_id")).bind(a.org).fetch_all(&app.db).await?;
    let snapshot:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(i) FROM app.revision_identity_snapshots i JOIN app.protocol_revisions r ON r.id=i.revision_id WHERE i.study_id=$1 AND i.organization_id=$2 ORDER BY r.revision DESC LIMIT 1")
        .bind(id).bind(a.org).fetch_optional(&app.db).await?;
    let manual:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(v) FROM app.manual_identity_verifications v JOIN app.patients p ON p.id=v.patient_id WHERE v.study_id=$1 AND v.organization_id=$2 AND v.verified_by=$3 AND v.patient_id=$4 AND v.patient_version=p.row_version ORDER BY v.verified_at DESC LIMIT 1")
        .bind(id).bind(a.org).bind(a.user).bind(r.get::<Uuid,_>("patient_id")).fetch_optional(&app.db).await?;
    Ok(Json(
        json!({"study_id":id,"origin":r.get::<String,_>("origin"),
        "assigned_doctor_id":r.get::<Option<Uuid>,_>("assigned_doctor_id"),
        "encounter_date":r.get::<Option<String>,_>("encounter_date"),
        "patient":{"id":r.get::<Uuid,_>("patient_id"),"sex":r.get::<Option<String>,_>("sex"),"identity_state":r.get::<String,_>("identity_state"),
          "origin":r.get::<String,_>("patient_origin"),"row_version":r.get::<i64,_>("patient_version"),
          "external_ids":links.iter().map(|l|json!({"source_id":l.get::<Uuid,_>("source_id"),"patient_guid":l.get::<Uuid,_>("external_guid")})).collect::<Vec<_>>()},
        "manual_identity_verification":manual,"approved_identity_snapshot":snapshot}),
    ))
}
