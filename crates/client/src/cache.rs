//! Encrypted, crash-safe local storage. A failed read never becomes an empty replacement cache.
use std::{
    collections::{BTreeMap, VecDeque},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};
/// Shared immutable bytes with explicit LRU limits. Unsaved originals may be
/// pinned until their disk job completes; small derived previews need no pin.
#[derive(Default)]
pub(crate) struct MemoryCache {
    values: BTreeMap<uuid::Uuid, Arc<Vec<u8>>>,
    order: VecDeque<uuid::Uuid>,
    bytes: usize,
}
impl MemoryCache {
    pub(crate) fn get(&mut self, id: &uuid::Uuid) -> Option<Arc<Vec<u8>>> {
        let bytes = self.values.get(id)?.clone();
        self.order.retain(|old| old != id);
        self.order.push_back(*id);
        Some(bytes)
    }
    pub(crate) fn contains_key(&self, id: &uuid::Uuid) -> bool {
        self.values.contains_key(id)
    }
    pub(crate) fn insert(&mut self, id: uuid::Uuid, bytes: Arc<Vec<u8>>) {
        if let Some(old) = self.values.insert(id, bytes.clone()) { self.bytes -= old.len(); }
        self.bytes += bytes.len();
        self.order.retain(|old| old != &id);
        self.order.push_back(id);
    }
    pub(crate) fn trim(&mut self, max_bytes: usize, max_entries: usize, may_evict: impl Fn(&uuid::Uuid) -> bool) {
        let mut candidates = self.order.len();
        while candidates > 0 && (self.bytes > max_bytes || self.values.len() > max_entries) {
            candidates -= 1;
            let Some(id) = self.order.pop_front() else { break; };
            if may_evict(&id) {
                if let Some(bytes) = self.values.remove(&id) { self.bytes -= bytes.len(); }
            } else {
                self.order.push_back(id);
            }
        }
    }
}
pub trait Protector: Send + Sync {
    fn protect(&self, bytes: &[u8]) -> Result<Vec<u8>, String>;
    fn unprotect(&self, bytes: &[u8]) -> Result<Vec<u8>, String>;
}
#[cfg(windows)]
pub struct Dpapi;
#[cfg(windows)]
impl Protector for Dpapi {
    fn protect(&self, b: &[u8]) -> Result<Vec<u8>, String> {
        crate::credentials::transform(b, true)
    }
    fn unprotect(&self, b: &[u8]) -> Result<Vec<u8>, String> {
        crate::credentials::transform(b, false)
    }
}
/// Explicit key provider for portable integration tests. The Windows app uses DPAPI.
pub struct AesProtector(pub [u8; 32]);
impl Protector for AesProtector {
    fn protect(&self, b: &[u8]) -> Result<Vec<u8>, String> {
        use aes_gcm::{
            Aes256Gcm, KeyInit, Nonce,
            aead::{Aead, Payload},
        };
        let id = uuid::Uuid::new_v4();
        let nonce = &id.as_bytes()[..12];
        let cipher = Aes256Gcm::new_from_slice(&self.0).unwrap();
        let mut out = nonce.to_vec();
        out.extend(
            cipher
                .encrypt(
                    Nonce::from_slice(nonce),
                    Payload {
                        msg: b,
                        aad: b"M53 offline cache v1",
                    },
                )
                .map_err(|_| "Cache encryption failed")?,
        );
        Ok(out)
    }
    fn unprotect(&self, b: &[u8]) -> Result<Vec<u8>, String> {
        use aes_gcm::{
            Aes256Gcm, KeyInit, Nonce,
            aead::{Aead, Payload},
        };
        if b.len() < 28 {
            return Err("Cache truncated".into());
        }
        Aes256Gcm::new_from_slice(&self.0)
            .unwrap()
            .decrypt(
                Nonce::from_slice(&b[..12]),
                Payload {
                    msg: &b[12..],
                    aad: b"M53 offline cache v1",
                },
            )
            .map_err(|_| "Cache authentication failed".into())
    }
}
#[derive(Clone)]
pub struct Store {
    root: PathBuf,
    cipher: Arc<dyn Protector>,
    lease: Arc<std::sync::RwLock<Option<std::fs::File>>>,
}
impl Store {
    pub fn new(root: PathBuf, cipher: Arc<dyn Protector>) -> Result<Self, String> {
        std::fs::create_dir_all(&root).map_err(|_| "Не удалось создать локальный кеш")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| "Cache permissions failed")?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(root.join("cache.lock"))
            .map_err(|_| "Не удалось открыть блокировку кеша")?;
        lock.try_lock().map_err(|_|"Кеш этого врача уже открыт другим экземпляром; неподтвержденные данные не перезаписываются")?;
        Ok(Self {
            root,
            cipher,
            lease: Arc::new(std::sync::RwLock::new(Some(lock))),
        })
    }
    /// Read-only clones can outlive the account session. Its writer lease cannot.
    pub(crate) fn try_release(&self) -> Result<bool, String> {
        let mut lease = match self.lease.try_write() {
            Ok(lease) => lease,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(false),
            Err(_) => return Err("Блокировка локального кеша повреждена".into()),
        };
        lease.take(); // Closing the file releases the OS lock; cache.lock stays in place.
        Ok(true)
    }
    pub(crate) fn renew_if_closed(self) -> Result<Self, String> {
        let closed = self.lease.read().map_err(|_| "Блокировка локального кеша повреждена")?.is_none();
        if closed { Self::new(self.root.clone(), self.cipher.clone()) } else { Ok(self) }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn path(&self, name: &str) -> Result<PathBuf, String> {
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err("Invalid cache key".into());
        }
        Ok(self.root.join(format!("{name}.dpapi")))
    }
    pub fn write(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        // Keep a shared lease through encryption and atomic replacement. Release
        // takes the exclusive guard, so no old clone can write into a new session.
        let lease = self.lease.read().map_err(|_| "Блокировка локального кеша повреждена")?;
        if lease.is_none() { return Err("Локальный кеш закрыт для записи".into()); }
        let encrypted = self.cipher.protect(bytes)?;
        let path = self.path(name)?;
        let temp = path.with_extension("pending");
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temp)
            .map_err(|_| "Локальный кеш: не удалось записать файл (проверьте диск)")?;
        file.write_all(&encrypted)
            .and_then(|_| file.sync_all())
            .map_err(|_| "Локальный кеш: ошибка диска; данные еще не сохранены")?;
        drop(file);
        atomic_replace(&temp, &path)?;
        #[cfg(unix)]
        std::fs::File::open(&self.root)
            .and_then(|f| f.sync_all())
            .map_err(|_| "Cache directory sync failed")?;
        Ok(())
    }
    pub fn read(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        let path = self.path(name)?;
        match std::fs::read(path) {
            Ok(bytes) => self.cipher.unprotect(&bytes).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err("Не удалось прочитать локальный кеш".into()),
        }
    }
}
pub(crate) fn atomic_replace(temp: &Path, path: &Path) -> Result<(), String> {
    #[cfg(not(windows))]
    {
        std::fs::rename(temp, path).map_err(|_| "Cache atomic rename failed".into())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        let from: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let ok = unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok == 0 {
            Err("Cache atomic replace failed".into())
        } else {
            Ok(())
        }
    }
}
