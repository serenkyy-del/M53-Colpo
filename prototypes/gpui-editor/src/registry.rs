//! Patient-list projections of saved data. Missing dates/authorship are never inferred from login.
use chrono::NaiveDate;
use m53_client::patient_demographics::PatientSex;
use m53_contracts::workflow::{Patient, Study, StudyDetail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use uuid::Uuid;

/// Called only for a newly created study, never to date an existing record on opening.
pub fn initialize_new_study(
    detail: &mut StudyDetail,
    journal: &mut Value,
    today: &str,
    user_id: Option<Uuid>,
    doctor: &str,
) {
    if detail.study.status == "approved" {
        return;
    }
    if let (Some(protocol), Some(today)) = (detail.protocol.as_mut(), iso_date(today)) {
        protocol
            .colpo
            .get_or_insert_with(Default::default)
            .fields
            .entry("context.exam_date".into())
            .or_insert(m53_domain::colpo::Answer::Text(today));
    }
    if !journal["native_study_meta"].is_object() {
        journal["native_study_meta"] =
            serde_json::json!({"doctor_user_id":user_id,"doctor_name":doctor});
    }
}

pub fn iso_date(value: &str) -> Option<String> {
    (value.len() == 10)
        .then(|| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
        .flatten()
        .map(|date| date.format("%Y-%m-%d").to_string())
}
pub fn display_date(value: Option<&str>) -> String {
    value
        .and_then(iso_date)
        .map(|s| format!("{}.{}.{}", &s[8..10], &s[5..7], &s[..4]))
        .unwrap_or_else(|| "Не указана".into())
}
pub fn patient_name(patient: &Patient) -> String {
    [
        &patient.family_name,
        &patient.given_name,
        patient.patronymic.as_deref().unwrap_or(""),
    ]
    .into_iter()
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" ")
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StudySummary {
    #[serde(default)]
    pub row_version: Option<i64>,
    pub exam_date: Option<String>,
    pub doctor_name: Option<String>,
    pub conclusion: Option<String>,
    #[serde(default)]
    pub revision_id: Option<Uuid>,
    #[serde(default)]
    pub mis_delivery: Option<Value>,
}
impl StudySummary {
    /// Server evidence for the latest submitted PDF of this approved revision.
    /// This does not stand in for the report screen's exact preview-byte guard.
    pub fn mis_delivered(&self, study: &Study) -> bool {
        let Some(job)=self.mis_delivery.as_ref() else { return false; };
        let valid_id=|key:&str|job[key].as_str().and_then(|id|Uuid::parse_str(id).ok()).filter(|id|!id.is_nil());
        study.status=="approved" && self.row_version==Some(study.row_version)
            && self.revision_id.is_some_and(|id|!id.is_nil() && valid_id("revision_id")==Some(id))
            && valid_id("job_id").is_some() && valid_id("template_version_id").is_some()
            && job["pdf_sha256"].as_str().is_some_and(|hash|hash.len()==64 && hash.bytes().all(|b|b.is_ascii_hexdigit()))
            && m53_client::delivery::delivered(job)
    }
    /// Lightweight server projection; unavailable on older servers.
    pub fn server_rows(value: &Value) -> BTreeMap<Uuid, Self> {
        value.as_array().into_iter().flatten().filter_map(|row| {
            let id = row["study_id"].as_str()?.parse().ok()?;
            let mut summary: Self = serde_json::from_value(row.clone()).ok()?;
            summary.exam_date = summary.exam_date.as_deref().and_then(iso_date);
            Some((id, summary))
        }).collect()
    }
    pub fn overlay_local(&mut self, detail: &Value, journal: &Value) {
        let local = Self::from_saved(detail, journal);
        self.row_version = local.row_version;
        self.exam_date = local.exam_date;
        self.conclusion = local.conclusion;
        self.revision_id = local.revision_id;
        self.mis_delivery = None;
        if local.doctor_name.is_some() { self.doctor_name = local.doctor_name; }
    }
    /// A save receipt can arrive before the light server projection. Keep its
    /// newer local version visible; an equal/newer remote row wins once clean.
    pub fn needs_overlay(&self, detail: &Value, unsent: bool) -> bool {
        if !detail.is_object() { return false; }
        if unsent { return true; }
        detail["study"]["row_version"].as_i64()
            .is_some_and(|local| self.row_version.is_none_or(|remote| remote < local))
    }
    pub fn from_saved(detail: &Value, journal: &Value) -> Self {
        let text = |v: &Value| {
            v.as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        Self {
            row_version: detail["study"]["row_version"].as_i64(),
            revision_id: detail["revision_id"].as_str().and_then(|id|id.parse().ok()),
            mis_delivery: None,
            exam_date: detail["protocol"]["colpo"]["fields"]["context.exam_date"]["value"]
                .as_str()
                .and_then(iso_date),
            doctor_name: text(&journal["native_study_meta"]["doctor_name"]),
            conclusion: if detail["protocol"]["conclusion"]["value"]["state"] == "assessed" {
                text(&detail["protocol"]["conclusion"]["value"]["value"])
            } else {
                None
            },
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct PatientRow {
    pub id: Uuid,
    pub name: String,
    pub external_id: String,
    pub birth_date: Option<String>,
    pub sex: Option<PatientSex>,
    pub sex_code: Option<m53_contracts::colposcopy_1c::Sex>,
    pub last_exam_date: Option<String>,
}
impl PatientRow {
    pub fn sex_label(&self)->&'static str{m53_client::patient_demographics::display_wire_sex(self.sex_code,self.sex)}
}
pub fn patient_rows(
    patients: &[Patient],
    studies: &[Study],
    summaries: &BTreeMap<Uuid, StudySummary>,
    sexes: &BTreeMap<Uuid, PatientSex>,
    query: &str,
) -> Vec<PatientRow> {
    let query = query.trim().to_lowercase().replace('ё', "е");
    let mut latest = BTreeMap::<Uuid, &str>::new();
    for study in studies.iter().filter(|s| s.status != "cancelled") {
        if let Some(date) = summaries
            .get(&study.id)
            .and_then(|s| s.exam_date.as_deref())
        {
            latest
                .entry(study.patient_id)
                .and_modify(|value| *value = (*value).max(date))
                .or_insert(date);
        }
    }
    patients
        .iter()
        .filter_map(|patient| {
            let name = patient_name(patient);
            let external_id = patient.external_id.clone().unwrap_or_default();
            let birth_date = patient.birth_date.as_deref().and_then(iso_date);
            let searchable = format!(
                "{name} {external_id} {}",
                display_date(birth_date.as_deref())
            )
            .to_lowercase()
            .replace('ё', "е");
            if !searchable.contains(&query) {
                return None;
            }
            let last_exam_date = latest.get(&patient.id).map(|date| (*date).to_owned());
            Some(PatientRow {
                id: patient.id,
                name,
                external_id,
                birth_date,
                sex: sexes.get(&patient.id).copied(),
                sex_code: patient.sex,
                last_exam_date,
            })
        })
        .collect()
}
pub fn sort_patients(rows: &mut [PatientRow], column: usize, descending: bool) {
    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    enum Order {
        Forward(String),
        Backward(std::cmp::Reverse<String>),
        Missing,
    }
    rows.sort_by_cached_key(|row| {
        let name = row.name.to_lowercase().replace('ё', "е");
        let value = match column {
            1 => row.birth_date.clone(),
            2 => (row.sex_label()!="Не указан").then(||row.sex_label().to_owned()),
            3 => row.last_exam_date.clone(),
            _ => Some(name.clone()),
        };
        let value = match value {
            Some(value) if descending => Order::Backward(std::cmp::Reverse(value)),
            Some(value) => Order::Forward(value),
            None => Order::Missing,
        };
        (value, name, row.id)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_keeps_server_delivery_evidence_and_discards_it_for_local_edits() {
        let id=Uuid::new_v4();
        let proof=serde_json::json!({"job_id":Uuid::new_v4(),"revision_id":Uuid::new_v4(),
            "template_version_id":Uuid::new_v4(),"pdf_sha256":"a".repeat(64),"state":"sent","delivered":true});
        let rows=StudySummary::server_rows(&serde_json::json!([{"study_id":id,"row_version":8,
            "exam_date":null,"doctor_name":null,"conclusion":null,"mis_delivery":proof}]));
        let mut summary=rows[&id].clone();
        assert_eq!(serde_json::to_value(&summary).unwrap()["mis_delivery"],proof,"registry lost current delivery evidence");
        summary.overlay_local(&serde_json::json!({"study":{"row_version":8},"protocol":{}}),&serde_json::json!({"dirty":true}));
        assert!(serde_json::to_value(&summary).unwrap()["mis_delivery"].is_null(),"unsent edits retained a delivery badge");
    }
    #[test]
    fn registry_badge_requires_approved_matching_version_and_valid_delivery_evidence() {
        let revision=Uuid::new_v4();
        let study:Study=serde_json::from_value(serde_json::json!({"id":Uuid::new_v4(),"patient_id":Uuid::new_v4(),
            "status":"approved","row_version":8,"study_instance_uid":"synthetic"})).unwrap();
        let proof=serde_json::json!({"job_id":Uuid::new_v4(),"revision_id":revision,"template_version_id":Uuid::new_v4(),
            "pdf_sha256":"a".repeat(64),"state":"sent","delivered":true});
        let summary=StudySummary{row_version:Some(8),revision_id:Some(revision),mis_delivery:Some(proof.clone()),..Default::default()};
        assert!(summary.mis_delivered(&study));
        let mut draft=study.clone();draft.status="draft".into();assert!(!summary.mis_delivered(&draft));
        for (key,value) in [("state",serde_json::json!("pending")),("state",serde_json::json!("retry")),
            ("state",serde_json::json!("blocked")),("delivered",serde_json::json!(false)),
            ("revision_id",serde_json::json!(Uuid::new_v4())),("template_version_id",serde_json::json!(Uuid::nil())),
            ("pdf_sha256",serde_json::json!("invalid"))] {
            let mut invalid=summary.clone();invalid.mis_delivery.as_mut().unwrap()[key]=value;
            assert!(!invalid.mis_delivered(&study),"invalid {key} gave a delivered badge");
        }
        let mut stale=summary.clone();stale.row_version=Some(7);assert!(!stale.mis_delivered(&study));
        assert!(!StudySummary::default().mis_delivered(&study));
    }
    fn projection_detail(row_version: i64) -> Value {
        serde_json::json!({"study":{"row_version":row_version},"protocol":{
            "colpo":{"fields":{"context.exam_date":{"value":"2026-10-21"}}},
            "conclusion":{"value":{"state":"assessed","value":"Новый текст"}}
        }})
    }
    #[test]
    fn a_receipted_newer_local_version_outlives_the_older_server_projection() {
        let mut summary = StudySummary {row_version:Some(7),exam_date:Some("2026-10-20".into()),conclusion:Some("Старый текст".into()),..Default::default()};
        let local = projection_detail(8);
        // The outbox has already received its ACK: dirty=false, pending=0.
        assert!(summary.needs_overlay(&local, false));
        summary.overlay_local(&local, &serde_json::json!({}));
        assert_eq!(summary.row_version, Some(8));
        assert_eq!(summary.exam_date.as_deref(), Some("2026-10-21"));
        assert_eq!(summary.conclusion.as_deref(), Some("Новый текст"));
    }
    #[test]
    fn equal_or_newer_remote_versions_are_authoritative_once_clean() {
        let local = projection_detail(8);
        for row_version in [8,9] {
            let summary = StudySummary {row_version:Some(row_version),conclusion:Some("Серверный текст".into()),..Default::default()};
            assert!(!summary.needs_overlay(&local, false));
            assert_eq!(summary.conclusion.as_deref(), Some("Серверный текст"));
        }
    }
    #[test]
    fn explicit_unsent_clears_override_a_projection_without_refilling_old_text() {
        let mut summary = StudySummary {row_version:Some(9),exam_date:Some("2026-10-20".into()),conclusion:Some("Старый текст".into()),..Default::default()};
        let local = serde_json::json!({"study":{"row_version":8},"protocol":{}});
        assert!(summary.needs_overlay(&local, true));
        summary.overlay_local(&local, &serde_json::json!({"dirty":true}));
        assert_eq!(summary.exam_date, None);
        assert_eq!(summary.conclusion, None);
    }
    #[test]
    fn legacy_summary_without_version_uses_an_available_full_cache_only() {
        let summary = StudySummary::default();
        assert!(summary.needs_overlay(&projection_detail(8), false));
        assert!(!summary.needs_overlay(&Value::Null, false));
        let id = Uuid::new_v4();
        let rows = StudySummary::server_rows(&serde_json::json!([{"study_id":id,"row_version":12,"exam_date":null,"doctor_name":null,"conclusion":null}]));
        assert_eq!(rows[&id].row_version, Some(12));
    }
    #[test]
    fn remote_summary_needs_no_detail_and_unsent_local_clear_wins() {
        let id = Uuid::new_v4();
        let rows = StudySummary::server_rows(&serde_json::json!([{
            "study_id": id, "exam_date":"2026-10-21", "doctor_id":Uuid::new_v4(),
            "doctor_name":"Врач", "conclusion":"Серверный текст"
        }]));
        let mut row = rows[&id].clone();
        assert_eq!(row.exam_date.as_deref(), Some("2026-10-21"));
        assert_eq!(row.doctor_name.as_deref(), Some("Врач"));
        row.overlay_local(&serde_json::json!({"protocol":{}}), &serde_json::json!({"dirty":true}));
        assert_eq!(row.conclusion, None);
        assert_eq!(row.exam_date, None);
        assert_eq!(row.doctor_name.as_deref(), Some("Врач"));
    }
    use serde_json::json;
    #[test]
    fn new_study_date_and_doctor_survive_retry_on_another_day() {
        let mut detail = StudyDetail {
            mis_task_bound: false,
            study: Study {
                id: Uuid::new_v4(),
                patient_id: Uuid::new_v4(),
                status: "draft".into(),
                row_version: 1,
                study_instance_uid: "2.25.1".into(),
            },
            patient: Patient { card_number: None,
                sex: None,
                id: Uuid::new_v4(),
                external_id: None,
                family_name: "Тест".into(),
                given_name: "Пациент".into(),
                patronymic: None,
                birth_date: None,
            },
            revision_id: None,
            protocol: Some(m53_client::model::blank_protocol(Uuid::new_v4())),
            annotations: vec![],
            assets: vec![],
        };
        let mut journal = json!({});
        initialize_new_study(&mut detail, &mut journal, "2026-10-20", None, "Первый врач");
        initialize_new_study(&mut detail, &mut journal, "2026-10-21", None, "Другой врач");
        let summary = StudySummary::from_saved(&json!(detail), &journal);
        assert_eq!(summary.exam_date.as_deref(), Some("2026-10-20"));
        assert_eq!(summary.doctor_name.as_deref(), Some("Первый врач"));
    }
    #[test]
    fn summary_uses_saved_exam_and_author_without_substituting_current_user() {
        let detail = json!({"protocol":{"colpo":{"fields":{"context.exam_date":{"value":"2026-10-19"}}},"conclusion":{"value":{"state":"assessed","value":"Заключение"}}}});
        let summary = StudySummary::from_saved(&detail, &json!({}));
        assert_eq!(summary.exam_date.as_deref(), Some("2026-10-19"));
        assert_eq!(summary.conclusion.as_deref(), Some("Заключение"));
        assert_eq!(summary.doctor_name, None);
        assert_eq!(
            StudySummary::from_saved(
                &detail,
                &json!({"native_study_meta":{"doctor_name":"Врач обследования"}})
            )
            .doctor_name
            .as_deref(),
            Some("Врач обследования")
        );
    }
    #[test]
    fn dates_sort_chronologically_and_missing_stays_last_in_both_directions() {
        let row = |name: &str, date: Option<&str>| PatientRow {
            id: Uuid::new_v4(),
            name: name.into(),
            external_id: String::new(),
            birth_date: None,
            sex: None,
            sex_code: None,
            last_exam_date: date.map(str::to_owned),
        };
        let mut rows = vec![
            row("Без даты", None),
            row("Октябрь", Some("2026-10-01")),
            row("Сентябрь", Some("2026-09-30")),
        ];
        sort_patients(&mut rows, 3, false);
        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["Сентябрь", "Октябрь", "Без даты"]
        );
        sort_patients(&mut rows, 3, true);
        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["Октябрь", "Сентябрь", "Без даты"]
        );
        assert_eq!(display_date(Some("2026-02-30")), "Не указана");
    }
}
