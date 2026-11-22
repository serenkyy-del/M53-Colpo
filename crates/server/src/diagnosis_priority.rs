//! Organization-wide ordered preferences. Clinical diagnoses remain immutable snapshots.
use crate::api::{App,Error,actor,operation_start,operation_finish};
use axum::{Json,extract::State,http::{HeaderMap,StatusCode}};
use serde::Deserialize;
use serde_json::{Value,json};
use sqlx::Row;
use std::{sync::Arc,collections::HashSet};
use uuid::Uuid;
pub const DEFAULT_CODES:&str="N71.1 N72 N74.0 N74.2 N75.0 N75.1 N75.8 N76.4 N76.8 N80.8 N84.1 N84.2 N84.3 N86 N87 N87.0 N87.1 N87.2 N87.9 N88 N88.0 N88.1 N88.2 N88.3 N88.4 N88.8 N88.9 N89.0 N89.1 N89.2 N89.5 N90.0 N90.1 N90.2 N90.4 N90.6 N90.7 N90.9 N99.2 A18.1 A54 A56 A59 A60 A63.0 B20 B21 B25 B37 B97.7 C53 D06 D26.0 D28.0 D28.1 Z01.4 Z12.4";
fn valid(ids:&[Uuid])->bool {ids.len()<=500 && ids.iter().all(|id|!id.is_nil()) && ids.iter().collect::<HashSet<_>>().len()==ids.len()}
#[derive(Deserialize,serde::Serialize)]
pub struct Update {expected_version:i64,expected_catalog_version:Option<Uuid>,diagnosis_ids:Vec<Uuid>}
pub async fn get(State(app):State<Arc<App>>,h:HeaderMap)->Result<Json<Value>,Error>{
 let a=actor(&app,&h).await?;
 let source:Uuid=std::env::var("M53_ODATA_CORE_SOURCE_ID").ok().and_then(|v|v.parse().ok()).ok_or(Error(StatusCode::SERVICE_UNAVAILABLE,"diagnoses_catalog_not_ready"))?;
 let row=sqlx::query("SELECT version,diagnosis_ids FROM app.diagnosis_priority_groups WHERE organization_id=$1").bind(a.org).fetch_optional(&app.db).await?;
 Ok(Json(match row {Some(r)=>json!({"version":r.get::<i64,_>("version"),"diagnosis_ids":r.get::<Vec<Uuid>,_>("diagnosis_ids")}),None=>{
 let snapshot=sqlx::query("SELECT c.current_version_id,(SELECT coalesce(jsonb_agg(jsonb_build_object('code',i.code,'guid',i.guid)),'[]'::jsonb) FROM app.diagnosis_catalog_items i WHERE i.version_id=c.current_version_id AND i.active AND btrim(i.name)<>'' AND i.code=ANY($3)) AS items FROM app.diagnosis_catalog_sync c JOIN app.integration_sources s ON s.id=c.source_id JOIN app.diagnosis_catalog_versions v ON v.id=c.current_version_id WHERE c.organization_id=$1 AND c.source_id=$2 AND s.enabled AND v.published").bind(a.org).bind(source).bind(DEFAULT_CODES.split_whitespace().collect::<Vec<_>>()).fetch_optional(&app.db).await?.ok_or(Error(StatusCode::SERVICE_UNAVAILABLE,"diagnoses_catalog_not_ready"))?;
 let catalog:Uuid=snapshot.get("current_version_id");
 let rows:Vec<Value>=serde_json::from_value(snapshot.get("items")).map_err(|_|Error(StatusCode::INTERNAL_SERVER_ERROR,"invalid_diagnoses_catalog"))?;
 let mut ids=Vec::new();
 for code in DEFAULT_CODES.split_whitespace(){let matches=rows.iter().filter(|r|r["code"].as_str()==Some(code)).collect::<Vec<_>>();if matches.len()!=1{return Err(Error(StatusCode::SERVICE_UNAVAILABLE,"diagnosis_priority_defaults_unresolved"));}ids.push(serde_json::from_value::<Uuid>(matches[0]["guid"].clone()).map_err(|_|Error(StatusCode::INTERNAL_SERVER_ERROR,"invalid_diagnoses_catalog"))?);}
  json!({"version":0,"diagnosis_ids":ids,"catalog_version_id":catalog})
}}))
}
pub async fn put(State(app):State<Arc<App>>,h:HeaderMap,Json(input):Json<Update>)->Result<Json<Value>,Error>{
 let a=actor(&app,&h).await?;
 if input.expected_version<0 || !valid(&input.diagnosis_ids){return Err(Error(StatusCode::BAD_REQUEST,"invalid_diagnosis_priority"));}
 let mut tx=app.db.begin().await?;
 let (op,cached)=operation_start(&mut tx,&a,&h,"diagnoses.priority.update".into(),&json!(input)).await?;
 if let Some(response)=cached{return Ok(Json(response));}
 sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))").bind(format!("diagnosis-priority:{}",a.org)).execute(&mut *tx).await?;
 let version:Option<i64>=sqlx::query_scalar("SELECT version FROM app.diagnosis_priority_groups WHERE organization_id=$1 FOR UPDATE").bind(a.org).fetch_optional(&mut *tx).await?;
 if version.unwrap_or(0)!=input.expected_version{return Err(Error(StatusCode::CONFLICT,"diagnosis_priority_conflict"));}
 let source:Uuid=std::env::var("M53_ODATA_CORE_SOURCE_ID").ok().and_then(|v|v.parse().ok()).ok_or(Error(StatusCode::SERVICE_UNAVAILABLE,"diagnoses_catalog_not_ready"))?;
 let catalog:Option<Uuid>=sqlx::query_scalar("SELECT c.current_version_id FROM app.diagnosis_catalog_sync c JOIN app.integration_sources s ON s.id=c.source_id JOIN app.diagnosis_catalog_versions v ON v.id=c.current_version_id WHERE c.organization_id=$1 AND c.source_id=$2 AND s.enabled AND v.published FOR SHARE OF c").bind(a.org).bind(source).fetch_optional(&mut *tx).await?.flatten();
 if input.expected_version==0 && (catalog.is_none() || catalog!=input.expected_catalog_version){return Err(Error(StatusCode::CONFLICT,"diagnosis_priority_catalog_conflict"));}
 let available:i64=sqlx::query_scalar("SELECT count(DISTINCT i.guid) FROM app.diagnosis_catalog_sync c JOIN app.integration_sources s ON s.id=c.source_id JOIN app.diagnosis_catalog_items i ON i.version_id=c.current_version_id WHERE c.organization_id=$1 AND c.source_id=$3 AND s.enabled AND i.guid=ANY($2) AND i.active AND btrim(i.code)<>'' AND btrim(i.name)<>''").bind(a.org).bind(&input.diagnosis_ids).bind(source).fetch_one(&mut *tx).await?;
 if available!=input.diagnosis_ids.len() as i64{return Err(Error(StatusCode::BAD_REQUEST,"diagnosis_priority_unavailable"));}
 let next=input.expected_version.checked_add(1).ok_or(Error(StatusCode::CONFLICT,"diagnosis_priority_conflict"))?;
 sqlx::query("INSERT INTO app.diagnosis_priority_groups(organization_id,version,diagnosis_ids,updated_by) VALUES($1,$2,$3,$4) ON CONFLICT(organization_id) DO UPDATE SET version=excluded.version,diagnosis_ids=excluded.diagnosis_ids,updated_by=excluded.updated_by,updated_at=clock_timestamp()").bind(a.org).bind(next).bind(&input.diagnosis_ids).bind(a.user).execute(&mut *tx).await?;
 sqlx::query("INSERT INTO app.audit_events(id,actor_id,action,entity_id,safe_description) VALUES($1,$2,'diagnoses.priority.updated',$3,'Organization diagnosis priority order updated')").bind(Uuid::new_v4()).bind(a.user).bind(a.org).execute(&mut *tx).await?;
 operation_finish(tx,&a,op,json!({"version":next,"diagnosis_ids":input.diagnosis_ids})).await
}
#[cfg(test)] mod tests {use super::*;
 #[test] fn accepts_empty_and_unique(){assert!(valid(&[]));assert!(valid(&[Uuid::new_v4()]));}
 #[test] fn rejects_invalid(){let id=Uuid::new_v4();assert!(!valid(&[Uuid::nil()]));assert!(!valid(&[id,id]));assert!(!valid(&(0..501).map(|_|Uuid::new_v4()).collect::<Vec<_>>()));}
}
