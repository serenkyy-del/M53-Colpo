//! Organization-wide portable protocol templates.
use crate::api::{self, App, Error};
use axum::{Json, extract::{Path, Query, State}, http::{HeaderMap, StatusCode}, response::{IntoResponse, Response}};
use m53_domain::visit_template::VisitTemplate;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Row, postgres::PgRow};
use std::sync::Arc;
use uuid::Uuid;

type Result<T> = std::result::Result<T, Error>;

fn validate(title: &str, payload: &VisitTemplate) -> Result<()> {
    if title.trim().is_empty() || title.chars().count()>200 || title.contains('\0')
        || payload.validate().is_err() || serde_json::to_vec(payload).map_or(true, |v| v.len()>4*1024*1024) {
        return Err(Error(StatusCode::BAD_REQUEST, "invalid_protocol_template"));
    }
    Ok(())
}
fn map_write_error(error: sqlx::Error) -> Error {
    if error.as_database_error().is_some_and(|e| e.constraint()==Some("protocol_template_title_conflict")) {
        Error(StatusCode::CONFLICT, "protocol_template_title_conflict")
    } else { error.into() }
}
// Match the client algorithm in Rust; PostgreSQL lower() is not equivalent for all Unicode.
async fn check_normalized_title(tx: &mut sqlx::Transaction<'_,sqlx::Postgres>, org: Uuid, id: Uuid, title: &str) -> Result<()> {
    let normalized=title.trim().to_lowercase();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("protocol-template-title:{org}:{normalized}"))
        .execute(&mut **tx).await?;
    let titles: Vec<String>=sqlx::query_scalar("SELECT title FROM app.protocol_templates WHERE organization_id=$1 AND id<>$2")
        .bind(org).bind(id).fetch_all(&mut **tx).await?;
    if titles.iter().any(|t|t.trim().to_lowercase()==normalized) {
        return Err(Error(StatusCode::CONFLICT,"protocol_template_title_conflict"));
    }
    Ok(())
}
fn require_operation(h: &HeaderMap) -> Result<()> {
    if !h.get("x-operation-id").and_then(|v|v.to_str().ok()).and_then(|v|Uuid::parse_str(v).ok()).is_some_and(|id|!id.is_nil()) {
        return Err(Error(StatusCode::BAD_REQUEST, "operation_id_required"));
    }
    Ok(())
}
fn metadata(r: &PgRow) -> Value {
    json!({"id":r.get::<Uuid,_>("id"), "title":r.get::<String,_>("title"), "row_version":r.get::<i64,_>("row_version"), "updated_at":r.get::<String,_>("updated_at_text")})
}
fn detail(r: &PgRow) -> Value {
    let mut result=metadata(r); result["protocol"]=r.get::<Value,_>("payload"); result
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter { after: Option<Uuid> }

pub async fn list(State(app):State<Arc<App>>, h:HeaderMap, Query(q):Query<Filter>) -> Result<Json<Value>> {
    let a=api::actor(&app,&h).await?;
    let rows=sqlx::query("SELECT id,title,row_version,(to_jsonb(updated_at) #>> '{}') AS updated_at_text FROM app.protocol_templates WHERE organization_id=$1 AND ($2::uuid IS NULL OR id>$2) ORDER BY id LIMIT 501").bind(a.org).bind(q.after).fetch_all(&app.db).await?;
    let more=rows.len()>500;
    let page=&rows[..rows.len().min(500)];
    Ok(Json(json!({"templates":page.iter().map(metadata).collect::<Vec<_>>(), "next_after":if more {page.last().map(|r|r.get::<Uuid,_>("id"))} else {None}})))
}
pub async fn get(State(app):State<Arc<App>>, h:HeaderMap, Path(id):Path<Uuid>) -> Result<Json<Value>> {
    let a=api::actor(&app,&h).await?;
    let row=sqlx::query("SELECT *,(to_jsonb(updated_at) #>> '{}') AS updated_at_text FROM app.protocol_templates WHERE organization_id=$1 AND id=$2").bind(a.org).bind(id).fetch_optional(&app.db).await?.ok_or(Error(StatusCode::NOT_FOUND,"protocol_template_not_found"))?;
    Ok(Json(detail(&row)))
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Create { id:Uuid, title:String, protocol:VisitTemplate }
pub async fn create(State(app):State<Arc<App>>, h:HeaderMap, Json(v):Json<Create>) -> Result<Response> {
    let a=api::actor(&app,&h).await?; require_operation(&h)?;
    if v.id.is_nil() { return Err(Error(StatusCode::BAD_REQUEST,"invalid_protocol_template")); }
    validate(&v.title,&v.protocol)?;
    let mut tx=app.db.begin().await?;
    let (op,previous)=api::operation_start(&mut tx,&a,&h,"/api/v1/protocol-templates".into(),&json!(v)).await?;
    if let Some(previous)=previous { return Ok(Json(previous).into_response()); }
    check_normalized_title(&mut tx,a.org,v.id,&v.title).await?;
    let row=sqlx::query("INSERT INTO app.protocol_templates(id,organization_id,title,payload,created_by,updated_by) VALUES($1,$2,$3,$4,$5,$5) ON CONFLICT(id) DO NOTHING RETURNING *,(to_jsonb(updated_at) #>> '{}') AS updated_at_text")
        .bind(v.id).bind(a.org).bind(&v.title).bind(json!(v.protocol)).bind(a.user).fetch_optional(&mut *tx).await.map_err(map_write_error)?.ok_or(Error(StatusCode::CONFLICT,"protocol_template_id_conflict"))?;
    api::audit(&mut tx,&a,v.id,"protocol_template.created",1).await?;
    Ok(api::operation_finish(tx,&a,op,detail(&row)).await?.into_response())
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Update { title:String, protocol:VisitTemplate, expected_version:i64 }
pub async fn update(State(app):State<Arc<App>>, h:HeaderMap, Path(id):Path<Uuid>, Json(v):Json<Update>) -> Result<Response> {
    let a=api::actor(&app,&h).await?; require_operation(&h)?;
    if v.expected_version<1 { return Err(Error(StatusCode::BAD_REQUEST,"invalid_expected_version")); }
    validate(&v.title,&v.protocol)?;
    let mut tx=app.db.begin().await?;
    let (op,previous)=api::operation_start(&mut tx,&a,&h,format!("/api/v1/protocol-templates/{id}"),&json!(v)).await?;
    if let Some(previous)=previous { return Ok(Json(previous).into_response()); }
    let old=sqlx::query("SELECT row_version,title FROM app.protocol_templates WHERE organization_id=$1 AND id=$2 FOR UPDATE").bind(a.org).bind(id).fetch_optional(&mut *tx).await?.ok_or(Error(StatusCode::NOT_FOUND,"protocol_template_not_found"))?;
    let current:i64=old.get("row_version");
    if current!=v.expected_version { return Ok((StatusCode::CONFLICT,Json(json!({"code":"protocol_template_version_conflict","current_version":current}))).into_response()); }
    if old.get::<String,_>("title")!=v.title { check_normalized_title(&mut tx,a.org,id,&v.title).await?; }
    let row=sqlx::query("UPDATE app.protocol_templates SET title=$2,payload=$3,updated_by=$4 WHERE id=$1 RETURNING *,(to_jsonb(updated_at) #>> '{}') AS updated_at_text").bind(id).bind(&v.title).bind(json!(v.protocol)).bind(a.user).fetch_one(&mut *tx).await.map_err(map_write_error)?;
    api::audit(&mut tx,&a,id,"protocol_template.updated",row.get("row_version")).await?;
    Ok(api::operation_finish(tx,&a,op,detail(&row)).await?.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn template() -> m53_domain::visit_template::VisitTemplate {
        let v: serde_json::Value = serde_json::from_str(include_str!("../../../examples/protocol/draft.json")).unwrap();
        let p = serde_json::from_value(v["protocol"].clone()).unwrap();
        m53_domain::visit_template::VisitTemplate::capture(&p).unwrap()
    }
    #[test]
    fn shared_template_validation_checks_title_and_portable_payload() {
        let mut t = template();
        assert!(validate("Нормальный приём", &t).is_ok());
        assert!(validate("  ", &t).is_err());
        assert!(validate(&"x".repeat(201), &t).is_err());
        t.format_version = 99;
        assert!(validate("Название", &t).is_err());
    }
}
