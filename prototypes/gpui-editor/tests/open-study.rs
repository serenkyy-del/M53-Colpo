use m53_client::{
    cache::{AesProtector, Store},
    connection::Connection,
    offline::{Offline, Profile},
    workspace::Workspace,
};
use m53_contracts::workflow::*;
use m53_domain::{protocol::Region, validation::{Dictionary, Term, TermKind}};
use serde_json::json;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;

#[test]
fn invalid_draft_does_not_block_open_and_survives_switching() {
    let root = std::env::temp_dir().join(format!("m53-switch-{}", Uuid::new_v4()));
    let store = Store::new(root.clone(), Arc::new(AesProtector([49; 32]))).unwrap();
    let raw = Connection::new("http://127.0.0.1:9", "synthetic").unwrap();
    let profile = Profile {
        server: raw.base.clone(),
        user_id: Uuid::new_v4(),
        doctor: "Synthetic".into(),
    };
    let offline = Offline::attach(raw.clone(), store.clone(), profile).unwrap();
    let dictionary = Dictionary {
        version_id: Uuid::new_v4(),
        clinically_approved: false,
        // Keep this an unknown-code regression within the dictionary's count
        // bound; excessive lists are covered by review_validation separately.
        terms: vec![Term {
            label: "Synthetic known probe".into(),
            code: "known-test".into(),
            region: Region::Cervix,
            kind: TermKind::Test,
        }],
    };
    let patient = Patient { card_number: None,
        sex: None,
        id: Uuid::new_v4(),
        external_id: None,
        family_name: "Synthetic".into(),
        given_name: "Test".into(),
        patronymic: None,
        birth_date: None,
    };
    let make_detail = || StudyDetail {
        mis_task_bound: false,
        study: Study {
            id: Uuid::new_v4(),
            patient_id: patient.id,
            status: "draft".into(),
            row_version: 1,
            study_instance_uid: Uuid::new_v4().to_string(),
        },
        patient: patient.clone(),
        revision_id: None,
        protocol: Some(m53_client::model::blank_protocol(dictionary.version_id)),
        annotations: vec![],
        assets: vec![],
    };
    let mut first = make_detail();
    let second = make_detail();
    let mut protocol = json!(first.protocol);
    protocol["tests"] = json!([{"region":"cervix","code":"unknown-test","application":{"value":"performed","origin":{"kind":"physician"}},"result":{"value":{"state":"not_assessed"},"origin":{"kind":"physician"}}}]);
    first.protocol = Some(serde_json::from_value(protocol).unwrap());
    {
        let mut state = offline.state.lock().unwrap();
        state.responses.insert(
            format!("/api/v1/studies/{}", second.study.id),
            json!(second),
        );
        state.responses.insert(
            format!("/api/v1/terminology?version_id={}", dictionary.version_id),
            json!(dictionary),
        );
    }
    let mut connection = raw;
    connection.offline = Some(offline.clone());
    let mut workspace = Workspace::default();
    workspace.attach(connection);
    workspace.detail = Some(first.clone());
    workspace.dictionary = Some(dictionary);
    workspace.dirty = true;
    workspace.journal = json!({"unfinished_fields":{"measurement":"unfinished input"}});
    assert!(workspace.save().is_err());
    let unfinished = workspace.journal["unfinished_fields"].take();
    assert!(
        workspace
            .save()
            .unwrap_err()
            .contains("unknown_or_inapplicable_term")
    );
    workspace.journal["unfinished_fields"] = unfinished;
    let open = |workspace: &mut Workspace, id| {
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            match workspace.open(id, None) {
                Ok(()) => break,
                Err(e) => {
                    assert!(e.starts_with("Локальная запись еще не завершена"), "{e}");
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    };
    open(&mut workspace, second.study.id);
    assert_eq!(workspace.detail.as_ref().unwrap().study.id, second.study.id);
    let saved = offline.editor(first.study.id).unwrap();
    assert_eq!(saved["detail"]["protocol"], json!(first.protocol));
    assert_eq!(
        saved["unfinished_fields"]["measurement"],
        "unfinished input"
    );
    open(&mut workspace, first.study.id);
    assert!(workspace.dirty);
    assert_eq!(
        json!(workspace.detail.as_ref().unwrap().protocol),
        json!(first.protocol)
    );
    assert_eq!(
        workspace.journal["unfinished_fields"]["measurement"],
        "unfinished input"
    );
    assert!(offline.state.lock().unwrap().pending.is_empty());
    let deadline = Instant::now() + Duration::from_secs(6);
    while offline.shutdown().is_err() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(workspace);
    drop(offline);
    drop(store);
    std::thread::sleep(Duration::from_millis(150));
    // Leave only an isolated encrypted synthetic fixture if a background reader
    // has not released its handle yet; never remove a user's application cache.
    let _ = std::fs::remove_dir_all(root);
}
