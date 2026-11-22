//! Selection directory is available to an authorized workstation before login.
use crate::api::{self, App, Error};
use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    after: Option<Uuid>,
    limit: Option<i64>,
}

pub async fn profiles(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Query(page): Query<Page>,
) -> Result<Json<Value>, Error> {
    let (_, org) = api::credential(&app, &h).await?;
    let limit = page.limit.unwrap_or(100);
    if !(1..=200).contains(&limit) {
        return Err(Error(StatusCode::BAD_REQUEST, "invalid_page_size"));
    }
    let mut rows=sqlx::query("SELECT d.id,d.display_name,b.source_id,b.user_guid,b.login,b.row_version,e.external_guid FROM app.doctors d JOIN app.doctor_1c_logins b ON b.doctor_id=d.id AND b.organization_id=d.organization_id JOIN app.doctor_external_ids e ON e.doctor_id=d.id AND e.source_id=b.source_id AND e.organization_id=d.organization_id JOIN app.integration_sources s ON s.id=b.source_id WHERE d.organization_id=$1 AND d.active AND b.enabled AND s.enabled AND ($2::uuid IS NULL OR d.id>$2) ORDER BY d.id LIMIT $3")
        .bind(org).bind(page.after).bind(limit+1).fetch_all(&app.db).await?;
    let has_next = rows.len() > limit as usize;
    if has_next {
        rows.pop();
    }
    let next = if has_next {
        rows.last().map(|r| r.get::<Uuid, _>("id"))
    } else {
        None
    };
    let now: i64 =
        sqlx::query_scalar("SELECT floor(extract(epoch from clock_timestamp()))::bigint")
            .fetch_one(&app.db)
            .await?;
    Ok(Json(
        json!({"organization_id":org,"server_time":now,"next_after":next,
        "auth_provider":"1c_web","items":rows.iter().map(|r|json!({"doctor_id":r.get::<Uuid,_>("id"),
        "display_name":r.get::<String,_>("display_name"),"source_id":r.get::<Uuid,_>("source_id"),
        "doctor_guid":r.get::<Uuid,_>("external_guid"),"user_guid":r.get::<Uuid,_>("user_guid"),
        "login":r.get::<String,_>("login"),"binding_version":r.get::<i64,_>("row_version")})).collect::<Vec<_>>()}),
    ))
}
