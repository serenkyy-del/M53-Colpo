//! Read-only, bounded certificate loading. Never contains an issuer/root private key.
use axum_server::tls_rustls::RustlsConfig;
use rustls::{
    RootCertStore, ServerConfig,
    client::{WebPkiServerVerifier, danger::ServerCertVerifier},
};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime, pem::PemObject};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;
type Error = Box<dyn std::error::Error + Send + Sync>;
#[derive(Clone)]
pub(crate) struct TlsFiles {
    directory: PathBuf,
    roots: PathBuf,
    names: Vec<String>,
    interval: Duration,
}
#[derive(Clone, Serialize)]
pub(crate) struct TlsSnapshot {
    pub ready: bool,
    pub generation: String,
    pub certificate_sha256: String,
    pub not_before: u64,
    pub not_after: u64,
    pub last_checked: u64,
    pub last_reload: u64,
    pub safe_error: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    generation: String,
    leaf_sha256: String,
    not_before: u64,
    not_after: u64,
}
impl TlsFiles {
    pub(crate) fn from_env(origin: &str) -> Result<Self, Error> {
        let directory = PathBuf::from(env::var("M53_TLS_DIRECTORY")?);
        let roots = PathBuf::from(env::var("M53_TLS_ROOT_CA_FILE")?);
        if !directory.is_absolute() || !roots.is_absolute() {
            return Err("absolute_tls_paths_required".into());
        }
        let canonical = reqwest::Url::parse(origin)?
            .host_str()
            .ok_or("tls_server_name_required")?
            .trim_matches(['[', ']'])
            .to_string();
        let mut names = env::var("M53_TLS_REQUIRED_NAMES")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if !names.contains(&canonical) {
            names.push(canonical)
        }
        if names.len() > 8 {
            return Err("too_many_tls_names".into());
        }
        for name in &names {
            ServerName::try_from(name.clone()).map_err(|_| "invalid_tls_name")?;
        }
        let seconds = env::var("M53_TLS_RELOAD_SECONDS")
            .unwrap_or_else(|_| "60".into())
            .parse::<u64>()?;
        if !(1..=3600).contains(&seconds) {
            return Err("invalid_tls_reload_interval".into());
        }
        Ok(Self {
            directory,
            roots,
            names,
            interval: Duration::from_secs(seconds),
        })
    }
    pub(crate) async fn load(&self) -> Result<(ServerConfig, TlsSnapshot, Vec<String>), Error> {
        let files = self.clone();
        tokio::task::spawn_blocking(move || files.load_blocking())
            .await
            .map_err(|_| "tls_load_worker_failed")?
    }
    fn load_blocking(&self) -> Result<(ServerConfig, TlsSnapshot, Vec<String>), Error> {
        let pointer = fs::read_link(self.directory.join("current"))?;
        let parts = pointer.iter().collect::<Vec<_>>();
        if pointer.is_absolute() || parts.len() != 2 || parts[0] != "generations" {
            return Err("invalid_tls_generation_pointer".into());
        }
        let generation = parts[1]
            .to_str()
            .ok_or("invalid_tls_generation")?
            .to_string();
        if Uuid::parse_str(&generation)?.to_string() != generation {
            return Err("invalid_tls_generation".into());
        }
        let base = self.directory.join("generations").canonicalize()?;
        let path = self.directory.join(&pointer).canonicalize()?;
        if !path.starts_with(&base) || path.parent() != Some(base.as_path()) {
            return Err("invalid_tls_generation_path".into());
        }
        for dir in [&self.directory, &base, &path] {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if fs::metadata(dir)?.permissions().mode() & 0o022 != 0 {
                    return Err("tls_directory_writable_by_others".into());
                }
            }
        }
        let manifest: Manifest =
            serde_json::from_slice(&read(&path.join("manifest.json"), 8192, false)?)?;
        let now = UnixTime::now();
        if manifest.schema_version != 1
            || manifest.generation != generation
            || manifest.not_before > now.as_secs()
            || manifest.not_after <= now.as_secs()
        {
            return Err("invalid_tls_manifest".into());
        }
        let chain =
            CertificateDer::pem_slice_iter(&read(&path.join("fullchain.pem"), 512 * 1024, false)?)
                .collect::<Result<Vec<_>, _>>()?;
        if chain.is_empty()
            || chain.len() > 4
            || crate::api::hash(chain[0].as_ref()) != manifest.leaf_sha256
        {
            return Err("invalid_tls_chain_manifest".into());
        }
        let key = PrivateKeyDer::from_pem_slice(&read(&path.join("key.pem"), 32768, true)?)?;
        let roots = CertificateDer::pem_slice_iter(&read(&self.roots, 512 * 1024, false)?)
            .collect::<Result<Vec<_>, _>>()?;
        if roots.is_empty() || roots.len() > 4 {
            return Err("invalid_tls_roots".into());
        }
        let fingerprints = roots
            .iter()
            .map(|c| crate::api::hash(c.as_ref()))
            .collect::<Vec<_>>();
        let mut store = RootCertStore::empty();
        for root in roots {
            store.add(root)?;
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier =
            WebPkiServerVerifier::builder_with_provider(Arc::new(store), provider.clone())
                .build()?;
        for name in &self.names {
            verifier.verify_server_cert(
                &chain[0],
                &chain[1..],
                &ServerName::try_from(name.clone())?,
                &[],
                now,
            )?;
        }
        let mut config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(chain, key)?;
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok((
            config,
            TlsSnapshot {
                ready: true,
                generation,
                certificate_sha256: manifest.leaf_sha256,
                not_before: manifest.not_before,
                not_after: manifest.not_after,
                last_checked: now.as_secs(),
                last_reload: now.as_secs(),
                safe_error: None,
            },
            fingerprints,
        ))
    }
    pub(crate) async fn watch(
        self,
        config: RustlsConfig,
        status: Arc<Mutex<TlsSnapshot>>,
        pinned_roots: Vec<String>,
    ) {
        let mut interval = tokio::time::interval(self.interval);
        interval.tick().await;
        loop {
            interval.tick().await;
            let loaded = self.load().await;
            let mut state = status.lock().unwrap_or_else(|e| e.into_inner());
            match loaded {
                Ok((candidate, mut snapshot, roots)) => {
                    if roots != pinned_roots {
                        state.last_checked = UnixTime::now().as_secs();
                        state.ready = state.not_before <= state.last_checked
                            && state.not_after > state.last_checked;
                        state.safe_error = Some("tls_root_change_rejected".into());
                        continue;
                    }
                    if snapshot.generation == state.generation
                        && (snapshot.certificate_sha256 != state.certificate_sha256
                            || snapshot.not_before != state.not_before
                            || snapshot.not_after != state.not_after)
                    {
                        state.last_checked = UnixTime::now().as_secs();
                        state.ready = state.not_before <= state.last_checked
                            && state.not_after > state.last_checked;
                        state.safe_error = Some("tls_generation_mutated".into());
                        continue;
                    }
                    if snapshot.generation != state.generation {
                        config.reload_from_config(Arc::new(candidate));
                    } else {
                        snapshot.last_reload = state.last_reload;
                    }
                    *state = snapshot;
                }
                Err(_) => {
                    let now = UnixTime::now().as_secs();
                    state.ready = state.not_before <= now && state.not_after > now;
                    state.last_checked = now;
                    state.safe_error = Some("tls_reload_rejected".into());
                    eprintln!("tls_reload_rejected");
                }
            }
        }
    }
}
fn read(path: &Path, limit: u64, key: bool) -> Result<Vec<u8>, Error> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.len() > limit {
        return Err("invalid_tls_file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if key && meta.permissions().mode() & 0o037 != 0 {
            return Err("tls_key_permissions".into());
        }
    }
    let _ = key;
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err("tls_file_too_large".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_files_and_symlinks() {
        let dir = std::env::temp_dir().join(format!("m53-tls-files-{}", Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let file = dir.join("key.pem");
        fs::write(&file, b"fixture").unwrap();
        assert!(read(&file, 3, false).is_err());
        assert_eq!(read(&file, 7, false).unwrap(), b"fixture");
        #[cfg(unix)]
        {
            use std::os::unix::{fs::PermissionsExt, fs::symlink};
            fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(read(&file, 7, true).is_err());
            fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
            assert!(read(&file, 7, true).is_ok());
            let link = dir.join("link");
            symlink(&file, &link).unwrap();
            assert!(read(&link, 7, false).is_err());
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
