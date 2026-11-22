use m53_client::{
    cache::{AesProtector, Store},
    connection::Connection,
    offline::{Offline, Profile},
};
use serde_json::json;
use std::{sync::Arc, thread, time::{Duration, Instant}};
use uuid::Uuid;

fn recovers_derived_cache(damage: &str) {
    let root = std::env::temp_dir().join(format!("m53-render-cache-test-{}", Uuid::new_v4()));
    let store = Store::new(root.clone(), Arc::new(AesProtector([23; 32]))).unwrap();
    let raw = Connection::new("https://127.0.0.1:9", "synthetic-key").unwrap();
    let offline = Offline::attach(
        raw.clone(),
        store.clone(),
        Profile {
            server: raw.base.clone(),
            user_id: Uuid::new_v4(),
            doctor: "Synthetic".into(),
        },
    )
    .unwrap();
    let original = Uuid::new_v4();
    store
        .write(&original.simple().to_string(), b"immutable approved PDF")
        .unwrap();
    let archive = json!({"file_id": original, "sha256": "immutable-hash"});
    {
        let mut state = offline.state.lock().unwrap();
        state
            .responses
            .insert("local-pdf/immutable".into(), archive.clone());
        state
            .responses
            .insert("receipt/immutable".into(), json!({"accepted": true}));
    }
    offline.cache_render("damaged", b"derived PDF").unwrap();
    offline.cache_render("healthy", b"healthy PDF").unwrap();
    let id = offline.state.lock().unwrap().responses["render/damaged"]["file_id"]
        .as_str()
        .unwrap()
        .parse::<Uuid>()
        .unwrap();
    let path = root.join(format!("{}.dpapi", id.simple()));
    match damage {
        "missing" => std::fs::remove_file(path).unwrap(),
        "ciphertext" => std::fs::write(path, b"broken ciphertext").unwrap(),
        "hash" => store
            .write(&id.simple().to_string(), b"different plaintext")
            .unwrap(),
        "index" => {
            offline
                .state
                .lock()
                .unwrap()
                .responses
                .get_mut("render/damaged")
                .unwrap()["file_id"] = json!("invalid");
        }
        _ => unreachable!(),
    }
    assert_eq!(offline.cached_render("damaged"), Ok(None));
    assert!(
        !offline
            .state
            .lock()
            .unwrap()
            .responses
            .contains_key("render/damaged")
    );
    assert_eq!(
        offline.cached_render("healthy").unwrap(),
        Some(b"healthy PDF".to_vec())
    );
    assert_eq!(
        offline.state.lock().unwrap().responses["local-pdf/immutable"],
        archive
    );
    assert_eq!(
        offline.state.lock().unwrap().responses["receipt/immutable"],
        json!({"accepted":true})
    );
    assert_eq!(
        store.read(&original.simple().to_string()).unwrap().unwrap(),
        b"immutable approved PDF"
    );
    let persisted: serde_json::Value =
        serde_json::from_slice(&store.read("workspace").unwrap().unwrap()).unwrap();
    assert!(persisted["responses"].get("render/damaged").is_none());
    assert_eq!(persisted["responses"]["local-pdf/immutable"], archive);
    offline.cache_render("damaged", b"rebuilt PDF").unwrap();
    assert_eq!(
        offline.cached_render("damaged").unwrap(),
        Some(b"rebuilt PDF".to_vec())
    );
    let until = Instant::now() + Duration::from_secs(3);
    while offline.shutdown().is_err() {
        assert!(Instant::now() < until, "temporary cache workers did not shut down");
        thread::sleep(Duration::from_millis(10));
    }
    drop(offline);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn missing_derived_pdf_is_a_rebuildable_cache_miss() {
    recovers_derived_cache("missing");
}
#[test]
fn corrupt_derived_pdf_is_a_rebuildable_cache_miss() {
    recovers_derived_cache("ciphertext");
}
#[test]
fn mismatched_derived_pdf_hash_is_a_rebuildable_cache_miss() {
    recovers_derived_cache("hash");
}
#[test]
fn corrupt_derived_pdf_index_is_a_rebuildable_cache_miss() {
    recovers_derived_cache("index");
}
