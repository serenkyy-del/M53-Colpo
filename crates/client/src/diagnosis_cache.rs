//! A complete, encrypted catalogue snapshot. Never stores clinical selections.
use crate::{
    cache::Store,
    connection::Connection,
    diagnoses::{Catalog, Download},
};
use serde::{Deserialize, Serialize};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;

const FILE: &str = "diagnoses-catalog-v1";
const MAX_PAGES: usize = 512;

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub catalog: Option<Arc<Catalog>>,
    pub priority: Option<Arc<crate::diagnosis_priority::Group>>,
    pub priority_error: String,
    pub loading: bool,
    pub error: String,
    pub revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Scope {
    server: String,
    organization: Uuid,
    user: Uuid,
}
impl Scope {
    pub(crate) fn from_connection(raw: &Connection) -> Option<Self> {
        let identity = raw.identity.as_ref()?;
        (raw.user_id == Some(identity.user_id)
            && !identity.user_id.is_nil()
            && !identity.organization_id.is_nil())
        .then(|| Self {
            server: raw.storage_base.clone(),
            organization: identity.organization_id,
            user: identity.user_id,
        })
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    format: u32,
    scope: Scope,
    catalog: Catalog,
}
impl Envelope {
    fn validate(&self, scope: &Scope) -> Result<(), String> {
        if self.format != 1 || &self.scope != scope {
            return Err("Кеш диагнозов относится к другому профилю или версии".into());
        }
        self.catalog.validate()
    }
}

pub(crate) fn download(
    current: Option<&Catalog>,
    mut get: impl FnMut(&str) -> Result<Vec<u8>, String>,
    cancel: impl Fn() -> bool,
) -> Result<Option<Catalog>, String> {
    let started = Instant::now();
    let cancelled = || cancel() || started.elapsed() > Duration::from_secs(120);
    if cancelled() {
        return Err("Загрузка каталога отменена".into());
    }
    let capabilities: serde_json::Value = serde_json::from_slice(&get("/api/v1/capabilities")?)
        .map_err(|_| "Не удалось проверить поддержку диагнозов сервером")?;
    if capabilities["diagnoses_v1"] != true {
        return Err("Сервер ещё не поддерживает диагнозы по МКБ‑10".into());
    }
    let metadata: m53_contracts::diagnoses::Metadata =
        serde_json::from_slice(&get("/api/v1/diagnoses/catalog")?)
            .map_err(|_| "Некорректная версия каталога диагнозов")?;
    let mut stage = Download::new(metadata.clone())?;
    if let Some(old) = current.filter(|old| old.metadata.version_id == metadata.version_id) {
        old.validate()?;
        if old.metadata != metadata {
            return Err("Версия каталога диагнозов изменилась без нового идентификатора".into());
        }
        return Ok(None);
    }
    for page in 0..MAX_PAGES {
        if cancelled() {
            return Err("Загрузка каталога отменена".into());
        }
        let mut path = format!(
            "/api/v1/diagnoses/catalog/page?version_id={}&limit=500",
            metadata.version_id
        );
        if let Some(after) = stage.next_after() {
            path.push_str(&format!("&after={after}"));
        }
        let data = serde_json::from_slice(&get(&path)?)
            .map_err(|_| "Некорректная страница каталога диагнозов")?;
        if let Some(complete) = stage.push(data)? {
            return Ok(Some(complete));
        }
        if page + 1 < MAX_PAGES {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    Err("Превышен предел страниц каталога диагнозов".into())
}

pub(crate) struct State {
    pub(crate) scope: Option<Scope>,
    pub(crate) snapshot: Snapshot,
    pub(crate) refresh: bool,
    pub(crate) priority_refresh: bool,
}
impl State {
    pub(crate) fn empty() -> Self {
        Self {
            scope: None,
            snapshot: Snapshot::default(),
            refresh: false,
            priority_refresh: false,
        }
    }
    pub(crate) fn load(store: &Store, raw: &Connection) -> Self {
        let mut state = Self::empty();
        state.scope = Scope::from_connection(raw);
        let Some(scope) = &state.scope else {
            return state;
        };
        match crate::diagnosis_priority::load(store, scope) {
            Ok(group) => state.snapshot.priority = group.map(Arc::new),
            Err(error) => state.snapshot.priority_error = error,
        }
        let opened = (|| -> Result<Option<Catalog>, String> {
            let Some(bytes) = store.read(FILE)? else {
                return Ok(None);
            };
            let envelope: Envelope =
                serde_json::from_slice(&bytes).map_err(|_| "Повреждён кеш каталога диагнозов")?;
            envelope.validate(scope)?;
            Ok(Some(envelope.catalog))
        })();
        match opened {
            Ok(catalog) => state.snapshot.catalog = catalog.map(Arc::new),
            Err(error) => state.snapshot.error = error,
        }
        state.refresh = true;
        state.priority_refresh = true;
        state
    }
    pub(crate) fn status(&mut self, loading: bool, error: String) {
        if self.snapshot.loading != loading || self.snapshot.error != error {
            self.snapshot.loading = loading;
            self.snapshot.error = error;
            self.snapshot.revision += 1;
        }
    }
    pub(crate) fn publish_priority(&mut self, store: &Store, scope: &Scope, group: crate::diagnosis_priority::Group) -> Result<(), String> {
        if self.scope.as_ref() != Some(scope) { return Err("Профиль группы диагнозов изменился".into()); }
        if self.snapshot.priority.as_deref() == Some(&group) {
            if !self.snapshot.priority_error.is_empty() { self.snapshot.priority_error.clear(); self.snapshot.revision += 1; }
            return Ok(());
        }
        if let Some(old) = self.snapshot.priority.as_deref() {
            if group.version < old.version || (group.version > 0 && group.version == old.version && group != *old) {
                return Err("Сервер вернул устаревшую или противоречивую группу диагнозов".into());
            }
        }
        crate::diagnosis_priority::save(store, scope, &group)?;
        if self.snapshot.priority.as_deref() != Some(&group) || !self.snapshot.priority_error.is_empty() {
            self.snapshot.priority = Some(Arc::new(group));
            self.snapshot.revision += 1;
        }
        self.snapshot.priority_error.clear();
        Ok(())
    }
    pub(crate) fn rebind(&mut self, store: &Store, raw: &Connection) {
        if self.scope != Scope::from_connection(raw) {
            let revision = self.snapshot.revision.saturating_add(1);
            *self = Self::load(store, raw);
            self.snapshot.revision = revision;
        } else {
            self.refresh = true;
            self.priority_refresh = true;
        }
    }
    pub(crate) fn publish(
        &mut self,
        store: &Store,
        scope: &Scope,
        catalog: Catalog,
    ) -> Result<(), String> {
        if self.scope.as_ref() != Some(scope) {
            return Err("Профиль каталога диагнозов изменился".into());
        }
        catalog.validate()?;
        let envelope = Envelope {
            format: 1,
            scope: scope.clone(),
            catalog,
        };
        let bytes =
            serde_json::to_vec(&envelope).map_err(|_| "Не удалось сохранить каталог диагнозов")?;
        store.write(FILE, &bytes)?;
        self.snapshot.catalog = Some(Arc::new(envelope.catalog));
        self.snapshot.revision += 1;
        self.status(false, String::new());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use m53_contracts::diagnoses::{Entry, Metadata, Page};
    use serde_json::json;
    use std::collections::VecDeque;
    fn catalog() -> Catalog {
        Catalog {
            metadata: Metadata {
                version_id: Uuid::from_u128(1),
                source_id: Uuid::from_u128(2),
                synced_at: "2026-10-26T00:00:00Z".into(),
                count: 2,
            },
            items: (3..=4)
                .map(|n| Entry {
                    guid: Uuid::from_u128(n),
                    code: format!("N{n}"),
                    name: format!("Синтетический {n}"),
                    full_name: String::new(),
                    active: true,
                    is_folder: false,
                    source_version: "v1".into(),
                })
                .collect(),
        }
    }
    #[test]
    fn diagnoses_loader_returns_only_a_complete_pinned_version_and_never_searches_network() {
        let expected = catalog();
        let mut queue = VecDeque::from([
            serde_json::to_vec(&json!({"diagnoses_v1":true})).unwrap(),
            serde_json::to_vec(&expected.metadata).unwrap(),
            serde_json::to_vec(&Page {
                version_id: expected.metadata.version_id,
                items: vec![expected.items[0].clone()],
                next_after: Some(expected.items[0].guid),
            })
            .unwrap(),
            serde_json::to_vec(&Page {
                version_id: expected.metadata.version_id,
                items: vec![expected.items[1].clone()],
                next_after: None,
            })
            .unwrap(),
        ]);
        let mut paths = vec![];
        let got = download(
            None,
            |path| {
                paths.push(path.to_owned());
                Ok(queue.pop_front().unwrap())
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(got, expected);
        assert!(paths[2].contains("version_id=00000000-0000-0000-0000-000000000001"));
        assert!(paths[3].contains("after=00000000-0000-0000-0000-000000000003"));
        assert!(paths.iter().all(|p| !p.contains("q=")));
        assert!(queue.is_empty());
    }
    #[test]
    fn diagnoses_loader_unchanged_version_does_not_download_all_pages() {
        let current = catalog();
        let mut calls = 0;
        assert!(
            download(
                Some(&current),
                |path| {
                    calls += 1;
                    Ok(if path == "/api/v1/capabilities" {
                        serde_json::to_vec(&json!({"diagnoses_v1":true})).unwrap()
                    } else {
                        serde_json::to_vec(&current.metadata).unwrap()
                    })
                },
                || false
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(calls, 2);
    }
    #[test]
    fn diagnoses_loader_requires_explicit_server_wire_capability() {
        let expected = catalog();
        let mut paths = vec![];
        let result = download(
            None,
            |path| {
                paths.push(path.to_string());
                if path == "/api/v1/capabilities" {
                    Ok(serde_json::to_vec(&json!({"diagnoses_v1":false})).unwrap())
                } else if path == "/api/v1/diagnoses/catalog" {
                    Ok(serde_json::to_vec(&expected.metadata).unwrap())
                } else {
                    Ok(serde_json::to_vec(&Page {
                        version_id: expected.metadata.version_id,
                        items: expected.items.clone(),
                        next_after: None,
                    })
                    .unwrap())
                }
            },
            || false,
        );
        assert!(
            result.is_err(),
            "an old server cannot accept diagnosis snapshots"
        );
        assert_eq!(paths, vec!["/api/v1/capabilities"]);
    }
    #[test]
    fn diagnoses_loader_failure_cancellation_or_version_mix_never_replaces_last_good_cache() {
        let current = catalog();
        let original = current.clone();
        for reply in [
            Err("offline".into()),
            Ok(serde_json::to_vec(
                &json!({"version_id":Uuid::from_u128(9),"items":[],"next_after":null}),
            )
            .unwrap()),
        ] {
            let mut calls = 0;
            assert!(
                download(
                    None,
                    |path| {
                        if path == "/api/v1/capabilities" {
                            return Ok(serde_json::to_vec(&json!({"diagnoses_v1":true})).unwrap());
                        }
                        calls += 1;
                        if calls == 1 {
                            Ok(serde_json::to_vec(&current.metadata).unwrap())
                        } else {
                            reply.clone()
                        }
                    },
                    || false
                )
                .is_err()
            );
            assert_eq!(current, original);
        }
        let mut calls = 0;
        assert!(
            download(
                None,
                |_| {
                    calls += 1;
                    Ok(vec![])
                },
                || true
            )
            .is_err()
        );
        assert_eq!(calls, 0);
    }
    #[test]
    fn diagnoses_cache_rejects_foreign_org_user_server_or_corrupt_payload() {
        let scope = Scope {
            server: "https://synthetic.invalid".into(),
            organization: Uuid::from_u128(5),
            user: Uuid::from_u128(6),
        };
        let env = Envelope {
            format: 1,
            scope: scope.clone(),
            catalog: catalog(),
        };
        assert!(env.validate(&scope).is_ok());
        for other in [
            Scope {
                server: "https://other.invalid".into(),
                ..scope.clone()
            },
            Scope {
                organization: Uuid::from_u128(7),
                ..scope.clone()
            },
            Scope {
                user: Uuid::from_u128(8),
                ..scope.clone()
            },
        ] {
            assert!(env.validate(&other).is_err());
        }
        let mut bad = env;
        bad.catalog.items.pop();
        assert!(bad.validate(&scope).is_err());
    }
    fn connection() -> Connection {
        let mut raw = Connection::new("https://synthetic.invalid", "test").unwrap();
        raw.user_id = Some(Uuid::from_u128(6));
        raw.identity = Some(crate::ui_guidance::Identity {
            organization_id: Uuid::from_u128(5),
            user_id: Uuid::from_u128(6),
            doctor_id: Uuid::from_u128(7),
            workstation_id: Uuid::from_u128(8),
        });
        raw
    }
    #[test]
    fn priority_is_published_only_after_durable_write_and_scoped_reload() {
        use crate::cache::{AesProtector, Protector};
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Cipher(Arc<AtomicBool>);
        impl Protector for Cipher {
            fn protect(&self, b: &[u8]) -> Result<Vec<u8>, String> {
                if self.0.load(Ordering::SeqCst) { Err("synthetic write failure".into()) }
                else { AesProtector([71; 32]).protect(b) }
            }
            fn unprotect(&self, b: &[u8]) -> Result<Vec<u8>, String> { AesProtector([71; 32]).unprotect(b) }
        }
        let root = std::env::temp_dir().join(format!("priority-cache-{}", Uuid::new_v4()));
        let fail = Arc::new(AtomicBool::new(false));
        let store = Store::new(root.clone(), Arc::new(Cipher(fail.clone()))).unwrap();
        let raw = connection();
        let scope = Scope::from_connection(&raw).unwrap();
        let mut state = State::load(&store, &raw);
        let group = crate::diagnosis_priority::Group { version: 1, catalog_version_id: None, diagnosis_ids: vec![Uuid::from_u128(3)] };
        state.publish_priority(&store, &scope, group.clone()).unwrap();
        assert_eq!(State::load(&store, &raw).snapshot.priority.as_deref(), Some(&group));
        state.snapshot.priority_error = "offline".into();
        let before_recovery = state.snapshot.revision;
        state.publish_priority(&store, &scope, group.clone()).unwrap();
        assert!(state.snapshot.priority_error.is_empty());
        assert!(state.snapshot.revision > before_recovery);
        assert!(state.publish_priority(&store, &scope, crate::diagnosis_priority::Group { version: 1, diagnosis_ids: vec![], catalog_version_id: None }).is_err());
        assert!(state.publish_priority(&store, &scope, crate::diagnosis_priority::Group { version: 0, catalog_version_id: Some(Uuid::from_u128(5)), diagnosis_ids: vec![] }).is_err());
        let revision = state.snapshot.revision;
        fail.store(true, Ordering::SeqCst);
        assert!(state.publish_priority(&store, &scope, crate::diagnosis_priority::Group { version: 2, ..group.clone() }).is_err());
        assert_eq!(state.snapshot.priority.as_deref(), Some(&group));
        assert_eq!(state.snapshot.revision, revision);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn diagnoses_cache_durable_reload_failed_write_and_profile_revision() {
        use crate::cache::{AesProtector, Protector};
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Cipher(Arc<AtomicBool>);
        impl Protector for Cipher {
            fn protect(&self, b: &[u8]) -> Result<Vec<u8>, String> {
                if self.0.load(Ordering::SeqCst) {
                    Err("synthetic disk failure".into())
                } else {
                    AesProtector([73; 32]).protect(b)
                }
            }
            fn unprotect(&self, b: &[u8]) -> Result<Vec<u8>, String> {
                AesProtector([73; 32]).unprotect(b)
            }
        }
        let root = std::env::temp_dir().join(format!("m53-diagnoses-{}", Uuid::new_v4()));
        let failing = Arc::new(AtomicBool::new(false));
        let store = Store::new(root.clone(), Arc::new(Cipher(failing.clone()))).unwrap();
        let mut raw = connection();
        let scope = Scope::from_connection(&raw).unwrap();
        let mut state = State::load(&store, &raw);
        state.publish(&store, &scope, catalog()).unwrap();
        let bytes = std::fs::read(root.join(format!("{FILE}.dpapi"))).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("Синтетический"));
        let before = state.snapshot.catalog.clone().unwrap();
        assert_eq!(
            State::load(&store, &raw).snapshot.catalog.unwrap().as_ref(),
            before.as_ref()
        );
        failing.store(true, Ordering::SeqCst);
        let mut replacement = catalog();
        replacement.metadata.version_id = Uuid::from_u128(50);
        assert!(state.publish(&store, &scope, replacement).is_err());
        assert_eq!(
            state.snapshot.catalog.as_ref().unwrap().as_ref(),
            before.as_ref()
        );
        assert_eq!(
            State::load(&store, &raw).snapshot.catalog.unwrap().as_ref(),
            before.as_ref()
        );
        let revision = state.snapshot.revision;
        raw.identity.as_mut().unwrap().organization_id = Uuid::from_u128(99);
        state.rebind(&store, &raw);
        assert!(
            state.snapshot.catalog.is_none(),
            "foreign profile must not reuse old catalog"
        );
        assert!(
            state.snapshot.revision > revision,
            "profile reset must notify the UI even with no clinical change"
        );
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}
