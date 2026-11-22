use m53_client::{model::blank_protocol, workspace::Workspace};
use m53_contracts::workflow::*;
use m53_domain::protocol::{Assessment, Field, Origin};
use serde_json::json;
use uuid::Uuid;
fn workspace() -> Workspace {
    let patient = Patient { card_number: None,
        sex: None,
        id: Uuid::new_v4(),
        external_id: Some(Uuid::new_v4().to_string()),
        family_name: "Тестовая".into(),
        given_name: "Анна".into(),
        patronymic: None,
        birth_date: None,
    };
    let detail = StudyDetail {
        mis_task_bound: false,
        study: Study {
            id: Uuid::new_v4(),
            patient_id: patient.id,
            status: "draft".into(),
            row_version: 12,
            study_instance_uid: "2.25.12".into(),
        },
        patient,
        revision_id: Some(Uuid::new_v4()),
        protocol: Some(blank_protocol(Uuid::new_v4())),
        annotations: vec![],
        assets: vec![],
    };
    let mut workspace = Workspace::default();
    workspace.detail = Some(detail);
    workspace
}
#[test]
fn web_checkpoint_cannot_change_patient_or_server_metadata() {
    let mut workspace = workspace();
    let original = workspace.detail.clone().unwrap();
    let mut incoming = original.clone();
    incoming.study.id = Uuid::new_v4();
    assert!(workspace.checkpoint(json!({"detail":incoming})).is_err());
    incoming = original.clone();
    incoming.study.status = "approved".into();
    incoming.study.row_version = 999;
    incoming.patient.external_id = Some("forged".into());
    incoming.revision_id = Some(Uuid::new_v4());
    incoming.protocol.as_mut().unwrap().conclusion = Field {
        value: Assessment::Assessed("Локальный текст".into()),
        origin: Origin::Physician,
    };
    workspace.checkpoint(json!({"detail":incoming,"unfinished_fields":{"/some/number":""},"report_template_draft":{"title":"Незавершенный шаблон"}})).unwrap();
    let current = workspace.detail.as_ref().unwrap();
    assert_eq!(current.study.status, "draft");
    assert_eq!(current.study.row_version, 12);
    assert_eq!(current.patient.external_id, original.patient.external_id);
    assert_eq!(current.revision_id, original.revision_id);
    assert!(workspace.dirty);
    assert_eq!(workspace.journal["unfinished_fields"]["/some/number"], "");
    assert_eq!(
        workspace.journal["report_template_draft"]["title"],
        "Незавершенный шаблон"
    );
    assert!(workspace.save().unwrap_err().contains("числовые"));
}
#[test]
fn approved_protocol_remains_immutable_in_web_bridge() {
    let mut workspace = workspace();
    workspace.detail.as_mut().unwrap().study.status = "approved".into();
    let mut incoming = workspace.detail.clone().unwrap();
    incoming.protocol.as_mut().unwrap().conclusion = Field {
        value: Assessment::Assessed("Изменение".into()),
        origin: Origin::Physician,
    };
    assert!(workspace.checkpoint(json!({"detail":incoming})).is_err());
    assert!(!workspace.dirty);
}

#[test]
fn opening_report_for_unchanged_approved_protocol_never_saves_a_revision() {
    let mut workspace = workspace();
    workspace.detail.as_mut().unwrap().study.status = "approved".into();
    let revision = workspace.detail.as_ref().unwrap().revision_id;
    workspace.save().unwrap(); // No API Connection exists: any attempted PUT would fail.
    assert_eq!(workspace.detail.as_ref().unwrap().revision_id, revision);
    assert!(!workspace.dirty);
}
