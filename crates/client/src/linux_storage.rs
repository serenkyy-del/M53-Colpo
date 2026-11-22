//! Platform storage for the standalone Linux client.
use crate::cache::{AesProtector, Store};
use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::{ErrorKind, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

pub fn app_root() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|path| PathBuf::from(path).join(".local/share")))
        .ok_or("Не задан каталог локальных данных")?;
    Ok(base.join(crate::APP_DATA_DIR))
}

pub fn open(root: &Path, namespace: impl AsRef<Path>) -> Result<Store, String> {
    let namespace = namespace.as_ref();
    if namespace.as_os_str().is_empty()
        || !namespace.components().all(|part| matches!(part, Component::Normal(_)))
    {
        return Err("Некорректный каталог профиля".into());
    }
    private_directory(root)?;
    let path = namespace.components().fold(root.to_path_buf(), |mut path, part| {
        path.push(part);
        path
    });
    let mut child = root.to_path_buf();
    for part in namespace.components() {
        child.push(part);
        private_directory(&child)?;
    }
    let key = read_or_create_key(root)?;
    Store::new(path, Arc::new(AesProtector(key)))
}

fn private_directory(path: &Path) -> Result<(), String> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|_| "Не удалось создать каталог профиля")?;
    let metadata = fs::symlink_metadata(path).map_err(|_| "Не удалось проверить каталог")?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err("У каталога профиля должен быть режим доступа 0700".into());
    }
    Ok(())
}

fn read_or_create_key(root: &Path) -> Result<[u8; 32], String> {
    let path = root.join("local.key");
    if matches!(fs::symlink_metadata(&path), Err(e) if e.kind() == ErrorKind::NotFound) {
        use ring::rand::SecureRandom;
        let mut key = [0u8; 32];
        ring::rand::SystemRandom::new()
            .fill(&mut key)
            .map_err(|_| "Не удалось создать ключ профиля")?;
        let pending = root.join(format!(".key-{}", uuid::Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&pending)
            .map_err(|_| "Не удалось сохранить ключ профиля")?;
        let result = file.write_all(&key).and_then(|()| file.sync_all()).and_then(|()| {
            match fs::hard_link(&pending, &path) {
                Ok(()) => fs::File::open(root)?.sync_all(),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(()),
                Err(e) => Err(e),
            }
        });
        drop(file);
        let _ = fs::remove_file(pending);
        result.map_err(|_| "Не удалось сохранить ключ профиля")?;
    }
    let metadata = fs::symlink_metadata(&path).map_err(|_| "Не удалось прочитать ключ профиля")?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 || metadata.len() != 32 {
        return Err("Недопустимый локальный ключ профиля".into());
    }
    fs::read(path)
        .map_err(|_| "Не удалось прочитать ключ профиля")?
        .try_into()
        .map_err(|_| "Недопустимый локальный ключ профиля".into())
}
