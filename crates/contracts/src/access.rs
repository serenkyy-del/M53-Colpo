//! Own M53 identity and signed offline-unlock permission. An offline grant is
//! not a bearer credential for any server endpoint.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const OFFLINE_GRANT_SECONDS: i64 = 30 * 24 * 60 * 60;
pub const OFFLINE_ISSUER: &str = "m53";
pub const OFFLINE_AUDIENCE: &str = "m53-offline-unlock";
pub const OFFLINE_PURPOSE: &str = "offline_unlock";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExternalDoctorId {
    pub source_id: Uuid,
    pub doctor_guid: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DoctorIdentity {
    pub id: Uuid,
    pub display_name: String,
    pub active: bool,
    pub origin: String,
    pub external_ids: Vec<ExternalDoctorId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OfflinePolicy {
    pub enabled: bool,
    pub grant_days: u32,
    pub allow_new_manual_encounter: bool,
    pub allow_continue_open_encounter: bool,
    pub allow_local_print: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Me {
    pub auth_provider: String,
    pub external_user_guid: Option<Uuid>,
    pub user_id: Uuid,
    pub organization_id: Uuid,
    pub display_name: String,
    pub doctor: Option<DoctorIdentity>,
    pub workstation_id: Uuid,
    pub credential_id: Uuid,
    pub auth_version: i64,
    pub roles: Vec<String>,
    /// Unix seconds; dates are UTC, independent of the workstation timezone.
    pub session_expires_at: i64,
    pub server_time: i64,
    pub offline_policy: OfflinePolicy,
}

/// Public JWK; `x` is base64url without padding of the 32-byte Ed25519 public key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OfflinePublicKey {
    pub kid: String,
    pub alg: String,
    pub kty: String,
    pub crv: String,
    pub x: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OfflinePermissions {
    pub allow_new_manual_encounter: bool,
    pub allow_continue_open_encounter: bool,
    pub allow_local_print: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OfflineClaims {
    pub auth_provider: String,
    pub external_user_guid: Option<Uuid>,
    pub schema_version: u32,
    pub iss: String,
    pub aud: String,
    pub purpose: String,
    pub jti: Uuid,
    pub sub: Uuid,
    pub organization_id: Uuid,
    pub doctor_id: Uuid,
    pub workstation_id: Uuid,
    pub credential_id: Uuid,
    pub auth_version: i64,
    pub display_name: String,
    pub external_doctor_ids: Vec<ExternalDoctorId>,
    pub roles: Vec<String>,
    pub permissions: OfflinePermissions,
    pub iat: i64,
    pub nbf: i64,
    pub exp: i64,
}

// Deliberately no Debug: do not accidentally log the signed grant/profile.
#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OfflineGrant {
    pub grant_id: Uuid,
    pub grant: String,
    pub token_type: String,
    pub expires_at: i64,
    /// Convenience metadata, not a trust root: compare with a key obtained from
    /// the configured M53 server over authenticated TLS and kept in protected
    /// local storage. Never trust a replacement key just because it is inline.
    pub key: OfflinePublicKey,
}
