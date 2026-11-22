//! Durable client-PDF payloads. Cache bytes exist until the matching 1C ACK commits.
//! This module never renders a report or registers its PDF as an original asset.
use crate::api::{App, Error, hash};
use axum::http::StatusCode;
use m53_contracts::colposcopy_1c::MAX_PDF_BYTES;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};
use std::path::{Component, Path, PathBuf};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

type Result<T> = std::result::Result<T, Error>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientPdfMetadata {
    pub revision_id: Uuid,
    pub template_version_id: Uuid,
    pub client_version: String,
    pub renderer_version: String,
}

fn valid_version(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

/// Caller owns clinical authorization, the transaction, and its final COMMIT.
/// A rollback may leave an unreferenced cache file; it must not remove other work.
pub async fn enqueue(
    app: &App,
    tx: &mut Transaction<'_, Postgres>,
    organization_id: Uuid,
    credential_id: Uuid,
    study_id: Uuid,
    metadata: &ClientPdfMetadata,
    expected_sha: &str,
    bytes: &[u8],
) -> Result<Value> {
    if metadata.revision_id.is_nil()
        || metadata.template_version_id.is_nil()
        || !valid_version(&metadata.client_version)
        || !valid_version(&metadata.renderer_version)
    {
        return Err(Error(StatusCode::BAD_REQUEST, "invalid_pdf_metadata"));
    }
    if expected_sha.len() != 64
        || !expected_sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error(StatusCode::BAD_REQUEST, "invalid_pdf_sha256"));
    }
    validate_pdf(bytes, expected_sha).map_err(|code| Error(StatusCode::BAD_REQUEST, code))?;
    let rows=sqlx::query("SELECT t.id AS task_id,t.source_id,t.application_key,t.patient_guid,to_char(t.appointment_date,'YYYY-MM-DD') AS appointment_date,t.state,t.assigned_doctor_id,s.status,pr.approved_at IS NOT NULL AS approved,(SELECT id FROM app.protocol_revisions WHERE study_id=s.id ORDER BY revision DESC LIMIT 1)=pr.id AS latest_revision,EXISTS(SELECT 1 FROM app.patient_source_links l WHERE l.source_id=t.source_id AND l.organization_id=t.organization_id AND l.external_guid=t.patient_guid AND l.patient_id=t.study_patient_id AND l.patient_id=s.patient_id) AS confirmed_patient,EXISTS(SELECT 1 FROM app.result_delivery_configs c WHERE c.source_id=t.source_id AND c.organization_id=t.organization_id) AS configured FROM app.colposcopy_tasks t JOIN app.studies s ON s.id=t.study_id JOIN app.patients p ON p.id=s.patient_id JOIN app.protocol_revisions pr ON pr.id=$3 AND pr.study_id=s.id JOIN app.report_template_versions v ON v.id=$4 AND v.published_at IS NOT NULL JOIN app.report_templates rt ON rt.id=v.template_id AND rt.organization_id=$1 WHERE t.organization_id=$1 AND p.organization_id=$1 AND t.study_id=$2 FOR UPDATE OF t,s")
        .bind(organization_id).bind(study_id).bind(metadata.revision_id).bind(metadata.template_version_id)
        .fetch_all(&mut **tx).await?;
    if rows.len() != 1 {
        return Err(Error(StatusCode::CONFLICT, "task_report_binding_required"));
    }
    let row = &rows[0];
    crate::workflow_revision::require_current_approved(tx, study_id, metadata.revision_id).await?;
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
    let task: Uuid = row.get("task_id");
    let patient_guid: Uuid = row.get("patient_guid");
    let date: String = row.get("appointment_date");
    if let Some(existing)=sqlx::query("SELECT j.id,j.state,j.sequence,j.payload_kind,j.cache_state,j.pdf_sha256,j.patient_guid,to_char(j.appointment_date,'YYYY-MM-DD') AS appointment_date,coalesce(j.protocol_revision_id,r.protocol_revision_id) AS snapshot_revision,coalesce(j.template_version_id,r.template_version_id) AS snapshot_template,coalesce(j.client_version,r.client_version) AS snapshot_client,coalesce(j.renderer_version,r.renderer_version) AS snapshot_renderer FROM app.result_delivery_jobs j LEFT JOIN app.reports r ON r.id=j.report_id WHERE j.task_id=$1 AND j.pdf_sha256=$2")
        .bind(task).bind(expected_sha).fetch_optional(&mut **tx).await? {
        if existing.get::<Uuid,_>("patient_guid")!=patient_guid
            || existing.get::<String,_>("appointment_date")!=date
            || existing.get::<Option<Uuid>,_>("snapshot_revision")!=Some(metadata.revision_id)
            || existing.get::<Option<Uuid>,_>("snapshot_template")!=Some(metadata.template_version_id)
            || existing.get::<Option<String>,_>("snapshot_client").as_deref()!=Some(&metadata.client_version)
            || existing.get::<Option<String>,_>("snapshot_renderer").as_deref()!=Some(&metadata.renderer_version)
        {
            return Err(Error(StatusCode::CONFLICT,"result_snapshot_conflict"));
        }
        return Ok(job_response(&existing));
    }
    crate::clinic::check_template_scope(tx,organization_id,credential_id,metadata.template_version_id).await?;
    let sequence: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(sequence),0)+1 FROM app.result_delivery_jobs WHERE task_id=$1",
    )
    .bind(task)
    .fetch_one(&mut **tx)
    .await?;
    let diagnoses = crate::result_diagnoses::freeze(tx, metadata.revision_id).await?;
    let id = Uuid::new_v4();
    let key = id.simple().to_string();
    let root = cache_root(app).map_err(|code| Error(StatusCode::SERVICE_UNAVAILABLE, code))?;
    #[cfg(unix)]
    let missing_parents = {
        let mut parents = Vec::new();
        let mut current = root.as_path();
        while !current.exists() {
            let Some(parent) = current.parent() else {
                break;
            };
            parents.push(parent.to_owned());
            current = parent;
        }
        parents
    };
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&root)?;
    let root = cache_root(app).map_err(|code| Error(StatusCode::SERVICE_UNAVAILABLE, code))?;
    #[cfg(unix)]
    for parent in missing_parents.iter().rev() {
        // Publishing fsyncs the cache directory itself. Newly created ancestors
        // also need their directory entries committed before a server receipt.
        tokio::fs::File::open(parent).await?.sync_all().await?;
    }
    crate::storage::publish(&root, &key, bytes).await?;
    sqlx::query("INSERT INTO app.result_delivery_jobs(id,task_id,source_id,organization_id,application_key,study_id,pdf_sha256,patient_guid,appointment_date,sequence,payload_kind,cache_key,pdf_byte_size,protocol_revision_id,template_version_id,client_version,renderer_version,cache_state,diagnoses_snapshot) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9::text::date,$10,'cache',$11,$12,$13,$14,$15,$16,'ready',$17)")
        .bind(id).bind(task).bind(row.get::<Uuid,_>("source_id")).bind(organization_id)
        .bind(row.get::<Uuid,_>("application_key")).bind(study_id).bind(expected_sha).bind(patient_guid)
        .bind(date).bind(sequence).bind(&key).bind(bytes.len() as i64).bind(metadata.revision_id)
        .bind(metadata.template_version_id).bind(&metadata.client_version).bind(&metadata.renderer_version).bind(diagnoses)
        .execute(&mut **tx).await?;
    Ok(
        json!({"job_id":id,"state":"pending","sequence":sequence,"payload_kind":"cache","cache_state":"ready","pdf_sha256":expected_sha}),
    )
}

fn job_response(row: &PgRow) -> Value {
    json!({"job_id":row.get::<Uuid,_>("id"),"state":row.get::<String,_>("state"),
           "sequence":row.get::<i64,_>("sequence"),"payload_kind":row.get::<String,_>("payload_kind"),
           "cache_state":row.get::<Option<String>,_>("cache_state"),
           "pdf_sha256":row.get::<String,_>("pdf_sha256")})
}

fn validate_pdf(bytes: &[u8], expected_sha: &str) -> std::result::Result<(), &'static str> {
    if bytes.is_empty() || bytes.len() > MAX_PDF_BYTES {
        return Err("pdf_size_invalid");
    }
    if !bytes.starts_with(b"%PDF-") {
        return Err("invalid_pdf_signature");
    }
    if hash(bytes) != expected_sha {
        return Err("pdf_integrity_error");
    }
    Ok(())
}

/// Resolve existing symlink ancestors even before the configured directory exists.
fn resolved_root(path: &Path) -> std::result::Result<PathBuf, &'static str> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err("result_cache_root_invalid");
    }
    let mut current = path;
    let mut missing = Vec::new();
    loop {
        match std::fs::canonicalize(current) {
            Ok(mut resolved) => {
                for part in missing.iter().rev() {
                    resolved.push(part);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    current
                        .file_name()
                        .ok_or("result_cache_root_invalid")?
                        .to_owned(),
                );
                current = current.parent().ok_or("result_cache_root_invalid")?;
            }
            Err(_) => return Err("result_cache_root_unavailable"),
        }
    }
}

/// Dedicated absolute root, outside the original/legacy asset tree in either direction.
pub fn cache_root(app: &App) -> std::result::Result<PathBuf, &'static str> {
    let configured =
        std::env::var_os("M53_DELIVERY_CACHE_DIR").ok_or("result_cache_root_required")?;
    checked_root(&app.storage, Path::new(&configured))
}

fn checked_root(storage: &Path, configured: &Path) -> std::result::Result<PathBuf, &'static str> {
    let root = resolved_root(configured)?;
    let originals = resolved_root(storage)?;
    if root.starts_with(&originals) || originals.starts_with(&root) {
        return Err("result_cache_overlaps_originals");
    }
    if root.exists() && !root.is_dir() {
        return Err("result_cache_root_invalid");
    }
    Ok(root)
}

fn cache_path(app: &App, job: Uuid, key: &str) -> std::result::Result<PathBuf, &'static str> {
    if job.is_nil() || key != job.simple().to_string() {
        return Err("invalid_result_cache_key");
    }
    Ok(cache_root(app)?.join(key))
}

/// Pending bytes are always verified; a missing unacknowledged payload is never purged.
pub async fn read_cache(
    app: &App,
    job: Uuid,
    key: &str,
    size: i64,
    sha: &str,
) -> std::result::Result<Vec<u8>, &'static str> {
    if size <= 0 || size as u64 > MAX_PDF_BYTES as u64 {
        return Err("pdf_size_invalid");
    }
    let path = cache_path(app, job, key)?;
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| "result_cache_unavailable")?;
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(MAX_PDF_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| "result_cache_unavailable")?;
    if bytes.len() != size as usize {
        return Err("pdf_size_invalid");
    }
    validate_pdf(&bytes, sha)?;
    Ok(bytes)
}

/// Caller must first commit state=sent and the matching 1C receipt. Missing bytes
/// after that COMMIT mean cleanup is complete; no original asset is ever removed.
pub async fn purge_cache(app: &App, job: Uuid, key: &str) -> std::result::Result<(), &'static str> {
    let path = cache_path(app, job, key)?;
    match tokio::fs::remove_file(&path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("result_cache_cleanup_failed"),
    }
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        match tokio::fs::File::open(parent).await {
            Ok(directory) => directory
                .sync_all()
                .await
                .map_err(|_| "result_cache_cleanup_failed")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("result_cache_cleanup_failed"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedicated_cache_never_resolves_to_an_original_directory() {
        let base = std::env::temp_dir().join(format!("m53-cache-boundary-{}", Uuid::new_v4()));
        let originals = base.join("originals");
        std::fs::create_dir_all(&originals).unwrap();
        let cache = base.join("new-parent/cache");
        assert_eq!(checked_root(&originals, &cache).unwrap(), cache);
        for invalid in [&originals, &originals.join("cache"), &base] {
            assert_eq!(
                checked_root(&originals, invalid),
                Err("result_cache_overlaps_originals")
            );
        }
        assert!(checked_root(&originals, Path::new("relative-cache")).is_err());
        let file = base.join("not-a-directory");
        std::fs::write(&file, b"fixture").unwrap();
        assert_eq!(
            checked_root(&originals, &file),
            Err("result_cache_root_invalid")
        );
        #[cfg(unix)]
        {
            let alias = base.join("symlink-to-originals");
            std::os::unix::fs::symlink(&originals, &alias).unwrap();
            assert_eq!(
                checked_root(&originals, &alias),
                Err("result_cache_overlaps_originals")
            );
            assert_eq!(
                checked_root(&originals, &alias.join("new-cache")),
                Err("result_cache_overlaps_originals")
            );
        }
        std::fs::remove_dir_all(base).unwrap();
    }
}
