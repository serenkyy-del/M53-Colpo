//! Startup diagnostics contain event codes only, never image or protocol contents.
#[cfg(windows)]
fn log(code: &str) {
    use std::io::Write;
    let Some(base) = std::env::var_os("LOCALAPPDATA") else {
        return;
    };
    let dir = std::path::PathBuf::from(base)
        .join(m53_client::APP_DATA_DIR)
        .join("logs");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("native-startup.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1024 * 1024) {
        let _ = std::fs::rename(&path, dir.join("previous.log"));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{} {code}", std::process::id());
    }
}
pub fn install() {
    #[cfg(windows)]
    {
        log("gpui-start");
        std::panic::set_hook(Box::new(|info| {
            // Only this known platform-startup prefix is safe to persist. Other
            // panic payloads may include patient data and remain omitted.
            let message = info
                .payload()
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| info.payload().downcast_ref::<&str>().copied());
            if let Some(message) =
                message.filter(|s| s.starts_with("failed to initialize DirectX:"))
            {
                log(&message
                    .chars()
                    .take(4096)
                    .collect::<String>()
                    .replace(['\n', '\r'], " "));
            }
            if let Some(location) = info.location() {
                log(&format!("panic {}:{}", location.file(), location.line()));
            } else {
                log("panic");
            }
        }));
    }
}
pub fn failure() {
    #[cfg(windows)]
    {
        log("startup-failed");
        #[link(name = "user32")]
        unsafe extern "system" {
            fn MessageBoxW(
                hwnd: *mut std::ffi::c_void,
                text: *const u16,
                caption: *const u16,
                kind: u32,
            ) -> i32;
        }
        let text: Vec<u16> = format!("Не удалось запустить рабочее место. Локальные данные сохранены. Журнал: %LOCALAPPDATA%\\{}\\logs\\native-startup.log", m53_client::APP_DATA_DIR).encode_utf16().chain(Some(0)).collect();
        let title: Vec<u16> = m53_client::APP_NAME
            .encode_utf16()
            .chain(Some(0))
            .collect();
        unsafe {
            MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), 0x10);
        }
    }
}
