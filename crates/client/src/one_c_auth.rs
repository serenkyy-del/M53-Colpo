//! Target 1C login: server-bound profiles, verified signed offline permission.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use m53_contracts::{access::*, one_c_access::*};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub doctor_id: Uuid,
    pub display_name: String,
    pub source_id: Uuid,
    pub doctor_guid: Uuid,
    pub user_guid: Uuid,
    pub login: String,
    pub binding_version: i64,
}
#[derive(Deserialize)]
pub struct Page {
    pub organization_id: Uuid,
    pub server_time: i64,
    pub next_after: Option<Uuid>,
    pub auth_provider: String,
    pub items: Vec<Profile>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Prepared {
    pub selected: Profile,
    pub me: Me,
    pub grant: OfflineGrant,
    pub pinned_key: OfflinePublicKey,
    pub last_time: i64,
}
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub fn verify(grant: &OfflineGrant, key: &OfflinePublicKey) -> Result<OfflineClaims, String> {
    verify_provider(grant, key, "1c_web")
}

fn verify_provider(grant: &OfflineGrant, key: &OfflinePublicKey, provider: &str) -> Result<OfflineClaims, String> {
    let fail = || "Недействительная подпись автономного разрешения".to_string();
    if key.alg != "EdDSA"
        || key.kty != "OKP"
        || key.crv != "Ed25519"
        || &grant.key != key
        || grant.token_type != "offline_unlock"
    {
        return Err(fail());
    }
    let public = B64.decode(&key.x).map_err(|_| fail())?;
    if public.len() != 32 || hex::encode(Sha256::digest(&public)) != key.kid {
        return Err(fail());
    }
    let parts: Vec<_> = grant.grant.split('.').collect();
    if parts.len() != 3 {
        return Err(fail());
    }
    let header: serde_json::Value =
        serde_json::from_slice(&B64.decode(parts[0]).map_err(|_| fail())?).map_err(|_| fail())?;
    if header["alg"] != "EdDSA"
        || header["typ"] != "m53-offline+jwt"
        || header["kid"] != key.kid
        || header.get("crit").is_some()
    {
        return Err(fail());
    }
    let sig = B64.decode(parts[2]).map_err(|_| fail())?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &public)
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
        .map_err(|_| fail())?;
    let c: OfflineClaims =
        serde_json::from_slice(&B64.decode(parts[1]).map_err(|_| fail())?).map_err(|_| fail())?;
    if c.schema_version != 1
        || c.iss != OFFLINE_ISSUER
        || c.aud != OFFLINE_AUDIENCE
        || c.purpose != OFFLINE_PURPOSE
        || c.auth_provider != provider
        || c.jti != grant.grant_id
        || c.exp != grant.expires_at
        || c.exp.checked_sub(c.iat) != Some(OFFLINE_GRANT_SECONDS)
        || c.nbf != c.iat
    {
        return Err(fail());
    }
    Ok(c)
}
#[cfg(feature = "standalone")]
#[derive(Clone, Serialize, Deserialize)]
pub struct LocalPrepared {
    pub me: Me,
    pub grant: OfflineGrant,
    pub pinned_key: OfflinePublicKey,
    pub last_time: i64,
}

#[cfg(feature = "standalone")]
impl LocalPrepared {
    pub fn claims(&self, time: i64) -> Result<OfflineClaims, String> {
        let claims = verify_provider(&self.grant, &self.pinned_key, "m53")?;
        let me = &self.me;
        let doctor = me.doctor.as_ref().ok_or("Нет профиля врача")?;
        let policy = &me.offline_policy;
        if [me.user_id, me.organization_id, me.workstation_id, me.credential_id, doctor.id, claims.jti]
            .iter().any(Uuid::is_nil)
            || me.auth_provider != "m53"
            || me.external_user_guid.is_some()
            || claims.external_user_guid.is_some()
            || !doctor.active
            || !doctor.external_ids.is_empty()
            || !claims.external_doctor_ids.is_empty()
            || claims.sub != me.user_id
            || claims.organization_id != me.organization_id
            || claims.workstation_id != me.workstation_id
            || claims.credential_id != me.credential_id
            || claims.doctor_id != doctor.id
            || claims.display_name != doctor.display_name
            || me.auth_version <= 0
            || claims.auth_version != me.auth_version
            || !me.roles.iter().any(|role| role == "doctor")
            || claims.roles != me.roles
            || !policy.enabled
            || policy.grant_days != 30
            || claims.permissions.allow_new_manual_encounter != policy.allow_new_manual_encounter
            || claims.permissions.allow_continue_open_encounter != policy.allow_continue_open_encounter
            || claims.permissions.allow_local_print != policy.allow_local_print
        {
            return Err("Автономное разрешение относится к другому профилю".into());
        }
        if time.saturating_add(300) < self.last_time
            || time.saturating_add(300) < claims.nbf
            || time >= claims.exp
        {
            return Err("Истёк автономный допуск или изменилось время компьютера. Войдите при доступном сервере".into());
        }
        Ok(claims)
    }
}

impl Prepared {
    pub fn validate(&self, time: i64) -> Result<(), String> {
        let c = verify(&self.grant, &self.pinned_key)?;
        let m = &self.me;
        let d = m.doctor.as_ref().ok_or("Нет профиля врача")?;
        if [
            m.user_id,
            m.organization_id,
            m.workstation_id,
            m.credential_id,
            c.jti,
            d.id,
        ]
        .iter()
        .any(Uuid::is_nil)
            || m.auth_version <= 0
            || !m.roles.iter().any(|r| r == "doctor")
            || m.auth_provider != "1c_web"
            || !d.active
            || d.id != self.selected.doctor_id
            || m.external_user_guid != Some(self.selected.user_guid)
            || !d.external_ids.iter().any(|e| {
                e.source_id == self.selected.source_id && e.doctor_guid == self.selected.doctor_guid
            })
            || c.sub != m.user_id
            || c.organization_id != m.organization_id
            || c.doctor_id != d.id
            || c.external_user_guid != m.external_user_guid
            || c.workstation_id != m.workstation_id
            || c.credential_id != m.credential_id
            || c.auth_version != m.auth_version
            || c.roles != m.roles
            || c.external_doctor_ids != d.external_ids
            || c.display_name != m.display_name
            || !m.offline_policy.enabled
            || m.offline_policy.grant_days != 30
            || c.permissions.allow_new_manual_encounter
                != m.offline_policy.allow_new_manual_encounter
            || c.permissions.allow_continue_open_encounter
                != m.offline_policy.allow_continue_open_encounter
            || c.permissions.allow_local_print != m.offline_policy.allow_local_print
        {
            return Err("Автономное разрешение относится к другому профилю".into());
        }
        if time + 300 < self.last_time || time + 300 < c.nbf || time >= c.exp {
            return Err(
                "Автономный допуск истек или время компьютера изменено; требуется онлайн-вход"
                    .into(),
            );
        }
        Ok(())
    }
    pub fn from_bundle(
        selected: Profile,
        bundle: OneCLoginResponse,
        key: OfflinePublicKey,
    ) -> Result<(Self, m53_contracts::workflow::Session), String> {
        if bundle.auth_method != "odata_basic"
            || bundle.session.user_id != bundle.me.user_id
            || bundle.session.token.is_empty()
            || bundle.session.display_name != bundle.me.display_name
        {
            return Err("Неполный ответ входа 1С".into());
        }
        let t = now();
        if (bundle.me.server_time - t).abs() > 300 || bundle.me.session_expires_at <= t {
            return Err("Время сервера и компьютера не совпадает".into());
        }
        let prepared = Self {
            selected,
            me: bundle.me,
            grant: bundle.offline_grant,
            pinned_key: key,
            last_time: t,
        };
        prepared.validate(t)?;
        Ok((prepared, bundle.session))
    }
}
pub fn unavailable(error: &str) -> bool {
    error.starts_with("Сервис недоступен:")
        || ["Ошибка сервиса 502 ", "Ошибка сервиса 504 "]
            .iter()
            .any(|c| error.starts_with(c))
        || (error.starts_with("Ошибка сервиса 503 ")
            && [
                "one_c_login_unavailable",
                "one_c_login_verification_pending",
            ]
            .iter()
            .any(|c| error.ends_with(c)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use serde_json::json;
    fn fixture() -> Prepared {
        fixture_for("1c_web")
    }
    fn fixture_for(provider: &str) -> Prepared {
        let pair = Ed25519KeyPair::from_seed_unchecked(&[9; 32]).unwrap();
        let public = pair.public_key().as_ref();
        let key = OfflinePublicKey {
            kid: hex::encode(Sha256::digest(public)),
            alg: "EdDSA".into(),
            kty: "OKP".into(),
            crv: "Ed25519".into(),
            x: B64.encode(public),
        };
        let id = |n| Uuid::from_u128(n);
        let t = now();
        let selected = Profile {
            doctor_id: id(1),
            display_name: "Synthetic".into(),
            source_id: id(2),
            doctor_guid: id(3),
            user_guid: id(4),
            login: "synthetic".into(),
            binding_version: 1,
        };
        let me = Me {
            auth_provider: provider.into(),
            external_user_guid: (provider == "1c_web").then_some(id(4)),
            user_id: id(5),
            organization_id: id(6),
            display_name: "Synthetic".into(),
            doctor: Some(DoctorIdentity {
                id: id(1),
                display_name: "Synthetic".into(),
                active: true,
                origin: "1c".into(),
                external_ids: (provider == "1c_web").then_some(ExternalDoctorId {
                    source_id: id(2),
                    doctor_guid: id(3),
                }).into_iter().collect(),
            }),
            workstation_id: id(7),
            credential_id: id(8),
            auth_version: 1,
            roles: vec!["doctor".into()],
            session_expires_at: t + 28800,
            server_time: t,
            offline_policy: OfflinePolicy {
                enabled: true,
                grant_days: 30,
                allow_new_manual_encounter: true,
                allow_continue_open_encounter: true,
                allow_local_print: true,
            },
        };
        let c = OfflineClaims {
            auth_provider: provider.into(),
            external_user_guid: (provider == "1c_web").then_some(id(4)),
            schema_version: 1,
            iss: OFFLINE_ISSUER.into(),
            aud: OFFLINE_AUDIENCE.into(),
            purpose: OFFLINE_PURPOSE.into(),
            jti: id(9),
            sub: id(5),
            organization_id: id(6),
            doctor_id: id(1),
            workstation_id: id(7),
            credential_id: id(8),
            auth_version: 1,
            display_name: "Synthetic".into(),
            external_doctor_ids: me.doctor.as_ref().unwrap().external_ids.clone(),
            roles: me.roles.clone(),
            permissions: OfflinePermissions {
                allow_new_manual_encounter: true,
                allow_continue_open_encounter: true,
                allow_local_print: true,
            },
            iat: t,
            nbf: t,
            exp: t + OFFLINE_GRANT_SECONDS,
        };
        let payload = format!(
            "{}.{}",
            B64.encode(
                serde_json::to_vec(&json!({"alg":"EdDSA","typ":"m53-offline+jwt","kid":key.kid}))
                    .unwrap()
            ),
            B64.encode(serde_json::to_vec(&c).unwrap())
        );
        let sig = pair.sign(payload.as_bytes());
        let grant = OfflineGrant {
            grant_id: id(9),
            grant: format!("{payload}.{}", B64.encode(sig.as_ref())),
            token_type: "offline_unlock".into(),
            expires_at: c.exp,
            key: key.clone(),
        };
        Prepared {
            selected,
            me,
            grant,
            pinned_key: key,
            last_time: t,
        }
    }
    #[cfg(feature = "standalone")]
    #[test]
    fn signed_local_grant_cannot_be_reused_after_expiry_or_for_another_account() {
        let prepared = fixture_for("m53");
        let now = prepared.last_time;
        let local = LocalPrepared {
            me: prepared.me,
            grant: prepared.grant,
            pinned_key: prepared.pinned_key,
            last_time: now,
        };
        assert!(local.claims(now).is_ok());
        assert!(local.claims(local.grant.expires_at).is_err());
        assert!(local.claims(now - 301).is_err());
        let mut changed = local.clone();
        changed.me.credential_id = Uuid::new_v4();
        assert!(changed.claims(now).is_err());
        changed = local.clone();
        changed.me.user_id = Uuid::new_v4();
        assert!(changed.claims(now).is_err());
        assert!(verify(&local.grant, &local.pinned_key).is_err());
    }
    #[test]
    fn signed_bundle_is_bound_to_physician_workstation_and_policy() {
        let p = fixture();
        assert!(p.validate(now()).is_ok());
        for field in [0, 1, 2, 3, 4] {
            let mut bad = p.clone();
            match field {
                0 => bad.me.user_id = Uuid::new_v4(),
                1 => bad.me.workstation_id = Uuid::new_v4(),
                2 => bad.selected.user_guid = Uuid::new_v4(),
                3 => bad.me.auth_provider = "own".into(),
                _ => bad.me.offline_policy.allow_local_print = false,
            };
            assert!(bad.validate(now()).is_err());
        }
    }
    #[test]
    fn altered_signature_untrusted_key_expiry_and_clock_rollback_fail() {
        let p = fixture();
        let mut bad = p.clone();
        bad.grant.grant.push('a');
        assert!(bad.validate(now()).is_err());
        let mut bad = p.clone();
        bad.pinned_key.x = B64.encode([2; 32]);
        assert!(bad.validate(now()).is_err());
        assert!(p.validate(p.grant.expires_at).is_err());
        assert!(p.validate(p.last_time - 301).is_err());
    }
    #[test]
    fn explicit_denials_never_allow_cached_login() {
        for e in [
            "Ошибка сервиса 401 Unauthorized. one_c_login_denied",
            "Ошибка сервиса 403 Forbidden. doctor_inactive",
            "Ошибка сервиса 429 Too Many Requests. login_temporarily_blocked",
            "Ошибка сервиса 503 Service Unavailable. offline_signing_key_missing",
            "Некорректный ответ сервиса",
        ] {
            assert!(!unavailable(e));
        }
        assert!(unavailable("Сервис недоступен: проверьте сеть и TLS"));
        assert!(unavailable(
            "Ошибка сервиса 503 Service Unavailable. one_c_login_unavailable"
        ));
    }
}
