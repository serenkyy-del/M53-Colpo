//! Explicit linking of provisional M53 care to an existing, real 1C task.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IdentityLinkPreviewRequest {
    pub source_id: Uuid,
    pub application_key: Uuid,
    pub patient_key: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExpectedPatientVersion {
    pub patient_id: Uuid,
    pub row_version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IdentityLinkRequest {
    pub source_id: Uuid,
    pub application_key: Uuid,
    pub patient_key: Uuid,
    pub expected_study_version: i64,
    pub expected_task_version: i64,
    /// Exactly the original and target patients; one item if they are the same.
    pub expected_patient_versions: Vec<ExpectedPatientVersion>,
    pub reason: String,
}

impl IdentityLinkRequest {
    pub fn preview_request(&self) -> IdentityLinkPreviewRequest {
        IdentityLinkPreviewRequest {
            source_id: self.source_id,
            application_key: self.application_key,
            patient_key: self.patient_key,
        }
    }
}

/// A manual encounter keeps its actual local date when uploaded after an outage.
/// The optional date exists solely to read legacy development requests.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewEncounter {
    pub patient_id: Uuid,
    pub order_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encounter_date: Option<String>,
}
