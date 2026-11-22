//! S5: authenticate from headers before Json/Bytes extractors ever poll the body.
use crate::api::{self, App, Error};
use axum::{
    extract::{MatchedPath, Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;
use uuid::Uuid;
pub const SMALL_JSON: usize = 32 * 1024;
pub const AUTH_JSON: usize = 8 * 1024;
pub const LARGE_JSON: usize = 64 * 1024 * 1024;
pub const ORIGINAL_BYTES: usize = 64 * 1024 * 1024;
pub const APPROVAL_JSON: usize = 128 * 1024;
pub const PRIORITY_JSON: usize = 64 * 1024;

fn limit(path: &str) -> usize {
    match path {
        "/api/v1/login"
        | "/api/v1/auth/1c/login"
        | "/api/v1/patients/:id"
        | "/api/v1/client-updates/status" => AUTH_JSON,
        "/api/v1/studies/:id/protocol" | "/api/v1/templates" | "/api/v1/studies/:id/reports" => {
            LARGE_JSON
        }
        "/api/v1/studies/:id/assets" => ORIGINAL_BYTES,
        "/api/v1/studies/:id/results/1c/pdf" => m53_contracts::colposcopy_1c::MAX_PDF_BYTES,
        "/api/v1/protocol-templates" | "/api/v1/protocol-templates/:id" => 5 * 1024 * 1024,
        "/api/v1/activity-events"
        | "/api/v1/workstation/activity-events"
        | "/api/colposcopy/tasks"
        | "/api/v1/phrase-templates"
        | "/api/v1/phrase-templates/:id"
        | "/api/v1/me/phrase-template-preferences" => 128 * 1024,
        "/api/v1/studies/:id/approve" => APPROVAL_JSON,
        "/api/v1/diagnoses/priority" => PRIORITY_JSON,
        _ => SMALL_JSON,
    }
}
pub async fn authorize_body(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    if !matches!(
        request.method().as_str(),
        "POST" | "PUT" | "PATCH" | "DELETE"
    ) {
        return next.run(request).await;
    }
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
        .unwrap_or("");
    let headers = request.headers();
    let permission = match route {
        "/api/colposcopy/tasks" => crate::colposcopy_tasks::source(&app, headers)
            .await
            .map(|_| ()),
        "/api/v1/patients/external" | "/api/v1/orders/external" => {
            crate::external::authorize_body(&app, headers).await
        }
        "/api/v1/login"
        | "/api/v1/auth/1c/login"
        | "/api/v1/client-updates/status"
        | "/api/v1/workstation/activity-events" => api::credential(&app, headers).await.map(|_| ()),
        path if path.starts_with("/api/v1/") => match api::actor(&app, headers).await {
            Err(error) => Err(error),
            Ok(actor) => {
                if path.starts_with("/api/v1/studies/:id/") {
                    let id = request
                        .uri()
                        .path()
                        .split('/')
                        .nth(4)
                        .and_then(|id| Uuid::parse_str(id).ok());
                    match id {
                        Some(id) => api::study(&app, &actor, id).await.map(|_| ()),
                        None => Err(Error(StatusCode::BAD_REQUEST, "invalid_study_id")),
                    }
                } else {
                    Ok(())
                }
            }
        },
        _ => return next.run(request).await,
    };
    if let Err(error) = permission {
        return error.into_response();
    }
    if headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|length| length > limit(route) as u64)
    {
        return Error(StatusCode::PAYLOAD_TOO_LARGE, "request_body_too_large").into_response();
    }
    next.run(request).await
}
