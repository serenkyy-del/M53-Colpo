//! Versioned JSON contracts. User identity comes from server authentication.
use m53_domain::protocol::Protocol;
use m53_domain::validation::{Dictionary, ValidationError, ValidationMode, validate};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const API_VERSION: &str = "v1";
pub const PROTOCOL_SCHEMA_VERSION: u32 = m53_domain::protocol::SCHEMA_VERSION;

/// A save request cannot set author, approval actor, or approval timestamp.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveProtocolRequest {
    pub study_id: Uuid,
    pub expected_version: i64,
    pub protocol: Protocol,
}
impl SaveProtocolRequest {
    pub fn validate(&self, dictionary: &Dictionary) -> Vec<ValidationError> {
        let mut errors = validate(&self.protocol, dictionary, ValidationMode::Draft);
        if self.study_id.is_nil() {
            errors.push(ValidationError {
                path: "study_id".into(),
                code: "nil_id".into(),
            });
        }
        if self.expected_version <= 0 || self.expected_version == i64::MAX {
            errors.push(ValidationError {
                path: "expected_version".into(),
                code: "out_of_range".into(),
            });
        }
        errors
    }
}
/// Approval operates on the saved revision; replacement form data is not accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApproveProtocolRequest {
    pub study_id: Uuid,
    pub revision_id: Uuid,
    pub expected_version: i64,
}
impl ApproveProtocolRequest {
    pub fn validate(&self) -> Vec<ValidationError> {
        let mut errors = Vec::new();
        for (path, id) in [
            ("study_id", self.study_id),
            ("revision_id", self.revision_id),
        ] {
            if id.is_nil() {
                errors.push(ValidationError {
                    path: path.into(),
                    code: "nil_id".into(),
                });
            }
        }
        if self.expected_version <= 0 || self.expected_version == i64::MAX {
            errors.push(ValidationError {
                path: "expected_version".into(),
                code: "out_of_range".into(),
            });
        }
        errors
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SavedProtocol {
    pub study_id: Uuid,
    pub revision_id: Uuid,
    pub row_version: i64,
}
/// Machine-readable errors contain paths and codes, never patient values or secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "code", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApiError {
    Validation { errors: Vec<ValidationError> },
    VersionConflict { current_version: i64 },
    Unauthenticated,
    Forbidden,
    NotFound,
    RevisionImmutable,
}

pub mod workflow;

pub mod access;
pub mod canonical_json;
pub mod colposcopy_1c;

pub mod continuity;

pub mod catalog;

pub mod one_c_access;
pub mod diagnoses;
