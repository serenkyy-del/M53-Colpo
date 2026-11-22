use m53_contracts::*;
use m53_domain::validation::ValidationError;
use uuid::Uuid;
#[test]
fn approval_cannot_replace_data_or_forge_actor() {
    let request = ApproveProtocolRequest {
        study_id: Uuid::from_u128(1),
        revision_id: Uuid::from_u128(2),
        expected_version: 1,
    };
    let mut json = serde_json::to_value(&request).unwrap();
    json["approved_by"] = serde_json::json!("00000000-0000-0000-0000-000000000003");
    assert!(serde_json::from_value::<ApproveProtocolRequest>(json).is_err());
}
#[test]
fn errors_are_machine_readable_and_do_not_echo_values() {
    let error = ApiError::Validation {
        errors: vec![ValidationError {
            path: "sections[0].adequacy".into(),
            code: "inadequacy_reason_required".into(),
        }],
    };
    let json = serde_json::to_value(&error).unwrap();
    assert_eq!(json["code"], "validation");
    assert_eq!(serde_json::from_value::<ApiError>(json).unwrap(), error);
}

#[test]
fn save_validates_ids_versions_and_rejects_forged_author() {
    let raw = include_str!("../../../examples/protocol/draft.json");
    let dictionary =
        serde_json::from_str(include_str!("../../../examples/protocol/dictionary.json")).unwrap();
    let mut request: SaveProtocolRequest = serde_json::from_str(raw).unwrap();
    assert!(request.validate(&dictionary).is_empty());
    request.study_id = Uuid::nil();
    request.expected_version = i64::MAX;
    let errors = request.validate(&dictionary);
    assert!(errors.iter().any(|e| e.path == "study_id"));
    assert!(errors.iter().any(|e| e.path == "expected_version"));
    let mut json: serde_json::Value = serde_json::from_str(raw).unwrap();
    json["author_id"] = serde_json::json!(Uuid::from_u128(7));
    assert!(serde_json::from_value::<SaveProtocolRequest>(json).is_err());
}

#[test]
fn approval_requires_saved_revision_and_positive_version() {
    let request = ApproveProtocolRequest {
        study_id: Uuid::nil(),
        revision_id: Uuid::nil(),
        expected_version: 0,
    };
    assert_eq!(request.validate().len(), 3);
}
