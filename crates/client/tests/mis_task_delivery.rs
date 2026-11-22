use m53_client::{cache::{AesProtector, Store}, connection::Connection, offline::{Offline, Profile}};
use m53_contracts::workflow::StudyDetail;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

#[test]
fn manual_or_legacy_study_cannot_create_a_mis_upload_but_can_keep_a_local_pdf() {
    let root = std::env::temp_dir().join(format!("m53-mis-binding-{}", Uuid::new_v4()));
    let raw = Connection::new("https://127.0.0.1:9", "synthetic-key").unwrap();
    let offline = Offline::attach(raw.clone(), Store::new(root.clone(), Arc::new(AesProtector([17;32]))).unwrap(),
        Profile {server:raw.base.clone(),user_id:Uuid::new_v4(),doctor:"Synthetic".into()}).unwrap();
    let value = json!({"study":{"id":Uuid::new_v4(),"patient_id":Uuid::new_v4(),"status":"approved","row_version":3,"study_instance_uid":"synthetic"},
        "patient":{"id":Uuid::new_v4(),"family_name":"Synthetic","given_name":"Patient","patronymic":null,"birth_date":null},
        "revision_id":Uuid::new_v4(),"protocol":null,"annotations":[],"assets":[]});
    let template=Uuid::new_v4();
    let manual:StudyDetail=serde_json::from_value(value.clone()).unwrap();
    let error=offline.enqueue_report(&manual,template,"synthetic",b"%PDF-1.7 synthetic").expect_err("manual study created a MIS upload");
    assert!(error.contains("задани"),"binding error should explain the missing MIS task: {error}");
    assert!(offline.state.lock().unwrap().pending.is_empty());
    offline.store_report(&manual,template,"synthetic",b"%PDF-1.7 local only").unwrap();
    let mut bound=value; bound["mis_task_bound"]=json!(true);
    let bound:StudyDetail=serde_json::from_value(bound).unwrap();
    offline.enqueue_report(&bound,template,"synthetic",b"%PDF-1.7 linked task").unwrap();
    assert_eq!(offline.state.lock().unwrap().pending.len(),1);
    let until=std::time::Instant::now()+std::time::Duration::from_secs(5);
    while offline.shutdown().is_err() {
        assert!(std::time::Instant::now()<until,"temporary workers did not stop");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    drop(offline);
    assert_eq!(root.parent(),Some(std::env::temp_dir().as_path()));
    std::fs::remove_dir_all(root).unwrap();
}
