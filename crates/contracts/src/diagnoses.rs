//! Versioned, paginated source diagnosis catalog. Clinical selections own their snapshots.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Metadata {
    pub version_id: Uuid,
    pub source_id: Uuid,
    pub synced_at: String,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Entry {
    pub guid: Uuid,
    pub code: String,
    pub name: String,
    pub full_name: String,
    pub active: bool,
    pub is_folder: bool,
    pub source_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Page {
    pub version_id: Uuid,
    pub items: Vec<Entry>,
    pub next_after: Option<Uuid>,
}
