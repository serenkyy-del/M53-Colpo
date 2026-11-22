//! Direct 1C inbox: acknowledge only committed requests; never delete clinical work.
use crate::api::{App, Error, actor, hash};
use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use m53_contracts::colposcopy_1c::{Operation, TaskAck, TaskRequest, UpsertTask};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use std::sync::Arc;
use uuid::Uuid;
type Result<T> = std::result::Result<T, Error>;

pub(crate) async fn source(app: &App, headers: &HeaderMap) -> Result<(Uuid, Uuid, Uuid)> {
    let key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 4096)
        .ok_or(Error(StatusCode::UNAUTHORIZED, "integration_key_required"))?;
    let row = sqlx::query("SELECT c.id,c.source_id,c.organization_id FROM app.integration_task_credentials c JOIN app.integration_sources s ON s.id=c.source_id AND s.organization_id=c.organization_id WHERE c.key_sha256=$1 AND c.revoked_at IS NULL AND (c.expires_at IS NULL OR c.expires_at>now()) AND s.enabled")
        .bind(hash(key)).fetch_optional(&app.db).await?
        .ok_or(Error(StatusCode::UNAUTHORIZED, "invalid_integration_key"))?;
    Ok((
        row.get("source_id"),
        row.get("organization_id"),
        row.get("id"),
    ))
}

fn normalized_name(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

async fn manual_match(
    tx: &mut Transaction<'_, Postgres>,
    org: Uuid,
    input: &UpsertTask,
) -> Result<Option<Uuid>> {
    let Some(birth) = input.birth_date.0.as_ref() else { return Ok(None); };
    let names = [&input.last_name, &input.first_name, &input.middle_name].map(|v| normalized_name(v));
    if names.iter().any(String::is_empty) { return Ok(None); }
    // Serialize competing imports across sources without using demographics as lock text.
    let key = hash(serde_json::to_vec(&(org, birth.as_str(), &names)).unwrap());
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("1c_manual_match:{key}")).execute(&mut **tx).await?;
    let find = |rows: Vec<sqlx::postgres::PgRow>| {
        let mut matching = rows.into_iter().filter(|r| {
            [r.get::<String,_>("family_name"),r.get::<String,_>("given_name"),r.get::<Option<String>,_>("patronymic").unwrap_or_default()]
                .map(|v|normalized_name(&v)) == names
        });
        let row=matching.next()?;
        if matching.next().is_some() || !row.get::<bool,_>("eligible") { return None; }
        use m53_contracts::colposcopy_1c::Sex;
        let sex=row.get::<Option<String>,_>("sex");
        if matches!((sex.as_deref(),input.sex),(Some("F"),Sex::M)|(Some("M"),Sex::F)) {return None;}

        Some(row.get::<Uuid,_>("id"))
    };
    const CANDIDATES:&str="SELECT p.id,p.family_name,p.given_name,p.patronymic,p.sex,(p.origin='manual' AND p.identity_state='provisional' AND NOT EXISTS(SELECT 1 FROM app.patient_source_links l WHERE l.patient_id=p.id) AND NOT EXISTS(SELECT 1 FROM app.patient_external_ids e WHERE e.patient_id=p.id)) AS eligible FROM app.patients p WHERE p.organization_id=$1 AND p.birth_date=$2::text::date";
    let Some(id)=find(sqlx::query(CANDIDATES).bind(org).bind(birth.as_str()).fetch_all(&mut **tx).await?) else {return Ok(None);};
    // The row lock fences demographic edits and FK-based source-link insertion.
    sqlx::query("SELECT id FROM app.patients WHERE id=$1 AND organization_id=$2 FOR UPDATE")
        .bind(id).bind(org).fetch_optional(&mut **tx).await?;
    let current=find(sqlx::query(CANDIDATES).bind(org).bind(birth.as_str()).fetch_all(&mut **tx).await?);
    Ok(current.filter(|candidate|*candidate==id))
}

async fn patient(
    tx: &mut Transaction<'_, Postgres>,
    source: Uuid,
    org: Uuid,
    input: &UpsertTask,
    request_id: Option<Uuid>,
) -> Result<Uuid> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("1c_patient:{source}:{}", input.patient_key))
        .execute(&mut **tx)
        .await?;
    let existing: Option<Uuid> = sqlx::query_scalar("SELECT patient_id FROM app.patient_source_links WHERE source_id=$1 AND external_guid=$2 AND organization_id=$3")
        .bind(source).bind(input.patient_key).bind(org).fetch_optional(&mut **tx).await?;

    if input.card_number.as_ref().is_some_and(|v| v.trim().is_empty() || v.len()>128 || v.chars().any(char::is_control)) {
        return Err(Error(StatusCode::BAD_REQUEST,"invalid_card_number"));
    }
    let birth = input.birth_date.0.as_ref().map(|d| d.as_str());
    let sex = match input.sex {
        m53_contracts::colposcopy_1c::Sex::F => "F",
        m53_contracts::colposcopy_1c::Sex::M => "M",
        m53_contracts::colposcopy_1c::Sex::O => "O",
    };
    let matched = if existing.is_none() { manual_match(tx, org, input).await? } else { None };
    let id = existing.or(matched).unwrap_or_else(Uuid::new_v4);
    if existing.is_none() && matched.is_none() {
        sqlx::query("INSERT INTO app.patients(id,organization_id,family_name,given_name,patronymic,birth_date,sex,card_number,origin,identity_state) VALUES($1,$2,$3,$4,$5,$6::date,$7,$8,'1c','confirmed')")
            .bind(id).bind(org).bind(&input.last_name).bind(&input.first_name).bind(&input.middle_name).bind(birth).bind(sex).bind(&input.card_number).execute(&mut **tx).await?;
    } else {
        sqlx::query("UPDATE app.patients SET family_name=$2,given_name=$3,patronymic=$4,birth_date=$5::date,sex=$7,card_number=coalesce($8,card_number),origin='1c',identity_state='confirmed' WHERE id=$1 AND organization_id=$6")
            .bind(id).bind(&input.last_name).bind(&input.first_name).bind(&input.middle_name).bind(birth).bind(org).bind(sex).bind(&input.card_number).execute(&mut **tx).await?;
    }
    if existing.is_none() {
        sqlx::query("INSERT INTO app.patient_source_links(source_id,external_guid,organization_id,patient_id) VALUES($1,$2,$3,$4)")
            .bind(source).bind(input.patient_key).bind(org).bind(id).execute(&mut **tx).await?;
        sqlx::query("INSERT INTO app.patient_external_ids(patient_id,source,external_id,dicom_issuer) VALUES($1,$2,$3,$4)")
            .bind(id).bind(format!("1c:{source}")).bind(input.patient_key.to_string()).bind(format!("M53-1C-{source}")).execute(&mut **tx).await?;
    }
    if matched.is_some() {
        sqlx::query("INSERT INTO app.audit_events(id,organization_id,entity_id,action,entity_version,safe_description,request_id) SELECT $1,$2,p.id,'patient.manual_linked_from_mis',p.row_version,'exact_full_name_and_birth_date',$4 FROM app.patients p WHERE p.id=$3")
            .bind(Uuid::new_v4()).bind(org).bind(id).bind(request_id).execute(&mut **tx).await?;
    }
    let snapshot = json!({"last_name":input.last_name,"first_name":input.first_name,"middle_name":input.middle_name,"birth_date":input.birth_date,"sex":input.sex,"card_number":input.card_number});
    sqlx::query("UPDATE app.patient_source_links SET source_hash=$3,last_seen_at=now() WHERE source_id=$1 AND external_guid=$2")
        .bind(source).bind(input.patient_key).bind(hash(m53_contracts::canonical_json::to_vec(&snapshot).map_err(|_|Error(StatusCode::BAD_REQUEST,"invalid_task"))?)).execute(&mut **tx).await?;
    Ok(id)
}

pub async fn receive(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<(StatusCode, Json<TaskAck>)> {
    let (source_id, org, credential) = source(&app, &headers).await?;
    let request_id=headers.get("x-m53-request-id").and_then(|v|v.to_str().ok()).and_then(|v|Uuid::parse_str(v).ok());
    let input: TaskRequest = serde_json::from_slice(&bytes)
        .map_err(|_| Error(StatusCode::BAD_REQUEST, "invalid_task"))?;
    if !input.validate().is_empty() || bytes.len() > 128 * 1024 {
        return Err(Error(StatusCode::BAD_REQUEST, "invalid_task"));
    }
    let request_hash = hash(
        input
            .normalized_body()
            .map_err(|_| Error(StatusCode::BAD_REQUEST, "invalid_task"))?,
    );
    let key = input.application_key();
    let ack = TaskAck::accepted(&input);
    let mut tx = app.db.begin().await?;
    sqlx::query("SET LOCAL lock_timeout='4s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("1c_task:{source_id}:{key}"))
        .execute(&mut *tx)
        .await?;
    let existing = sqlx::query(
        "SELECT * FROM app.colposcopy_tasks WHERE source_id=$1 AND application_key=$2 FOR UPDATE",
    )
    .bind(source_id)
    .bind(key)
    .fetch_optional(&mut *tx)
    .await?;
    if existing.as_ref().is_some_and(|r| {
        r.get::<Option<String>, _>("last_request_hash").as_deref() == Some(&request_hash)
    }) {
        if let TaskRequest::Upsert(t) = &input {
            let linked=patient(&mut tx, source_id, org, t, request_id).await?;
            if existing.as_ref().and_then(|r|r.get::<Option<Uuid>,_>("study_patient_id")).is_some_and(|id|id!=linked) {
                return Err(Error(StatusCode::CONFLICT,"task_patient_change_requires_review"));
            }
        }
        tx.commit().await?;
        return Ok((StatusCode::OK, Json(ack)));
    }
    let fresh = existing.is_none();
    let id = existing
        .as_ref()
        .map(|r| r.get("id"))
        .unwrap_or_else(Uuid::new_v4);
    let sequence = existing
        .as_ref()
        .map(|r| r.get::<i64, _>("request_sequence"))
        .unwrap_or(0)
        + 1;
    let version = existing
        .as_ref()
        .map(|r| r.get::<i64, _>("row_version") + 1)
        .unwrap_or(1);
    let receipt = Uuid::new_v4();
    let (patient_guid, snapshot, state) = match &input {
        TaskRequest::Cancel(_) => (
            existing
                .as_ref()
                .and_then(|r| r.get::<Option<Uuid>, _>("patient_guid")),
            existing
                .as_ref()
                .map(|r| r.get::<Value, _>("patient_snapshot"))
                .unwrap_or(json!({})),
            "cancelled".to_string(),
        ),
        TaskRequest::Upsert(t) => {
            let linked = patient(&mut tx, source_id, org, t, request_id).await?;
            if let Some(row) = &existing
                && row
                    .get::<Option<Uuid>, _>("study_patient_id")
                    .is_some_and(|id| id != linked)
            {
                return Err(Error(
                    StatusCode::CONFLICT,
                    "task_patient_change_requires_review",
                ));
            }
            let previous = existing
                .as_ref()
                .map(|r| r.get::<String, _>("state"))
                .unwrap_or_else(|| "waiting".into());
            let state = if previous == "cancelled" {
                if let Some(study) = existing
                    .as_ref()
                    .and_then(|r| r.get::<Option<Uuid>, _>("study_id"))
                {
                    let status: String =
                        sqlx::query_scalar("SELECT status FROM app.studies WHERE id=$1")
                            .bind(study)
                            .fetch_one(&mut *tx)
                            .await?;
                    if status == "approved" {
                        "completed"
                    } else {
                        "in_progress"
                    }
                } else {
                    "waiting"
                }
                .to_string()
            } else {
                previous
            };
            (
                Some(t.patient_key),
                json!({"last_name":t.last_name,"first_name":t.first_name,"middle_name":t.middle_name,"birth_date":t.birth_date,"sex":t.sex,"card_number":t.card_number}),
                state,
            )
        }
    };
    if fresh {
        sqlx::query("INSERT INTO app.colposcopy_tasks(id,source_id,organization_id,application_key,task_name,appointment_date,patient_guid,patient_snapshot,state,row_version,request_sequence,last_request_hash,last_receipt_id,cancelled_at) VALUES($1,$2,$3,$4,$5,$6::date,$7,$8,$9,$10,$11,$12,$13,CASE WHEN $9='cancelled' THEN now() ELSE NULL END)")
            .bind(id).bind(source_id).bind(org).bind(key).bind(input.task_name()).bind(input.appointment_date().as_str()).bind(patient_guid).bind(snapshot).bind(&state).bind(version).bind(sequence).bind(&request_hash).bind(receipt).execute(&mut *tx).await?;
    } else {
        sqlx::query("UPDATE app.colposcopy_tasks SET task_name=$2,appointment_date=$3::date,patient_guid=$4,patient_snapshot=$5,state=$6,row_version=$7,request_sequence=$8,last_request_hash=$9,last_receipt_id=$10,cancelled_at=CASE WHEN $6='cancelled' THEN now() ELSE NULL END,updated_at=now() WHERE id=$1")
            .bind(id).bind(input.task_name()).bind(input.appointment_date().as_str()).bind(patient_guid).bind(snapshot).bind(&state).bind(version).bind(sequence).bind(&request_hash).bind(receipt).execute(&mut *tx).await?;
    }
    let operation = match input.operation() {
        Operation::Upsert => "upsert",
        Operation::Cancel => "cancel",
    };
    sqlx::query("INSERT INTO app.task_inbox(id,task_id,source_id,organization_id,application_key,credential_id,request_sequence,operation,request_hash,appointment_date,payload,response) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10::date,$11,$12)")
        .bind(receipt).bind(id).bind(source_id).bind(org).bind(key).bind(credential).bind(sequence).bind(operation).bind(&request_hash).bind(input.appointment_date().as_str()).bind(json!(input)).bind(json!(ack)).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO app.task_events(id,task_id,source_id,organization_id,application_key,inbox_id,event_type,task_version) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
        .bind(Uuid::new_v4()).bind(id).bind(source_id).bind(org).bind(key).bind(receipt).bind(operation).bind(version).execute(&mut *tx).await?;
    if matches!(input, TaskRequest::Upsert(_))
        && existing
            .as_ref()
            .is_some_and(|r| r.get::<String, _>("state") == "cancelled")
    {
        sqlx::query("UPDATE app.result_delivery_jobs j SET state='retry',safe_error_code=NULL,next_attempt_at=now() WHERE j.task_id=$1 AND j.state='blocked' AND j.safe_error_code='task_cancelled' AND j.patient_guid=$2 AND j.appointment_date=$3::date AND NOT EXISTS(SELECT 1 FROM app.result_delivery_jobs newer WHERE newer.task_id=j.task_id AND newer.sequence>j.sequence AND newer.state='sent')")
            .bind(id).bind(patient_guid).bind(input.appointment_date().as_str()).execute(&mut *tx).await?;
    }
    crate::core_sync::enqueue(&mut tx, id)
        .await
        .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "enrichment_queue_failed"))?;
    tx.commit().await?;
    Ok((
        if fresh {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(ack),
    ))
}

pub async fn worklist(State(app): State<Arc<App>>, headers: HeaderMap) -> Result<Json<Vec<Value>>> {
    let a = actor(&app, &headers).await?;
    let doctor: Option<Uuid> = sqlx::query_scalar("SELECT doctor_id FROM app.users WHERE id=$1")
        .bind(a.user)
        .fetch_one(&app.db)
        .await?;
    let rows=sqlx::query("SELECT t.id,t.source_id,t.application_key,t.task_name,t.appointment_date::text AS day,t.patient_guid,t.patient_snapshot,t.state,t.assigned_doctor_id,t.study_id,t.row_version,CASE WHEN j.state='done' AND j.request_sequence=t.request_sequence AND j.patient_guid=t.patient_guid THEN j.core_patient_snapshot ELSE NULL END AS core_patient FROM app.colposcopy_tasks t LEFT JOIN app.task_core_enrichment j ON j.task_id=t.id WHERE t.organization_id=$1 AND t.state<>'cancelled' AND (t.assigned_doctor_id IS NULL OR t.assigned_doctor_id=$2) ORDER BY t.appointment_date,t.id LIMIT 500")
        .bind(a.org).bind(doctor).fetch_all(&app.db).await?;
    Ok(Json(rows.iter().map(|r|json!({"id":r.get::<Uuid,_>("id"),"source_id":r.get::<Uuid,_>("source_id"),"application_key":r.get::<Uuid,_>("application_key"),"task_name":r.get::<Uuid,_>("task_name"),"appointment_date":r.get::<String,_>("day"),"patient_key":r.get::<Option<Uuid>,_>("patient_guid"),"patient":r.get::<Value,_>("patient_snapshot"),"core_patient":r.get::<Option<Value>,_>("core_patient"),"state":r.get::<String,_>("state"),"assigned_doctor_id":r.get::<Option<Uuid>,_>("assigned_doctor_id"),"study_id":r.get::<Option<Uuid>,_>("study_id"),"row_version":r.get::<i64,_>("row_version")})).collect()))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimRequest {
    expected_version: i64,
    colposcope_model: Option<String>,
}

pub async fn claim(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<ClaimRequest>,
) -> Result<Json<Value>> {
    let a = actor(&app, &headers).await?;
    crate::api::validate_colposcope_model(input.colposcope_model.as_deref())?;
    let doctor: Uuid=sqlx::query_scalar("SELECT d.id FROM app.doctors d JOIN app.users u ON u.doctor_id=d.id AND u.organization_id=d.organization_id WHERE u.id=$1 AND d.active")
        .bind(a.user).fetch_optional(&app.db).await?.ok_or(Error(StatusCode::FORBIDDEN,"doctor_profile_required"))?;
    let mut tx = app.db.begin().await?;
    let task = sqlx::query(
        "SELECT *,appointment_date::text AS appointment_date_text FROM app.colposcopy_tasks WHERE id=$1 AND organization_id=$2 FOR UPDATE",
    )
    .bind(id)
    .bind(a.org)
    .fetch_one(&mut *tx)
    .await?;
    crate::api::revalidate_write_access(&mut tx, &a).await?;
    if task.get::<String, _>("state") == "cancelled" {
        return Err(Error(StatusCode::CONFLICT, "task_cancelled"));
    }
    if let Some(owner) = task.get::<Option<Uuid>, _>("assigned_doctor_id") {
        if owner != doctor {
            return Err(Error(StatusCode::CONFLICT, "task_already_claimed"));
        }
        let study: Option<Uuid> = task.get("study_id");
        crate::api::revalidate_write_access(&mut tx, &a).await?;
        tx.commit().await?;
        return Ok(Json(
            json!({"task_id":id,"study_id":study,"row_version":task.get::<i64,_>("row_version")}),
        ));
    }
    if input.expected_version != task.get::<i64, _>("row_version") {
        return Err(Error(StatusCode::CONFLICT, "stale_task_version"));
    }
    let patient: Uuid=sqlx::query_scalar("SELECT patient_id FROM app.patient_source_links WHERE source_id=$1 AND external_guid=$2 AND organization_id=$3")
        .bind(task.get::<Uuid,_>("source_id")).bind(task.get::<Option<Uuid>,_>("patient_guid")).bind(a.org).fetch_one(&mut *tx).await?;
    let study = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO app.studies(id,patient_id,author_id,study_instance_uid,organization_id,assigned_doctor_id,origin,encounter_date,colposcope_model) VALUES($1,$2,$3,$4,$5,$6,'1c_task',$7::text::date,$8)",
    )
    .bind(study)
    .bind(patient)
    .bind(a.user)
    .bind(format!("2.25.{}", Uuid::new_v4().as_u128()))
    .bind(a.org).bind(doctor).bind(task.get::<String,_>("appointment_date_text")).bind(input.colposcope_model)
    .execute(&mut *tx)
    .await?;
    let version = task.get::<i64, _>("row_version") + 1;
    sqlx::query("UPDATE app.colposcopy_tasks SET assigned_doctor_id=$2,study_id=$3,study_patient_id=$4,state='in_progress',row_version=$5,updated_at=now() WHERE id=$1")
        .bind(id).bind(doctor).bind(study).bind(patient).bind(version).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO app.task_events(id,task_id,source_id,organization_id,application_key,event_type,task_version,actor_user_id) VALUES($1,$2,$3,$4,$5,'claimed',$6,$7)")
        .bind(Uuid::new_v4()).bind(id).bind(task.get::<Uuid,_>("source_id")).bind(a.org).bind(task.get::<Uuid,_>("application_key")).bind(version).bind(a.user).execute(&mut *tx).await?;
    crate::api::audit(&mut tx, &a, id, "task.claimed", version).await?;
    crate::api::revalidate_write_access(&mut tx, &a).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"task_id":id,"study_id":study,"row_version":version}),
    ))
}
