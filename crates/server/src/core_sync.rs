//! Optional, durable read-only Core enrichment. Network I/O happens between
//! short transactions; a generation/lease fences stale HTTP responses.
use crate::{
    api::{App, hash},
    odata_core::{CoreClient, CoreError, Doctor, OdataUserAccount, Patient},
};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use std::collections::HashSet;
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, SyncError>;

#[derive(Clone, Copy, Debug)]
pub enum SyncError {
    Configuration,
    Database,
    InvalidSource,
    Busy,
    LostLease,
    PageLimit,
    Core(CoreError),
}

impl SyncError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Configuration => "core_sync_configuration",
            Self::Database => "core_sync_database",
            Self::InvalidSource => "core_sync_invalid_source",
            Self::Busy => "core_sync_busy",
            Self::LostLease => "core_sync_lost_lease",
            Self::PageLimit => "core_sync_page_limit",
            Self::Core(error) => error.code(),
        }
    }
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code())
    }
}
impl std::error::Error for SyncError {}
impl From<sqlx::Error> for SyncError {
    fn from(_: sqlx::Error) -> Self {
        Self::Database
    }
}
impl From<CoreError> for SyncError {
    fn from(error: CoreError) -> Self {
        Self::Core(error)
    }
}

struct Config {
    client: CoreClient,
    source: Uuid,
}

impl Config {
    fn from_env() -> Result<Option<Self>> {
        let Some(client) = CoreClient::from_env()? else {
            return Ok(None);
        };
        let source = std::env::var("M53_ODATA_CORE_SOURCE_ID")
            .ok()
            .and_then(|value| value.parse::<Uuid>().ok())
            .filter(|value| !value.is_nil())
            .ok_or(SyncError::Configuration)?;
        Ok(Some(Self { client, source }))
    }
}

/// Call inside the task-inbox transaction, after saving its new sequence. This
/// does not contact Core. Replay acknowledgements need no additional enqueue.
pub async fn enqueue(tx: &mut Transaction<'_, Postgres>, task: Uuid) -> Result<()> {
    sqlx::query(
        "INSERT INTO app.task_core_enrichment(task_id,source_id,patient_guid,request_sequence,state) \
         SELECT id,source_id,patient_guid,request_sequence,CASE WHEN state='cancelled' OR patient_guid IS NULL THEN 'skipped' ELSE 'pending' END \
         FROM app.colposcopy_tasks WHERE id=$1 \
         ON CONFLICT(task_id) DO UPDATE SET patient_guid=excluded.patient_guid,request_sequence=excluded.request_sequence,\
         state=excluded.state,attempts=0,lease_id=NULL,lease_until=NULL,next_attempt_at=now(),safe_error_code=NULL,\
         core_patient_snapshot=NULL,source_version=NULL,source_hash=NULL,enriched_at=NULL,updated_at=now()",
    )
    .bind(task)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

struct Job {
    task: Uuid,
    source: Uuid,
    patient: Uuid,
    sequence: i64,
    lease: Uuid,
}

async fn claim(app: &App, source: Uuid, task: Option<Uuid>) -> Result<Option<Job>> {
    let mut tx = app.db.begin().await?;
    let row = sqlx::query(
        "SELECT j.task_id,j.patient_guid,j.request_sequence FROM app.task_core_enrichment j \
         JOIN app.integration_sources s ON s.id=j.source_id AND s.enabled \
         WHERE j.source_id=$1 AND ($2::uuid IS NULL OR j.task_id=$2) AND j.patient_guid IS NOT NULL \
         AND ((j.state IN ('pending','retry') AND j.next_attempt_at<=now()) OR (j.state='running' AND j.lease_until<now())) \
         ORDER BY j.next_attempt_at,j.task_id FOR UPDATE OF j SKIP LOCKED LIMIT 1",
    )
    .bind(source)
    .bind(task)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tx.commit().await?;
        return Ok(None);
    };
    let job = Job {
        task: row.get("task_id"),
        source,
        patient: row.get("patient_guid"),
        sequence: row.get("request_sequence"),
        lease: Uuid::new_v4(),
    };
    sqlx::query("UPDATE app.task_core_enrichment SET state='running',attempts=attempts+1,lease_id=$2,lease_until=now()+interval '30 seconds',updated_at=now() WHERE task_id=$1")
        .bind(job.task).bind(job.lease).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Some(job))
}

async fn complete(
    app: &App,
    job: &Job,
    response: std::result::Result<Option<Patient>, CoreError>,
) -> Result<()> {
    let mut tx = app.db.begin().await?;
    // Match task lock ordering in inbox/claim before taking the job row lock.
    let task = sqlx::query("SELECT t.request_sequence,t.patient_guid,t.state FROM app.colposcopy_tasks t JOIN app.integration_sources s ON s.id=t.source_id AND s.enabled WHERE t.id=$1 AND t.source_id=$2 FOR UPDATE OF t")
        .bind(job.task).bind(job.source).fetch_optional(&mut *tx).await?;
    let Some(task) = task else {
        return Err(SyncError::LostLease);
    };
    let owned = sqlx::query("SELECT lease_id,request_sequence,patient_guid,lease_until>now() AS live FROM app.task_core_enrichment WHERE task_id=$1 FOR UPDATE")
        .bind(job.task).fetch_optional(&mut *tx).await?;
    if !owned.is_some_and(|row| {
        row.get::<Option<Uuid>, _>("lease_id") == Some(job.lease)
            && row.get::<Option<bool>, _>("live") == Some(true)
            && row.get::<i64, _>("request_sequence") == job.sequence
            && row.get::<Option<Uuid>, _>("patient_guid") == Some(job.patient)
    }) {
        tx.commit().await?;
        return Ok(()); // A new request/lease already owns this job.
    }
    if task.get::<i64, _>("request_sequence") != job.sequence
        || task.get::<Option<Uuid>, _>("patient_guid") != Some(job.patient)
        || task.get::<String, _>("state") == "cancelled"
    {
        sqlx::query("UPDATE app.task_core_enrichment SET state='skipped',lease_id=NULL,lease_until=NULL,safe_error_code='core_stale_task',updated_at=now() WHERE task_id=$1 AND lease_id=$2")
            .bind(job.task).bind(job.lease).execute(&mut *tx).await?;
    } else {
        match response {
            Ok(Some(patient)) => {
                let snapshot =
                    serde_json::to_value(&patient).map_err(|_| SyncError::Configuration)?;
                let digest = hash(
                    m53_contracts::canonical_json::to_vec(&snapshot)
                        .map_err(|_| SyncError::Configuration)?,
                );
                sqlx::query("UPDATE app.task_core_enrichment SET state='done',lease_id=NULL,lease_until=NULL,safe_error_code=NULL,core_patient_snapshot=$3,source_version=$4,source_hash=$5,enriched_at=now(),updated_at=now() WHERE task_id=$1 AND lease_id=$2")
                    .bind(job.task).bind(job.lease).bind(snapshot).bind(&patient.source_version).bind(&digest).execute(&mut *tx).await?;
                // Only metadata on an existing exact GUID link is updated. The
                // inbox patient snapshot and clinical patient row stay intact.
                sqlx::query("UPDATE app.patient_source_links SET core_source_version=$3,core_source_hash=$4,core_last_seen_at=now() WHERE source_id=$1 AND external_guid=$2")
                    .bind(job.source).bind(job.patient).bind(&patient.source_version).bind(&digest).execute(&mut *tx).await?;
            }
            other => {
                let code = match other {
                    Ok(None) => "odata_core_patient_not_found",
                    Err(error) => error.code(),
                    Ok(Some(_)) => unreachable!(),
                };
                sqlx::query("UPDATE app.task_core_enrichment SET state='retry',lease_id=NULL,lease_until=NULL,safe_error_code=$3,next_attempt_at=now()+make_interval(secs=>least(3600.0,attempts::double precision*attempts::double precision*5.0)),updated_at=now() WHERE task_id=$1 AND lease_id=$2")
                    .bind(job.task).bind(job.lease).bind(code).execute(&mut *tx).await?;
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Disabled until URL and source binding are explicitly configured. Returns
/// false when disabled or idle; the API receipt never waits for this worker.
pub async fn run_once(app: &App) -> Result<bool> {
    let Some(config) = Config::from_env()? else {
        return Ok(false);
    };
    let Some(job) = claim(app, config.source, None).await? else {
        return Ok(false);
    };
    let response = config.client.patient(job.patient).await;
    complete(app, &job, response).await?;
    Ok(true)
}

pub async fn enrich_patient(app: &App, task: Uuid) -> Result<bool> {
    let Some(config) = Config::from_env()? else {
        return Ok(false);
    };
    let Some(job) = claim(app, config.source, Some(task)).await? else {
        return Ok(false);
    };
    let response = config.client.patient(job.patient).await;
    complete(app, &job, response).await?;
    Ok(true)
}

async fn save_doctor(
    tx: &mut Transaction<'_, Postgres>,
    source: Uuid,
    org: Uuid,
    doctor: Doctor,
) -> Result<Option<Uuid>> {
    let value = serde_json::to_value(&doctor).map_err(|_| SyncError::Configuration)?;
    let digest =
        hash(m53_contracts::canonical_json::to_vec(&value).map_err(|_| SyncError::Configuration)?);
    let link = sqlx::query("SELECT doctor_id,source_hash FROM app.doctor_external_ids WHERE source_id=$1 AND external_guid=$2 AND organization_id=$3 FOR UPDATE")
        .bind(source).bind(doctor.guid).bind(org).fetch_optional(&mut **tx).await?;
    // Only an eligible new physician is imported. An existing source link is
    // retained when an explicit source response makes that physician inactive.
    if link.is_none() && !doctor.active {
        return Ok(None);
    }
    let id = link
        .as_ref()
        .map(|row| row.get::<Uuid, _>("doctor_id"))
        .unwrap_or_else(Uuid::new_v4);
    let details = json!({"department_guid":doctor.department_guid,"position_guid":doctor.position_guid,"specialization_guid":doctor.specialization_guid,
        "participates_in_calendar_planning":doctor.participates_in_calendar_planning,
        "eligibility_policy":"obgyn_calendar_planning_v1"});
    if let Some(link) = link {
        if link.get::<Option<String>, _>("source_hash").as_deref() != Some(&digest) {
            sqlx::query("UPDATE app.doctors SET display_name=$2,family_name=$3,given_name=$4,patronymic=$5,active=$6,source_details=$7,row_version=row_version+1,updated_at=now() WHERE id=$1 AND organization_id=$8")
                .bind(id).bind(&doctor.display_name).bind(&doctor.family_name).bind(&doctor.given_name).bind(&doctor.patronymic).bind(doctor.active).bind(details).bind(org).execute(&mut **tx).await?;
        }
        sqlx::query("UPDATE app.doctor_external_ids SET source_hash=$3,source_version=$4,last_seen_at=now() WHERE source_id=$1 AND external_guid=$2")
            .bind(source).bind(doctor.guid).bind(&digest).bind(&doctor.source_version).execute(&mut **tx).await?;
    } else {
        sqlx::query("INSERT INTO app.doctors(id,organization_id,display_name,family_name,given_name,patronymic,active,origin,source_details) VALUES($1,$2,$3,$4,$5,$6,$7,'1c',$8)")
            .bind(id).bind(org).bind(&doctor.display_name).bind(&doctor.family_name).bind(&doctor.given_name).bind(&doctor.patronymic).bind(doctor.active).bind(details).execute(&mut **tx).await?;
        sqlx::query("INSERT INTO app.doctor_external_ids(source_id,external_guid,organization_id,doctor_id,source_version,source_hash) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(source).bind(doctor.guid).bind(org).bind(id).bind(&doctor.source_version).bind(&digest).execute(&mut **tx).await?;
    }
    Ok(Some(id))
}

async fn save_user_account(
    tx: &mut Transaction<'_, Postgres>,
    source: Uuid,
    org: Uuid,
    doctor_id: Uuid,
    eligible: bool,
    accounts: &[OdataUserAccount],
) -> Result<()> {
    let mut active = accounts
        .iter()
        .filter(|account| account.active && !account.login.is_empty());
    let selected = active.next();
    if eligible && selected.is_some() && active.next().is_none() {
        let selected = selected.ok_or(SyncError::Configuration)?;
        sqlx::query("INSERT INTO app.doctor_1c_logins(doctor_id,organization_id,source_id,user_guid,login,enabled) VALUES($1,$2,$3,$4,$5,true) ON CONFLICT(doctor_id) DO UPDATE SET organization_id=excluded.organization_id,source_id=excluded.source_id,user_guid=excluded.user_guid,login=excluded.login,enabled=true")
            .bind(doctor_id).bind(org).bind(source).bind(selected.guid).bind(&selected.login)
            .execute(&mut **tx).await?;
    } else {
        // Never invent an account GUID or blank login when the exact directory
        // link is absent/disabled/ambiguous. Preserve any old mapping as disabled.
        sqlx::query("UPDATE app.doctor_1c_logins SET enabled=false WHERE doctor_id=$1 AND organization_id=$2 AND source_id=$3")
            .bind(doctor_id).bind(org).bind(source).execute(&mut **tx).await?;
    }
    Ok(())
}

async fn own_doctor_sync(
    tx: &mut Transaction<'_, Postgres>,
    source: Uuid,
    lease: Uuid,
) -> Result<()> {
    let owned = sqlx::query("SELECT c.lease_id,c.lease_until>now() AS live FROM app.core_doctor_sync c JOIN app.integration_sources s ON s.id=c.source_id AND s.enabled WHERE c.source_id=$1 FOR UPDATE OF c")
        .bind(source).fetch_optional(&mut **tx).await?;
    if !owned.is_some_and(|row| {
        row.get::<Option<Uuid>, _>("lease_id") == Some(lease)
            && row.get::<Option<bool>, _>("live") == Some(true)
    }) {
        return Err(SyncError::LostLease);
    }
    Ok(())
}

/// Explicit administrator/CLI operation, never a side effect of user login.
/// Updates profiles only: does not create passwords, sessions, users or rights.
pub async fn sync_doctors(app: &App, source: Uuid) -> Result<usize> {
    let config = Config::from_env()?.ok_or(SyncError::Configuration)?;
    if config.source != source {
        return Err(SyncError::InvalidSource);
    }
    let org: Uuid = sqlx::query_scalar(
        "SELECT organization_id FROM app.integration_sources WHERE id=$1 AND enabled",
    )
    .bind(source)
    .fetch_optional(&app.db)
    .await?
    .ok_or(SyncError::InvalidSource)?;
    let lease = Uuid::new_v4();
    let row = sqlx::query("INSERT INTO app.core_doctor_sync(source_id,lease_id,lease_until) VALUES($1,$2,now()+interval '30 seconds') ON CONFLICT(source_id) DO UPDATE SET lease_id=excluded.lease_id,lease_until=excluded.lease_until,safe_error_code=NULL,updated_at=now() WHERE app.core_doctor_sync.lease_until IS NULL OR app.core_doctor_sync.lease_until<now() RETURNING confirmed_skip")
        .bind(source).bind(lease).fetch_optional(&app.db).await?.ok_or(SyncError::Busy)?;
    let mut skip =
        u32::try_from(row.get::<i64, _>("confirmed_skip")).map_err(|_| SyncError::Configuration)?;
    let mut count = 0;
    let result: Result<()> = async {
        let mut seen = HashSet::new();
        let mut complete = false;
        for _ in 0..1000 {
            let page = config.client.doctors_page(skip, 200).await?;
            for doctor in page.items {
                seen.insert(doctor.guid);
                let eligible=doctor.active;
                // Core only supplies a directory link. It never authenticates
                // the user and neither passwords nor hashes are requested.
                let accounts=if eligible {config.client.user_accounts(doctor.guid).await?} else {vec![]};
                let mut tx = app.db.begin().await?;
                own_doctor_sync(&mut tx,source,lease).await?;
                if let Some(id)=save_doctor(&mut tx, source, org, doctor).await? {
                    save_user_account(&mut tx,source,org,id,eligible,&accounts).await?;
                    count += 1;
                }
                sqlx::query("UPDATE app.core_doctor_sync SET lease_until=now()+interval '30 seconds',updated_at=now() WHERE source_id=$1 AND lease_id=$2")
                    .bind(source).bind(lease).execute(&mut *tx).await?;
                tx.commit().await?;
            }
            let mut tx=app.db.begin().await?;
            own_doctor_sync(&mut tx,source,lease).await?;
            if let Some(next) = page.next_skip {
                skip = next;
                sqlx::query("UPDATE app.core_doctor_sync SET confirmed_skip=$3,lease_until=now()+interval '30 seconds',updated_at=now() WHERE source_id=$1 AND lease_id=$2")
                    .bind(source).bind(lease).bind(i64::from(skip)).execute(&mut *tx).await?;
                tx.commit().await?;
            } else {
                sqlx::query("UPDATE app.core_doctor_sync SET confirmed_skip=0,lease_until=now()+interval '30 seconds',updated_at=now() WHERE source_id=$1 AND lease_id=$2")
                    .bind(source).bind(lease).execute(&mut *tx).await?;
                tx.commit().await?;
                complete = true;
                break;
            }
        }
        if !complete { return Err(SyncError::PageLimit); }

        // Filtered pages cannot prove that a previously known physician was
        // archived, removed from planning or moved to another position. Ask for
        // those exact GUIDs; a missing record alone never removes local rights.
        let linked: Vec<Uuid> = sqlx::query_scalar("SELECT external_guid FROM app.doctor_external_ids WHERE source_id=$1 AND organization_id=$2 ORDER BY external_guid")
            .bind(source).bind(org).fetch_all(&app.db).await?;
        for guid in linked {
            if seen.contains(&guid) { continue; }
            // No transaction or database lock is held during Core HTTP.
            let response = config.client.doctor(guid).await?;
            let accounts=if response.as_ref().is_some_and(|doctor|doctor.active) {
                config.client.user_accounts(guid).await?
            } else {vec![]};
            let mut tx = app.db.begin().await?;
            own_doctor_sync(&mut tx,source,lease).await?;
            if let Some(doctor) = response {
                let eligible=doctor.active;
                if let Some(id)=save_doctor(&mut tx,source,org,doctor).await? {
                    save_user_account(&mut tx,source,org,id,eligible,&accounts).await?;
                    count += 1;
                }
            }
            sqlx::query("UPDATE app.core_doctor_sync SET lease_until=now()+interval '30 seconds',updated_at=now() WHERE source_id=$1 AND lease_id=$2")
                .bind(source).bind(lease).execute(&mut *tx).await?;
            tx.commit().await?;
        }
        let mut tx = app.db.begin().await?;
        own_doctor_sync(&mut tx,source,lease).await?;
        sqlx::query("UPDATE app.core_doctor_sync SET confirmed_skip=0,lease_id=NULL,lease_until=NULL,last_success_at=now(),safe_error_code=NULL,updated_at=now() WHERE source_id=$1 AND lease_id=$2")
            .bind(source).bind(lease).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }.await;
    if let Err(error) = result {
        let _ = sqlx::query("UPDATE app.core_doctor_sync SET lease_id=NULL,lease_until=NULL,safe_error_code=$3,updated_at=now() WHERE source_id=$1 AND lease_id=$2")
            .bind(source).bind(lease).bind(error.code()).execute(&app.db).await;
        return Err(error);
    }
    Ok(count)
}

/// Safe DTO for API views: no lease internals and no overwritten inbox fields.
pub async fn patient_enrichment(app: &App, task: Uuid) -> Result<Option<Value>> {
    Ok(sqlx::query_scalar("SELECT core_patient_snapshot FROM app.task_core_enrichment j JOIN app.colposcopy_tasks t ON t.id=j.task_id WHERE j.task_id=$1 AND j.state='done' AND j.request_sequence=t.request_sequence AND j.patient_guid=t.patient_guid")
        .bind(task).fetch_optional(&app.db).await?.flatten())
}
