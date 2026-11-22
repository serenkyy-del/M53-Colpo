//! Immutable original responses: rights are checked by the caller before cache
//! validation. Keep a verified byte snapshot; never stream a mutable file after
//! its hash check or block a Tokio reactor while hashing a large image.
use crate::api::{Error, hash};
use axum::{
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use std::path::PathBuf;
use uuid::Uuid;
pub(crate) fn unchanged(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(','))
        .map(str::trim)
        .any(|v| v == "*" || v.strip_prefix("W/").unwrap_or(v) == etag)
}
pub(crate) async fn response(
    path: PathBuf,
    id: Uuid,
    sha256: String,
    byte_size: i64,
    mime: String,
    headers: &HeaderMap,
) -> Result<Response, Error> {
    let etag = format!("\"{sha256}\"");
    let meta = tokio::fs::metadata(&path).await?;
    if byte_size < 0 || !meta.is_file() || meta.len() != byte_size as u64 {
        return Err(Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "asset_integrity_error",
        ));
    }
    let mut response = if unchanged(headers, &etag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, Error> {
            let bytes = std::fs::read(path)?;
            if bytes.len() as i64 != byte_size || hash(&bytes) != sha256 {
                return Err(Error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "asset_integrity_error",
                ));
            }
            Ok(bytes)
        })
        .await
        .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "asset_read_worker_error"))??;
        (
            [
                (header::CONTENT_TYPE, mime),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{id}\""),
                ),
                (
                    header::CONTENT_SECURITY_POLICY,
                    "default-src 'none'; sandbox".into(),
                ),
            ],
            bytes,
        )
            .into_response()
    };
    response.headers_mut().insert(
        "x-content-sha256",
        etag.trim_matches('"')
            .parse()
            .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "asset_integrity_error"))?,
    );
    response.headers_mut().insert(
        header::ETAG,
        etag.parse()
            .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "asset_integrity_error"))?,
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "private, no-cache".parse().expect("static header"),
    );
    response.headers_mut().insert(
        header::VARY,
        "Authorization, X-Workstation-Key"
            .parse()
            .expect("static header"),
    );
    Ok(response)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conditional_get_compares_weak_etags_without_accepting_other_hashes() {
        let mut h = HeaderMap::new();
        h.insert(
            header::IF_NONE_MATCH,
            "\"old\", W/\"exact\"".parse().unwrap(),
        );
        assert!(unchanged(&h, "\"exact\""));
        assert!(!unchanged(&h, "\"other\""));
        h.insert(header::IF_NONE_MATCH, "*".parse().unwrap());
        assert!(unchanged(&h, "\"anything\""));
    }
}
