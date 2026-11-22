//! Bounded local diagnostics: event codes and source locations, never clinical fields or keys.
use std::{io::Write, path::PathBuf};
pub fn path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|p| {
        PathBuf::from(p)
            .join(crate::APP_DATA_DIR)
            .join("logs")
            .join("client.log")
    })
}
pub fn event(code: &str) {
    if let Some(path) = path() {
        let _ = append(&path, code);
    }
}
fn append(path: &std::path::Path, code: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > 1024 * 1024) {
        let previous = path.with_extension("previous.log");
        let _ = std::fs::remove_file(&previous);
        let _ = std::fs::rename(path, previous);
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    writeln!(file, "{seconds} pid={} {code}", std::process::id())
}
pub fn install() {
    std::panic::set_hook(Box::new(|info| {
        if let Some(location) = info.location() {
            event(&format!(
                "rust-panic {}:{}",
                location.file(),
                location.line()
            ));
        } else {
            event("rust-panic");
        }
        // Do not format the panic payload: it may contain patient or credential data.
    }));
    event("start-ui-fix1");
}
#[cfg(test)]
mod tests {
    #[test]
    fn log_is_bounded_and_retains_previous_file() {
        let root = std::env::temp_dir().join(format!("m53-log-{}", uuid::Uuid::new_v4()));
        let path = root.join("client.log");
        super::append(&path, "host-start").unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("host-start")
        );
        std::fs::write(&path, vec![b'x'; 1024 * 1024 + 1]).unwrap();
        super::append(&path, "host-failed").unwrap();
        assert!(
            std::fs::metadata(path.with_extension("previous.log"))
                .unwrap()
                .len()
                > 1024 * 1024
        );
        assert!(std::fs::metadata(&path).unwrap().len() < 200);
        std::fs::remove_dir_all(root).unwrap();
    }
}
