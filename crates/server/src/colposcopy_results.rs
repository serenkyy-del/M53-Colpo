//! Direct PDF delivery to the supplied 1C contract. Independent of the legacy PACS hook.
//! Every job keeps its PDF/GUID/date snapshot; HTTP runs without a task row transaction.
use crate::api::{App, Error, hash};
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use axum::http::StatusCode;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use m53_contracts::colposcopy_1c::{
    CalendarDate, MAX_PDF_BYTES, ResultFailure, ResultKey, ResultReceipt, ResultRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Connection as _, PgConnection, Postgres, Row, Transaction, postgres::PgRow};
use std::{
    env,
    path::{Path, PathBuf},
};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

type WorkerResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
const CALLBACK_SUFFIX: &str = "/hs/reception/colposcopy-result";
#[cfg(test)]
const TEST_HTTP_CALLBACK: &str =
    "http://192.168.73.45:885/UMZ_BIT-DEV/hs/reception/colposcopy-result";
const MAX_RECEIPT_BYTES: usize = 32 * 1024;

/// Caller authenticates/authorizes the physician and owns COMMIT/rollback of this transaction.
pub async fn enqueue(
    app: &App,
    tx: &mut Transaction<'_, Postgres>,
    organization_id: Uuid,
    study_id: Uuid,
    report_id: Uuid,
) -> Result<Value, Error> {
    let rows = sqlx::query("SELECT t.id AS task_id,t.source_id,t.organization_id,t.application_key,t.patient_guid,to_char(t.appointment_date,'YYYY-MM-DD') AS appointment_date,t.state,t.assigned_doctor_id,t.study_patient_id,r.pdf_asset_id,a.storage_key,a.sha256,a.byte_size,a.mime_type,a.kind,s.status,pr.id AS revision_id,pr.approved_at IS NOT NULL AS approved,(SELECT id FROM app.protocol_revisions WHERE study_id=s.id ORDER BY revision DESC LIMIT 1)=pr.id AS latest_revision,EXISTS(SELECT 1 FROM app.patient_source_links l WHERE l.source_id=t.source_id AND l.organization_id=t.organization_id AND l.external_guid=t.patient_guid AND l.patient_id=t.study_patient_id) AS confirmed_patient,EXISTS(SELECT 1 FROM app.result_delivery_configs c WHERE c.source_id=t.source_id AND c.organization_id=t.organization_id) AS configured FROM app.colposcopy_tasks t JOIN app.studies s ON s.id=t.study_id JOIN app.patients p ON p.id=s.patient_id JOIN app.reports r ON r.id=$3 AND r.study_id=s.id JOIN app.protocol_revisions pr ON pr.id=r.protocol_revision_id JOIN app.assets a ON a.id=r.pdf_asset_id AND a.study_id=s.id WHERE t.organization_id=$1 AND p.organization_id=$1 AND t.study_id=$2 FOR UPDATE OF t")
        .bind(organization_id).bind(study_id).bind(report_id).fetch_all(&mut **tx).await?;
    if rows.len() != 1 {
        return Err(Error(StatusCode::CONFLICT, "task_report_binding_required"));
    }
    let row = &rows[0];
    crate::workflow_revision::require_current_approved(tx, study_id, row.get("revision_id")).await?;
    if row.get::<String, _>("state") == "cancelled" {
        return Err(Error(StatusCode::CONFLICT, "task_cancelled"));
    }
    if row.get::<Option<Uuid>, _>("assigned_doctor_id").is_none() {
        return Err(Error(StatusCode::CONFLICT, "task_not_claimed"));
    }
    if row.get::<String, _>("status") != "approved"
        || !row.get::<bool, _>("approved")
        || !row.get::<bool, _>("latest_revision")
    {
        return Err(Error(StatusCode::CONFLICT, "approved_report_required"));
    }
    if !row.get::<bool, _>("confirmed_patient") {
        return Err(Error(StatusCode::CONFLICT, "patient_identity_unconfirmed"));
    }
    if !row.get::<bool, _>("configured") {
        return Err(Error(
            StatusCode::CONFLICT,
            "result_delivery_not_configured",
        ));
    }
    if row.get::<String, _>("kind") != "pdf"
        || row.get::<String, _>("mime_type") != "application/pdf"
    {
        return Err(Error(StatusCode::BAD_REQUEST, "pdf_required"));
    }
    let asset: Uuid = row.get("pdf_asset_id");
    let expected_sha: String = row.get("sha256");
    read_pdf(
        &app.storage,
        asset,
        &row.get::<String, _>("storage_key"),
        row.get("byte_size"),
        &expected_sha,
    )
    .await
    .map_err(|code| Error(StatusCode::BAD_REQUEST, code))?;
    let task: Uuid = row.get("task_id");
    let patient_guid: Uuid = row.get("patient_guid");
    let date: String = row.get("appointment_date");
    if let Some(existing) = sqlx::query("SELECT id,state,sequence,patient_guid,to_char(appointment_date,'YYYY-MM-DD') AS appointment_date FROM app.result_delivery_jobs WHERE task_id=$1 AND pdf_sha256=$2")
        .bind(task).bind(&expected_sha).fetch_optional(&mut **tx).await? {
        if existing.get::<Uuid,_>("patient_guid") != patient_guid || existing.get::<String,_>("appointment_date") != date { return Err(Error(StatusCode::CONFLICT,"result_snapshot_conflict")); }
        return Ok(job_response(&existing));
    }
    let sequence: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(sequence),0)+1 FROM app.result_delivery_jobs WHERE task_id=$1",
    )
    .bind(task)
    .fetch_one(&mut **tx)
    .await?;
    let diagnoses = crate::result_diagnoses::freeze(tx, row.get("revision_id")).await?;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO app.result_delivery_jobs(id,task_id,source_id,organization_id,application_key,study_id,report_id,pdf_asset_id,pdf_sha256,patient_guid,appointment_date,sequence,diagnoses_snapshot) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11::text::date,$12,$13)")
        .bind(id).bind(task).bind(row.get::<Uuid,_>("source_id")).bind(organization_id).bind(row.get::<Uuid,_>("application_key")).bind(study_id).bind(report_id).bind(asset).bind(expected_sha).bind(patient_guid).bind(date).bind(sequence).bind(diagnoses).execute(&mut **tx).await?;
    Ok(json!({"job_id":id,"state":"pending","sequence":sequence}))
}
fn job_response(row: &PgRow) -> Value {
    json!({"job_id":row.get::<Uuid,_>("id"),"state":row.get::<String,_>("state"),"sequence":row.get::<i64,_>("sequence")})
}

async fn read_pdf(
    storage: &Path,
    asset: Uuid,
    key: &str,
    expected_bytes: i64,
    expected_sha: &str,
) -> Result<Vec<u8>, &'static str> {
    if key != asset.simple().to_string() {
        return Err("invalid_storage_key");
    }
    if expected_bytes <= 0 || expected_bytes as u64 > MAX_PDF_BYTES as u64 {
        return Err("pdf_size_invalid");
    }
    let file = tokio::fs::File::open(storage.join(key))
        .await
        .map_err(|_| "pdf_unavailable")?;
    let mut bytes = Vec::with_capacity(expected_bytes as usize);
    file.take(MAX_PDF_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| "pdf_unavailable")?;
    if bytes.len() != expected_bytes as usize || bytes.len() > MAX_PDF_BYTES {
        return Err("pdf_size_invalid");
    }
    if !bytes.starts_with(b"%PDF-") {
        return Err("invalid_pdf_signature");
    }
    if hash(&bytes) != expected_sha {
        return Err("pdf_integrity_error");
    }
    Ok(bytes)
}

/// Holds no plaintext credentials in integration_settings. Intentionally not Debug.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransportSecret {
    key: ResultKey,
    basic_username: Option<String>,
    basic_password: Option<String>,
}
impl TransportSecret {
    fn valid(&self) -> bool {
        match (&self.basic_username, &self.basic_password) {
            (None, None) => true,
            (Some(user), Some(password)) => !user.is_empty() && !password.is_empty(),
            _ => false,
        }
    }
}
fn cipher() -> Result<Aes256Gcm, &'static str> {
    let path = env::var_os("M53_MASTER_KEY_FILE").ok_or("master_key_unavailable")?;
    let bytes = std::fs::read(path).map_err(|_| "master_key_unavailable")?;
    if bytes.len() != 32 {
        return Err("master_key_invalid");
    }
    Aes256Gcm::new_from_slice(&bytes).map_err(|_| "master_key_invalid")
}
fn endpoint(raw: &str) -> Result<reqwest::Url, &'static str> {
    endpoint_with_test_http(
        raw,
        env::var("M53_1C_PDF_ALLOW_TEST_HTTP").as_deref() == Ok("1"),
    )
}
fn endpoint_with_test_http(raw: &str, allow_test_http: bool) -> Result<reqwest::Url, &'static str> {
    endpoint_with_base(raw, allow_test_http, env::var("M53_1C_BASE_URL").ok().as_deref())
}
fn endpoint_with_base(raw: &str, allow_test_http: bool, base: Option<&str>) -> Result<reqwest::Url, &'static str> {
    let url = reqwest::Url::parse(raw).map_err(|_| "invalid_result_endpoint")?;
    if !url.path().ends_with(CALLBACK_SUFFIX)
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("invalid_result_endpoint");
    }
    let loopback = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    // This opt-in belongs only to the user-approved 1C development callback.
    // Compare the original string: URL normalization must not widen its scope
    // to alternate hosts, ports, path encodings or a different publication.
    let approved_test_http = allow_test_http && base.and_then(|base| crate::one_c_endpoint::endpoint(base, "hs/reception/colposcopy-result")).as_deref() == Some(raw);
    if url.scheme() != "https" && !(url.scheme() == "http" && (loopback || approved_test_http)) {
        return Err("result_endpoint_requires_tls");
    }
    Ok(url)
}
async fn configuration(
    connection: &mut PgConnection,
    source: Uuid,
) -> Result<(reqwest::Url, TransportSecret), &'static str> {
    let row = sqlx::query("SELECT s.id,s.endpoint,c.nonce,c.ciphertext FROM app.result_delivery_configs d JOIN app.integration_sources src ON src.id=d.source_id JOIN app.integration_settings s ON s.id=d.integration_id AND s.kind='1c_pdf' JOIN app.integration_secrets c ON c.integration_id=s.id WHERE d.source_id=$1 AND d.enabled AND src.enabled")
        .bind(source).fetch_optional(connection).await.map_err(|_| "result_configuration_unavailable")?.ok_or("result_configuration_unavailable")?;
    let id: Uuid = row.get("id");
    let nonce: Vec<u8> = row.get("nonce");
    if nonce.len() != 12 {
        return Err("result_secret_invalid");
    }
    let bytes = cipher()?
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &row.get::<Vec<u8>, _>("ciphertext"),
                aad: id.as_bytes(),
            },
        )
        .map_err(|_| "result_secret_invalid")?;
    let secret: TransportSecret =
        serde_json::from_slice(&bytes).map_err(|_| "result_secret_invalid")?;
    if !secret.valid() {
        return Err("result_secret_invalid");
    }
    Ok((endpoint(&row.get::<String, _>("endpoint"))?, secret))
}

struct Delivery {
    state: &'static str,
    safe_error: Option<&'static str>,
    receipt: Option<Value>,
}
impl Delivery {
    fn retry(code: &'static str) -> Self {
        Self {
            state: "retry",
            safe_error: Some(code),
            receipt: None,
        }
    }
    fn blocked(code: &'static str) -> Self {
        Self {
            state: "blocked",
            safe_error: Some(code),
            receipt: None,
        }
    }
}
async fn post_result(
    client: &reqwest::Client,
    url: reqwest::Url,
    secret: &TransportSecret,
    payload: ResultRequest,
) -> Delivery {
    let application_key = payload.application_key;
    let bytes = match serde_json::to_vec(&payload) {
        Ok(b) => b,
        Err(_) => return Delivery::blocked("invalid_result_payload"),
    };
    if !ResultRequest::validate_json_size(bytes.len()).is_empty() {
        return Delivery::blocked("result_json_too_large");
    }
    let mut request = client
        .post(url)
        .header("X-API-Key", secret.key.as_str())
        .header("Content-Type", "application/json; charset=utf-8")
        .body(bytes);
    if let (Some(user), Some(password)) = (&secret.basic_username, &secret.basic_password) {
        request = request.basic_auth(user, Some(password));
    }
    let mut response = match request.send().await {
        Ok(r) => r,
        Err(_) => return Delivery::retry("result_transport_failed"),
    };
    let status = response.status().as_u16();
    if response
        .content_length()
        .is_some_and(|n| n > MAX_RECEIPT_BYTES as u64)
    {
        return Delivery::retry("result_receipt_too_large");
    }
    let mut bytes = vec![];
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if bytes.len() + chunk.len() <= MAX_RECEIPT_BYTES => {
                bytes.extend_from_slice(&chunk)
            }
            Ok(Some(_)) => return Delivery::retry("result_receipt_too_large"),
            Ok(None) => break,
            Err(_) => return Delivery::retry("result_transport_failed"),
        }
    }
    if matches!(status, 200 | 201) {
        return match serde_json::from_slice::<ResultReceipt>(&bytes) {
            Ok(receipt) if receipt.validate_for(status, application_key).is_empty() => Delivery {
                state: "sent",
                safe_error: None,
                receipt: serde_json::to_value(receipt).ok(),
            },
            _ => Delivery::retry("invalid_result_receipt"),
        };
    }
    let code = match status {
        400 => "one_c_invalid_request",
        401 | 403 => "one_c_access_denied",
        404 => "one_c_task_not_found",
        409 => "one_c_conflict",
        413 => "one_c_result_too_large",
        _ => "one_c_delivery_failed",
    };
    let mut delivery = if matches!(status, 400 | 401 | 403 | 404 | 409 | 413) {
        Delivery::blocked(code)
    } else {
        Delivery::retry(code)
    };
    if let Ok(failure) = serde_json::from_slice::<ResultFailure>(&bytes)
        && !failure.success
    {
        if failure.error_code == "file-recovery-required" {
            delivery = Delivery::blocked("one_c_file_recovery_required");
        }
        let diagnostic = failure.diagnostic_id;
        if !diagnostic.is_empty()
            && diagnostic.len() <= 128
            && diagnostic
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            delivery.receipt = Some(
                json!({"success":false,"diagnostic_id":diagnostic,"error_code":delivery.safe_error}),
            );
        }
    }
    delivery
}

/// One head-of-line job per task. Only permanently stale identity snapshots may be bypassed;
/// their data remains blocked, and a newer sent result prohibits every older resend.
pub async fn run_once(app: &App) -> WorkerResult<bool> {
    let cleanup_progress = cleanup_pending(app).await?;
    let candidates=sqlx::query("SELECT j.id,j.task_id FROM app.result_delivery_jobs j JOIN app.result_delivery_configs c ON c.source_id=j.source_id AND c.enabled JOIN app.integration_sources s ON s.id=j.source_id AND s.enabled WHERE j.state IN ('pending','retry') AND j.next_attempt_at<=now() AND (j.locked_at IS NULL OR j.locked_at<now()-interval '2 minutes') AND NOT EXISTS(SELECT 1 FROM app.result_delivery_jobs older WHERE older.task_id=j.task_id AND older.sequence<j.sequence AND older.state<>'sent' AND NOT (older.state='blocked' AND older.safe_error_code IN ('stale_identity_snapshot','superseded_result_snapshot'))) ORDER BY j.next_attempt_at,j.created_at,j.id LIMIT 16").fetch_all(&app.db).await?;
    for candidate in candidates {
        let task: Uuid = candidate.get("task_id");
        let id: Uuid = candidate.get("id");
        let mut connection = app.db.acquire().await?;
        // A cancelled worker/panic must close the physical connection, not return a
        // session advisory lock to the pool. No transaction remains open during HTTP.
        connection.close_on_drop();
        let locked: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtextextended($1,0))")
                .bind(format!("1c_pdf:{task}"))
                .fetch_one(&mut *connection)
                .await?;
        if !locked {
            continue;
        }
        let token = Uuid::new_v4();
        let row=sqlx::query("UPDATE app.result_delivery_jobs j SET locked_at=now(),lock_token=$2,attempts=attempts+1 WHERE id=$1 AND state IN ('pending','retry') AND next_attempt_at<=now() AND (locked_at IS NULL OR locked_at<now()-interval '2 minutes') AND NOT EXISTS(SELECT 1 FROM app.result_delivery_jobs older WHERE older.task_id=j.task_id AND older.sequence<j.sequence AND older.state<>'sent' AND NOT (older.state='blocked' AND older.safe_error_code IN ('stale_identity_snapshot','superseded_result_snapshot'))) RETURNING j.*,(SELECT storage_key FROM app.assets WHERE id=j.pdf_asset_id) AS legacy_storage_key,(SELECT byte_size FROM app.assets WHERE id=j.pdf_asset_id) AS legacy_byte_size,to_char(j.appointment_date,'YYYY-MM-DD') AS snapshot_date")
            .bind(id).bind(token).fetch_optional(&mut *connection).await?;
        let Some(row) = row else {
            continue;
        };
        let delivery = deliver_job(app, &mut connection, &row).await;
        // HTTP has finished. Commit the verified receipt before any cache unlink.
        let mut receipt_tx = connection.begin().await?;
        let updated = sqlx::query("UPDATE app.result_delivery_jobs SET state=$3,receipt=$4,safe_error_code=$5,sent_at=CASE WHEN $3='sent' THEN now() ELSE NULL END,cache_state=CASE WHEN payload_kind='cache' AND $3='sent' THEN 'cleanup_pending' ELSE cache_state END,locked_at=NULL,lock_token=NULL,next_attempt_at=CASE WHEN $3='retry' THEN now()+make_interval(secs=>least(3600,least(attempts,1000)*least(attempts,1000)*5)) ELSE next_attempt_at END WHERE id=$1 AND lock_token=$2")
            .bind(id).bind(token).bind(delivery.state).bind(delivery.receipt).bind(delivery.safe_error).execute(&mut *receipt_tx).await?;
        receipt_tx.commit().await?;
        if updated.rows_affected() == 1
            && delivery.state == "sent"
            && row.get::<String, _>("payload_kind") == "cache"
        {
            cleanup_job(app, &mut connection, id).await?;
        }
        return Ok(true);
    }
    Ok(cleanup_progress)
}
/// Recover a crash after receipt COMMIT without resending the PDF or requiring 1C configuration.
async fn cleanup_pending(app: &App) -> WorkerResult<bool> {
    let rows = sqlx::query("SELECT id,task_id FROM app.result_delivery_jobs WHERE state='sent' AND payload_kind='cache' AND cache_state='cleanup_pending' AND next_attempt_at<=now() ORDER BY sent_at,id LIMIT 16").fetch_all(&app.db).await?;
    for row in rows {
        let mut connection = app.db.acquire().await?;
        connection.close_on_drop();
        let task: Uuid = row.get("task_id");
        let locked: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtextextended($1,0))")
                .bind(format!("1c_pdf:{task}"))
                .fetch_one(&mut *connection)
                .await?;
        if !locked {
            continue;
        }
        cleanup_job(app, &mut connection, row.get("id")).await?;
        return Ok(true);
    }
    Ok(false)
}

async fn cleanup_job(app: &App, connection: &mut PgConnection, id: Uuid) -> WorkerResult<()> {
    // This committed state and matching receipt are prerequisites for unlink. No path
    // from assets/storage_key or an unacknowledged job ever reaches purge_cache.
    let row = sqlx::query("SELECT application_key,cache_key,receipt FROM app.result_delivery_jobs WHERE id=$1 AND state='sent' AND payload_kind='cache' AND cache_state='cleanup_pending'")
        .bind(id).fetch_optional(&mut *connection).await?;
    let Some(row) = row else {
        return Ok(());
    };
    let valid_receipt = row
        .get::<Option<Value>, _>("receipt")
        .and_then(|value| serde_json::from_value::<ResultReceipt>(value).ok())
        .is_some_and(|receipt| {
            receipt
                .validate_for(200, row.get("application_key"))
                .is_empty()
        });
    let outcome = if !valid_receipt {
        Err("result_cache_receipt_invalid")
    } else if let Some(key) = row.get::<Option<String>, _>("cache_key") {
        crate::result_payloads::purge_cache(app, id, &key).await
    } else {
        Err("invalid_delivery_cache_snapshot")
    };
    match outcome {
        Ok(()) => {
            sqlx::query("UPDATE app.result_delivery_jobs SET cache_state='purged',safe_error_code=NULL WHERE id=$1 AND state='sent' AND payload_kind='cache' AND cache_state='cleanup_pending'")
                .bind(id).execute(&mut *connection).await?;
        }
        Err(code) => {
            // Keep the verified receipt and sent status. Retry only the local cleanup,
            // with a delay so CLI while-run_once and background workers cannot spin.
            sqlx::query("UPDATE app.result_delivery_jobs SET safe_error_code=$2,next_attempt_at=now()+interval '30 seconds' WHERE id=$1 AND state='sent' AND payload_kind='cache' AND cache_state='cleanup_pending'")
                .bind(id).bind(code).execute(&mut *connection).await?;
        }
    }
    Ok(())
}

async fn deliver_job(app: &App, connection: &mut PgConnection, row: &PgRow) -> Delivery {
    let task: Uuid = row.get("task_id");
    let newer_sent = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM app.result_delivery_jobs WHERE task_id=$1 AND sequence>$2 AND state='sent')")
        .bind(task).bind(row.get::<i64,_>("sequence")).fetch_one(&mut *connection).await;
    match newer_sent {
        Ok(true) => return Delivery::blocked("superseded_result_snapshot"),
        Err(_) => return Delivery::retry("result_order_unavailable"),
        Ok(false) => {}
    }
    let current=match sqlx::query("SELECT patient_guid,to_char(appointment_date,'YYYY-MM-DD') AS appointment_date,state FROM app.colposcopy_tasks WHERE id=$1").bind(task).fetch_one(&mut *connection).await {Ok(r)=>r,Err(_)=>return Delivery::retry("task_state_unavailable")};
    if current.get::<String, _>("state") == "cancelled" {
        return Delivery::blocked("task_cancelled");
    }
    if current.get::<Uuid, _>("patient_guid") != row.get::<Uuid, _>("patient_guid")
        || current.get::<String, _>("appointment_date") != row.get::<String, _>("snapshot_date")
    {
        return Delivery::blocked("stale_identity_snapshot");
    }
    let (url, secret) = match configuration(connection, row.get("source_id")).await {
        Ok(c) => c,
        Err(code) => return Delivery::blocked(code),
    };
    let pdf = match row.get::<String, _>("payload_kind").as_str() {
        "cache" => {
            let key = row.get::<Option<String>, _>("cache_key");
            let size = row.get::<Option<i64>, _>("pdf_byte_size");
            match (key, size) {
                (Some(key), Some(size))
                    if row.get::<Option<String>, _>("cache_state").as_deref() == Some("ready") =>
                {
                    crate::result_payloads::read_cache(
                        app,
                        row.get("id"),
                        &key,
                        size,
                        &row.get::<String, _>("pdf_sha256"),
                    )
                    .await
                }
                _ => Err("invalid_delivery_cache_snapshot"),
            }
        }
        "legacy_asset" => {
            let asset = row.get::<Option<Uuid>, _>("pdf_asset_id");
            let key = row.get::<Option<String>, _>("legacy_storage_key");
            let size = row.get::<Option<i64>, _>("legacy_byte_size");
            match (asset, key, size) {
                (Some(asset), Some(key), Some(size)) => {
                    read_pdf(
                        &app.storage,
                        asset,
                        &key,
                        size,
                        &row.get::<String, _>("pdf_sha256"),
                    )
                    .await
                }
                _ => Err("legacy_pdf_unavailable"),
            }
        }
        _ => Err("invalid_delivery_payload_kind"),
    };
    let bytes = match pdf {
        Ok(bytes) => bytes,
        Err(code) => return Delivery::blocked(code),
    };
    let date = match CalendarDate::parse(&row.get::<String, _>("snapshot_date")) {
        Ok(d) => d,
        Err(_) => return Delivery::blocked("invalid_snapshot_date"),
    };
    let application_key: Uuid = row.get("application_key");
    let diagnoses = match serde_json::from_value(row.get::<Value, _>("diagnoses_snapshot")) {
        Ok(value) => value,
        Err(_) => return Delivery::blocked("invalid_result_diagnoses"),
    };
    let payload = ResultRequest {
        application_key,
        appointment_date: date,
        patient_key: row.get("patient_guid"),
        file_name: format!("{application_key}.pdf"),
        pdf_base64: STANDARD.encode(&bytes),
        diagnoses,
    };
    if !payload.validate(bytes.len()).is_empty() {
        return Delivery::blocked("invalid_result_payload");
    }
    let client = match crate::network::direct_client() {
        Ok(c) => c,
        Err(_) => return Delivery::blocked("result_transport_configuration_invalid"),
    };
    post_result(&client, url, &secret, payload).await
}

/// Administrative setup only; stores the key and optional platform Basic credentials
/// together under AES-GCM. It never contacts 1C and is disabled unless explicitly enabled.
pub async fn configure_from_env() -> WorkerResult<()> {
    let source =
        Uuid::parse_str(&env::var("M53_1C_PDF_SOURCE_ID").map_err(|_| "source_id_required")?)
            .map_err(|_| "invalid_source_id")?;
    if source.is_nil() {
        return Err("invalid_source_id".into());
    }
    let url = endpoint(&env::var("M53_1C_PDF_ENDPOINT").map_err(|_| "result_endpoint_required")?)?;
    fn secret_file(variable: &str) -> Result<String, &'static str> {
        let path = env::var_os(variable).ok_or("secret_file_required")?;
        let text =
            std::fs::read_to_string(PathBuf::from(path)).map_err(|_| "secret_file_unavailable")?;
        Ok(text.trim_end_matches(['\r', '\n']).into())
    }
    let key =
        ResultKey::new(secret_file("M53_1C_PDF_KEY_FILE")?).map_err(|_| "invalid_result_key")?;
    let basic_username = env::var_os("M53_1C_PDF_BASIC_USER_FILE")
        .map(|_| secret_file("M53_1C_PDF_BASIC_USER_FILE"))
        .transpose()?;
    let basic_password = env::var_os("M53_1C_PDF_BASIC_PASSWORD_FILE")
        .map(|_| secret_file("M53_1C_PDF_BASIC_PASSWORD_FILE"))
        .transpose()?;
    let secret = TransportSecret {
        key,
        basic_username,
        basic_password,
    };
    if !secret.valid() {
        return Err("incomplete_basic_credentials".into());
    }
    let plaintext = serde_json::to_vec(&secret).map_err(|_| "invalid_result_secret")?;
    let cipher = cipher()?;
    let db = crate::deployment::connect(crate::deployment::Purpose::Migrator)
        .await
        .map_err(|_| "development_database_unavailable")?;
    let mut tx = db.begin().await?;
    sqlx::query("SET LOCAL ROLE colposcopy_owner")
        .execute(&mut *tx)
        .await?;
    let org: Uuid = sqlx::query_scalar(
        "SELECT organization_id FROM app.integration_sources WHERE id=$1 FOR UPDATE",
    )
    .bind(source)
    .fetch_one(&mut *tx)
    .await?;
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT integration_id FROM app.result_delivery_configs WHERE source_id=$1 FOR UPDATE",
    )
    .bind(source)
    .fetch_optional(&mut *tx)
    .await?;
    let id = existing.unwrap_or_else(Uuid::new_v4);
    let random = Uuid::new_v4();
    let nonce = &random.as_bytes()[..12];
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: &plaintext,
                aad: id.as_bytes(),
            },
        )
        .map_err(|_| "result_secret_encryption_failed")?;
    sqlx::query("INSERT INTO app.integration_settings(id,kind,endpoint,schema_version,settings) VALUES($1,'1c_pdf',$2,1,'{}'::jsonb) ON CONFLICT(id) DO UPDATE SET endpoint=excluded.endpoint,settings=excluded.settings,row_version=app.integration_settings.row_version+1").bind(id).bind(url.as_str()).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO app.integration_secrets(integration_id,algorithm,key_id,nonce,ciphertext) VALUES($1,'AES-256-GCM',$2,$3,$4) ON CONFLICT(integration_id) DO UPDATE SET key_id=excluded.key_id,nonce=excluded.nonce,ciphertext=excluded.ciphertext")
        .bind(id).bind(env::var("M53_MASTER_KEY_ID").unwrap_or_else(|_|"v1".into())).bind(nonce).bind(ciphertext).execute(&mut *tx).await?;
    let enabled = env::var("M53_1C_PDF_ENABLED").as_deref() == Ok("1");
    sqlx::query("INSERT INTO app.result_delivery_configs(source_id,organization_id,integration_id,enabled) VALUES($1,$2,$3,$4) ON CONFLICT(source_id) DO UPDATE SET enabled=excluded.enabled,updated_at=now()")
        .bind(source).bind(org).bind(id).bind(enabled).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    fn endpoint_with_test_http(raw: &str, allow_test_http: bool) -> Result<reqwest::Url, &'static str> {
        super::endpoint_with_base(raw, allow_test_http, Some("http://192.168.73.45:885/UMZ_BIT-DEV/"))
    }

    use super::*;
    fn secret() -> TransportSecret {
        TransportSecret {
            key: ResultKey::new("x".repeat(32)).unwrap(),
            basic_username: None,
            basic_password: None,
        }
    }
    fn payload() -> ResultRequest {
        ResultRequest {
            application_key: Uuid::from_u128(1),
            appointment_date: CalendarDate::parse("2028-02-29").unwrap(),
            patient_key: Uuid::from_u128(2),
            file_name: format!("{}.pdf", Uuid::from_u128(1)),
            pdf_base64: STANDARD.encode(b"%PDF-1.7\n"),
            diagnoses: vec![],
        }
    }
    #[test]
    fn configured_base_moves_pdf_and_rejects_other_destinations() {
        for host in ["192.168.73.45", "192.168.73.46"] {
            let base = format!("http://{host}:885/UMZ_BIT-DEV/");
            let url = format!("{base}hs/reception/colposcopy-result");
            assert!(super::endpoint_with_base(&url, true, Some(&base)).is_ok());
            assert!(super::endpoint_with_base(&url, false, Some(&base)).is_err());
            assert!(super::endpoint_with_base(&url, true, None).is_err());
            for other in [url.replace(host, "192.168.73.47"), url.replace(":885", ":886"), url.replace("UMZ_BIT-DEV", "OTHER")] {
                assert!(super::endpoint_with_base(&other, true, Some(&base)).is_err());
            }
        }
    }

    #[test]
    fn moved_test_callback_host_is_exactly_allowed() {
        assert!(endpoint_with_test_http("http://192.168.73.45:885/UMZ_BIT-DEV/hs/reception/colposcopy-result", true).is_ok());
        assert!(endpoint_with_test_http("http://192.168.73.18:885/UMZ_BIT-DEV/hs/reception/colposcopy-result", true).is_err());
    }

    #[test]
    fn endpoint_is_only_the_callback_with_trusted_tls_or_loopback() {
        assert!(
            endpoint("https://example.invalid/published/hs/reception/colposcopy-result").is_ok()
        );
        assert!(endpoint("http://127.0.0.1:1234/hs/reception/colposcopy-result").is_ok());
        for url in [
            "http://example.invalid/hs/reception/colposcopy-result",
            "https://example.invalid/wrong",
            "https://example.invalid/hs/reception/colposcopy-result/",
            "https://example.invalid/hs/reception/colposcopy-result?key=x",
            "https://user:password@example.invalid/hs/reception/colposcopy-result",
        ] {
            assert!(endpoint(url).is_err());
        }
    }
    #[test]
    fn test_http_opt_in_is_limited_to_the_exact_approved_callback() {
        // Previously mistaken publications must be REJECTED, never allowlisted.
        assert!(
            endpoint_with_test_http(
                "http://192.168.73.45:885/UMZ_BIT_DEV/hs/reception/colposcopy-result",
                true,
            )
            .is_err()
        );
        assert!(
            endpoint_with_test_http(
                "http://192.168.73.45:885/UMZ_BIR-DEV/hs/reception/colposcopy-result",
                true,
            )
            .is_err()
        );
        assert_eq!(
            endpoint_with_test_http(TEST_HTTP_CALLBACK, false),
            Err("result_endpoint_requires_tls")
        );
        assert_eq!(
            endpoint_with_test_http(TEST_HTTP_CALLBACK, true)
                .unwrap()
                .as_str(),
            TEST_HTTP_CALLBACK
        );
        for raw in [
            "http://192.168.73.45:885/UMZ_BIT-DEV/",
            "http://192.168.73.19:885/UMZ_BIT-DEV/hs/reception/colposcopy-result",
            "http://192.168.73.45:889/UMZ_BIT-DEV/hs/reception/colposcopy-result",
            "http://192.168.73.45:888/UMZ_BIT-DEV/hs/reception/colposcopy-result",
            "http://192.168.73.45/UMZ_BIT-DEV/hs/reception/colposcopy-result",
            "http://192.168.73.45:885/UMZ_BIT/hs/reception/colposcopy-result",
            "http://192.168.73.45:885/umz_bit-dev/hs/reception/colposcopy-result",
            "http://192.168.73.45:885/UMZ_BIT-DEV/hs/reception/colposcopy-result/",
            "http://192.168.73.45:885/UMZ_BIT-DEV/hs/reception/colposcopy-result?key=value",
            "http://192.168.73.45:885/UMZ_BIT-DEV/hs/reception/colposcopy-result#fragment",
            "http://user@192.168.73.45:885/UMZ_BIT-DEV/hs/reception/colposcopy-result",
            "http://192.168.73.45.example.invalid:885/UMZ_BIT-DEV/hs/reception/colposcopy-result",
            "http://192.168.73.45:0885/UMZ_BIT-DEV/hs/reception/colposcopy-result",
            "http://192.168.73.45:885/UMZ_BIT-DEV/other/../hs/reception/colposcopy-result",
            "http://192.168.73.45:885/UMZ_BIT-DEV/%68s/reception/colposcopy-result",
            "http://192.168.73.45:885//UMZ_BIT-DEV/hs/reception/colposcopy-result",
            "http://3232254226:885/UMZ_BIT-DEV/hs/reception/colposcopy-result",
            "http://0xc0a84912:885/UMZ_BIT-DEV/hs/reception/colposcopy-result",
            "HTTP://192.168.73.45:885/UMZ_BIT-DEV/hs/reception/colposcopy-result",
            " http://192.168.73.45:885/UMZ_BIT-DEV/hs/reception/colposcopy-result",
            "http://192.168.73.45:885/UMZ_BIT-DEV/hs/reception/colposcopy-result ",
        ] {
            assert!(endpoint_with_test_http(raw, true).is_err(), "{raw}");
        }
        for raw in [
            "https://example.invalid/published/hs/reception/colposcopy-result",
            "http://127.0.0.1:1234/hs/reception/colposcopy-result",
            "http://[::1]:1234/hs/reception/colposcopy-result",
        ] {
            assert!(endpoint_with_test_http(raw, false).is_ok());
            assert!(endpoint_with_test_http(raw, true).is_ok());
        }
    }
    async fn mock(status: u16, body: Value) -> (reqwest::Url, tokio::task::JoinHandle<()>) {
        use axum::{Json, Router, routing::post};
        let router = Router::new().route(
            CALLBACK_SUFFIX,
            post(
                move |headers: axum::http::HeaderMap, Json(input): Json<Value>| {
                    let body = body.clone();
                    async move {
                        assert_eq!(headers.get_all("x-api-key").iter().count(), 1);
                        assert_eq!(
                            headers.get("x-api-key").unwrap(),
                            "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
                        );
                        assert!(input.get("Key").is_none());
                        assert!(input.get("key").is_none());
                        let decoded = STANDARD
                            .decode(input["pdf_base64"].as_str().unwrap())
                            .unwrap();
                        assert!(decoded.starts_with(b"%PDF-"));
                        (StatusCode::from_u16(status).unwrap(), Json(body))
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = endpoint(&format!(
            "http://{}{CALLBACK_SUFFIX}",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (url, task)
    }
    fn receipt() -> Value {
        json!({"success":true,"application_key":Uuid::from_u128(1),"reception_ref_key":Uuid::from_u128(3),"reception_number":"test","file_ref_key":Uuid::from_u128(4),"action":"unchanged"})
    }
    #[tokio::test]
    async fn only_a_matching_durable_receipt_completes_delivery() {
        let client = crate::network::direct_client().unwrap();
        let (url, server) = mock(200, receipt()).await;
        assert_eq!(
            post_result(&client, url, &secret(), payload()).await.state,
            "sent"
        );
        server.abort();
        let (url, server) = mock(202, receipt()).await;
        assert_eq!(
            post_result(&client, url, &secret(), payload()).await.state,
            "retry"
        );
        server.abort();
        let mut wrong = receipt();
        wrong["application_key"] = json!(Uuid::from_u128(9));
        let (url, server) = mock(200, wrong).await;
        assert_eq!(
            post_result(&client, url, &secret(), payload()).await.state,
            "retry"
        );
        server.abort();
        let (url, server) = mock(200, json!({"success":false})).await;
        assert_eq!(
            post_result(&client, url, &secret(), payload()).await.state,
            "retry"
        );
        server.abort();
    }
    #[tokio::test]
    async fn file_recovery_and_access_errors_block_instead_of_automatic_writes() {
        let client = crate::network::direct_client().unwrap();
        let (url,server)=mock(409,json!({"success":false,"error_code":"file-recovery-required","diagnostic_id":"synthetic_id"})).await;
        let result = post_result(&client, url, &secret(), payload()).await;
        assert_eq!(result.state, "blocked");
        assert_eq!(result.safe_error, Some("one_c_file_recovery_required"));
        server.abort();
        let (url, server) = mock(
            401,
            json!({"success":false,"error_code":"invalid_key","diagnostic_id":"synthetic_id"}),
        )
        .await;
        assert_eq!(
            post_result(&client, url, &secret(), payload()).await.state,
            "blocked"
        );
        server.abort();
        let (url, server) = mock(
            500,
            json!({"success":false,"error_code":"internal_error","diagnostic_id":"synthetic_id"}),
        )
        .await;
        assert_eq!(
            post_result(&client, url, &secret(), payload()).await.state,
            "retry"
        );
        server.abort();
    }
    #[tokio::test]
    async fn optional_platform_basic_is_separate_from_the_json_key() {
        use axum::{Json, Router, http::HeaderMap, routing::post};
        let router = Router::new().route(
            CALLBACK_SUFFIX,
            post(|headers: HeaderMap, Json(input): Json<Value>| async move {
                let expected = format!(
                    "Basic {}",
                    STANDARD.encode("synthetic-user:synthetic-password")
                );
                assert_eq!(headers.get("authorization").unwrap(), expected.as_str());
                assert_eq!(headers.get_all("x-api-key").iter().count(), 1);
                assert_eq!(
                    headers.get("x-api-key").unwrap(),
                    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
                );
                assert!(headers.get("x-m53-signature").is_none());
                assert!(input.get("Key").is_none());
                assert!(input.get("basic_password").is_none());
                Json(receipt())
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = endpoint(&format!(
            "http://{}{CALLBACK_SUFFIX}",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let mut transport = secret();
        transport.basic_username = Some("synthetic-user".into());
        transport.basic_password = Some("synthetic-password".into());
        assert!(transport.valid());
        let result = post_result(
            &crate::network::direct_client().unwrap(),
            url,
            &transport,
            payload(),
        )
        .await;
        assert_eq!(result.state, "sent");
        server.abort();
        transport.basic_password = None;
        assert!(!transport.valid());
    }
    #[tokio::test]
    async fn html_oversized_receipts_and_redirects_do_not_acknowledge_delivery() {
        use axum::{Json, Router, routing::post};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let redirected_hits = Arc::new(AtomicUsize::new(0));
        let counter = redirected_hits.clone();
        let router = Router::new()
            .route(
                "/html/hs/reception/colposcopy-result",
                post(|| async { "<html>error</html>" }),
            )
            .route(
                "/large/hs/reception/colposcopy-result",
                post(|| async { "x".repeat(MAX_RECEIPT_BYTES + 1) }),
            )
            .route(
                "/redirect/hs/reception/colposcopy-result",
                post(move || async move {
                    (
                        StatusCode::TEMPORARY_REDIRECT,
                        [(
                            "location",
                            format!("http://{address}/redirected/hs/reception/colposcopy-result"),
                        )],
                    )
                }),
            )
            .route(
                "/redirected/hs/reception/colposcopy-result",
                post(move || {
                    let counter = counter.clone();
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        Json(receipt())
                    }
                }),
            );
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let client = crate::network::direct_client().unwrap();
        for name in ["html", "large", "redirect"] {
            let url = endpoint(&format!("http://{address}/{name}{CALLBACK_SUFFIX}")).unwrap();
            let result = post_result(&client, url, &secret(), payload()).await;
            assert_eq!(result.state, "retry");
            assert!(result.receipt.is_none());
        }
        assert_eq!(redirected_hits.load(Ordering::SeqCst), 0);
        server.abort();
    }
    #[tokio::test]
    async fn stored_pdf_must_match_metadata_digest_and_signature() {
        let directory = env::temp_dir().join(format!("m53-result-unit-{}", Uuid::new_v4()));
        tokio::fs::create_dir(&directory).await.unwrap();
        let id = Uuid::new_v4();
        let key = id.simple().to_string();
        let bytes = b"%PDF-1.7\n";
        tokio::fs::write(directory.join(&key), bytes).await.unwrap();
        assert!(
            read_pdf(&directory, id, &key, bytes.len() as i64, &hash(bytes))
                .await
                .is_ok()
        );
        assert!(
            read_pdf(
                &directory,
                id,
                "../elsewhere",
                bytes.len() as i64,
                &hash(bytes)
            )
            .await
            .is_err()
        );
        assert!(
            read_pdf(&directory, id, &key, bytes.len() as i64, &hash(b"other"))
                .await
                .is_err()
        );
        tokio::fs::write(directory.join(&key), b"not a pdf")
            .await
            .unwrap();
        assert!(
            read_pdf(&directory, id, &key, 9, &hash(b"not a pdf"))
                .await
                .is_err()
        );
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }
}
