//! Server directory pages. `next_after` is only pagination, never a delta token.
use crate::access::DoctorIdentity;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DoctorPage {
    pub items: Vec<DoctorIdentity>,
    pub next_after: Option<Uuid>,
}
