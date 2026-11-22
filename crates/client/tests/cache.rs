use m53_client::{
    cache::{AesProtector, Store},
    connection::Connection,
    offline::{LocalState, Offline, Profile},
    patient_demographics::PatientSex,
};
use m53_contracts::workflow::*;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;
fn wait(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(6);
    while !check() {
        assert!(
            Instant::now() < deadline,
            "Timed out waiting for local durability"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn encrypted_atomic_cache_detects_tampering() {
    let root = std::env::temp_dir().join(format!("m53-cache-{}", Uuid::new_v4()));
    let store = Store::new(root.clone(), Arc::new(AesProtector([7; 32]))).unwrap();
    assert!(Store::new(root.clone(), Arc::new(AesProtector([7; 32]))).is_err());
    let text = b"synthetic confidential local draft";
    store.write("workspace", text).unwrap();
    let encrypted = std::fs::read(root.join("workspace.dpapi")).unwrap();
    assert!(!encrypted.windows(text.len()).any(|w| w == text));
    assert_eq!(store.read("workspace").unwrap().unwrap(), text);
    let mut corrupted = encrypted;
    corrupted[20] ^= 1;
    std::fs::write(root.join("workspace.dpapi"), &corrupted).unwrap();
    assert!(store.read("workspace").is_err());
    assert_eq!(
        std::fs::read(root.join("workspace.dpapi")).unwrap(),
        corrupted
    );
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn offline_actions_return_immediately_and_survive_restart_without_acknowledgement() {
    let root = std::env::temp_dir().join(format!("m53-outbox-{}", Uuid::new_v4()));
    let store = Store::new(root.clone(), Arc::new(AesProtector([8; 32]))).unwrap();
    let mut raw = Connection::new("http://127.0.0.1:9", "synthetic key").unwrap();
    raw.user_id = Some(Uuid::from_u128(1));
    let profile = Profile {
        server: raw.base.clone(),
        user_id: raw.user_id.unwrap(),
        doctor: "Synthetic".into(),
    };
    let offline = Offline::attach(raw.clone(), store.clone(), profile.clone()).unwrap();
    let mut local = raw.clone();
    local.offline = Some(offline.clone());
    let now = Instant::now();
    let patient: Patient = offline
        .create_patient(
            NewPatient {
                family_name: "Synthetic".into(),
                given_name: "Offline".into(),
                patronymic: None,
                birth_date: None,
            },
            Some(PatientSex::Female),
        )
        .unwrap();
    let study: Study = local
        .post(
            "/api/v1/studies",
            &NewStudy {
                patient_id: patient.id,
                order_id: None,
            },
        )
        .unwrap();
    let before = offline.information().1;
    assert!(
        offline
            .mutate(
                &format!("/api/v1/studies/{}/reports", study.id),
                "DELETE",
                serde_json::json!({})
            )
            .is_err()
    );
    assert!(
        offline
            .mutate(
                &format!("/api/v1/studies/{}/reports/{}", study.id, Uuid::new_v4()),
                "DELETE",
                serde_json::json!({})
            )
            .is_err()
    );
    assert_eq!(
        offline.information().1,
        before,
        "report deletion must not enter the outbox"
    );
    assert!(now.elapsed() < Duration::from_millis(300));
    offline.checkpoint(
        serde_json::json!({"detail":null,"advanced":"{ unfinished json and a clinical draft"}),
    );
    let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 6, image::Rgb([21, 45, 71])));
    let asset = local
        .upload(study.id, "original", m53_client::imaging::png(&image).unwrap())
        .unwrap();
    assert_eq!(asset.mime_type, "image/jpeg");
    assert!(local.download_shared(asset.id).unwrap().starts_with(b"\xff\xd8"));
    wait(|| offline.information().2);
    let state: LocalState =
        serde_json::from_slice(&store.read("workspace").unwrap().unwrap()).unwrap();
    assert_eq!(state.pending.len(), 3);
    assert_eq!(
        state.patient_sexes.get(&patient.id),
        Some(&PatientSex::Female)
    );
    // The outbox preserves the recorded demographic value across restarts.
    assert_eq!(state.pending[0].body["sex"], "F");
    assert!(state.confirmed.is_empty());
    assert!(!state.blobs[&asset.id].backend_confirmed);
    assert!(
        store
            .read(&asset.id.simple().to_string())
            .unwrap()
            .is_some()
    );
    wait(|| offline.shutdown().is_ok());
    std::thread::sleep(Duration::from_millis(100));
    let restored = Offline::attach(raw, store.clone(), profile).unwrap();
    assert_eq!(restored.information().1, 3);
    assert_eq!(
        restored.patient_sexes().get(&patient.id),
        Some(&PatientSex::Female)
    );
    // A refreshed patient response omitting sex must not erase the local field.
    restored
        .state
        .lock()
        .unwrap()
        .responses
        .insert("/api/v1/patients".into(), serde_json::json!([patient]));
    assert_eq!(
        restored.patient_sexes().get(&patient.id),
        Some(&PatientSex::Female)
    );
    assert_eq!(
        restored.journal()["advanced"],
        "{ unfinished json and a clinical draft"
    );
    wait(|| restored.information().2);
    wait(|| restored.shutdown().is_ok());
    std::thread::sleep(Duration::from_millis(100));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn closing_study_preserves_incomplete_draft_and_account_without_network() {
    use m53_client::workspace::Workspace;
    let root = std::env::temp_dir().join(format!("m53-close-study-{}", Uuid::new_v4()));
    let store = Store::new(root.clone(), Arc::new(AesProtector([19; 32]))).unwrap();
    let mut raw = Connection::new("http://127.0.0.1:9", "synthetic key").unwrap();
    raw.user_id = Some(Uuid::new_v4());
    let profile = Profile {
        server: raw.base.clone(),
        user_id: raw.user_id.unwrap(),
        doctor: "Synthetic".into(),
    };
    let offline = Offline::attach(raw.clone(), store.clone(), profile.clone()).unwrap();
    let mut connection = raw.clone();
    connection.offline = Some(offline.clone());
    let patient = offline
        .create_patient(
            NewPatient {
                family_name: "Synthetic".into(),
                given_name: "Draft".into(),
                patronymic: None,
                birth_date: None,
            },
            None,
        )
        .unwrap();
    let study: Study = connection
        .post(
            "/api/v1/studies",
            &NewStudy {
                patient_id: patient.id,
                order_id: None,
            },
        )
        .unwrap();
    let mut workspace = Workspace::default();
    workspace.attach(connection);
    workspace.detail = Some(
        serde_json::from_value(
            offline
                .cached(&format!("/api/v1/studies/{}", study.id))
                .unwrap(),
        )
        .unwrap(),
    );
    workspace.journal = serde_json::json!({"unfinished_fields":{"cycle":"не закончено"}});
    workspace.persist();
    wait(|| workspace.close_study().is_ok());
    assert!(workspace.detail.is_none());
    assert!(
        workspace.connection.is_some(),
        "closing a study must not log out the physician"
    );
    assert_eq!(
        offline.editor(study.id).unwrap()["unfinished_fields"]["cycle"],
        "не закончено"
    );
    wait(|| offline.information().2);
    wait(|| offline.shutdown().is_ok());
    std::thread::sleep(Duration::from_millis(100));
    let restored = Offline::attach(raw, store, profile).unwrap();
    assert_eq!(
        restored.editor(study.id).unwrap()["unfinished_fields"]["cycle"],
        "не закончено"
    );
    assert_eq!(
        restored.information().1,
        2,
        "unconfirmed patient/study operations stay queued"
    );
    wait(|| restored.shutdown().is_ok());
    std::thread::sleep(Duration::from_millis(100));
    std::fs::remove_dir_all(root).unwrap();
}
