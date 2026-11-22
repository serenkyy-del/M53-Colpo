//! Signed v1 update distribution. No publishing authority is exposed to clients.
use crate::api::{App, Error, credential, hash};
use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use std::{path::PathBuf, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use uuid::Uuid;
type Result<T> = std::result::Result<T, Error>;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    platform: String,
    channel: String,
    build_sequence: u64,
    api_version: i32,
    cache_format: i32,
    launcher_version: i32,
}
pub fn safe_name(s: &str) -> bool {
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    !s.is_empty()
        && s.len() <= 128
        && !s.starts_with('.')
        && !s.ends_with('.')
        && !s.contains("..")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && !matches!(
            stem.as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        )
}
pub async fn check(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Query(q): Query<Check>,
) -> Result<Json<Value>> {
    let (_, org) = credential(&app, &headers).await?;
    if !safe_name(&q.platform)
        || !safe_name(&q.channel)
        || q.build_sequence > i64::MAX as u64
        || q.api_version <= 0
        || q.cache_format <= 0
        || q.launcher_version <= 0
    {
        return Err(Error(StatusCode::BAD_REQUEST, "invalid_update_check"));
    }
    let envelope:Option<Value>=sqlx::query_scalar("SELECT r.envelope FROM app.client_releases r JOIN app.client_update_policies p ON p.release_id=r.id WHERE p.organization_id=$1 AND p.enabled AND r.withdrawn_at IS NULL AND r.platform=$2 AND r.channel=$3 AND r.build_sequence>$4 AND r.min_api<=$5 AND r.max_api>=$5 AND r.cache_format=$6 AND r.min_launcher<=$7 AND r.published_at<=extract(epoch FROM now()) AND r.expires_at>extract(epoch FROM now()) ORDER BY r.build_sequence DESC LIMIT 1")
 .bind(org).bind(q.platform).bind(q.channel).bind(q.build_sequence as i64).bind(q.api_version).bind(q.cache_format).bind(q.launcher_version).fetch_optional(&app.db).await?;
    Ok(Json(json!({"update":envelope})))
}
fn range(raw: Option<&str>, size: u64) -> std::result::Result<(u64, u64, bool), ()> {
    let Some(raw) = raw else {
        return Ok((0, size - 1, false));
    };
    let value = raw.strip_prefix("bytes=").ok_or(())?;
    if value.contains(',') {
        return Err(());
    }
    let (a, b) = value.split_once('-').ok_or(())?;
    let (start, end) = if a.is_empty() {
        let count = b.parse::<u64>().map_err(|_| ())?;
        if count == 0 {
            return Err(());
        }
        (size.saturating_sub(count), size - 1)
    } else {
        (
            a.parse::<u64>().map_err(|_| ())?,
            if b.is_empty() {
                size - 1
            } else {
                b.parse::<u64>().map_err(|_| ())?.min(size - 1)
            },
        )
    };
    if start >= size || start > end {
        return Err(());
    }
    Ok((start, end, true))
}
pub async fn file(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path((id, name)): Path<(Uuid, String)>,
) -> Result<Response> {
    let (_, org) = credential(&app, &headers).await?;
    if !safe_name(&name) {
        return Err(Error(StatusCode::BAD_REQUEST, "invalid_update_file"));
    }
    file_for_org(app, org, id, name, headers).await
}
pub(crate) async fn file_for_org(
    app: Arc<App>,
    org: Uuid,
    id: Uuid,
    name: String,
    headers: HeaderMap,
) -> Result<Response> {
    if !safe_name(&name) {
        return Err(Error(StatusCode::BAD_REQUEST, "invalid_update_file"));
    }
    let manifest:Value=sqlx::query_scalar("SELECT r.manifest FROM app.client_releases r JOIN app.client_update_policies p ON p.release_id=r.id WHERE r.id=$1 AND p.organization_id=$2 AND p.enabled AND r.withdrawn_at IS NULL AND r.published_at<=extract(epoch FROM now()) AND r.expires_at>extract(epoch FROM now())").bind(id).bind(org).fetch_optional(&app.db).await?.ok_or(Error(StatusCode::NOT_FOUND,"update_not_available"))?;
    let entry = manifest["files"]
        .as_array()
        .and_then(|f| f.iter().find(|f| f["name"].as_str() == Some(&name)))
        .ok_or(Error(StatusCode::NOT_FOUND, "update_file_not_found"))?;
    let size = entry["size"]
        .as_u64()
        .filter(|s| *s > 0 && *s <= 512 * 1024 * 1024)
        .ok_or(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "invalid_release_metadata",
        ))?;
    let digest = entry["sha256"]
        .as_str()
        .filter(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        })
        .ok_or(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "invalid_release_metadata",
        ))?;
    let root = std::env::var_os("M53_CLIENT_RELEASES_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "release_storage_not_configured",
        ))?;
    let directory = root.join(id.to_string());
    let path = directory.join(&name);
    for path in [&root, &directory, &path] {
        let meta = tokio::fs::symlink_metadata(path)
            .await
            .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "release_file_unavailable"))?;
        if meta.file_type().is_symlink() {
            return Err(Error(
                StatusCode::SERVICE_UNAVAILABLE,
                "release_file_unavailable",
            ));
        }
    }
    let mut input = tokio::fs::File::open(&path)
        .await
        .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "release_file_unavailable"))?;
    let metadata = input
        .metadata()
        .await
        .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "release_file_unavailable"))?;
    if !metadata.is_file() || metadata.len() != size {
        return Err(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "release_file_unavailable",
        ));
    }
    let etag = format!("\"{digest}\"");
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|s| s.to_str().ok())
        == Some(&etag)
    {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response());
    }
    let use_range = headers
        .get(header::IF_RANGE)
        .is_none_or(|v| v.to_str().ok() == Some(&etag));
    let requested = if use_range {
        headers
            .get(header::RANGE)
            .map(|v| {
                v.to_str()
                    .map_err(|_| Error(StatusCode::RANGE_NOT_SATISFIABLE, "invalid_range"))
            })
            .transpose()?
    } else {
        None
    };
    let (start, end, partial) = match range(requested, size) {
        Ok(v) => v,
        Err(_) => {
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{size}"))],
            )
                .into_response());
        }
    };
    input
        .seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "release_file_unavailable"))?;
    let count = end - start + 1;
    let (sender, receiver) = tokio::sync::mpsc::channel::<std::io::Result<Bytes>>(2);
    tokio::spawn(async move {
        let mut remaining = count;
        while remaining > 0 {
            let mut bytes = vec![0; remaining.min(64 * 1024) as usize];
            match input.read(&mut bytes).await {
                Ok(0) => {
                    let _ = sender
                        .send(Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof)))
                        .await;
                    break;
                }
                Ok(n) => {
                    remaining -= n as u64;
                    bytes.truncate(n);
                    if sender.send(Ok(Bytes::from(bytes))).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = sender.send(Err(e)).await;
                    break;
                }
            }
        }
    });
    let mut result = Response::builder()
        .status(if partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(header::ETAG, etag)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, count)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CACHE_CONTROL, "private, no-cache");
    if partial {
        result = result.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"));
    }
    Ok(result
        .body(Body::from_stream(
            tokio_stream::wrappers::ReceiverStream::new(receiver),
        ))
        .expect("static response headers"))
}
#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    attempt_id: Uuid,
    release_id: Uuid,
    build_sequence: i64,
    state: String,
    safe_error_code: Option<String>,
}
pub async fn status(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(input): Json<Status>,
) -> Result<Json<Value>> {
    let (key, org) = credential(&app, &headers).await?;
    if input.attempt_id.is_nil()
        || input.release_id.is_nil()
        || input.build_sequence <= 0
        || !matches!(
            input.state.as_str(),
            "staged" | "healthy" | "failed" | "rolled_back"
        )
        || input.safe_error_code.as_ref().is_some_and(|s| {
            s.is_empty()
                || s.len() > 128
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
    {
        return Err(Error(StatusCode::BAD_REQUEST, "invalid_update_status"));
    }
    let fingerprint = hash(serde_json::to_vec(&input).expect("serializable status"));
    let mut tx = app.db.begin().await?;
    let workstation: Uuid =
        sqlx::query_scalar("SELECT workstation_id FROM app.api_credentials WHERE id=$1")
            .bind(key)
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("update:{workstation}:{}", input.attempt_id))
        .execute(&mut *tx)
        .await?;
    let permitted:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app.client_releases r JOIN app.client_update_policies p ON p.release_id=r.id WHERE r.id=$1 AND r.build_sequence=$2 AND p.organization_id=$3)").bind(input.release_id).bind(input.build_sequence).bind(org).fetch_one(&mut *tx).await?;
    if !permitted {
        return Err(Error(StatusCode::NOT_FOUND, "update_not_available"));
    }
    if let Some(r)=sqlx::query("SELECT release_id,build_sequence,state FROM app.client_update_attempts WHERE workstation_id=$1 AND attempt_id=$2").bind(workstation).bind(input.attempt_id).fetch_optional(&mut *tx).await? {
  if r.get::<Uuid,_>("release_id")!=input.release_id||r.get::<i64,_>("build_sequence")!=input.build_sequence{return Err(Error(StatusCode::CONFLICT,"update_attempt_conflict"));}
  let old:Option<String>=sqlx::query_scalar("SELECT request_hash FROM app.client_update_events WHERE workstation_id=$1 AND attempt_id=$2 AND state=$3").bind(workstation).bind(input.attempt_id).bind(&input.state).fetch_optional(&mut *tx).await?;
  if let Some(old)=old{if old!=fingerprint{return Err(Error(StatusCode::CONFLICT,"update_status_conflict"));}return Ok(Json(json!({"accepted":true})));}
  let old:String=r.get("state");
  if old=="healthy"||old=="rolled_back"||(old=="failed"&&input.state!="rolled_back"){return Err(Error(StatusCode::CONFLICT,"update_status_transition"));}
 }
    sqlx::query("INSERT INTO app.client_update_attempts(workstation_id,attempt_id,release_id,build_sequence,state) VALUES($1,$2,$3,$4,$5) ON CONFLICT(workstation_id,attempt_id) DO UPDATE SET state=excluded.state,updated_at=now()").bind(workstation).bind(input.attempt_id).bind(input.release_id).bind(input.build_sequence).bind(&input.state).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO app.client_update_events(workstation_id,attempt_id,state,request_hash,safe_error_code) VALUES($1,$2,$3,$4,$5)").bind(workstation).bind(input.attempt_id).bind(&input.state).bind(fingerprint).bind(&input.safe_error_code).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO app.workstation_update_state(workstation_id,release_id,attempt_id,state,last_healthy_sequence) VALUES($1,$2,$3,$4,$5) ON CONFLICT(workstation_id) DO UPDATE SET release_id=excluded.release_id,attempt_id=excluded.attempt_id,state=excluded.state,last_seen_at=now(),last_healthy_sequence=greatest(app.workstation_update_state.last_healthy_sequence,excluded.last_healthy_sequence)").bind(workstation).bind(input.release_id).bind(input.attempt_id).bind(&input.state).bind(if input.state=="healthy"{Some(input.build_sequence)}else{None}).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"accepted":true})))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn byte_ranges() {
        assert_eq!(range(Some("bytes=2-4"), 10), Ok((2, 4, true)));
        assert_eq!(range(Some("bytes=-3"), 10), Ok((7, 9, true)));
        assert_eq!(range(Some("bytes=8-"), 10), Ok((8, 9, true)));
        for value in [
            "bytes=10-",
            "bytes=-0",
            "bytes=3-2",
            "bytes=1-2,4-5",
            "items=0-1",
        ] {
            assert!(range(Some(value), 10).is_err());
        }
    }
    #[test]
    fn windows_names() {
        for name in [
            "../x", "CON.exe", "LPT1.dll", "a..exe", "a:b.exe", "a/b", "a\\b", ".hidden", "foo.",
        ] {
            assert!(!safe_name(name));
        }
        assert!(safe_name("M53ColpoLauncher.exe"));
    }
}
