//! Explicit continuity-care linking. Never merge patients or rewrite clinical history.
use crate::api::{self, Actor, App, Error};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use m53_contracts::continuity::{IdentityLinkPreviewRequest, IdentityLinkRequest};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};
use std::{collections::BTreeMap, sync::Arc};
use uuid::Uuid;

type Result<T> = std::result::Result<T, Error>;

fn conflict(code: &'static str) -> Error {
    Error(StatusCode::CONFLICT, code)
}

fn validate_selection(input: &IdentityLinkPreviewRequest) -> Result<()> {
    if input.source_id.is_nil() || input.application_key.is_nil() || input.patient_key.is_nil() {
        return Err(Error(StatusCode::BAD_REQUEST, "invalid_identity_selection"));
    }
    Ok(())
}

async fn doctor(tx: &mut Transaction<'_, Postgres>, actor: &Actor) -> Result<Uuid> {
    sqlx::query_scalar("SELECT d.id FROM app.users u JOIN app.doctors d ON d.id=u.doctor_id AND d.organization_id=u.organization_id WHERE u.id=$1 AND u.organization_id=$2 AND NOT u.disabled AND d.active")
        .bind(actor.user).bind(actor.org).fetch_optional(&mut **tx).await?
        .ok_or(Error(StatusCode::FORBIDDEN,"doctor_profile_required"))
}

#[derive(Clone)]
struct PatientIdentity {
    id: Uuid,
    version: i64,
    state: String,
    snapshot: Value,
}
impl PatientIdentity {
    fn from_row(row: &PgRow) -> Self {
        Self {
            id: row.get("id"),
            version: row.get("row_version"),
            state: row.get("identity_state"),
            snapshot: json!({"patient_id":row.get::<Uuid,_>("id"),
                "row_version":row.get::<i64,_>("row_version"),
                "family_name":row.get::<String,_>("family_name"),
                "given_name":row.get::<String,_>("given_name"),
                "patronymic":row.get::<Option<String>,_>("patronymic"),
                "birth_date":row.get::<Option<String>,_>("birth_date_text"),
                "card_number":row.get::<Option<String>,_>("card_number"),
                "identity_state":row.get::<String,_>("identity_state"),
                "origin":row.get::<String,_>("origin")}),
        }
    }
}

struct Selection {
    task: PgRow,
    study: PgRow,
    original: PatientIdentity,
    target: PatientIdentity,
    doctor: Uuid,
    blockers: Vec<&'static str>,
}

async fn selection(
    tx: &mut Transaction<'_, Postgres>,
    actor: &Actor,
    study_id: Uuid,
    input: &IdentityLinkPreviewRequest,
    lock: bool,
) -> Result<Selection> {
    let doctor = doctor(tx, actor).await?;
    // Same order as claim/result delivery: task, study, then patients by UUID.
    let task_sql = if lock {
        "SELECT t.*,s.enabled AS source_enabled FROM app.colposcopy_tasks t JOIN app.integration_sources s ON s.id=t.source_id AND s.organization_id=t.organization_id WHERE t.source_id=$1 AND t.application_key=$2 AND t.organization_id=$3 FOR UPDATE OF t"
    } else {
        "SELECT t.*,s.enabled AS source_enabled FROM app.colposcopy_tasks t JOIN app.integration_sources s ON s.id=t.source_id AND s.organization_id=t.organization_id WHERE t.source_id=$1 AND t.application_key=$2 AND t.organization_id=$3"
    };
    let task = sqlx::query(task_sql)
        .bind(input.source_id)
        .bind(input.application_key)
        .bind(actor.org)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error(StatusCode::NOT_FOUND, "task_not_found"))?;
    if task.get::<Option<Uuid>, _>("patient_guid") != Some(input.patient_key) {
        return Err(conflict("task_patient_mismatch"));
    }
    let study_sql = if lock {
        "SELECT s.* FROM app.studies s JOIN app.patients p ON p.id=s.patient_id WHERE s.id=$1 AND p.organization_id=$2 FOR UPDATE OF s"
    } else {
        "SELECT s.* FROM app.studies s JOIN app.patients p ON p.id=s.patient_id WHERE s.id=$1 AND p.organization_id=$2"
    };
    let study = sqlx::query(study_sql)
        .bind(study_id)
        .bind(actor.org)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error(StatusCode::NOT_FOUND, "study_not_found"))?;
    if study.get::<Option<Uuid>, _>("assigned_doctor_id") != Some(doctor) {
        return Err(Error(
            StatusCode::FORBIDDEN,
            "study_assigned_to_other_doctor",
        ));
    }
    let target: Uuid = sqlx::query_scalar("SELECT patient_id FROM app.patient_source_links WHERE source_id=$1 AND external_guid=$2 AND organization_id=$3")
        .bind(input.source_id).bind(input.patient_key).bind(actor.org)
        .fetch_optional(&mut **tx).await?.ok_or(conflict("patient_identity_unconfirmed"))?;
    let original: Uuid = study.get("patient_id");
    let patient_sql = if lock {
        "SELECT *,birth_date::text AS birth_date_text FROM app.patients WHERE organization_id=$1 AND id=ANY($2::uuid[]) ORDER BY id FOR UPDATE"
    } else {
        "SELECT *,birth_date::text AS birth_date_text FROM app.patients WHERE organization_id=$1 AND id=ANY($2::uuid[]) ORDER BY id"
    };
    let patients = sqlx::query(patient_sql)
        .bind(actor.org)
        .bind(vec![original, target])
        .fetch_all(&mut **tx)
        .await?
        .iter()
        .map(PatientIdentity::from_row)
        .map(|p| (p.id, p))
        .collect::<BTreeMap<_, _>>();
    let original = patients
        .get(&original)
        .cloned()
        .ok_or(conflict("patient_organization_mismatch"))?;
    let target = patients
        .get(&target)
        .cloned()
        .ok_or(conflict("patient_organization_mismatch"))?;
    let history: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app.protocol_revisions WHERE study_id=$1 AND approved_at IS NOT NULL) OR EXISTS(SELECT 1 FROM app.study_publications WHERE study_id=$1) OR EXISTS(SELECT 1 FROM app.result_delivery_jobs WHERE study_id=$1) OR EXISTS(SELECT 1 FROM app.reports WHERE study_id=$1)")
        .bind(study_id).fetch_one(&mut **tx).await?;
    let other_task: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM app.colposcopy_tasks WHERE study_id=$1 AND id<>$2)",
    )
    .bind(study_id)
    .bind(task.get::<Uuid, _>("id"))
    .fetch_one(&mut **tx)
    .await?;
    let mut blockers = vec![];
    if !task.get::<bool, _>("source_enabled") {
        blockers.push("source_disabled");
    }
    if task.get::<String, _>("state") == "cancelled" {
        blockers.push("task_cancelled");
    }
    if task
        .get::<Option<Uuid>, _>("assigned_doctor_id")
        .is_some_and(|id| id != doctor)
    {
        blockers.push("task_already_claimed");
    }
    if task
        .get::<Option<Uuid>, _>("study_id")
        .is_some_and(|id| id != study_id)
    {
        blockers.push("task_already_linked");
    }
    if other_task {
        blockers.push("study_already_linked");
    }
    if study.get::<String, _>("origin") != "manual_continuity" {
        blockers.push("manual_continuity_required");
    }
    if study.get::<String, _>("status") == "cancelled" {
        blockers.push("study_cancelled");
    }
    if history {
        blockers.push("identity_correction_required");
    }
    if target.state != "confirmed" {
        blockers.push("patient_identity_unconfirmed");
    }
    if original.id != target.id && original.state != "provisional" {
        blockers.push("patient_identity_already_confirmed");
    }
    if original.id != target.id && study.get::<Option<Uuid>, _>("order_id").is_some() {
        blockers.push("order_patient_mismatch");
    }
    Ok(Selection {
        task,
        study,
        original,
        target,
        doctor,
        blockers,
    })
}

fn preview_response(
    study_id: Uuid,
    input: &IdentityLinkPreviewRequest,
    selected: &Selection,
) -> Value {
    let differences = ["family_name","given_name","patronymic","birth_date"].into_iter()
        .filter(|field| selected.original.snapshot[*field] != selected.target.snapshot[*field])
        .map(|field|json!({"field":field,"before":selected.original.snapshot[field],"after":selected.target.snapshot[field]}))
        .collect::<Vec<_>>();
    json!({"study_id":study_id,"study_version":selected.study.get::<i64,_>("row_version"),
        "task_id":selected.task.get::<Uuid,_>("id"),"task_version":selected.task.get::<i64,_>("row_version"),
        "source_id":input.source_id,"application_key":input.application_key,"patient_key":input.patient_key,
        "source_patient":selected.original.snapshot,"target_patient":selected.target.snapshot,
        "doctor_id":selected.doctor,"task_state":selected.task.get::<String,_>("state"),
        "binding_allowed":selected.blockers.is_empty(),"blockers":selected.blockers,"differences":differences})
}

pub async fn preview(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(study_id): Path<Uuid>,
    Json(input): Json<IdentityLinkPreviewRequest>,
) -> Result<Json<Value>> {
    validate_selection(&input)?;
    let actor = api::actor(&app, &headers).await?;
    api::study(&app, &actor, study_id).await?;
    let mut tx = app.db.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let selected = selection(&mut tx, &actor, study_id, &input, false).await?;
    let response = preview_response(study_id, &input, &selected);
    tx.commit().await?;
    Ok(Json(response))
}

pub async fn identity_link(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(study_id): Path<Uuid>,
    Json(input): Json<IdentityLinkRequest>,
) -> Result<Json<Value>> {
    let request = input.preview_request();
    validate_selection(&request)?;
    if headers
        .get("x-operation-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v).ok())
        .is_none_or(|id| id.is_nil())
    {
        return Err(Error(StatusCode::BAD_REQUEST, "operation_id_required"));
    }
    if input.reason.trim().is_empty()
        || input.reason.chars().count() > 512
        || input.reason.chars().any(char::is_control)
        || input.expected_study_version <= 0
        || input.expected_study_version == i64::MAX
        || input.expected_task_version <= 0
        || input.expected_task_version == i64::MAX
        || input.expected_patient_versions.is_empty()
        || input.expected_patient_versions.len() > 2
    {
        return Err(Error(StatusCode::BAD_REQUEST, "invalid_identity_link"));
    }
    let mut expected = BTreeMap::new();
    for patient in &input.expected_patient_versions {
        if patient.patient_id.is_nil()
            || patient.row_version <= 0
            || patient.row_version == i64::MAX
            || expected
                .insert(patient.patient_id, patient.row_version)
                .is_some()
        {
            return Err(Error(StatusCode::BAD_REQUEST, "invalid_patient_versions"));
        }
    }
    let actor = api::actor(&app, &headers).await?;
    api::study(&app, &actor, study_id).await?;
    let mut tx = app.db.begin().await?;
    sqlx::query("SET LOCAL lock_timeout='4s'")
        .execute(&mut *tx)
        .await?;
    let (operation, cached) = api::operation_start(
        &mut tx,
        &actor,
        &headers,
        format!("/api/v1/studies/{study_id}/identity-link"),
        &json!(input),
    )
    .await?;
    if let Some(cached) = cached {
        return Ok(Json(cached));
    }
    let selected = selection(&mut tx, &actor, study_id, &request, true).await?;
    if let Some(code) = selected.blockers.first() {
        return Err(conflict(code));
    }
    if selected.study.get::<i64, _>("row_version") != input.expected_study_version {
        return Err(conflict("version_conflict"));
    }
    if selected.task.get::<i64, _>("row_version") != input.expected_task_version {
        return Err(conflict("stale_task_version"));
    }
    let actual = [&selected.original, &selected.target]
        .into_iter()
        .map(|p| (p.id, p.version))
        .collect::<BTreeMap<_, _>>();
    if expected != actual {
        return Err(conflict("stale_patient_version"));
    }
    let task_id: Uuid = selected.task.get("id");
    let study_version = input.expected_study_version + 1;
    let task_version = input.expected_task_version + 1;
    let event = Uuid::new_v4();
    sqlx::query("SELECT set_config('m53.identity_link_study',$1,true)")
        .bind(study_id.to_string())
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE app.studies SET patient_id=$2,row_version=$3 WHERE id=$1")
        .bind(study_id)
        .bind(selected.target.id)
        .bind(study_version)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE app.colposcopy_tasks SET study_id=$2,study_patient_id=$3,assigned_doctor_id=$4,state='in_progress',row_version=$5,updated_at=now() WHERE id=$1")
        .bind(task_id).bind(study_id).bind(selected.target.id).bind(selected.doctor).bind(task_version)
        .execute(&mut *tx).await?;
    sqlx::query("INSERT INTO app.identity_link_events(id,organization_id,study_id,source_patient_id,target_patient_id,source_id,application_key,patient_guid,task_id,doctor_id,actor_user_id,study_version,task_version,source_patient_version,target_patient_version,source_patient_snapshot,target_patient_snapshot,reason) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)")
        .bind(event).bind(actor.org).bind(study_id).bind(selected.original.id).bind(selected.target.id)
        .bind(input.source_id).bind(input.application_key).bind(input.patient_key).bind(task_id)
        .bind(selected.doctor).bind(actor.user).bind(study_version).bind(task_version)
        .bind(selected.original.version).bind(selected.target.version).bind(&selected.original.snapshot)
        .bind(&selected.target.snapshot).bind(input.reason.trim()).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO app.task_events(id,task_id,source_id,organization_id,application_key,event_type,task_version,actor_user_id) VALUES($1,$2,$3,$4,$5,'identity_linked',$6,$7)")
        .bind(Uuid::new_v4()).bind(task_id).bind(input.source_id).bind(actor.org)
        .bind(input.application_key).bind(task_version).bind(actor.user).execute(&mut *tx).await?;
    api::audit(
        &mut tx,
        &actor,
        study_id,
        "study.identity_linked",
        study_version,
    )
    .await?;
    api::operation_finish(
        tx,
        &actor,
        operation,
        json!({"study_id":study_id,
        "patient_id":selected.target.id,"source_patient_id":selected.original.id,
        "task_id":task_id,"source_id":input.source_id,"application_key":input.application_key,
        "patient_key":input.patient_key,"row_version":study_version,"task_version":task_version,
        "identity_state":"confirmed","event_id":event,"requires_new_report":true}),
    )
    .await
}

/// Provisional care can be saved and printed locally, but final approval awaits identity.
pub(crate) async fn can_approve(
    tx: &mut Transaction<'_, Postgres>,
    study_id: Uuid,
    actor: &Actor,
) -> Result<()> {
    let row = sqlx::query("SELECT s.origin,p.identity_state,p.id AS patient_id,p.row_version AS patient_version FROM app.studies s JOIN app.patients p ON p.id=s.patient_id WHERE s.id=$1 AND p.organization_id=$2 FOR SHARE OF p")
        .bind(study_id).bind(actor.org).fetch_one(&mut **tx).await?;
    let legacy = row.get::<String, _>("origin") == "legacy"
        && std::env::var("M53_LEGACY_UNLINKED_DOCTORS").as_deref() == Ok("1");
    if !legacy {
        doctor(tx, actor).await?;
        if row.get::<String, _>("identity_state") != "confirmed" {
            let verified:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app.manual_identity_verifications WHERE study_id=$1 AND organization_id=$2 AND patient_id=$3 AND patient_version=$4 AND verified_by=$5)")
                .bind(study_id).bind(actor.org).bind(row.get::<Uuid,_>("patient_id")).bind(row.get::<i64,_>("patient_version")).bind(actor.user).fetch_one(&mut **tx).await?;
            if row.get::<String,_>("origin")!="manual_continuity" || !verified {return Err(conflict("patient_identity_unconfirmed"));}
        }
    }
    Ok(())
}

/// Must run after approval and before the caller commits that same transaction.
pub(crate) async fn snapshot_approval(
    tx: &mut Transaction<'_, Postgres>,
    revision_id: Uuid,
    study_id: Uuid,
    actor: &Actor,
    selected_photo_ids: Option<&[Uuid]>,
) -> Result<()> {
    let row=sqlx::query("SELECT s.patient_id,p.identity_state,jsonb_build_object('patient_id',p.id,'family_name',p.family_name,'given_name',p.given_name,'patronymic',p.patronymic,'birth_date',p.birth_date::text,'card_number',p.card_number,'sex',p.sex,'identity_state',p.identity_state,'row_version',p.row_version,'source_links',coalesce((SELECT jsonb_agg(jsonb_build_object('source_id',l.source_id,'patient_guid',l.external_guid) ORDER BY l.source_id) FROM app.patient_source_links l WHERE l.patient_id=p.id AND l.organization_id=$3),'[]'::jsonb)) AS patient_snapshot,jsonb_build_object('user_id',u.id,'display_name',u.display_name,'doctor_id',u.doctor_id,'doctor_name',d.display_name) AS author_snapshot,jsonb_build_object('user_id',approver.id,'display_name',approver.display_name,'doctor_id',approver.doctor_id,'doctor_name',ad.display_name) AS approver_snapshot FROM app.studies s JOIN app.patients p ON p.id=s.patient_id JOIN app.protocol_revisions r ON r.study_id=s.id AND r.id=$1 AND r.approved_at IS NOT NULL AND r.approved_by=$4 JOIN app.users u ON u.id=r.author_id LEFT JOIN app.doctors d ON d.id=u.doctor_id JOIN app.users approver ON approver.id=r.approved_by LEFT JOIN app.doctors ad ON ad.id=approver.doctor_id WHERE s.id=$2 AND p.organization_id=$3 AND s.organization_id=$3")
        .bind(revision_id).bind(study_id).bind(actor.org).bind(actor.user).fetch_one(&mut **tx).await?;
    let tasks = sqlx::query("SELECT source_id,application_key,patient_guid FROM app.colposcopy_tasks WHERE study_id=$1 AND organization_id=$2")
        .bind(study_id).bind(actor.org).fetch_all(&mut **tx).await?;
    if tasks.len() > 1 {
        return Err(conflict("ambiguous_task_binding"));
    }
    let task = tasks.first();
    let manual:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(v) FROM app.manual_identity_verifications v JOIN app.patients p ON p.id=v.patient_id WHERE v.study_id=$1 AND v.organization_id=$2 AND v.verified_by=$3 AND v.patient_id=p.id AND v.patient_version=p.row_version ORDER BY v.verified_at DESC LIMIT 1")
        .bind(study_id).bind(actor.org).bind(actor.user).fetch_optional(&mut **tx).await?;
    let mut author_snapshot:Value=row.get("author_snapshot");
    let mut performer=crate::study_history::performer(tx,study_id,actor.org,None,false).await?;
    if performer.is_object() {performer["captured_from"]=performer["source"].clone();performer["source"]=json!("approval_performer_snapshot");performer["frozen"]=json!(true);}
    author_snapshot["performer"]=performer;
    let mut patient_snapshot:Value=row.get("patient_snapshot");
    if let Some(manual)=manual {patient_snapshot["manual_identity_verification"]=manual;}
    sqlx::query("INSERT INTO app.revision_identity_snapshots(revision_id,study_id,organization_id,patient_id,identity_state,patient_snapshot,author_snapshot,approver_snapshot,source_id,application_key,patient_guid,selected_photo_ids) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)")
        .bind(revision_id).bind(study_id).bind(actor.org).bind(row.get::<Uuid,_>("patient_id"))
        .bind(row.get::<String,_>("identity_state")).bind(patient_snapshot)
        .bind(author_snapshot).bind(row.get::<Value,_>("approver_snapshot"))
        .bind(task.map(|t|t.get::<Uuid,_>("source_id"))).bind(task.map(|t|t.get::<Uuid,_>("application_key")))
        .bind(task.and_then(|t|t.get::<Option<Uuid>,_>("patient_guid"))).bind(selected_photo_ids).execute(&mut **tx).await?;
    Ok(())
}

#[derive(serde::Deserialize,serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManualVerification {
    expected_version:i64,
    patient_id:Uuid,
    expected_patient_version:i64,
    confirmed:bool,
    patient_data:Value,
}
pub async fn manual_verification(State(app):State<Arc<App>>,h:HeaderMap,Path(id):Path<Uuid>,Json(input):Json<ManualVerification>) -> Result<Json<Value>> {
    let actor=api::actor(&app,&h).await?;
    if !input.confirmed || input.expected_version<1 || input.expected_patient_version<1 || input.patient_id.is_nil() {
        return Err(Error(StatusCode::BAD_REQUEST,"manual_verification_required"));
    }
    if actor.operation_id.is_none_or(|id|id.is_nil()) {return Err(Error(StatusCode::BAD_REQUEST,"operation_id_required"));}
    let mut tx=app.db.begin().await?;
    let (operation,cached)=api::operation_start(&mut tx,&actor,&h,format!("/api/v1/studies/{id}/manual-identity-verification"),&json!(input)).await?;
    if let Some(cached)=cached{return Ok(Json(cached));}
    let doctor=doctor(&mut tx,&actor).await?;
    let study=sqlx::query("SELECT s.patient_id,s.row_version,s.status,s.origin FROM app.studies s JOIN app.patients p ON p.id=s.patient_id WHERE s.id=$1 AND p.organization_id=$2 AND s.organization_id=$2 FOR UPDATE OF s")
        .bind(id).bind(actor.org).fetch_optional(&mut *tx).await?.ok_or(Error(StatusCode::NOT_FOUND,"study_not_found"))?;
    if study.get::<i64,_>("row_version")!=input.expected_version {return Err(conflict("version_conflict"));}
    if study.get::<Uuid,_>("patient_id")!=input.patient_id {return Err(conflict("patient_changed"));}
    if study.get::<String,_>("status")!="draft" || study.get::<String,_>("origin")!="manual_continuity" {return Err(conflict("manual_verification_not_applicable"));}
    let patient=sqlx::query("SELECT row_version,identity_state,origin,jsonb_build_object('family_name',family_name,'given_name',given_name,'patronymic',patronymic,'birth_date',birth_date::text,'sex',sex) AS patient_data FROM app.patients WHERE id=$1 AND organization_id=$2 FOR SHARE")
        .bind(input.patient_id).bind(actor.org).fetch_one(&mut *tx).await?;
    if patient.get::<i64,_>("row_version")!=input.expected_patient_version {return Err(conflict("patient_version_conflict"));}
    if patient.get::<Value,_>("patient_data")!=input.patient_data {return Err(conflict("patient_changed"));}
    if patient.get::<String,_>("identity_state")!="provisional" || patient.get::<String,_>("origin")!="manual" {return Err(conflict("manual_verification_not_applicable"));}
    let verification:Value=sqlx::query_scalar("INSERT INTO app.manual_identity_verifications(study_id,organization_id,patient_id,patient_version,verified_by,doctor_id,study_version) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING to_jsonb(manual_identity_verifications)")
        .bind(id).bind(actor.org).bind(input.patient_id).bind(input.expected_patient_version).bind(actor.user).bind(doctor).bind(input.expected_version).fetch_one(&mut *tx).await?;
    api::audit(&mut tx,&actor,id,"study.manual_identity_verified",input.expected_version).await?;
    api::operation_finish(tx,&actor,operation,json!({"study_id":id,"row_version":input.expected_version,"manual_identity_verification":verification})).await
}
