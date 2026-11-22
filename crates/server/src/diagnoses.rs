//! Bounded read-only Core scan; only a complete generation becomes visible.
//! Clinical reads never wait for Core and never rewrite a selected snapshot.
use crate::{api::{App,Error,actor},odata_core::{CoreClient,Diagnosis}};
use axum::{Json,extract::{Query,State},http::{HeaderMap,StatusCode}};
use serde::Deserialize;
use m53_contracts::diagnoses::{Metadata,Page,Entry};
use sqlx::Row;
use std::{collections::BTreeMap,sync::Arc,time::Duration};
use uuid::Uuid;

fn unavailable()->Error { Error(StatusCode::SERVICE_UNAVAILABLE,"diagnoses_catalog_not_ready") }
fn source()->Result<Uuid,Error> {
    std::env::var("M53_ODATA_CORE_SOURCE_ID").ok().and_then(|v|v.parse().ok()).filter(|id:&Uuid|!id.is_nil()).ok_or_else(unavailable)
}
#[derive(Deserialize)]
pub struct PageQuery {version_id:Uuid,after:Option<Uuid>,limit:Option<i64>}

pub async fn metadata(State(app):State<Arc<App>>,h:HeaderMap)->Result<Json<Metadata>,Error> {
    let a=actor(&app,&h).await?;let source=source()?;
    let r=sqlx::query("SELECT v.id,v.source_id,to_char(v.synced_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS synced_at,v.row_count FROM app.diagnosis_catalog_sync c JOIN app.diagnosis_catalog_versions v ON v.id=c.current_version_id JOIN app.integration_sources s ON s.id=c.source_id WHERE c.source_id=$1 AND c.organization_id=$2 AND s.enabled AND v.published")
        .bind(source).bind(a.org).fetch_optional(&app.db).await?.ok_or_else(unavailable)?;
    Ok(Json(Metadata {version_id:r.get("id"),source_id:r.get("source_id"),synced_at:r.get("synced_at"),count:r.get::<i64,_>("row_count") as u64}))
}
pub async fn page(State(app):State<Arc<App>>,h:HeaderMap,Query(q):Query<PageQuery>)->Result<Json<Page>,Error> {
    let a=actor(&app,&h).await?;let limit=q.limit.unwrap_or(500);
    if !(1..=500).contains(&limit) || q.version_id.is_nil() || q.after.is_some_and(|id|id.is_nil()) { return Err(Error(StatusCode::BAD_REQUEST,"invalid_diagnoses_catalog_page")); }
    let unavailable=||Error(StatusCode::CONFLICT,"diagnoses_catalog_version_unavailable");
    let source=source().map_err(|_|unavailable())?;
    // One statement pins authorization/version and rows to the same SQL snapshot.
    let r=sqlx::query("SELECT v.id,(SELECT coalesce(jsonb_agg(to_jsonb(x) ORDER BY x.guid),'[]'::jsonb) FROM (SELECT guid,code,name,full_name,active,is_folder,source_version FROM app.diagnosis_catalog_items WHERE version_id=v.id AND ($4::uuid IS NULL OR guid>$4) ORDER BY guid LIMIT $5)x) AS items FROM app.diagnosis_catalog_versions v JOIN app.integration_sources s ON s.id=v.source_id WHERE v.id=$1 AND v.source_id=$2 AND v.organization_id=$3 AND s.enabled AND v.published")
        .bind(q.version_id).bind(source).bind(a.org).bind(q.after).bind(limit+1).fetch_optional(&app.db).await?.ok_or_else(unavailable)?;
    let mut items:Vec<Entry>=serde_json::from_value(r.get("items")).map_err(|_|Error(StatusCode::INTERNAL_SERVER_ERROR,"invalid_diagnoses_catalog"))?;
    let next_after=if items.len()>limit as usize {items.truncate(limit as usize);items.last().map(|item|item.guid)}else{None};
    Ok(Json(Page {version_id:q.version_id,items,next_after}))
}

/// Only this optional command/worker performs network I/O. The active source is
/// validated before I/O and again before publish. Lease fences expired workers.
pub async fn sync(app:&App)->Result<(),&'static str> {
    let source=source().map_err(|_|"diagnoses_catalog_configuration")?;
    let client=CoreClient::from_env().map_err(|e|e.code())?.ok_or("diagnoses_catalog_configuration")?;
    let lease=Uuid::new_v4();
    let org:Uuid=sqlx::query_scalar("INSERT INTO app.diagnosis_catalog_sync(source_id,organization_id,lease_id,lease_until) SELECT id,organization_id,$2,clock_timestamp()+interval '15 minutes' FROM app.integration_sources WHERE id=$1 AND enabled ON CONFLICT(source_id) DO UPDATE SET lease_id=excluded.lease_id,lease_until=excluded.lease_until WHERE app.diagnosis_catalog_sync.lease_until IS NULL OR app.diagnosis_catalog_sync.lease_until<clock_timestamp() RETURNING organization_id")
        .bind(source).bind(lease).fetch_optional(&app.db).await.map_err(|_|"diagnoses_catalog_database")?.ok_or("diagnoses_catalog_busy_or_invalid_source")?;
    let result=tokio::time::timeout(Duration::from_secs(600),scan(&client)).await.map_err(|_|"diagnoses_catalog_timeout").and_then(|v|v);
    let result=match result {Ok(items)=>publish(app,source,org,lease,items).await,Err(code)=>Err(code)};
    // A crashed process can leave only a lease; never a visible partial version.
    let code=result.as_ref().err().copied();
    sqlx::query("UPDATE app.diagnosis_catalog_sync SET lease_id=NULL,lease_until=NULL,last_checked_at=clock_timestamp(),safe_error_code=$3 WHERE source_id=$1 AND lease_id=$2")
        .bind(source).bind(lease).bind(code).execute(&app.db).await.map_err(|_|"diagnoses_catalog_database")?;
    result
}
async fn scan(client:&CoreClient)->Result<Vec<Diagnosis>,&'static str> {
    let mut items=BTreeMap::new();let mut skip=0;let mut bytes=0usize;
    for _ in 0..=250 {
        let page=client.diagnoses_page(skip,200).await.map_err(|e|e.code())?;
        for row in page.items {
            bytes=bytes.saturating_add(row.code.len()+row.name.len()+row.full_name.len()+row.source_version.len()+128);
            if bytes>32*1024*1024 || items.insert(row.guid,row).is_some() || items.len()>50000 {return Err("diagnoses_catalog_invalid_scan");}
        }
        let Some(next)=page.next_skip else {
            if items.is_empty(){return Err("diagnoses_catalog_empty_scan");}
            return Ok(items.into_values().collect());
        };
        skip=next;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err("diagnoses_catalog_page_limit")
}
async fn publish(app:&App,source:Uuid,org:Uuid,lease:Uuid,items:Vec<Diagnosis>)->Result<(),&'static str> {
    async {
        let body=serde_json::to_value(&items).unwrap();
        let digest=crate::api::hash(m53_contracts::canonical_json::to_vec(&body).unwrap());
        let mut tx=app.db.begin().await?;
        let current=sqlx::query("SELECT c.current_version_id FROM app.diagnosis_catalog_sync c JOIN app.integration_sources s ON s.id=c.source_id WHERE c.source_id=$1 AND c.organization_id=$2 AND c.lease_id=$3 AND c.lease_until>clock_timestamp() AND s.enabled FOR UPDATE OF c")
            .bind(source).bind(org).bind(lease).fetch_optional(&mut *tx).await?;
        let Some(current)=current else{return Ok(false)};
        let previous:Option<Uuid>=current.get("current_version_id");
        let old:Option<String>=sqlx::query_scalar("SELECT content_sha256 FROM app.diagnosis_catalog_versions WHERE id=$1 AND published").bind(previous).fetch_optional(&mut *tx).await?;
        if old.as_deref()==Some(&digest) { tx.commit().await?;return Ok(true); }
        let id=Uuid::new_v4();
        sqlx::query("INSERT INTO app.diagnosis_catalog_versions(id,source_id,organization_id,content_sha256,row_count) VALUES($1,$2,$3,$4,$5)").bind(id).bind(source).bind(org).bind(digest).bind(items.len() as i64).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO app.diagnosis_catalog_items(version_id,guid,code,name,full_name,active,is_folder,source_version) SELECT $1,x.* FROM jsonb_to_recordset($2) AS x(guid uuid,code text,name text,full_name text,active boolean,is_folder boolean,source_version text)").bind(id).bind(body).execute(&mut *tx).await?;
        sqlx::query("UPDATE app.diagnosis_catalog_versions SET published=true WHERE id=$1").bind(id).execute(&mut *tx).await?;
        sqlx::query("UPDATE app.diagnosis_catalog_sync SET current_version_id=$2 WHERE source_id=$1 AND lease_id=$3").bind(source).bind(id).bind(lease).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO app.audit_events(id,actor_id,action,entity_id,safe_description) VALUES($1,NULL,'diagnoses.catalog.synced',$2,'Complete read-only 1C catalogue generation published')").bind(Uuid::new_v4()).bind(id).execute(&mut *tx).await?;
        tx.commit().await?;Ok::<_,sqlx::Error>(true)
    }.await.map_err(|e|{if let sqlx::Error::Database(d)=&e { eprintln!("Diagnosis catalogue DB error code={:?}",d.code()); } "diagnoses_catalog_database"}).and_then(|ok|if ok {Ok(())}else{Err("diagnoses_catalog_lost_lease")})
}
