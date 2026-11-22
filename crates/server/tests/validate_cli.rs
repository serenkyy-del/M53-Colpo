use std::process::Command;
fn command() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_m53-server"));
    cmd.current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    cmd.args([
        "validate",
        "examples/protocol/draft.json",
        "examples/protocol/dictionary.json",
    ]);
    cmd
}
#[test]
fn draft_passes_and_approval_rejects_unreviewed_dictionary() {
    let draft = command().output().unwrap();
    assert!(draft.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&draft.stdout).unwrap()["valid"],
        true
    );
    let approval = command().arg("--approval").output().unwrap();
    assert_eq!(approval.status.code(), Some(1));
    let error: m53_contracts::ApiError = serde_json::from_slice(&approval.stdout).unwrap();
    assert!(
        matches!(error, m53_contracts::ApiError::Validation { errors } if errors.iter().any(|e| e.code=="dictionary_not_approved"))
    );
}
