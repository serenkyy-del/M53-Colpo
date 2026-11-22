//! Anonymous downloads expose only explicitly published software, never provisioning.
use crate::api::{App, Error};
use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use std::{env, path::PathBuf, sync::Arc};
use uuid::Uuid;
type Result<T> = std::result::Result<T, Error>;
fn config() -> Result<(Uuid, String, PathBuf)> {
    if env::var("M53_PUBLIC_DOWNLOADS").as_deref() != Ok("1") {
        return Err(Error(StatusCode::NOT_FOUND, "not_found"));
    }
    let org = env::var("M53_PUBLIC_CLIENT_ORGANIZATION_ID")
        .ok()
        .and_then(|s| s.parse::<Uuid>().ok())
        .filter(|v| !v.is_nil())
        .ok_or(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "public_downloads_not_configured",
        ))?;
    let channel = env::var("M53_PUBLIC_CLIENT_CHANNEL").unwrap_or_else(|_| "test".into());
    let dir = env::var_os("M53_PUBLIC_INSTALLERS_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "public_downloads_not_configured",
        ))?;
    if !crate::client_updates::safe_name(&channel) {
        return Err(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "public_downloads_not_configured",
        ));
    }
    Ok((org, channel, dir))
}
async fn published(app: &App, id: Option<Uuid>) -> Result<(Uuid, Value, Value, PathBuf)> {
    let (org, channel, dir) = config()?;
    let row=sqlx::query("SELECT r.id,r.envelope,r.manifest FROM app.client_releases r JOIN app.client_update_policies p ON p.release_id=r.id WHERE p.organization_id=$1 AND p.enabled AND r.withdrawn_at IS NULL AND r.platform='windows-x86_64-msvc' AND r.channel=$2 AND r.min_api<=2 AND r.max_api>=2 AND r.cache_format=1 AND r.published_at<=extract(epoch FROM now()) AND r.expires_at>extract(epoch FROM now()) AND ($3::uuid IS NULL OR r.id=$3) ORDER BY r.build_sequence DESC LIMIT 1")
 .bind(org).bind(channel).bind(id).fetch_optional(&app.db).await?.ok_or(Error(StatusCode::NOT_FOUND,"public_release_not_found"))?;
    Ok((row.get("id"), row.get("envelope"), row.get("manifest"), dir))
}
pub async fn latest(State(app): State<Arc<App>>) -> Result<impl IntoResponse> {
    let (_, envelope, _, _) = published(&app, None).await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"update":envelope})),
    ))
}
pub async fn file(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path((id, name)): Path<(Uuid, String)>,
) -> Result<Response> {
    published(&app, Some(id)).await?;
    let (org, _, _) = config()?;
    crate::client_updates::file_for_org(app, org, id, name, headers).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    release_id: Uuid,
    size: u64,
    sha256: String,
    envelope_sha256: String,
}
pub async fn installer(State(app): State<Arc<App>>) -> Result<Response> {
    let (id, envelope, _, root) = published(&app, None).await?;
    let directory = root.join(id.to_string());
    let path = directory.join("M53Colpo-Setup.exe");
    let metadata = directory.join("installer.json");
    for p in [&root, &directory, &path, &metadata] {
        let m = tokio::fs::symlink_metadata(p).await.map_err(|_| {
            Error(
                StatusCode::SERVICE_UNAVAILABLE,
                "public_installer_not_ready",
            )
        })?;
        if m.file_type().is_symlink() {
            return Err(Error(
                StatusCode::SERVICE_UNAVAILABLE,
                "public_installer_not_ready",
            ));
        }
    }
    if tokio::fs::metadata(&metadata)
        .await
        .map_err(|_| {
            Error(
                StatusCode::SERVICE_UNAVAILABLE,
                "public_installer_not_ready",
            )
        })?
        .len()
        > 8192
    {
        return Err(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "invalid_installer_metadata",
        ));
    }
    let artifact: Artifact =
        serde_json::from_slice(&tokio::fs::read(metadata).await.map_err(|_| {
            Error(
                StatusCode::SERVICE_UNAVAILABLE,
                "public_installer_not_ready",
            )
        })?)
        .map_err(|_| {
            Error(
                StatusCode::SERVICE_UNAVAILABLE,
                "invalid_installer_metadata",
            )
        })?;
    let canonical = m53_contracts::canonical_json::to_vec(&envelope).map_err(|_| {
        Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "invalid_installer_metadata",
        )
    })?;
    if artifact.release_id != id
        || artifact.size == 0
        || artifact.size > 16 * 1024 * 1024
        || artifact.sha256.len() != 64
        || !artifact
            .sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || artifact.envelope_sha256 != crate::api::hash(canonical)
    {
        return Err(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "invalid_installer_metadata",
        ));
    }
    let file_metadata = tokio::fs::metadata(&path).await.map_err(|_| {
        Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "public_installer_not_ready",
        )
    })?;
    if !file_metadata.is_file() || file_metadata.len() != artifact.size {
        return Err(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "public_installer_integrity_failed",
        ));
    }
    let input = tokio::fs::read(&path).await.map_err(|_| {
        Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "public_installer_not_ready",
        )
    })?;
    if input.len() as u64 != artifact.size || crate::api::hash(&input) != artifact.sha256 {
        return Err(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "public_installer_integrity_failed",
        ));
    }
    Ok((
        [
            (
                header::CONTENT_TYPE,
                "application/vnd.microsoft.portable-executable",
            ),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"M53Colpo-Setup.exe\"",
            ),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        Body::from(input),
    )
        .into_response())
}
