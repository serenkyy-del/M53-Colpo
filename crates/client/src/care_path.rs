//! Local, per-study workflow. Completion is explicit and applies to the exact saved inputs.
//! Completing a UI step never approves a protocol or publishes a report.
use m53_contracts::workflow::StudyDetail;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    FinishExamination,
    FinishProtocol,
    SaveAndExit,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub study_id: Uuid,
    pub examination_complete: bool,
    pub protocol_complete: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct Completion {
    study_id: Uuid,
    examination: Option<String>,
    protocol: Option<String>,
}
fn fingerprint(value: Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(m53_contracts::canonical_json::to_vec(&value).unwrap())
    )
}
fn examination(detail: &StudyDetail, journal: &Value) -> String {
    let mut value=json!({
        "originals": detail.assets.iter().filter(|a| a.kind == "original")
            .map(|a| json!({"id":a.id,"sha256":a.sha256})).collect::<Vec<_>>(),
        "annotations": detail.annotations,
        "selected_photos": journal["selected_photos"],
    });
    let mut removed:Vec<_>=journal["native_editor"]["photos"].as_array().into_iter().flatten()
        .filter(|p|p["removed"]==true).filter_map(|p|p["id"].as_str()).collect();
    removed.sort_unstable();
    if !removed.is_empty(){value["removed_photos"]=json!(removed);}
    fingerprint(value)
}
fn protocol(detail: &StudyDetail) -> String {
    fingerprint(json!(detail.protocol))
}
fn record(journal: &Value, study_id: Uuid) -> Completion {
    serde_json::from_value::<Completion>(journal["native_workflow"].clone())
        .ok()
        .filter(|v| v.study_id == study_id)
        .unwrap_or(Completion {
            study_id,
            ..Default::default()
        })
}
pub fn status(detail: &StudyDetail, journal: &Value) -> Status {
    // An already approved revision remains readable without inventing completion for old drafts.
    let approved = detail.study.status == "approved"
        && detail.revision_id.is_some()
        && detail.protocol.is_some();
    let saved = record(journal, detail.study.id);
    let examination_complete =
        approved || saved.examination.as_deref() == Some(examination(detail, journal).as_str());
    let protocol_complete = approved
        || examination_complete
            && detail.protocol.is_some()
            && crate::workspace::check_unfinished_input(journal).is_ok()
            && saved.protocol.as_deref() == Some(protocol(detail).as_str());
    Status {
        study_id: detail.study.id,
        examination_complete,
        protocol_complete,
    }
}
pub fn complete(
    detail: &StudyDetail,
    journal: &mut Value,
    action: Action,
) -> Result<Status, String> {
    let mut saved = record(journal, detail.study.id);
    match action {
        Action::FinishExamination => {
            let current = examination(detail, journal);
            if saved.examination.as_ref() != Some(&current) {
                saved.protocol = None;
            }
            saved.examination = Some(current);
        }
        Action::FinishProtocol => {
            if !status(detail, journal).examination_complete {
                return Err("Сначала завершите этап «Обследование»".into());
            }
            crate::workspace::check_unfinished_input(journal)?;
            if detail.protocol.is_none() {
                return Err("Сначала заполните и сохраните протокол".into());
            }
            saved.protocol = Some(protocol(detail));
        }
        Action::SaveAndExit => return Ok(status(detail, journal)),
    }
    journal["native_workflow"] = json!(saved);
    Ok(status(detail, journal))
}
pub fn require_report(detail: &StudyDetail, journal: &Value) -> Result<(), String> {
    if status(detail, journal).protocol_complete {
        Ok(())
    } else {
        Err("Сначала завершите этапы «Обследование» и «Протокол»".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use m53_contracts::workflow::{Patient, Study};
    fn detail() -> StudyDetail {
        let patient = Uuid::new_v4();
        StudyDetail {
            mis_task_bound: false,
            study: Study {
                id: Uuid::new_v4(),
                patient_id: patient,
                status: "draft".into(),
                row_version: 1,
                study_instance_uid: "2.25.1".into(),
            },
            patient: Patient { card_number: None,
                sex: None,
                id: patient,
                external_id: None,
                family_name: "Тест".into(),
                given_name: "Пациент".into(),
                patronymic: None,
                birth_date: None,
            },
            revision_id: None,
            protocol: Some(crate::model::blank_protocol(Uuid::new_v4())),
            annotations: vec![],
            assets: vec![],
        }
    }
    #[test]
    fn printing_requires_server_approved_revision() {
        let mut d = detail();
        assert_eq!(require_print(&d).unwrap_err(), "Ответ не утверждён");
        d.study.status = "approved".into(); d.revision_id = None;
        assert!(require_print(&d).is_err());
        d.revision_id = Some(Uuid::new_v4()); assert!(require_print(&d).is_ok());
        d.study.status = "draft".into(); assert!(require_print(&d).is_err());
    }
    #[test]
    fn steps_are_explicit_ordered_and_survive_serialization() {
        let d = detail();
        let mut j = json!({});
        assert!(!status(&d, &j).examination_complete);
        assert!(complete(&d, &mut j, Action::FinishProtocol).is_err());
        assert!(require_report(&d, &j).is_err());
        assert!(
            complete(&d, &mut j, Action::FinishExamination)
                .unwrap()
                .examination_complete
        );
        assert!(require_report(&d, &j).is_err());
        complete(&d, &mut j, Action::FinishProtocol).unwrap();
        let restored = serde_json::from_slice(&serde_json::to_vec(&j).unwrap()).unwrap();
        assert!(require_report(&d, &restored).is_ok());
        assert!(!status(&detail(), &restored).examination_complete);
    }
    #[test]
    fn changing_inputs_invalidates_downstream_completion_not_viewing_or_saving() {
        let mut d = detail();
        let mut j = json!({"selected_photos":[]});
        complete(&d, &mut j, Action::FinishExamination).unwrap();
        complete(&d, &mut j, Action::FinishProtocol).unwrap();
        j["native_editor"] = json!({"selected":5,"zoom":2});
        d.study.row_version += 1;
        d.revision_id = Some(Uuid::new_v4());
        assert!(status(&d, &j).protocol_complete);
        d.protocol.as_mut().unwrap().conclusion.value =
            m53_domain::protocol::Assessment::Assessed("Изменено".into());
        assert!(status(&d, &j).examination_complete);
        assert!(!status(&d, &j).protocol_complete);
        complete(&d, &mut j, Action::FinishProtocol).unwrap();
        j["selected_photos"] = json!([Uuid::new_v4()]);
        assert!(!status(&d, &j).examination_complete);
        complete(&d, &mut j, Action::FinishExamination).unwrap();
        assert!(!status(&d, &j).protocol_complete);
    }
    #[test]
    fn unfinished_input_is_retained_and_blocks_report_but_not_save_and_exit() {
        let d = detail();
        let mut j = json!({});
        complete(&d, &mut j, Action::FinishExamination).unwrap();
        j["unfinished_fields"] = json!({"some_date":"не закончено"});
        assert!(complete(&d, &mut j, Action::FinishProtocol).is_err());
        assert!(complete(&d, &mut j, Action::SaveAndExit).is_ok());
        assert_eq!(j["unfinished_fields"]["some_date"], "не закончено");
    }
}

pub fn require_print(detail: &StudyDetail) -> Result<(), String> {
    if detail.study.status != "approved" || detail.revision_id.is_none() || detail.protocol.is_none() { return Err("Ответ не утверждён".into()); }
    Ok(())
}
