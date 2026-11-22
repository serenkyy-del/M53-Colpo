//! Verified, versioned local print runtime; no moving downloads on workstations.
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, Instant},
};
const MANIFEST: &str = include_str!("../../../../assets/print-runtime/manifest.json");

#[derive(Deserialize)]
struct File {
    path: String,
    size: u64,
    sha256: String,
}
#[derive(Deserialize)]
struct Manifest {
    runtime_id: String,
    archive: String,
    archive_size: u64,
    archive_sha256: String,
    files: Vec<File>,
}
pub(super) fn hash(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}
fn safe_path(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\\', ':'])
        && name
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
}
fn check_file(path: &Path, size: u64, sha: &str) -> Result<(), String> {
    let mut file = fs::File::open(path).map_err(|_| "Нет компонента автономной печати")?;
    if file
        .metadata()
        .map_err(|_| "Не удалось проверить компонент печати")?
        .len()
        != size
    {
        return Err("Поврежден компонент автономной печати".into());
    }
    let mut digest = Sha256::new();
    let mut buffer = [0; 128 * 1024];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|_| "Не удалось проверить компонент печати")?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    if hex::encode(digest.finalize()) != sha {
        return Err("Контрольная сумма компонента печати не совпадает".into());
    }
    Ok(())
}
/// Publish only an already verified stage. Serialized across processes; a damaged
/// version is retained in an owned quarantine, never served or mixed with a repair.
fn activate(
    root: &Path,
    stage: &Path,
    destination: &Path,
    valid: impl Fn(&Path) -> bool,
) -> Result<(), String> {
    if !root.is_absolute()
        || stage.parent() != Some(root)
        || destination.parent() != Some(root)
        || !stage
            .file_name()
            .and_then(|v| v.to_str())
            .and_then(|v| v.strip_prefix("stage-"))
            .is_some_and(|v| uuid::Uuid::parse_str(v).is_ok())
    {
        return Err("Некорректный каталог восстановления печати".into());
    }
    let id = destination
        .file_name()
        .and_then(|v| v.to_str())
        .filter(|v| safe_path(v))
        .ok_or("Некорректная версия печати")?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(format!("{id}.install.lock")))
        .map_err(|_| "Не удалось подготовить восстановление печати")?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => return Err("Другой процесс восстанавливает печать; повторите попытку".into()),
        }
    }
    // Another process may have finished while this process verified/unpacked its archive.
    if valid(destination) {
        return Ok(());
    }
    if !valid(stage) {
        return Err("Не прошла проверка восстановленных компонентов печати".into());
    }
    if destination.exists() {
        if valid(destination) {
            return Ok(());
        }
        let quarantine = root.join(format!("quarantine-{id}-{}", uuid::Uuid::new_v4()));
        fs::rename(destination, quarantine).map_err(
            |_| "Компоненты печати заняты; закройте другие окна программы и повторите попытку",
        )?;
    }
    // rename never overwrites an existing directory on Windows.
    if fs::rename(stage, destination).is_err() && !valid(destination) {
        return Err(
            "Не удалось установить восстановленные компоненты печати; повторите попытку".into(),
        );
    }
    Ok(())
}
pub(super) fn runtime() -> Result<PathBuf, String> {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    if let Some(path) = PATH.get() {
        return Ok(path.clone());
    }
    let m: Manifest = serde_json::from_str(MANIFEST).map_err(|_| "Некорректный комплект печати")?;
    if !safe_path(&m.runtime_id)
        || m.files.len() > 1000
        || m.files.iter().any(|f| !safe_path(&f.path))
    {
        return Err("Некорректные пути комплекта печати".into());
    }
    let base = std::env::var_os("LOCALAPPDATA").ok_or("Нет папки локальных компонентов")?;
    let root = PathBuf::from(base).join(crate::APP_DATA_DIR).join("print-runtime");
    let destination = root.join(&m.runtime_id);
    let valid = |path: &Path| {
        m.files
            .iter()
            .all(|f| check_file(&path.join(&f.path), f.size, &f.sha256).is_ok())
    };
    if !valid(&destination) {
        let exe = std::env::current_exe().map_err(|_| "Нет каталога приложения")?;
        let archive = exe
            .parent()
            .ok_or("Нет каталога приложения")?
            .join(&m.archive);
        // Development uses the same pinned archive; distribution has no machine path fallback.
        #[cfg(debug_assertions)]
        let archive = if archive.is_file() {
            archive
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../tmp/print-runtime-bundle")
                .join(&m.archive)
        };
        if cfg!(feature = "standalone") && !archive.is_file() {
            return Err("Установите компоненты печати через Setup.ps1 или Install-PrintRuntime.ps1".into());
        }
        check_file(&archive, m.archive_size, &m.archive_sha256)?;
        fs::create_dir_all(&root).map_err(|_| "Не удалось подготовить компоненты печати")?;
        let stage = root.join(format!("stage-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&stage).map_err(|_| "Не удалось подготовить компоненты печати")?;
        let result: Result<(), String> = (|| {
            let input = fs::File::open(archive).map_err(|_| "Нет архива печати")?;
            let mut zip = zip::ZipArchive::new(input).map_err(|_| "Поврежден архив печати")?;
            if zip.len() != m.files.len()
                || m.files.iter().map(|f| f.size).sum::<u64>() > 512 * 1024 * 1024
            {
                return Err("Некорректный размер комплекта печати".into());
            }
            for f in &m.files {
                let mut entry = zip
                    .by_name(&f.path)
                    .map_err(|_| "Нет файла в архиве печати")?;
                if entry.size() != f.size
                    || entry.unix_mode().is_some_and(|v| v & 0o170000 == 0o120000)
                {
                    return Err("Некорректный файл архива печати".into());
                }
                let path = stage.join(&f.path);
                fs::create_dir_all(path.parent().unwrap())
                    .map_err(|_| "Не удалось распаковать печать")?;
                let mut output = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .map_err(|_| "Не удалось распаковать печать")?;
                let copied = std::io::copy(&mut entry.by_ref().take(f.size + 1), &mut output)
                    .map_err(|_| "Не удалось распаковать печать")?;
                if copied != f.size {
                    return Err("Некорректный размер распакованного компонента".into());
                }
                output
                    .flush()
                    .map_err(|_| "Не удалось записать компонент печати")?;
                check_file(&path, f.size, &f.sha256)?;
            }
            activate(&root, &stage, &destination, valid)
        })();
        // Exact unique directory owned by this operation only; never clinical/cache data.
        let _ = fs::remove_dir_all(&stage);
        result?;
    }
    let _ = PATH.set(destination.clone());
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let p = std::env::temp_dir()
                .join(format!("m53-print-repair-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
        fn stage(&self) -> PathBuf {
            let p = self.0.join(format!("stage-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&p).unwrap();
            fs::write(p.join("engine"), b"pinned").unwrap();
            p
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
            assert!(
                self.0
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("m53-print-repair-test-")
            );
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn verified(path: &Path) -> bool {
        check_file(&path.join("engine"), 6, &hash(b"pinned")).is_ok()
    }
    #[test]
    fn activate_repairs_damage_and_keeps_unrelated_files() {
        let s = Scratch::new();
        let destination = s.0.join("version");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("engine"), b"damage").unwrap();
        fs::write(s.0.join("clinical-sentinel"), b"keep").unwrap();
        activate(&s.0, &s.stage(), &destination, verified).unwrap();
        assert!(verified(&destination));
        let q: Vec<_> = fs::read_dir(&s.0)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|p| p.file_name().to_string_lossy().starts_with("quarantine-"))
            .collect();
        assert_eq!(q.len(), 1);
        assert_eq!(fs::read(q[0].path().join("engine")).unwrap(), b"damage");
        assert_eq!(fs::read(s.0.join("clinical-sentinel")).unwrap(), b"keep");
    }
    #[test]
    fn activate_never_replaces_valid_concurrent_installation() {
        let s = Scratch::new();
        let destination = s.0.join("version");
        let installed = s.stage();
        fs::rename(installed, &destination).unwrap();
        fs::write(destination.join("other-process-marker"), b"keep").unwrap();
        let stage = s.stage();
        activate(&s.0, &stage, &destination, verified).unwrap();
        assert!(stage.exists());
        assert_eq!(
            fs::read(destination.join("other-process-marker")).unwrap(),
            b"keep"
        );
        assert!(
            !fs::read_dir(&s.0)
                .unwrap()
                .filter_map(Result::ok)
                .any(|p| p.file_name().to_string_lossy().starts_with("quarantine-"))
        );
    }
    #[test]
    fn activate_concurrent_repairs_only_quarantine_damage_once() {
        let s = Scratch::new();
        let destination = s.0.join("version");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("engine"), b"damage").unwrap();
        let a = s.stage();
        let b = s.stage();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        std::thread::scope(|scope| {
            for stage in [a, b] {
                let barrier = barrier.clone();
                let root = &s.0;
                let destination = &destination;
                scope.spawn(move || {
                    barrier.wait();
                    activate(root, &stage, destination, verified).unwrap();
                });
            }
        });
        assert!(verified(&destination));
        assert_eq!(
            fs::read_dir(&s.0)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|p| p.file_name().to_string_lossy().starts_with("quarantine-"))
                .count(),
            1
        );
    }
    #[test]
    fn activate_locked_version_asks_to_close_other_window() {
        use std::os::windows::fs::OpenOptionsExt;
        let s = Scratch::new();
        let destination = s.0.join("version");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("engine"), b"damage").unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS)
            .open(&destination)
            .unwrap();
        let error = activate(&s.0, &s.stage(), &destination, verified).unwrap_err();
        assert!(error.contains("закройте"));
        assert_eq!(fs::read(destination.join("engine")).unwrap(), b"damage");
        drop(held);
    }
    #[test]
    fn archive_paths_are_confined() {
        for bad in ["../x", "/x", "a/../b", "a\\b", "C:/x", "a//b", ""] {
            assert!(!safe_path(bad));
        }
        assert!(safe_path("chrome/a.exe"));
    }
    #[test]
    fn manifest_pins_every_component_and_licenses() {
        let m: Manifest = serde_json::from_str(MANIFEST).unwrap();
        let paths: std::collections::BTreeSet<_> = m.files.iter().map(|f| &f.path).collect();
        assert_eq!(paths.len(), m.files.len());
        assert!(
            m.files
                .iter()
                .all(|f| safe_path(&f.path) && f.size > 0 && f.sha256.len() == 64)
        );
        for name in [
            "chrome/chrome-headless-shell.exe",
            "chrome/LICENSE.headless_shell",
            "pdfjs/pdf.mjs",
            "pdfjs/pdf.worker.mjs",
            "pdfjs/LICENSE",
        ] {
            assert!(m.files.iter().any(|f| f.path == name));
        }
    }
}
