//! DPAPI in the current Windows user's context. No physician password/session is persisted.
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, ptr};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    },
};
#[derive(Serialize, Deserialize)]
struct Profile {
    url: String,
    encrypted_key: Vec<u8>,
}
fn path() -> Result<PathBuf, String> {
    Ok(
        PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA недоступен")?)
            .join(crate::APP_DATA_DIR)
            .join("workstation.json"),
    )
}
pub(crate) fn transform(bytes: &[u8], protect: bool) -> Result<Vec<u8>, String> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len().try_into().map_err(|_| "Ключ слишком большой")?,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    // SAFETY: input is valid throughout the call. DPAPI allocates output with LocalAlloc.
    let ok = unsafe {
        if protect {
            CryptProtectData(
                &input,
                ptr::null(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                ptr::null_mut(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if ok == 0 {
        return Err("DPAPI не смог обработать ключ текущего пользователя".into());
    }
    if output.pbData.is_null() {
        return Err("DPAPI вернул пустой результат".into());
    }
    // SAFETY: output length and allocation are owned by DPAPI, copied before freeing once.
    let result =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(output.pbData as _);
    }
    Ok(result)
}
pub fn save(url: &str, key: &str) -> Result<(), String> {
    use std::io::Write;
    let path = path()?;
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|_| "Не удалось создать профиль")?;
    let profile = Profile {
        url: crate::transport::saved_origin(url, key)?,
        encrypted_key: transform(key.as_bytes(), true)?,
    };
    let bytes = serde_json::to_vec(&profile).map_err(|_| "Ошибка профиля")?;
    let pending = path.with_extension(format!("{}.pending", uuid::Uuid::new_v4()));
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true)
        .open(&pending).map_err(|_| "Не удалось сохранить профиль")?;
    file.write_all(&bytes).and_then(|_| file.sync_all()).map_err(|_| "Не удалось сохранить профиль")?;
    drop(file);
    crate::cache::atomic_replace(&pending, &path)
}
pub fn load() -> Result<(String, String), String> {
    let (url, key) = load_stored()?;
    Ok((crate::transport::saved_origin(&url, &key)?, key))
}
pub(crate) fn load_stored() -> Result<(String, String), String> {
    let bytes = std::fs::read(path()?).map_err(|_| "Профиль не найден")?;
    let profile: Profile = serde_json::from_slice(&bytes).map_err(|_| "Профиль поврежден")?;
    let key = String::from_utf8(transform(&profile.encrypted_key, false)?)
        .map_err(|_| "Ключ поврежден")?;
    Ok((profile.url, key))
}
