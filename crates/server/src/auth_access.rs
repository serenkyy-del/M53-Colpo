//! M53 identity and a separate, signed 30-day offline-unlock permission.
//! JWS grants are never accepted as API bearer sessions. The Windows client
//! still needs its own DPAPI-protected password verifier and prepared resources.
use crate::api::{App, Error, actor, hash};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use m53_contracts::access::{
    DoctorIdentity, ExternalDoctorId, Me, OFFLINE_AUDIENCE, OFFLINE_GRANT_SECONDS, OFFLINE_ISSUER,
    OFFLINE_PURPOSE, OfflineClaims, OfflineGrant, OfflinePermissions, OfflinePolicy,
    OfflinePublicKey,
};
use ring::{
    rand::SystemRandom,
    signature::{self, Ed25519KeyPair, KeyPair},
};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Row, Transaction};
use std::sync::Arc;
use uuid::Uuid;

type Result<T> = std::result::Result<T, Error>;
const JWS_TYPE: &str = "m53-offline+jwt";
const MAX_GRANT_BYTES: usize = 32 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProtectedHeader {
    alg: String,
    typ: String,
    kid: String,
}

struct SigningKey {
    pair: Ed25519KeyPair,
    public: OfflinePublicKey,
}

impl SigningKey {
    fn from_pkcs8(bytes: &[u8]) -> std::result::Result<Self, &'static str> {
        let pair = Ed25519KeyPair::from_pkcs8(bytes).map_err(|_| "offline_signing_key_invalid")?;
        let public = OfflinePublicKey {
            kid: hash(pair.public_key().as_ref()),
            alg: "EdDSA".into(),
            kty: "OKP".into(),
            crv: "Ed25519".into(),
            x: URL_SAFE_NO_PAD.encode(pair.public_key().as_ref()),
        };
        Ok(Self { pair, public })
    }

    async fn load() -> Result<Self> {
        let path = std::env::var_os("M53_OFFLINE_SIGNING_KEY_FILE").ok_or(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "offline_signing_key_not_configured",
        ))?;
        let metadata = tokio::fs::metadata(&path).await.map_err(|_| {
            Error(
                StatusCode::SERVICE_UNAVAILABLE,
                "offline_signing_key_unavailable",
            )
        })?;
        if !metadata.is_file() || metadata.len() > 16 * 1024 {
            return Err(Error(
                StatusCode::SERVICE_UNAVAILABLE,
                "offline_signing_key_invalid",
            ));
        }
        let bytes = tokio::fs::read(path).await.map_err(|_| {
            Error(
                StatusCode::SERVICE_UNAVAILABLE,
                "offline_signing_key_unavailable",
            )
        })?;
        Self::from_pkcs8(&bytes).map_err(|code| Error(StatusCode::SERVICE_UNAVAILABLE, code))
    }

    fn sign(&self, claims: &OfflineClaims) -> std::result::Result<String, &'static str> {
        let header = ProtectedHeader {
            alg: "EdDSA".into(),
            typ: JWS_TYPE.into(),
            kid: self.public.kid.clone(),
        };
        let header = serde_json::to_vec(&header).map_err(|_| "offline_grant_encoding_failed")?;
        let claims = serde_json::to_vec(claims).map_err(|_| "offline_grant_encoding_failed")?;
        let message = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header),
            URL_SAFE_NO_PAD.encode(claims)
        );
        let token = format!(
            "{message}.{}",
            URL_SAFE_NO_PAD.encode(self.pair.sign(message.as_bytes()).as_ref())
        );
        if token.len() > MAX_GRANT_BYTES {
            return Err("offline_grant_too_large");
        }
        Ok(token)
    }
}

/// Generates binary PKCS#8 for an explicit administrator command. This helper
/// does not save it, print it or replace any existing key.
pub fn generate_signing_key() -> std::result::Result<Vec<u8>, &'static str> {
    Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map(|document| document.as_ref().to_vec())
        .map_err(|_| "offline_signing_key_generation_failed")
}

fn policy_values(days: Option<&str>, new_care: Option<&str>) -> OfflinePolicy {
    let enabled = days == Some("30") && new_care == Some("1");
    OfflinePolicy {
        enabled,
        grant_days: 30,
        allow_new_manual_encounter: enabled,
        allow_continue_open_encounter: enabled,
        allow_local_print: enabled,
    }
}

fn policy() -> OfflinePolicy {
    policy_values(
        std::env::var("M53_OFFLINE_GRANT_DAYS").ok().as_deref(),
        std::env::var("M53_OFFLINE_ALLOW_NEW_ENCOUNTERS")
            .ok()
            .as_deref(),
    )
}

async fn context(
    tx: &mut Transaction<'_, Postgres>,
    user: Uuid,
    org: Uuid,
    credential: Uuid,
) -> Result<Me> {
    // This is also used after taking the user lock for grant issuance. Role,
    // profile and workstation authority come from the database, not the client.
    let row = sqlx::query("SELECT u.display_name,u.doctor_id,u.auth_version,u.auth_provider,u.external_user_guid,c.workstation_id,floor(extract(epoch from clock_timestamp()))::bigint AS server_time FROM app.users u JOIN app.api_credentials c ON c.id=$3 JOIN app.workstations w ON w.id=c.workstation_id AND w.organization_id=u.organization_id WHERE u.id=$1 AND u.organization_id=$2 AND NOT u.disabled AND c.purpose='client' AND c.revoked_at IS NULL AND (c.expires_at IS NULL OR c.expires_at>clock_timestamp()) AND 'clinical'=ANY(c.permissions) AND EXISTS(SELECT 1 FROM app.user_roles r WHERE r.user_id=u.id AND r.role_code='doctor')")
        .bind(user).bind(org).bind(credential).fetch_optional(&mut **tx).await?
        .ok_or(Error(StatusCode::UNAUTHORIZED,"invalid_user_session"))?;
    let roles: Vec<String> = sqlx::query_scalar(
        "SELECT role_code FROM app.user_roles WHERE user_id=$1 ORDER BY role_code",
    )
    .bind(user)
    .fetch_all(&mut **tx)
    .await?;
    let doctor = if let Some(doctor_id) = row.get::<Option<Uuid>, _>("doctor_id") {
        let doctor = sqlx::query("SELECT id,display_name,active,origin FROM app.doctors WHERE id=$1 AND organization_id=$2")
            .bind(doctor_id).bind(org).fetch_optional(&mut **tx).await?
            .ok_or(Error(StatusCode::FORBIDDEN,"doctor_profile_required"))?;
        if !doctor.get::<bool, _>("active") {
            return Err(Error(StatusCode::FORBIDDEN, "doctor_inactive"));
        }
        let links = sqlx::query("SELECT source_id,external_guid FROM app.doctor_external_ids WHERE doctor_id=$1 AND organization_id=$2 ORDER BY source_id,external_guid")
            .bind(doctor_id).bind(org).fetch_all(&mut **tx).await?;
        Some(DoctorIdentity {
            id: doctor_id,
            display_name: doctor.get("display_name"),
            active: true,
            origin: doctor.get("origin"),
            external_ids: links
                .iter()
                .map(|link| ExternalDoctorId {
                    source_id: link.get("source_id"),
                    doctor_guid: link.get("external_guid"),
                })
                .collect(),
        })
    } else {
        None
    };
    Ok(Me {
        auth_provider: row.get("auth_provider"),
        external_user_guid: row.get("external_user_guid"),
        user_id: user,
        organization_id: org,
        display_name: row.get("display_name"),
        doctor,
        workstation_id: row.get("workstation_id"),
        credential_id: credential,
        auth_version: row.get("auth_version"),
        roles,
        // Filled from the particular bearer session by the handler below.
        session_expires_at: 0,
        server_time: row.get("server_time"),
        offline_policy: policy(),
    })
}

fn bearer_hash(headers: &HeaderMap) -> Result<String> {
    let token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .filter(|token| !token.is_empty())
        .ok_or(Error(StatusCode::UNAUTHORIZED, "user_session_required"))?;
    Ok(hash(token))
}

async fn current_session(
    tx: &mut Transaction<'_, Postgres>,
    headers: &HeaderMap,
    user: Uuid,
    credential: Uuid,
) -> Result<i64> {
    sqlx::query_scalar("SELECT floor(extract(epoch from expires_at))::bigint FROM app.user_sessions WHERE token_hash=$1 AND user_id=$2 AND credential_id=$3 AND revoked_at IS NULL AND expires_at>clock_timestamp()")
        .bind(bearer_hash(headers)?).bind(user).bind(credential).fetch_optional(&mut **tx).await?
        .ok_or(Error(StatusCode::UNAUTHORIZED,"invalid_user_session"))
}

pub async fn me(State(app): State<Arc<App>>, headers: HeaderMap) -> Result<Json<Me>> {
    let actor = actor(&app, &headers).await?;
    let mut tx = app.db.begin().await?;
    sqlx::query("SELECT id FROM app.users WHERE id=$1 FOR SHARE")
        .bind(actor.user)
        .fetch_one(&mut *tx)
        .await?;
    let mut result = context(&mut tx, actor.user, actor.org, actor.credential).await?;
    result.session_expires_at =
        current_session(&mut tx, &headers, actor.user, actor.credential).await?;
    tx.commit().await?;
    Ok(Json(result))
}

/// Public verification metadata contains no physician or workstation data.
pub async fn offline_key() -> Result<Json<OfflinePublicKey>> {
    Ok(Json(SigningKey::load().await?.public))
}

pub async fn offline_grant(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> Result<Json<OfflineGrant>> {
    let actor = actor(&app, &headers).await?;
    let policy = policy();
    if !policy.enabled {
        return Err(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "offline_policy_not_configured",
        ));
    }
    let signing = SigningKey::load().await?;
    let mut tx = app.db.begin().await?;
    // Same credential -> user -> workstation -> session order as clinical writes.
    // Keep the existing exclusive user lock used by concurrent login/grant issuance.
    crate::api::revalidate_grant_access(&mut tx, &actor).await?;
    let current = context(&mut tx, actor.user, actor.org, actor.credential).await?;
    let _ = current_session(&mut tx, &headers, actor.user, actor.credential).await?;
    if !cfg!(feature="standalone") && std::env::var("M53_AUTH_PROVIDER").as_deref() == Ok("1c_web") {
        let fresh:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app.user_sessions WHERE token_hash=$1 AND user_id=$2 AND credential_id=$3 AND auth_provider='1c_web' AND external_verified_at>clock_timestamp()-interval '5 minutes' AND revoked_at IS NULL AND expires_at>clock_timestamp())")
            .bind(bearer_hash(&headers)?).bind(actor.user).bind(actor.credential).fetch_one(&mut *tx).await?;
        if !fresh {
            return Err(Error(StatusCode::UNAUTHORIZED, "fresh_1c_login_required"));
        }
    }
    let doctor = current
        .doctor
        .ok_or(Error(StatusCode::FORBIDDEN, "doctor_profile_required"))?;
    let id = Uuid::new_v4();
    let claims = OfflineClaims {
        auth_provider: current.auth_provider,
        external_user_guid: current.external_user_guid,
        schema_version: 1,
        iss: OFFLINE_ISSUER.into(),
        aud: OFFLINE_AUDIENCE.into(),
        purpose: OFFLINE_PURPOSE.into(),
        jti: id,
        sub: current.user_id,
        organization_id: current.organization_id,
        doctor_id: doctor.id,
        workstation_id: current.workstation_id,
        credential_id: current.credential_id,
        auth_version: current.auth_version,
        display_name: doctor.display_name,
        external_doctor_ids: doctor.external_ids,
        roles: current.roles,
        permissions: OfflinePermissions {
            allow_new_manual_encounter: true,
            allow_continue_open_encounter: true,
            allow_local_print: true,
        },
        iat: current.server_time,
        nbf: current.server_time,
        exp: current
            .server_time
            .checked_add(OFFLINE_GRANT_SECONDS)
            .ok_or(Error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "offline_grant_time_invalid",
            ))?,
    };
    let token = signing
        .sign(&claims)
        .map_err(|code| Error(StatusCode::INTERNAL_SERVER_ERROR, code))?;
    sqlx::query("INSERT INTO app.offline_access_grants(id,organization_id,user_id,doctor_id,workstation_id,credential_id,auth_version,key_id,token_sha256,issued_at,expires_at,allow_new_manual_encounter,allow_continue_open_encounter,allow_local_print) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,to_timestamp($10::double precision),to_timestamp($11::double precision),true,true,true)")
        .bind(id).bind(claims.organization_id).bind(claims.sub).bind(claims.doctor_id)
        .bind(claims.workstation_id).bind(claims.credential_id).bind(claims.auth_version)
        .bind(&signing.public.kid).bind(hash(&token)).bind(claims.iat as f64).bind(claims.exp as f64)
        .execute(&mut *tx).await?;
    sqlx::query("INSERT INTO app.audit_events(id,actor_id,action,entity_id,entity_version,safe_description,credential_id) VALUES($1,$2,'offline_grant.issued',$3,$4,'',$5)")
        .bind(Uuid::new_v4()).bind(claims.sub).bind(id).bind(claims.auth_version).bind(claims.credential_id).execute(&mut *tx).await?;
    crate::api::revalidate_write_access(&mut tx, &actor).await?;
    tx.commit().await?;
    Ok(Json(OfflineGrant {
        grant_id: id,
        grant: token,
        token_type: "offline_unlock".into(),
        expires_at: claims.exp,
        key: signing.public,
    }))
}

/// Verify the offline permission in its own context. This is deliberately not
/// wired into API `actor`: possession of a valid grant does not log in online.
/// `key` must be trusted metadata from the configured HTTPS server, not an
/// untrusted JWK supplied beside a token. `now` must obey the client's persisted
/// server-time/clock-rollback checks; an offline wall clock is not a server check.
pub fn verify_offline_grant(
    token: &str,
    key: &OfflinePublicKey,
    expected: &OfflineBinding,
    now: i64,
) -> std::result::Result<OfflineClaims, &'static str> {
    if token.len() > MAX_GRANT_BYTES
        || key.alg != "EdDSA"
        || key.kty != "OKP"
        || key.crv != "Ed25519"
    {
        return Err("offline_grant_invalid");
    }
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 3 {
        return Err("offline_grant_invalid");
    }
    let public = URL_SAFE_NO_PAD
        .decode(&key.x)
        .map_err(|_| "offline_key_invalid")?;
    if public.len() != 32 || hash(&public) != key.kid {
        return Err("offline_key_invalid");
    }
    let header: ProtectedHeader = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(parts[0])
            .map_err(|_| "offline_grant_invalid")?,
    )
    .map_err(|_| "offline_grant_invalid")?;
    if header.alg != "EdDSA" || header.typ != JWS_TYPE || header.kid != key.kid {
        return Err("offline_grant_invalid");
    }
    let signature = URL_SAFE_NO_PAD
        .decode(parts[2])
        .map_err(|_| "offline_grant_invalid")?;
    signature::UnparsedPublicKey::new(&signature::ED25519, &public)
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .map_err(|_| "offline_grant_invalid")?;
    let claims: OfflineClaims = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(parts[1])
            .map_err(|_| "offline_grant_invalid")?,
    )
    .map_err(|_| "offline_grant_invalid")?;
    if claims.schema_version != 1
        || claims.auth_provider != expected.auth_provider
        || claims.external_user_guid != expected.external_user_guid
        || !matches!(claims.auth_provider.as_str(), "m53" | "1c_web")
        || (claims.auth_provider == "1c_web"
            && claims.external_user_guid.is_none_or(|id| id.is_nil()))
        || claims.iss != OFFLINE_ISSUER
        || claims.aud != OFFLINE_AUDIENCE
        || claims.purpose != OFFLINE_PURPOSE
        || claims.jti.is_nil()
        || claims.sub.is_nil()
        || claims.doctor_id.is_nil()
        || claims.organization_id.is_nil()
        || claims.workstation_id.is_nil()
        || claims.credential_id.is_nil()
        || claims.auth_version <= 0
        || claims.iat < 0
        || claims.nbf != claims.iat
        || claims.exp.checked_sub(claims.iat) != Some(OFFLINE_GRANT_SECONDS)
        || now < claims.nbf
        || now >= claims.exp
        || claims.sub != expected.user_id
        || claims.organization_id != expected.organization_id
        || claims.doctor_id != expected.doctor_id
        || claims.workstation_id != expected.workstation_id
        || claims.credential_id != expected.credential_id
        || claims.auth_version != expected.auth_version
        || !claims.roles.iter().any(|role| role == "doctor")
        || !claims.permissions.allow_new_manual_encounter
        || !claims.permissions.allow_continue_open_encounter
        || !claims.permissions.allow_local_print
        || claims
            .external_doctor_ids
            .iter()
            .any(|identity| identity.source_id.is_nil() || identity.doctor_guid.is_nil())
    {
        return Err("offline_grant_invalid");
    }
    Ok(claims)
}

#[derive(Clone)]
pub struct OfflineBinding {
    pub auth_provider: String,
    pub external_user_guid: Option<Uuid>,
    pub user_id: Uuid,
    pub organization_id: Uuid,
    pub doctor_id: Uuid,
    pub workstation_id: Uuid,
    pub credential_id: Uuid,
    pub auth_version: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (SigningKey, OfflineClaims, OfflineBinding) {
        let key = SigningKey::from_pkcs8(&generate_signing_key().unwrap()).unwrap();
        let binding = OfflineBinding {
            auth_provider: "m53".into(),
            external_user_guid: None,
            user_id: Uuid::from_u128(1),
            organization_id: Uuid::from_u128(2),
            doctor_id: Uuid::from_u128(3),
            workstation_id: Uuid::from_u128(4),
            credential_id: Uuid::from_u128(5),
            auth_version: 7,
        };
        let claims = OfflineClaims {
            auth_provider: binding.auth_provider.clone(),
            external_user_guid: binding.external_user_guid,
            schema_version: 1,
            iss: OFFLINE_ISSUER.into(),
            aud: OFFLINE_AUDIENCE.into(),
            purpose: OFFLINE_PURPOSE.into(),
            jti: Uuid::new_v4(),
            sub: binding.user_id,
            organization_id: binding.organization_id,
            doctor_id: binding.doctor_id,
            workstation_id: binding.workstation_id,
            credential_id: binding.credential_id,
            auth_version: binding.auth_version,
            display_name: "Synthetic offline doctor".into(),
            external_doctor_ids: vec![ExternalDoctorId {
                source_id: Uuid::from_u128(6),
                doctor_guid: Uuid::from_u128(7),
            }],
            roles: vec!["doctor".into()],
            permissions: OfflinePermissions {
                allow_new_manual_encounter: true,
                allow_continue_open_encounter: true,
                allow_local_print: true,
            },
            iat: 1_700_000_000,
            nbf: 1_700_000_000,
            exp: 1_700_000_000 + OFFLINE_GRANT_SECONDS,
        };
        (key, claims, binding)
    }

    #[test]
    fn signed_grant_obeys_exact_thirty_day_boundaries_and_bindings() {
        let (key, claims, binding) = fixture();
        let grant = key.sign(&claims).unwrap();
        assert_eq!(
            verify_offline_grant(&grant, &key.public, &binding, claims.iat).unwrap(),
            claims
        );
        assert!(verify_offline_grant(&grant, &key.public, &binding, claims.exp - 1).is_ok());
        assert!(verify_offline_grant(&grant, &key.public, &binding, claims.exp).is_err());
        assert!(verify_offline_grant(&grant, &key.public, &binding, claims.iat - 1).is_err());
        let mut other = binding.clone();
        other.credential_id = Uuid::new_v4();
        assert!(verify_offline_grant(&grant, &key.public, &other, claims.iat).is_err());
        other = binding;
        other.auth_version += 1;
        assert!(verify_offline_grant(&grant, &key.public, &other, claims.iat).is_err());
    }

    #[test]
    fn tampering_wrong_purpose_and_algorithm_confusion_are_rejected() {
        let (key, mut claims, binding) = fixture();
        let token = key.sign(&claims).unwrap();
        let mut parts: Vec<String> = token.split('.').map(str::to_owned).collect();
        let mut forged = claims.clone();
        forged.display_name = "Replaced synthetic doctor name".into();
        parts[1] = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&forged).unwrap());
        assert!(verify_offline_grant(&parts.join("."), &key.public, &binding, claims.iat).is_err());
        parts = token.split('.').map(str::to_owned).collect();
        parts[0] = URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\",\"typ\":\"JWT\",\"kid\":\"none\"}");
        assert!(verify_offline_grant(&parts.join("."), &key.public, &binding, claims.iat).is_err());
        claims.purpose = "server_bearer".into();
        assert!(
            verify_offline_grant(
                &key.sign(&claims).unwrap(),
                &key.public,
                &binding,
                claims.iat
            )
            .is_err()
        );
        let (other, _, _) = fixture();
        assert!(verify_offline_grant(&token, &other.public, &binding, claims.iat).is_err());
    }

    #[test]
    fn public_metadata_and_claims_contain_no_server_or_password_secrets() {
        let (key, claims, _) = fixture();
        let public = serde_json::to_value(&key.public).unwrap();
        assert_eq!(public.as_object().unwrap().len(), 5);
        assert_eq!(
            URL_SAFE_NO_PAD
                .decode(public["x"].as_str().unwrap())
                .unwrap()
                .len(),
            32
        );
        let payload = serde_json::to_value(&claims).unwrap();
        for name in [
            "password",
            "password_hash",
            "password_verifier",
            "token",
            "workstation_key",
            "private_key",
        ] {
            assert!(payload.get(name).is_none());
            assert!(public.get(name).is_none());
        }
        assert!(SigningKey::from_pkcs8(b"not a private key").is_err());
    }

    #[test]
    fn policy_never_invents_a_different_duration_or_permission() {
        assert!(policy_values(Some("30"), Some("1")).enabled);
        assert!(!policy_values(None, None).enabled);
        assert!(!policy_values(Some("365"), Some("1")).enabled);
        assert!(!policy_values(Some("30"), Some("0")).enabled);
    }
}
