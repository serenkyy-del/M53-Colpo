//! Account-bound offline unlock; plaintext physician passwords and server sessions are never persisted.
use crate::{
    cache::Store,
    connection::Connection,
    offline::{Offline, Profile},
};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};
#[cfg(windows)]
use std::path::PathBuf;
#[derive(Clone, Serialize, Deserialize)]
struct Resume {
    profile: Profile,
    key: String,
    login: String,
    password_verifier: String,
    #[serde(default)]
    target: Option<crate::one_c_auth::Prepared>,
    #[serde(default)]
    denied: bool,
    #[cfg(feature = "standalone")]
    #[serde(default)]
    local: Option<crate::one_c_auth::LocalPrepared>,
}
/// Safe data for the physician picker; no verifier, workstation key, or server token.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoginProfile {
    pub user_id: uuid::Uuid,
    pub login: String,
    pub display_name: String,
    #[serde(default)]
    pub target: Option<crate::one_c_auth::Profile>,
}
fn profiles() -> Result<Vec<Resume>, String> {
    let store = profile_store(Path::new("offline"))?;
    if let Some(bytes) = store.read("accounts")? {
        return serde_json::from_slice(&bytes)
            .map_err(|_| "Поврежден список профилей; кеш не удален".into());
    }
    store
        .read("last-account")?
        .map(|bytes| {
            serde_json::from_slice(&bytes)
                .map(|profile| vec![profile])
                .map_err(|_| "Поврежден профиль; кеш не удален".into())
        })
        .unwrap_or_else(|| Ok(vec![]))
}
/// Migration verifies the same installed identity; no grant or cache is changed.
#[cfg(not(feature = "standalone"))]
pub(crate) fn verify_transport_identity(
    old: &str, key: &str, organization: uuid::Uuid, workstation: uuid::Uuid,
    credential: uuid::Uuid, offline_key: &m53_contracts::access::OfflinePublicKey,
) -> Result<(), String> {
    for resume in profiles()?.iter().filter(|r| r.profile.server.trim_end_matches('/') == old) {
        if resume.key != key { return Err("Ключ прежнего профиля отличается; кеш не объединён".into()); }
        if let Some(target) = &resume.target {
            if target.me.organization_id != organization || target.me.workstation_id != workstation
                || target.me.credential_id != credential || &target.pinned_key != offline_key {
                return Err("HTTPS относится к другой организации, рабочему месту или ключу автономного входа".into());
            }
        }
    }
    Ok(())
}
pub fn saved_profiles(server: &str) -> Result<Vec<LoginProfile>, String> {
    let mut profiles: Vec<_> = profiles()?
        .into_iter()
        .filter(|profile| !profile.denied)
        .filter(|profile| {
            crate::transport::same_server(&profile.profile.server, server)
        })
        .map(|profile| LoginProfile {
            user_id: profile.profile.user_id,
            login: profile.login,
            display_name: profile.profile.doctor,
            target: profile.target.map(|t| t.selected),
        })
        .collect();
    profiles.sort_by_key(|profile| (profile.display_name.to_lowercase(), profile.login.clone()));
    Ok(profiles)
}
#[cfg(windows)]
fn windows_app_root() -> Result<PathBuf, String> {
    Ok(
        PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA unavailable")?)
            .join(crate::APP_DATA_DIR),
    )
}
fn profile_store(namespace: &Path) -> Result<Store, String> {
    #[cfg(windows)]
    {
        Store::new(windows_app_root()?.join(namespace), Arc::new(crate::cache::Dpapi))
    }
    #[cfg(target_os = "linux")]
    {
        crate::linux_storage::open(&crate::linux_storage::app_root()?, namespace)
    }
}

fn account_store(profile: &Profile) -> Result<Store, String> {
    use sha2::{Digest, Sha256};
    let id = hex::encode(Sha256::digest(
        format!("{}:{}", crate::transport::cache_origin(&profile.server), profile.user_id).as_bytes(),
    ));
    profile_store(&Path::new("offline").join(id))
}
pub fn connect(
    mut raw: Connection,
    login: &str,
    password: &str,
    existing: Option<Arc<Offline>>,
) -> Result<Connection, String> {
    let profile = Profile {
        server: raw.storage_base.clone(),
        user_id: raw.user_id.ok_or("No authenticated user")?,
        doctor: raw.doctor.clone(),
    };
    if existing
        .as_ref()
        .is_some_and(|o| o.state.lock().unwrap().profile.user_id != profile.user_id)
    {
        return Err("Текущая работа принадлежит другому врачу; сначала выйдите из его профиля без удаления кеша".into());
    }
    let salt = SaltString::encode_b64(uuid::Uuid::new_v4().as_bytes()).map_err(|_| "Salt error")?;
    let verifier = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| "Verifier error")?
        .to_string();
    let resume = Resume {
        profile: profile.clone(),
        key: raw.key.clone(),
        login: login.into(),
        password_verifier: verifier,
        target: None,
        denied: false,
        #[cfg(feature = "standalone")]
        local: None,
    };
    save_resume(&resume)?;
    let offline = if let Some(offline) = existing {
        offline.install_auth(raw.clone())?;
        offline
    } else {
        Offline::attach(raw.clone(), account_store(&profile)?, profile)?
    };
    raw.offline = Some(offline);
    Ok(raw)
}
fn save_resume(resume: &Resume) -> Result<(), String> {
    let mut accounts = profiles()?;
    accounts.retain(|account| {
        !crate::transport::same_server(&account.profile.server, &resume.profile.server)
            || account.profile.user_id != resume.profile.user_id
    });
    accounts.push(resume.clone());
    profile_store(Path::new("offline"))?.write(
        "accounts",
        &serde_json::to_vec(&accounts).map_err(|_| "Profile serialization error")?,
    )?;
    profile_store(Path::new("offline"))?.write(
        "last-account",
        &serde_json::to_vec(&resume).map_err(|_| "Profile serialization error")?,
    )?;

    Ok(())
}
pub fn connect_1c(
    mut raw: Connection,
    password: &str,
    prepared: crate::one_c_auth::Prepared,
) -> Result<Connection, String> {
    prepared.validate(crate::one_c_auth::now())?;
    let profile = Profile {
        server: raw.storage_base.clone(),
        user_id: prepared.me.user_id,
        doctor: prepared.me.display_name.clone(),
    };
    let salt = SaltString::encode_b64(uuid::Uuid::new_v4().as_bytes()).map_err(|_| "Salt error")?;
    let verifier = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| "Verifier error")?
        .to_string();
    let resume = Resume {
        profile: profile.clone(),
        key: raw.key.clone(),
        login: prepared.selected.login.clone(),
        password_verifier: verifier,
        target: Some(prepared),
        denied: false,
        #[cfg(feature = "standalone")]
        local: None,
    };
    save_resume(&resume)?;
    raw.offline = Some(Offline::attach(
        raw.clone(),
        account_store(&profile)?,
        profile,
    )?);
    Ok(raw)
}
#[cfg(feature = "standalone")]
pub fn connect_local(
    mut raw: Connection,
    login: &str,
    password: &str,
) -> Result<Connection, String> {
    let me: m53_contracts::access::Me = raw.get("/api/v1/me")?;
    let time = crate::one_c_auth::now();
    if Some(me.user_id) != raw.user_id || me.display_name != raw.doctor
        || me.server_time.abs_diff(time) > 300 || me.session_expires_at <= time
    {
        return Err("Не удалось подтвердить сеанс врача и время сервера".into());
    }
    let pinned_key = raw.get("/api/v1/auth/offline-key")?;
    for resume in profiles()?.into_iter().filter(|profile| {
        crate::transport::same_server(&profile.profile.server, &raw.storage_base)
            && profile.profile.user_id == me.user_id
    }) {
        if let Some(local) = resume.local
            && (local.me.organization_id != me.organization_id || local.pinned_key != pinned_key)
        {
            return Err("Изменилась организация или ключ автономного входа сервера".into());
        }
    }
    let local = crate::one_c_auth::LocalPrepared {
        me,
        grant: raw.post("/api/v1/auth/offline-grant", &serde_json::json!({}))?,
        pinned_key,
        last_time: time,
    };
    let claims = local.claims(time)?;
    raw.apply_offline_claims(claims)?;
    let profile = Profile { server: raw.storage_base.clone(), user_id: local.me.user_id, doctor: raw.doctor.clone() };
    let salt = SaltString::encode_b64(uuid::Uuid::new_v4().as_bytes()).map_err(|_| "Salt error")?;
    let password_verifier = Argon2::default()
        .hash_password(password.as_bytes(), &salt).map_err(|_| "Verifier error")?.to_string();
    save_resume(&Resume {
        profile: profile.clone(), key: raw.key.clone(), login: login.into(), password_verifier,
        target: None, denied: false, local: Some(local),
    })?;
    raw.offline = Some(Offline::attach(raw.clone(), account_store(&profile)?, profile)?);
    Ok(raw)
}

#[cfg(feature = "standalone")]
pub fn resume_local(server: &str, key: &str, login: &str, password: &str) -> Result<Connection, String> {
    let mut resume = profiles()?.into_iter()
        .find(|r| crate::transport::same_server(&r.profile.server, server) && r.login == login)
        .ok_or("Первый вход врача требует связи с сервером")?;
    if resume.key != key {
        return Err("Ключ рабочего места изменён. Войдите при доступном сервере".into());
    }
    let time = crate::one_c_auth::now();
    let local = resume.local.as_mut().ok_or("Войдите при доступном сервере")?;
    local.claims(time)?;
    local.last_time = local.last_time.max(time);
    let raw = unlock(resume.clone(), login, password)?;
    save_resume(&resume)?;
    Ok(raw)
}

#[cfg(feature = "standalone")]
pub fn deny_local(server: &str, login: &str) -> Result<(), String> {
    deny_local_if(server, |resume| resume.login == login)
}

#[cfg(feature = "standalone")]
pub(crate) fn deny_local_user(server: &str, user: uuid::Uuid) -> Result<(), String> {
    deny_local_if(server, |resume| resume.profile.user_id == user)
}

#[cfg(feature = "standalone")]
fn deny_local_if(server: &str, matches: impl Fn(&Resume) -> bool) -> Result<(), String> {
    let mut accounts = profiles()?;
    for resume in &mut accounts {
        if crate::transport::same_server(&resume.profile.server, server) && matches(resume) {
            resume.denied = true;
        }
    }
    profile_store(Path::new("offline"))?.write(
        "accounts", &serde_json::to_vec(&accounts).map_err(|_| "Ошибка профиля")?,
    )
}

pub fn deny_target(server: &str, id: uuid::Uuid) -> Result<(), String> {
    let mut accounts = profiles()?;
    for r in &mut accounts {
        if crate::transport::same_server(&r.profile.server, server)
            && r.target
                .as_ref()
                .is_some_and(|t| t.selected.doctor_id == id)
        {
            r.denied = true;
        }
    }
    profile_store(Path::new("offline"))?.write(
        "accounts",
        &serde_json::to_vec(&accounts).map_err(|_| "Ошибка профиля")?,
    )
}
pub fn target_profiles(server: &str) -> Result<Vec<crate::one_c_auth::Profile>, String> {
    Ok(saved_profiles(server)?
        .into_iter()
        .filter_map(|p| p.target)
        .collect())
}
pub fn resume_1c(
    server: &str,
    key: &str,
    selected: &crate::one_c_auth::Profile,
    password: &str,
) -> Result<Connection, String> {
    let mut resume = profiles()?
        .into_iter()
        .find(|r| {
            crate::transport::same_server(&r.profile.server, server)
                && r.target
                    .as_ref()
                    .is_some_and(|t| t.selected.doctor_id == selected.doctor_id)
        })
        .ok_or("Первый вход врача требует связи с API и 1С")?;
    if resume.key != key {
        return Err("Ключ рабочего места изменен; требуется онлайн-вход".into());
    }
    if resume.denied {
        return Err("Онлайн-вход отклонен; требуется успешная проверка 1С".into());
    }
    let t = resume.target.as_mut().ok_or("Нет автономного профиля 1С")?;
    if &t.selected != selected {
        return Err("Привязка врача изменилась; требуется онлайн-вход".into());
    }
    t.validate(crate::one_c_auth::now())?;
    t.last_time = t.last_time.max(crate::one_c_auth::now());
    let connection = unlock(resume.clone(), &selected.login, password)?;
    save_resume(&resume)?;
    Ok(connection)
}
pub fn resume(login: &str, password: &str) -> Result<Connection, String> {
    let profile = profile_store(Path::new("offline"))?
        .read("last-account")?
        .ok_or("Нет ранее сохраненной работы")?;
    let resume: Resume =
        serde_json::from_slice(&profile).map_err(|_| "Поврежден профиль; кеш не удален")?;
    unlock(resume, login, password)
}
pub fn resume_for(server: &str, login: &str, password: &str) -> Result<Connection, String> {
    let resume = profiles()?
        .into_iter()
        .find(|profile| {
            crate::transport::same_server(&profile.profile.server, server)
                && profile.login == login
        })
        .ok_or("Нет сохраненного профиля этого врача для выбранного сервера")?;
    unlock(resume, login, password)
}
fn unlock(resume: Resume, login: &str, password: &str) -> Result<Connection, String> {
    if resume.login != login
        || !PasswordHash::new(&resume.password_verifier).is_ok_and(|hash| {
            Argon2::default()
                .verify_password(password.as_bytes(), &hash)
                .is_ok()
        })
    {
        return Err("Не удалось открыть кеш этого врача".into());
    }
    let mut raw = Connection::new(&resume.profile.server, &resume.key)?;
    #[cfg(feature = "standalone")]
    {
        if resume.denied {
            return Err("Онлайн-вход был отклонён. Войдите при доступном сервере".into());
        }
        let local = resume.local.as_ref().ok_or("Требуется первый онлайн-вход")?;
        let claims = local.claims(crate::one_c_auth::now())?;
        if claims.sub != resume.profile.user_id {
            return Err("Автономное разрешение относится к другому профилю".into());
        }
        raw.apply_offline_claims(claims)?;
    }
    if let Some(target) = &resume.target {
        target.validate(crate::one_c_auth::now())?;
        if resume.denied {
            return Err("Онлайн-вход был отклонен".into());
        }
        raw.apply_offline_claims(crate::one_c_auth::verify(&target.grant, &target.pinned_key)?)?;
    }
    raw.user_id = Some(resume.profile.user_id);
    raw.doctor = resume.profile.doctor.clone();
    let offline = Offline::attach(raw.clone(), account_store(&resume.profile)?, resume.profile)?;
    raw.offline = Some(offline);
    Ok(raw)
}
