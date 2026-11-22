//! Per-field plain text libraries. Clinical values never enter audit descriptions.
use crate::api::{self, Actor, App, Error};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Row, postgres::PgRow};
use std::sync::Arc;
use uuid::Uuid;
type Result<T> = std::result::Result<T, Error>;
fn bad(code: &'static str) -> Error {
    Error(StatusCode::BAD_REQUEST, code)
}
fn row(r: &PgRow) -> Value {
    json!({"id":r.get::<Uuid,_>("id"),"field_code":r.get::<String,_>("field_code"),"scope":r.get::<String,_>("scope"),"title":r.get::<String,_>("title"),"body":r.get::<String,_>("body"),"row_version":r.get::<i64,_>("row_version"),"can_edit":true,"updated_at":r.get::<String,_>("updated_at_text")})
}
fn cached(h: &HeaderMap, a: &Actor, value: Value) -> Response {
    let etag = format!(
        "\"{}\"",
        api::hash(serde_json::to_vec(&json!([a.user, value])).expect("JSON"))
    );
    let matches = h
        .get("if-none-match")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"));
    let mut response = if matches {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        Json(value).into_response()
    };
    response
        .headers_mut()
        .insert("etag", etag.parse().expect("hash"));
    response.headers_mut().insert(
        "cache-control",
        "private, no-cache".parse().expect("static"),
    );
    response.headers_mut().insert(
        "vary",
        "Authorization, X-Workstation-Key".parse().expect("static"),
    );
    response
}
fn conflict(code: &'static str, version: i64) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({"code":code,"current_version":version})),
    )
        .into_response()
}
fn require_operation(h: &HeaderMap) -> Result<()> {
    let valid = h
        .get("x-operation-id")
        .and_then(|s| s.to_str().ok())
        .and_then(|s| Uuid::parse_str(s).ok())
        .is_some_and(|id| !id.is_nil());
    if !valid {
        return Err(bad("operation_id_required"));
    }
    Ok(())
}
async fn limit(app: &App, field: &str) -> Result<usize> {
    let n: Option<i32> = sqlx::query_scalar(
        "SELECT max_bytes FROM app.phrase_template_fields WHERE field_code=$1 AND enabled",
    )
    .bind(field)
    .fetch_optional(&app.db)
    .await?;
    Ok(n.ok_or(bad("unknown_phrase_field"))? as usize)
}
fn validate(title: &str, body: &str, max: usize) -> Result<()> {
    if title.trim().is_empty()
        || title.chars().count() > 200
        || body.trim().is_empty()
        || body.len() > max
        || title.contains('\0')
        || body.contains('\0')
    {
        return Err(bad("invalid_phrase_text"));
    }
    Ok(())
}
pub async fn fields(State(app): State<Arc<App>>, h: HeaderMap) -> Result<Response> {
    let a = api::actor(&app, &h).await?;
    let rows=sqlx::query("SELECT field_code,name,target_kind,max_bytes,enabled FROM app.phrase_template_fields ORDER BY field_code").fetch_all(&app.db).await?;
    Ok(cached(
        &h,
        &a,
        json!({"fields":rows.iter().map(|r|json!({"field_code":r.get::<String,_>("field_code"),"name":r.get::<String,_>("name"),"target_kind":r.get::<String,_>("target_kind"),"max_bytes":r.get::<i32,_>("max_bytes"),"enabled":r.get::<bool,_>("enabled")})).collect::<Vec<_>>()}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    field_code: String,
    visibility: Option<String>,
}
pub async fn list(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Query(q): Query<Filter>,
) -> Result<Response> {
    let a = api::actor(&app, &h).await?;
    limit(&app, &q.field_code).await?;
    let shared = match q.visibility.as_deref().unwrap_or("all") {
        "all" => true,
        "personal" => false,
        _ => return Err(bad("invalid_phrase_visibility")),
    };
    let rows=sqlx::query("SELECT *,(to_jsonb(updated_at) #>> '{}') AS updated_at_text FROM app.phrase_templates WHERE organization_id=$1 AND field_code=$2 AND (owner_user_id=$3 OR (scope='shared' AND $4)) ORDER BY lower(title),id").bind(a.org).bind(q.field_code).bind(a.user).bind(shared).fetch_all(&app.db).await?;
    Ok(cached(
        &h,
        &a,
        json!({"templates":rows.iter().map(row).collect::<Vec<_>>()}),
    ))
}
pub async fn get(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Response> {
    let a = api::actor(&app, &h).await?;
    let r=sqlx::query("SELECT *,(to_jsonb(updated_at) #>> '{}') AS updated_at_text FROM app.phrase_templates WHERE id=$1 AND organization_id=$2 AND (scope='shared' OR owner_user_id=$3)").bind(id).bind(a.org).bind(a.user).fetch_one(&app.db).await?;
    Ok(cached(&h, &a, row(&r)))
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    id: Uuid,
    field_code: String,
    scope: String,
    title: String,
    body: String,
}
pub async fn create(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Json(v): Json<Create>,
) -> Result<Response> {
    let a = api::actor(&app, &h).await?;
    require_operation(&h)?;
    if v.id.is_nil() || !matches!(v.scope.as_str(), "personal" | "shared") {
        return Err(bad("invalid_phrase_identity"));
    }
    let mut tx = app.db.begin().await?;
    let (op, previous) = api::operation_start(
        &mut tx,
        &a,
        &h,
        "/api/v1/phrase-templates".into(),
        &json!(v),
    )
    .await?;
    if let Some(previous) = previous {
        return Ok(Json(previous).into_response());
    }
    validate(&v.title, &v.body, limit(&app, &v.field_code).await?)?;
    let r=sqlx::query("INSERT INTO app.phrase_templates(id,organization_id,field_code,scope,owner_user_id,title,body,created_by,updated_by) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$8) ON CONFLICT(id) DO NOTHING RETURNING *,(to_jsonb(updated_at) #>> '{}') AS updated_at_text").bind(v.id).bind(a.org).bind(&v.field_code).bind(&v.scope).bind(if v.scope=="personal"{Some(a.user)}else{None}).bind(&v.title).bind(&v.body).bind(a.user).fetch_optional(&mut *tx).await?;
    let r = r.ok_or(Error(StatusCode::CONFLICT, "phrase_template_id_conflict"))?;
    api::audit(&mut tx, &a, v.id, "phrase.created", 1).await?;
    Ok(api::operation_finish(tx, &a, op, row(&r))
        .await?
        .into_response())
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    title: String,
    body: String,
    expected_version: i64,
}
pub async fn update(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(v): Json<Update>,
) -> Result<Response> {
    let a = api::actor(&app, &h).await?;
    require_operation(&h)?;
    if v.expected_version < 1 {
        return Err(bad("invalid_expected_version"));
    }
    let mut tx = app.db.begin().await?;
    let (op, previous) = api::operation_start(
        &mut tx,
        &a,
        &h,
        format!("/api/v1/phrase-templates/{id}"),
        &json!(v),
    )
    .await?;
    if let Some(previous) = previous {
        return Ok(Json(previous).into_response());
    }
    let old=sqlx::query("SELECT * FROM app.phrase_templates WHERE id=$1 AND organization_id=$2 AND (scope='shared' OR owner_user_id=$3) FOR UPDATE").bind(id).bind(a.org).bind(a.user).fetch_one(&mut *tx).await?;
    let current = old.get::<i64, _>("row_version");
    if current != v.expected_version {
        return Ok(conflict("phrase_template_version_conflict", current));
    }
    validate(
        &v.title,
        &v.body,
        limit(&app, &old.get::<String, _>("field_code")).await?,
    )?;
    let r=sqlx::query("UPDATE app.phrase_templates SET title=$2,body=$3,updated_by=$4 WHERE id=$1 RETURNING *,(to_jsonb(updated_at) #>> '{}') AS updated_at_text").bind(id).bind(&v.title).bind(&v.body).bind(a.user).fetch_one(&mut *tx).await?;
    api::audit(&mut tx, &a, id, "phrase.updated", r.get("row_version")).await?;
    Ok(api::operation_finish(tx, &a, op, row(&r))
        .await?
        .into_response())
}
fn pref(r: &PgRow) -> Value {
    json!({"apply_mode":r.get::<String,_>("apply_mode"),"include_shared":r.get::<bool,_>("include_shared"),"row_version":r.get::<i64,_>("row_version")})
}
pub async fn preferences(State(app): State<Arc<App>>, h: HeaderMap) -> Result<Response> {
    let a = api::actor(&app, &h).await?;
    let r = sqlx::query(
        "SELECT * FROM app.user_phrase_preferences WHERE user_id=$1 AND organization_id=$2",
    )
    .bind(a.user)
    .bind(a.org)
    .fetch_optional(&app.db)
    .await?;
    Ok(cached(
        &h,
        &a,
        r.as_ref()
            .map(pref)
            .unwrap_or(json!({"apply_mode":"append","include_shared":true,"row_version":0})),
    ))
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    apply_mode: String,
    include_shared: bool,
    expected_version: i64,
}
pub async fn save_preferences(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Json(v): Json<Preferences>,
) -> Result<Response> {
    let a = api::actor(&app, &h).await?;
    require_operation(&h)?;
    if v.expected_version < 0 || !matches!(v.apply_mode.as_str(), "append" | "replace") {
        return Err(bad("invalid_phrase_preferences"));
    }
    let mut tx = app.db.begin().await?;
    let (op, previous) = api::operation_start(
        &mut tx,
        &a,
        &h,
        "/api/v1/me/phrase-template-preferences".into(),
        &json!(v),
    )
    .await?;
    if let Some(previous) = previous {
        return Ok(Json(previous).into_response());
    }
    // Also serializes first-write races, where no preference row exists yet.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,1))")
        .bind(a.user.to_string())
        .execute(&mut *tx)
        .await?;
    let current:Option<i64>=sqlx::query_scalar("SELECT row_version FROM app.user_phrase_preferences WHERE user_id=$1 AND organization_id=$2 FOR UPDATE").bind(a.user).bind(a.org).fetch_optional(&mut *tx).await?;
    if current.unwrap_or(0) != v.expected_version {
        return Ok(conflict(
            "phrase_preferences_version_conflict",
            current.unwrap_or(0),
        ));
    }
    let r=sqlx::query("INSERT INTO app.user_phrase_preferences(organization_id,user_id,apply_mode,include_shared) VALUES($1,$2,$3,$4) ON CONFLICT(user_id) DO UPDATE SET apply_mode=excluded.apply_mode,include_shared=excluded.include_shared RETURNING *").bind(a.org).bind(a.user).bind(v.apply_mode).bind(v.include_shared).fetch_one(&mut *tx).await?;
    api::audit(
        &mut tx,
        &a,
        a.user,
        "phrase.preferences_updated",
        r.get("row_version"),
    )
    .await?;
    Ok(api::operation_finish(tx, &a, op, pref(&r))
        .await?
        .into_response())
}
