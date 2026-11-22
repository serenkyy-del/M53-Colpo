//! Bounded, keyset-paged work lists. No clinical payloads or global study downloads.
use crate::api::{App, Error, actor, patient_row};
use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;
type Result<T> = std::result::Result<T, Error>;
#[derive(Default, Deserialize)]
pub struct PatientPage {
    pub q: Option<String>,
    pub sort: Option<String>,
    #[serde(default)]
    pub descending: bool,
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}
#[derive(Deserialize)]
pub struct TaskPage {
    pub date: String,
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct Cursor {
    version: u8,
    scope: String,
    id: Uuid,
}
fn bad() -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_page_query")
}
fn limit(value: Option<i64>) -> Result<i64> {
    let n = value.unwrap_or(50);
    if !(1..=100).contains(&n) {
        return Err(bad());
    }
    Ok(n)
}
fn scope(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
fn decode(value: Option<&str>, scope: &str) -> Result<Option<Uuid>> {
    let Some(value) = value else { return Ok(None) };
    if value.len() > 512 {
        return Err(bad());
    }
    let c: Cursor =
        serde_json::from_slice(&hex::decode(value).map_err(|_| bad())?).map_err(|_| bad())?;
    if c.version != 1 || c.scope != scope || c.id.is_nil() {
        return Err(bad());
    }
    Ok(Some(c.id))
}
fn encode(id: Uuid, scope: &str) -> String {
    hex::encode(
        serde_json::to_vec(&Cursor {
            version: 1,
            scope: scope.into(),
            id,
        })
        .expect("cursor"),
    )
}
pub async fn patients(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Query(q): Query<PatientPage>,
) -> Result<Json<Value>> {
    let a = actor(&app, &h).await?;
    let n = limit(q.limit)?;
    let search =
        q.q.unwrap_or_default()
            .trim()
            .to_lowercase()
            .replace('ё', "е");
    if search.len() > 256 {
        return Err(bad());
    }
    let sort = q.sort.as_deref().unwrap_or("name");
    let missing = if q.descending { "" } else { "~" };
    let key = match sort {
        "name" => "translate(lower(concat_ws(' ',p.family_name,p.given_name,p.patronymic)),'АБВГДЕЁЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯ','абвгдеежзийклмнопрстуфхцчшщъыьэюя')".to_string(),
        "birth_date" => format!("coalesce(p.birth_date::text,'{missing}')"),
        "sex" => format!("CASE WHEN p.sex IN ('F','M') THEN p.sex ELSE '{missing}' END"),
        "last_exam_date" => format!("coalesce(last_exam.exam_date,'{missing}')"),
        _ => return Err(bad()),
    };
    let context = scope(&format!(
        "patients:{}:{}:{}:{}:{}",
        a.org, a.user, sort, q.descending, search
    ));
    let after = decode(q.cursor.as_deref(), &context)?;
    // Only constants selected above are interpolated. All user input is bound.
    let direction = if q.descending { "DESC" } else { "ASC" };
    let comparison = if q.descending { "<" } else { ">" };
    let last_exam = r#"LEFT JOIN LATERAL (SELECT max(coalesce(nullif(r.form #>> '{colpo,fields,context.exam_date,value}',''),s.encounter_date::text)) AS exam_date
 FROM app.studies s LEFT JOIN LATERAL (SELECT form FROM app.protocol_revisions WHERE study_id=s.id ORDER BY revision DESC LIMIT 1) r ON true
 WHERE s.patient_id=p.id AND (s.organization_id IS NULL OR s.organization_id=p.organization_id)) last_exam ON true"#;
    // Expensive last-exam projection runs for this page only, unless it is the sort key.
    let grouped_last_exam = r#"LEFT JOIN (
 SELECT s.patient_id,max(coalesce(nullif(r.form #>> '{colpo,fields,context.exam_date,value}',''),s.encounter_date::text)) AS exam_date
 FROM app.studies s JOIN app.patients scoped ON scoped.id=s.patient_id
 LEFT JOIN LATERAL (SELECT form FROM app.protocol_revisions WHERE study_id=s.id ORDER BY revision DESC LIMIT 1) r ON true
 WHERE scoped.organization_id=$1 AND (s.organization_id IS NULL OR s.organization_id=scoped.organization_id)
 GROUP BY s.patient_id
 ) last_exam ON last_exam.patient_id=p.id"#;
    let date_sort = sort == "last_exam_date";
    // A blank-search date sort needs all candidates; aggregate their studies once.
    // Keep the filtered per-patient path for selective searches.
    let key_join = if date_sort && search.is_empty() { grouped_last_exam } else if date_sort { last_exam } else { "" };
    let candidates_mode = if date_sort { "MATERIALIZED" } else { "NOT MATERIALIZED" };
    let candidate_exam = if date_sort { "last_exam.exam_date" } else { "NULL::text" };
    let projection_exam = if date_sort { "page.projected_exam" } else { "last_exam.exam_date" };
    let projection_join = if date_sort { "" } else { last_exam };
    let sql = format!(
        r#"WITH candidates AS {candidates_mode} (
 SELECT p.id, ({key}) COLLATE "C" AS sort_key, {candidate_exam} AS projected_exam FROM app.patients p {key_join}
 WHERE p.organization_id=$1 AND ($2='' OR strpos(translate(lower(concat_ws(' ',p.family_name,p.given_name,p.patronymic,p.birth_date::text,to_char(p.birth_date,'DD.MM.YYYY'))),'АБВГДЕЁЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯё','абвгдеежзийклмнопрстуфхцчшщъыьэюяе'),$2)>0
 OR EXISTS(SELECT 1 FROM app.patient_external_ids e WHERE e.patient_id=p.id AND strpos(lower(e.external_id),$2)>0))
 ), page AS MATERIALIZED (
 SELECT * FROM candidates WHERE $3::uuid IS NULL OR (sort_key,id) {comparison} (SELECT sort_key,id FROM candidates WHERE id=$3)
 ORDER BY sort_key {direction},id {direction} LIMIT $4
 ) SELECT p.*,p.birth_date::text AS birth_date_text,
 (SELECT e.external_id FROM app.patient_external_ids e WHERE e.patient_id=p.id ORDER BY e.source LIMIT 1) AS external_id,
 {projection_exam} AS last_exam_date FROM page JOIN app.patients p ON p.id=page.id {projection_join}
 ORDER BY page.sort_key {direction},page.id {direction}"#
    );
    let rows = sqlx::query(&sql)
        .bind(a.org)
        .bind(search)
        .bind(after)
        .bind(n + 1)
        .fetch_all(&app.db)
        .await?;
    let more = rows.len() > n as usize;
    let rows = &rows[..rows.len().min(n as usize)];
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut p = serde_json::to_value(patient_row(r)).expect("patient");
            p["last_exam_date"] = json!(r.get::<Option<String>, _>("last_exam_date"));
            p
        })
        .collect();
    Ok(Json(
        json!({"items":items,"next_cursor":if more {rows.last().map(|r|encode(r.get("id"),&context))} else {None}}),
    ))
}
pub async fn tasks(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Query(q): Query<TaskPage>,
) -> Result<Json<Value>> {
    let a = actor(&app, &h).await?;
    let n = limit(q.limit)?;
    m53_contracts::colposcopy_1c::CalendarDate::parse(&q.date).map_err(|_| bad())?;
    let context = scope(&format!("tasks:{}:{}:{}", a.org, a.user, q.date));
    let after = decode(q.cursor.as_deref(), &context)?;
    let rows=sqlx::query(r#"SELECT t.id,t.source_id,t.application_key,t.task_name,t.appointment_date::text AS day,t.patient_guid,t.patient_snapshot,t.state,t.assigned_doctor_id,t.study_id,t.row_version,
 CASE WHEN j.state='done' AND j.request_sequence=t.request_sequence AND j.patient_guid=t.patient_guid THEN j.core_patient_snapshot ELSE NULL END AS core_patient
 FROM app.colposcopy_tasks t LEFT JOIN app.task_core_enrichment j ON j.task_id=t.id
 WHERE t.organization_id=$1 AND t.state<>'cancelled' AND t.appointment_date=$3::text::date
 AND (t.assigned_doctor_id IS NULL OR t.assigned_doctor_id=(SELECT doctor_id FROM app.users WHERE id=$2))
 AND ($4::uuid IS NULL OR t.id>$4)
 AND NOT EXISTS(SELECT 1 FROM app.result_delivery_jobs d LEFT JOIN app.reports r ON r.id=d.report_id
 WHERE d.task_id=t.id AND d.state='sent' AND d.sent_at IS NOT NULL AND d.receipt->>'success'='true'
 AND d.receipt->>'application_key'=t.application_key::text
 AND coalesce(d.protocol_revision_id,r.protocol_revision_id)=(SELECT id FROM app.protocol_revisions WHERE study_id=t.study_id ORDER BY revision DESC LIMIT 1))
 ORDER BY t.id LIMIT $5"#).bind(a.org).bind(a.user).bind(q.date).bind(after).bind(n+1).fetch_all(&app.db).await?;
    let more = rows.len() > n as usize;
    let rows = &rows[..rows.len().min(n as usize)];
    let items:Vec<Value>=rows.iter().map(|r|json!({"id":r.get::<Uuid,_>("id"),"source_id":r.get::<Uuid,_>("source_id"),"application_key":r.get::<Uuid,_>("application_key"),"task_name":r.get::<Uuid,_>("task_name"),"appointment_date":r.get::<String,_>("day"),"patient_key":r.get::<Option<Uuid>,_>("patient_guid"),"patient":r.get::<Value,_>("patient_snapshot"),"core_patient":r.get::<Option<Value>,_>("core_patient"),"state":r.get::<String,_>("state"),"assigned_doctor_id":r.get::<Option<Uuid>,_>("assigned_doctor_id"),"study_id":r.get::<Option<Uuid>,_>("study_id"),"row_version":r.get::<i64,_>("row_version")})).collect();
    Ok(Json(
        json!({"items":items,"next_cursor":if more {rows.last().map(|r|encode(r.get("id"),&context))} else {None}}),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_limits() {
        assert_eq!(limit(None).unwrap(), 50);
        for n in [0, -1, 101, i64::MAX] {
            assert!(limit(Some(n)).is_err());
        }
    }
    #[test]
    fn cursor_is_bound_to_query_without_names() {
        let id = Uuid::from_u128(1);
        let s = scope("organization sort query");
        let c = encode(id, &s);
        assert_eq!(decode(Some(&c), &s).unwrap(), Some(id));
        assert!(decode(Some(&c), &scope("other query")).is_err());
        assert!(decode(Some(&"a".repeat(513)), &s).is_err());
        assert!(decode(Some("invalid"), &s).is_err());
    }
}
