//! 1C web-login input. The workstation credential remains a request header.
//! A profile selection is bound to the server directory version, never to an
//! arbitrary username or upstream URL supplied by the client.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Password-bearing request: deliberately has no `Debug` implementation.
#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OneCLoginRequest {
    pub doctor_id: Uuid,
    pub binding_version: i64,
    pub password: String,
}

impl OneCLoginRequest {
    pub fn valid(&self) -> bool {
        !self.doctor_id.is_nil()
            && self.binding_version > 0
            && self.binding_version < i64::MAX
            && self.password.len() <= 1024
            && !self.password.contains('\0')
    }
}

/// Failures never include the password, username, upstream URL or response body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "code", rename_all = "snake_case", deny_unknown_fields)]
pub enum OneCLoginError {
    OneCLoginDenied,
    OneCLoginUnavailable,
    /// HTTP 2xx from a web page is not evidence of an authenticated 1C user.
    OneCLoginVerificationPending,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn profile_binding_is_required_and_upstream_fields_are_not_client_inputs() {
        let request = json!({"doctor_id":Uuid::from_u128(1),"binding_version":2,"password":""});
        assert!(
            serde_json::from_value::<OneCLoginRequest>(request.clone())
                .unwrap()
                .valid()
        );
        for field in [
            "login",
            "source_id",
            "user_guid",
            "url",
            "role",
            "auth_provider",
        ] {
            let mut changed = request.clone();
            changed[field] = json!("client-selected-value");
            assert!(serde_json::from_value::<OneCLoginRequest>(changed).is_err());
        }
        for (id, version, password) in [
            (Uuid::nil(), 1, String::new()),
            (Uuid::from_u128(1), 0, String::new()),
            (Uuid::from_u128(1), i64::MAX, String::new()),
            (Uuid::from_u128(1), 1, "a".repeat(1025)),
            (Uuid::from_u128(1), 1, "null\0byte".to_owned()),
        ] {
            assert!(
                !OneCLoginRequest {
                    doctor_id: id,
                    binding_version: version,
                    password
                }
                .valid()
            );
        }
    }

    #[test]
    fn failure_contracts_have_codes_and_no_credential_fields() {
        assert_eq!(
            serde_json::to_value(OneCLoginError::OneCLoginDenied).unwrap(),
            json!({"code":"one_c_login_denied"})
        );
        assert_eq!(
            serde_json::to_value(OneCLoginError::OneCLoginUnavailable).unwrap(),
            json!({"code":"one_c_login_unavailable"})
        );
        assert_eq!(
            serde_json::to_value(OneCLoginError::OneCLoginVerificationPending).unwrap(),
            json!({"code":"one_c_login_verification_pending"})
        );
    }
}

/// A successful online check prepares this account's next local entry.
/// Passwords and local password verifiers are never returned by the backend.
#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OneCLoginResponse {
    pub auth_method: String,
    pub session: crate::workflow::Session,
    pub me: crate::access::Me,
    pub offline_grant: crate::access::OfflineGrant,
}
