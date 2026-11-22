//! Encrypted workstation settings for the standalone Linux client.
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
struct Profile {
    url: String,
    key: String,
}

pub fn save(url: &str, key: &str) -> Result<(), String> {
    let profile = Profile { url: crate::transport::saved_origin(url, key)?, key: key.into() };
    let bytes = serde_json::to_vec(&profile).map_err(|_| "Ошибка профиля")?;
    store()?.write("profile", &bytes)
}

pub fn load() -> Result<(String, String), String> {
    let bytes = store()?.read("profile")?.ok_or("Профиль не найден")?;
    let profile: Profile = serde_json::from_slice(&bytes).map_err(|_| "Профиль повреждён")?;
    Ok((crate::transport::saved_origin(&profile.url, &profile.key)?, profile.key))
}

fn store() -> Result<crate::cache::Store, String> {
    crate::linux_storage::open(&crate::linux_storage::app_root()?, "workstation")
}
