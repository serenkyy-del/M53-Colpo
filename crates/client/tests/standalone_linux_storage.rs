#![cfg(all(target_os = "linux", feature = "standalone"))]

use std::os::unix::fs::PermissionsExt;

#[test]
fn linux_local_store_survives_reopen_with_protected_key_and_ciphertext() {
    let root = std::env::temp_dir().join(format!("colposcopy-linux-test-{}", uuid::Uuid::new_v4()));
    let store = m53_client::linux_storage::open(&root, "account").unwrap();
    let content = b"synthetic workstation key";
    store.write("profile", content).unwrap();
    drop(store);
    let encrypted = std::fs::read(root.join("account/profile.dpapi")).unwrap();
    assert_ne!(encrypted, content);
    assert_eq!(std::fs::metadata(&root).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(std::fs::metadata(root.join("local.key")).unwrap().permissions().mode() & 0o777, 0o600);
    let reopened = m53_client::linux_storage::open(&root, "account").unwrap();
    assert_eq!(reopened.read("profile").unwrap().as_deref(), Some(content.as_slice()));
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}
