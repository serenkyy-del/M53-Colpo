//! Durable client outbox. Only an authenticated, matching backend receipt acknowledges an operation.
use crate::{
    cache::{MemoryCache, Store},
    connection::Connection,
    patient_demographics::PatientSex,
};
use m53_contracts::{SavedProtocol, workflow::*};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;
#[path = "offline_sync_schedule.rs"]
mod sync_schedule;
use sync_schedule::{SyncSchedule, transport_outage};
#[path = "offline_workers.rs"]
mod workers;
use workers::Workers;
#[path = "offline_diagnoses.rs"]
mod diagnosis_network;
#[path = "offline_visit_templates.rs"]
mod visit_templates;
#[path = "offline_patient_edit.rs"]
mod patient_edit_network;
#[path = "offline_protocol_history.rs"]
mod protocol_history_network;
#[cfg(test)]
#[path = "offline_review_tests.rs"]
mod review_tests;
pub use visit_templates::{VisitTemplateDetail, VisitTemplatePage, VisitTemplateSummary};
fn digest(b: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(b.as_ref()))
}
const THUMBNAIL_PENDING: &str = "Миниатюра пока недоступна: сервер еще не подготовил preview";
const THUMBNAIL_MAGIC: &[u8] = b"M53THM01";
const RECOVERY_ERROR: &str = "Не удалось восстановить локальный файл: он отсутствует или повреждён. Данные и очередь сохранены; требуется проверка кеша";
fn thumbnail_key(id: Uuid) -> String {
    format!("thumbnail-v1-{}", id.simple())
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetFailure {
    Http(u16),
    Integrity,
    SessionRequired,
    Stopped,
}
impl AssetFailure {
    pub fn message(self) -> String {
        match self {
            Self::Http(401 | 403) | Self::SessionRequired => {
                "Нужен повторный вход для загрузки снимков отчета".into()
            }
            Self::Http(404 | 410) => {
                "Выбранный снимок не найден на сервере; отчет не сформирован".into()
            }
            Self::Http(status) => format!("Сервер отклонил загрузку выбранного снимка ({status})"),
            Self::Integrity => {
                "SHA-256 выбранного снимка не совпадает; отчет не сформирован".into()
            }
            Self::Stopped => "Подготовка отчета отменена при закрытии рабочего места".into(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Profile {
    pub server: String,
    pub user_id: Uuid,
    pub doctor: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Operation {
    pub id: Uuid,
    pub entity_id: Uuid,
    pub version_id: Option<Uuid>,
    pub path: String,
    pub method: String,
    pub body: Value,
    pub blob: Option<Uuid>,
    pub started: bool,
    pub blocked: Option<String>,
    pub sequence: u64,
}
impl Operation {
    pub fn scope(&self) -> &str {
        self.path.split('?').next().unwrap()
    }
    pub fn request_hash(&self) -> String {
        digest(m53_contracts::canonical_json::to_vec(&json!({"scope":self.scope(),"body":self.body,"entity_id":self.entity_id.to_string(),"version_id":self.version_id.map(|v|v.to_string())})).unwrap())
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Blob {
    pub file_id: Uuid,
    pub sha256: String,
    pub size: usize,
    pub persisted: bool,
    pub backend_confirmed: bool,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct LocalState {
    #[serde(skip)]
    pub activity: Option<crate::activity::Journal>,
    #[serde(skip)]
    pub activity_intents: BTreeMap<String, Uuid>,
    pub format_version: u32,
    pub profile: Profile,
    pub responses: BTreeMap<String, Value>,
    pub blobs: BTreeMap<Uuid, Blob>,
    pub pending: VecDeque<Operation>,
    pub confirmed: BTreeMap<Uuid, String>,
    pub journal: Value,
    #[serde(default)]
    pub editors: BTreeMap<Uuid, Value>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    server_only_studies: BTreeSet<Uuid>,
    pub remote_conflicts: BTreeMap<Uuid, Value>,
    pub conflict_history: Vec<Operation>,
    #[serde(default)]
    pub aliases: BTreeMap<Uuid, Uuid>,
    /// Local patient fields not supported by the current server contract.
    /// Kept outside response snapshots so a refresh cannot erase them.
    #[serde(default)]
    pub patient_sexes: BTreeMap<Uuid, PatientSex>,
    pub sequence: u64,
    #[serde(skip)]
    pub durable_sequence: u64,
    #[serde(skip)]
    metadata_generations: BTreeMap<String, u64>,
    /// Recomputed from encrypted files at attach; independent of transient HTTP errors.
    #[serde(skip)]
    recovery_errors: BTreeSet<Uuid>,
    #[serde(skip)]
    pub online: bool,
    #[serde(skip)]
    pub error: String,
}
fn approved_study(detail: &Value) -> Option<Uuid> {
    if detail.pointer("/study/status").and_then(Value::as_str) != Some("approved")
        || !detail["protocol"].is_object()
    {
        return None;
    }
    Uuid::parse_str(detail["revision_id"].as_str()?).ok().filter(|id| !id.is_nil())?;
    Uuid::parse_str(detail.pointer("/study/id")?.as_str()?).ok()
}

fn metadata_key(path: &str) -> String {
    if path == "/api/v1/patients" || path.starts_with("/api/v1/patients/page?") {
        "/api/v1/patients".into()
    } else if path == "/api/v1/studies" || path.starts_with("/api/v1/studies?") || path.starts_with("/api/v1/studies/summaries") {
        "/api/v1/studies".into()
    } else { path.into() }
}
impl LocalState {
    fn checkpoint_ready(&self) -> bool {
        self.durable_sequence >= self.sequence && self.blobs.iter().all(|(id, blob)| {
            blob.persisted || self.recovery_errors.contains(id)
        })
    }
    pub(crate) fn references_missing_file(&self, value: &Value) -> bool {
        if self.recovery_errors.is_empty() { return false; }
        match value {
            Value::String(value) => value.parse::<Uuid>().is_ok_and(|id| self.recovery_errors.contains(&id)),
            Value::Array(values) => values.iter().any(|value| self.references_missing_file(value)),
            Value::Object(values) => values.values().any(|value| self.references_missing_file(value)),
            _ => false,
        }
    }
    pub(crate) fn operation_has_missing_file(&self, operation: &Operation) -> bool {
        operation.blob.is_some_and(|id| self.recovery_errors.contains(&id))
            || self.references_missing_file(&operation.body)
    }
    pub(crate) fn study_has_missing_file(&self, id: Uuid) -> bool {
        if self.recovery_errors.is_empty() { return false; }
        self.responses.get(&format!("/api/v1/studies/{id}")).is_some_and(|value| self.references_missing_file(value))
            || self.editors.get(&id).is_some_and(|value| self.references_missing_file(value))
            || (self.journal.pointer("/detail/study/id") == Some(&json!(id)) && self.references_missing_file(&self.journal))
            || crate::outbox::study_indices(self,id).into_iter().any(|index| self.operation_has_missing_file(&self.pending[index]))
    }
    pub(crate) fn has_local_revision_conflict(&self, id: Uuid) -> bool {
        self.editors.get(&id).is_some_and(|editor| editor["revision_conflict"] == true)
    }

    fn retain_diverged_editor(&mut self, remote: &StudyDetail) {
        if remote.study.status != "draft" { return; }
        let id = remote.study.id;
        let Some(editor) = self.editors.get_mut(&id) else { return };
        if !crate::workspace::journal_needs_upload(editor) { return; }
        let Ok(local) = serde_json::from_value::<StudyDetail>(editor["detail"].clone()) else { return };
        let remote_newer = remote.study.row_version > local.study.row_version
            || (remote.study.row_version == local.study.row_version && remote.revision_id != local.revision_id);
        if local.study.patient_id != remote.study.patient_id || !remote_newer { return; }
        let same_input = local.protocol == remote.protocol
            && json!(local.annotations) == json!(remote.annotations);
        if same_input { return; }
        if editor["revision_conflict"] != true || self.remote_conflicts.get(&id) != Some(&json!(remote)) {
            editor["revision_conflict"] = json!(true);
            if self.journal.pointer("/detail/study/id") == Some(&json!(id)) {
                self.journal["revision_conflict"] = json!(true);
            }
            self.remote_conflicts.insert(id, json!(remote));
            self.change();
        }
    }

    fn evict_approved_protocols(&mut self) -> bool {
        let mut approved = BTreeMap::new();
        for (path, detail) in &self.responses {
            if let Some(id) = approved_study(detail)
                && *path == format!("/api/v1/studies/{id}")
            {
                approved.insert(id, detail);
            }
        }
        let is_confirmed_copy = |editor: &Value| {
            if crate::workspace::journal_needs_upload(editor) {
                return false;
            }
            let detail = &editor["detail"];
            let Some(id) = detail.pointer("/study/id").and_then(Value::as_str)
                .and_then(|id| Uuid::parse_str(id).ok()) else { return false };
            approved_study(detail) == Some(id) || approved.get(&id).is_some_and(|server| {
                detail["revision_id"] == server["revision_id"]
                    && detail["protocol"] == server["protocol"]
                    && detail["annotations"] == server["annotations"]
            })
        };
        let clear_journal = is_confirmed_copy(&self.journal);
        let old_editors = self.editors.len();
        self.editors.retain(|_, editor| !is_confirmed_copy(editor));
        if clear_journal {
            self.journal = Value::Null;
        }
        let approved_ids: BTreeSet<_> = approved.into_keys().collect();
        for id in &approved_ids {
            self.responses.remove(&format!("/api/v1/studies/{id}"));
            if crate::outbox::study_indices(self, *id).is_empty() && !self.has_local_revision_conflict(*id)
                && self.remote_conflicts.get(id).and_then(approved_study) == Some(*id)
            {
                self.remote_conflicts.remove(id);
            }
        }
        self.server_only_studies.extend(approved_ids.iter().copied());
        !approved_ids.is_empty() || clear_journal || self.editors.len() != old_editors
    }

    fn status_error(&self) -> String {
        if self.recovery_errors.is_empty() {
            self.error.clone()
        } else if self.error.is_empty() || self.error == RECOVERY_ERROR {
            RECOVERY_ERROR.into()
        } else {
            format!("{RECOVERY_ERROR}; {}", self.error)
        }
    }
    fn metadata_generation(&self, path: &str) -> u64 {
        self.metadata_generations.get(&metadata_key(path)).copied().unwrap_or(0)
    }
    fn touch_metadata(&mut self, path: &str) {
        *self.metadata_generations.entry(metadata_key(path)).or_default() += 1;
    }
    fn touch_operation(&mut self, path: &str) {
        if let Some(id) = study_id(path) {
            self.touch_metadata(&format!("/api/v1/studies/{id}"));
            self.touch_metadata("/api/v1/studies");
            if path.contains("/results/1c") { self.touch_metadata(&format!("/api/v1/studies/{id}/results/1c")); }
        } else { self.touch_metadata(path); }
    }
    fn change(&mut self) {
        self.sequence += 1;
    }
}
enum DiskJob {
    Blob(Uuid, Arc<Vec<u8>>),
    Thumbnail { asset: Uuid, bytes: Arc<Vec<u8>> },
    Flush(mpsc::Sender<Result<(), String>>),
}
#[derive(Default)]
struct FetchQueue {
    pending: VecDeque<String>,
    inflight: BTreeSet<String>,
    retry_after: BTreeMap<String, Instant>,
    attempts: BTreeMap<String, u32>,
    invalidated: BTreeSet<String>,
    terminal: BTreeMap<String, AssetFailure>,
}
impl FetchQueue {
    fn reject(&mut self, path: &str, failure: AssetFailure) {
        if self.terminal.len() >= 200 && !self.terminal.contains_key(path) {
            self.terminal.pop_first();
        }
        self.terminal.insert(path.into(), failure);
        self.pending.retain(|queued| queued != path);
    }
    fn enqueue(&mut self, path: &str, priority: bool, invalidate: bool) {
        if self.terminal.contains_key(path) {
            if !invalidate {
                return;
            }
            self.terminal.remove(path);
        }
        if self.inflight.contains(path) {
            if invalidate {
                self.invalidated.insert(path.into());
            }
            return;
        }
        if let Some(index) = self.pending.iter().position(|p| p == path) {
            if priority && index > 0 {
                let path = self.pending.remove(index).unwrap();
                self.pending.push_front(path);
            }
        } else if priority || self.pending.len() < 200 {
            if self.pending.len() >= 200 {
                if let Some(displaced) = self.pending.pop_back() {
                    self.retry_after.remove(&displaced);
                    self.attempts.remove(&displaced);
                }
            }
            if priority {
                self.pending.push_front(path.into());
            } else {
                self.pending.push_back(path.into());
            }
        }
    }
    fn take(&mut self) -> Option<String> {
        let now = Instant::now();
        let index = self
            .pending
            .iter()
            .position(|p| self.retry_after.get(p).is_none_or(|until| *until <= now))?;
        let path = self.pending.remove(index)?;
        self.inflight.insert(path.clone());
        Some(path)
    }
    fn complete(&mut self, path: &str) {
        self.inflight.remove(path);
        self.retry_after.remove(path);
        self.attempts.remove(path);
        self.terminal.remove(path);
        if self.invalidated.remove(path) {
            self.enqueue(path, true, false);
        }
    }
    fn failed(&mut self, path: &str) {
        self.inflight.remove(path);
        self.invalidated.remove(path);
        if self.terminal.contains_key(path) {
            return;
        }
        if self.pending.len() < 200 {
            let attempts = self.attempts.entry(path.into()).or_default();
            *attempts = (*attempts + 1).min(5);
            self.retry_after.insert(
                path.into(),
                Instant::now() + Duration::from_secs(1 << *attempts),
            );
            self.enqueue(path, false, false);
        } else {
            self.retry_after.remove(path);
            self.attempts.remove(path);
        }
    }
}
#[derive(Default)]
struct PerformanceCounters {
    memory_hits: std::sync::atomic::AtomicU64,
    disk_hits: std::sync::atomic::AtomicU64,
    asset_downloads: std::sync::atomic::AtomicU64,
    asset_download_bytes: std::sync::atomic::AtomicU64,
    asset_download_ms: std::sync::atomic::AtomicU64,
    metadata_requests: std::sync::atomic::AtomicU64,
    metadata_ms: std::sync::atomic::AtomicU64,
    stale_responses: std::sync::atomic::AtomicU64,
    unchanged_responses: std::sync::atomic::AtomicU64,
    index_writes: std::sync::atomic::AtomicU64,
    index_bytes: std::sync::atomic::AtomicU64,
}
/// Aggregate timings/counts only: no patient IDs, paths, values or credentials.
#[derive(Clone, Debug, Serialize)]
pub struct Performance {
    pub memory_hits: u64,
    pub disk_hits: u64,
    pub asset_downloads: u64,
    pub asset_download_bytes: u64,
    pub asset_download_ms: u64,
    pub metadata_requests: u64,
    pub metadata_ms: u64,
    pub stale_responses: u64,
    pub unchanged_responses: u64,
    pub index_writes: u64,
    pub index_bytes: u64,
}
pub struct Offline {
    diagnosis_catalog: Mutex<crate::diagnosis_cache::State>,
    pub state: Arc<Mutex<LocalState>>,
    store: Store,
    memory: Mutex<MemoryCache>,
    thumbnails: Mutex<MemoryCache>,
    fetch: Mutex<FetchQueue>,
    asset_fetch: Mutex<FetchQueue>,
    foreground: Mutex<Option<Uuid>>,
    registry_paths: Mutex<Vec<String>>,
    registry_epoch: std::sync::atomic::AtomicU64,
    performance: PerformanceCounters,
    disk: mpsc::Sender<DiskJob>,
    transport: Mutex<Connection>,
    wake: std::sync::atomic::AtomicBool,
    paused: std::sync::atomic::AtomicBool,
    stopping: std::sync::atomic::AtomicBool,
    workers: Workers,
    sync: Mutex<SyncSchedule>,
}
fn registry_path(path: &str) -> bool {
    [
        "/api/v1/patients/page?",
        "/api/v1/tasks/page?",
        "/api/v1/studies?patient_id=",
        "/api/v1/studies/summaries?patient_id=",
    ]
    .iter()
    .any(|prefix| path.starts_with(prefix))
}
impl Offline {
    pub fn attach(raw: Connection, store: Store, profile: Profile) -> Result<Arc<Self>, String> {
        let store = store.renew_if_closed()?;
        let mut state = match store.read("workspace")? {
            Some(bytes) => serde_json::from_slice::<LocalState>(&bytes).map_err(
                |_| "Поврежден индекс кеша; существующая работа сохранена без перезаписи",
            )?,
            None => LocalState {
                activity: None,
                activity_intents: BTreeMap::new(),
                format_version: 1,
                profile: profile.clone(),
                responses: BTreeMap::new(),
                blobs: BTreeMap::new(),
                pending: VecDeque::new(),
                confirmed: BTreeMap::new(),
                journal: Value::Null,
                editors: BTreeMap::new(),
                server_only_studies: BTreeSet::new(),
                remote_conflicts: BTreeMap::new(),
                conflict_history: vec![],
                aliases: BTreeMap::new(),
                patient_sexes: BTreeMap::new(),
                sequence: 1,
                durable_sequence: 0,
                metadata_generations: BTreeMap::new(),
                recovery_errors: BTreeSet::new(),
                online: false,
                error: String::new(),
            },
        };
        if state.format_version != 1 {
            return Err(
                "Формат кеша этой версии приложения не поддерживается; данные не перезаписаны"
                    .into(),
            );
        }
        if state.profile.user_id != profile.user_id || !crate::transport::same_server(&state.profile.server, &profile.server) {
            return Err("Кеш принадлежит другому врачу или серверу".into());
        }
        state.durable_sequence = state.sequence;
        state.online = false;
        // A crash may leave a complete encrypted file behind an older index.
        // Recover only verified bytes and commit the index before any worker can send.
        let mut recovered = state.evict_approved_protocols();
        recovered |= archive_retired_publications(&mut state);
        for (id, blob) in state.blobs.iter_mut().filter(|(_, blob)| !blob.persisted) {
            match store.read(&blob.file_id.simple().to_string()) {
                Ok(Some(bytes)) if bytes.len() == blob.size && digest(&bytes) == blob.sha256 => {
                    blob.persisted = true;
                    recovered = true;
                }
                _ => { state.recovery_errors.insert(*id); }
            }
        }
        if recovered {
            state.change();
            store.write("workspace", &serde_json::to_vec(&state).map_err(|_| "Ошибка записи состояния")?)?;
            state.durable_sequence = state.sequence;
        }
        archive_rejected_approval(&mut state);
        retry_validated_draft_rejections(&mut state);
        let (disk_tx, disk_rx) = mpsc::channel();
        let diagnosis_catalog=Mutex::new(crate::diagnosis_cache::State::load(&store,&raw));
        let instance = Arc::new(Self {
            diagnosis_catalog,
            state: Arc::new(Mutex::new(state)),
            store,
            memory: Mutex::new(MemoryCache::default()),
            thumbnails: Mutex::new(MemoryCache::default()),
            fetch: Mutex::new(FetchQueue::default()),
            asset_fetch: Mutex::new(FetchQueue::default()),
            foreground: Mutex::new(None),
            registry_paths: Mutex::new(vec![]),
            registry_epoch: std::sync::atomic::AtomicU64::new(0),
            performance: PerformanceCounters::default(),
            disk: disk_tx,
            transport: Mutex::new(raw.clone()),
            wake: std::sync::atomic::AtomicBool::new(false),
            paused: std::sync::atomic::AtomicBool::new(false),
            stopping: std::sync::atomic::AtomicBool::new(false),
            workers: Workers::default(),
            sync: Mutex::new(SyncSchedule::default()),
        });
        instance.start_disk(disk_rx);
        instance.start_network(raw);
        instance.start_metadata_network();
        instance.start_diagnoses();
        instance.start_diagnosis_priorities();
        instance.start_asset_network();
        instance.start_events();
        for path in [
            "/api/v1/me",
            "/api/v1/doctors",
            "/api/v1/capabilities",
            "/api/v1/templates",
            "/api/v1/terminology/versions",
            "/api/v1/terminology",
        ] {
            instance.fetch(path);
        }
        Ok(instance)
    }
    fn start_disk(self: &Arc<Self>, receiver: mpsc::Receiver<DiskJob>) {
        let this = self.clone();
        self.workers.disk(move || {
            loop {
                while let Ok(job) = receiver.try_recv() {
                    let (id, bytes) = match job {
                        DiskJob::Blob(id, bytes) => (id, bytes),
                        DiskJob::Thumbnail { asset, bytes } => {
                            // Derived previews are optional, not part of the
                            // clinical durable-before-send barrier.
                            let mut envelope = Vec::with_capacity(56 + bytes.len());
                            envelope.extend_from_slice(THUMBNAIL_MAGIC);
                            envelope.extend_from_slice(asset.as_bytes());
                            envelope.extend_from_slice(&Sha256::digest(bytes.as_ref()));
                            envelope.extend_from_slice(&bytes);
                            let _ = this.store.write(&thumbnail_key(asset), &envelope);
                            continue;
                        }
                        DiskJob::Flush(reply) => {
                            let _ = reply.send(this.flush_on_writer());
                            continue;
                        }
                    };
                    let result = this.store.write(&id.simple().to_string(), &bytes);
                    let mut state = this.state.lock().unwrap();
                    match result {
                        Ok(()) => {
                            if let Some(blob) = state.blobs.get_mut(&id) {
                                blob.persisted = true;
                            }
                            state.recovery_errors.remove(&id);
                            state.change();
                            drop(state);
                            this.trim_memory();
                        }
                        Err(error) => {
                            state.error = error;
                            drop(state);
                            let _ = this.disk.send(DiskJob::Blob(id, bytes));
                            thread::sleep(Duration::from_secs(1));
                            break;
                        }
                    }
                }
                let snapshot = {
                    let state = this.state.lock().unwrap();
                    if state.sequence > state.durable_sequence {
                        Some(state.clone())
                    } else {
                        None
                    }
                };
                if let Some(snapshot) = snapshot {
                    let sequence = snapshot.sequence;
                    match this.write_index(snapshot) {
                        Ok(()) => {
                            let mut state = this.state.lock().unwrap();
                            state.durable_sequence = state.durable_sequence.max(sequence);
                        }
                        Err(error) => {
                            this.state.lock().unwrap().error = error;
                            thread::sleep(Duration::from_secs(1));
                        }
                    }
                }
                if this.stopping.load(std::sync::atomic::Ordering::SeqCst)
                    && this.workers.network_finished() && this.workers.writers_finished() && this.checkpoint_ready() { break; }
                thread::sleep(Duration::from_millis(50));
            }
        });
    }
    fn flush_on_writer(&self) -> Result<(), String> {
        let snapshot = {
            let state = self.state.lock().unwrap();
            if state.durable_sequence >= state.sequence {
                return Ok(());
            }
            state.clone()
        };
        let sequence = snapshot.sequence;
        self.write_index(snapshot)?;
        let mut s = self.state.lock().unwrap();
        s.durable_sequence = s.durable_sequence.max(sequence);
        Ok(())
    }
    fn write_index(&self, mut snapshot: LocalState) -> Result<(), String> {
        snapshot.evict_approved_protocols();
        let bytes = serde_json::to_vec(&snapshot).map_err(|_| "Ошибка записи состояния")?;
        self.store.write("workspace", &bytes)?;
        self.performance
            .index_writes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.performance
            .index_bytes
            .fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
    fn flush_now(&self) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.disk
            .send(DiskJob::Flush(tx))
            .map_err(|_| "Локальная запись остановлена")?;
        rx.recv_timeout(Duration::from_secs(30))
            .map_err(|_| "Локальная запись PDF еще не подтверждена")?
    }
    fn trim_memory(&self) {
        let persisted: BTreeSet<_> = self
            .state
            .lock()
            .unwrap()
            .blobs
            .iter()
            .filter(|(_, b)| b.persisted)
            .map(|(id, _)| *id)
            .collect();
        self.memory
            .lock()
            .unwrap()
            .trim(128 * 1024 * 1024, usize::MAX, |id| persisted.contains(id));
    }
    pub fn fetch(&self, path: &str) {
        self.enqueue_fetch(path, false, true);
    }
    pub fn fetch_priority(&self, path: &str) {
        self.enqueue_fetch(path, true, true);
    }
    /// Only the visible directory page(s) and selected patient's short lists.
    pub fn set_registry_paths(&self, paths: Vec<String>) {
        let mut active = self.registry_paths.lock().unwrap();
        let old = std::mem::replace(&mut *active, paths.clone());
        drop(active);
        {
            let mut queue = self.fetch.lock().unwrap();
            queue
                .pending
                .retain(|p| !registry_path(p) || paths.contains(p));
            queue
                .retry_after
                .retain(|p, _| !registry_path(p) || paths.contains(p));
            queue
                .attempts
                .retain(|p, _| !registry_path(p) || paths.contains(p));
            queue
                .invalidated
                .retain(|p| !registry_path(p) || paths.contains(p));
        }
        for path in &paths {
            if !old.contains(path) {
                self.fetch_priority(path);
            }
        }
    }
    pub fn registry_epoch(&self) -> u64 {
        self.registry_epoch
            .load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn refresh_registry(&self) {
        self.registry_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let paths = self.registry_paths.lock().unwrap().clone();
        for path in paths {
            self.fetch(&path);
        }
    }
    pub fn fetch_study_foreground(&self, id: Uuid) {
        *self.foreground.lock().unwrap() = Some(id);
        self.fetch_priority(&format!("/api/v1/studies/{id}"));
    }
    pub fn clear_foreground(&self) {
        *self.foreground.lock().unwrap() = None;
    }
    fn enqueue_fetch(&self, path: &str, priority: bool, invalidate: bool) {
        if registry_path(path)
            && !self
                .registry_paths
                .lock()
                .unwrap()
                .iter()
                .any(|p| p == path)
        {
            return;
        }
        let queue = if path.starts_with("/api/v1/assets/") {
            &self.asset_fetch
        } else {
            &self.fetch
        };
        queue.lock().unwrap().enqueue(path, priority, invalidate);
    }
    pub fn performance(&self) -> Performance {
        use std::sync::atomic::Ordering::Relaxed;
        let p = &self.performance;
        Performance {
            memory_hits: p.memory_hits.load(Relaxed),
            disk_hits: p.disk_hits.load(Relaxed),
            asset_downloads: p.asset_downloads.load(Relaxed),
            asset_download_bytes: p.asset_download_bytes.load(Relaxed),
            asset_download_ms: p.asset_download_ms.load(Relaxed),
            metadata_requests: p.metadata_requests.load(Relaxed),
            metadata_ms: p.metadata_ms.load(Relaxed),
            stale_responses: p.stale_responses.load(Relaxed),
            unchanged_responses: p.unchanged_responses.load(Relaxed),
            index_writes: p.index_writes.load(Relaxed),
            index_bytes: p.index_bytes.load(Relaxed),
        }
    }
    /// Read a UI snapshot without scheduling repeated network refreshes.
    pub fn cached(&self, path: &str) -> Option<Value> {
        self.state.lock().unwrap().responses.get(path).cloned()
    }
    pub fn get(&self, path: &str) -> Result<Value, String> {
        let cached = self.state.lock().unwrap().responses.get(path).cloned();
        if let Some(value) = cached {
            return Ok(value);
        }
        self.enqueue_fetch(path, true, false);
        Err("Данные еще не в локальном кеше; загружаются в фоне".into())
    }
    pub(crate) fn write_guard(&self) -> Result<impl Drop + '_, String> { self.workers.writer() }
    pub fn checkpoint(&self, journal: Value) {
        let Ok(_write) = self.write_guard() else { return; };
        self.checkpoint_admitted(journal);
    }
    // Workspace holds the same admission guard across local edits and this commit.
    pub(crate) fn checkpoint_admitted(&self, mut journal: Value) {
        let mut state = self.state.lock().unwrap();
        normalize(&mut journal, &state.aliases);
        if let Some(id) = journal.pointer("/detail/study/id").and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
            && state.has_local_revision_conflict(id)
        {
            // The conflict is owned by Rust and is cleared only by explicit resolution.
            journal["revision_conflict"] = json!(true);
        }
        if state.journal != journal {
            if let Some(detail) = journal.get("detail").filter(|v| !v.is_null())
                && let Some(id) = detail.pointer("/study/id").and_then(Value::as_str)
            {
                let path = format!("/api/v1/studies/{id}");
                if state.responses.get(&path) != Some(detail) {
                    state.responses.insert(path.clone(), detail.clone());
                    state.touch_metadata(&path);
                }
            }
            if let Some(id) = journal
                .pointer("/detail/study/id")
                .and_then(Value::as_str)
                .and_then(|v| Uuid::parse_str(v).ok())
            {
                let preserve_local_editor = approved_study(&journal["detail"]) == Some(id)
                    && state.editors.get(&id).is_some_and(crate::workspace::journal_needs_upload);
                if !preserve_local_editor {
                    state.editors.insert(id, journal.clone());
                }
            }
            state.journal = journal;
            state.change();
        }
    }
    pub(crate) fn study_for_open(&self, id: Uuid) -> Result<Value, String> {
        let path = format!("/api/v1/studies/{id}");
        match self.read_live_value(&path, "протокол обследования") {
            Ok(value) => {
                let detail: StudyDetail = serde_json::from_value(value.clone())
                    .map_err(|_| "Некорректный ответ сервера об обследовании")?;
                if detail.study.id != id {
                    return Err("Сервер вернул другое обследование".into());
                }
                let mut state = self.state.lock().unwrap();
                state.retain_diverged_editor(&detail);
                let approved = approved_study(&value) == Some(id);
                if detail.study.status == "approved" && !approved {
                    return Err("Сервер вернул утверждённый протокол без содержимого или номера редакции".into());
                }
                archive_stale_reopen(&mut state, id, &detail);
                if !approved && !crate::outbox::study_indices(&state, id).is_empty()
                    && let Some(local) = state.responses.get(&path)
                    && local.pointer("/study/status").and_then(Value::as_str) == Some("draft")
                {
                    return Ok(local.clone());
                }
                self.confirm_online(&mut state);
                if approved && state.pending.iter().any(|op| {
                    op.method == "POST" && op.path == format!("{path}/reopen") && op.blocked.is_none()
                }) {
                    return Err("Ожидается подтверждение новой редакции".into());
                }
                let changed = if approved {
                    state.server_only_studies.insert(id)
                } else {
                    state.server_only_studies.remove(&id)
                };
                if changed {
                    state.change();
                }
                Ok(value)
            }
            Err(error) => {
                let state = self.state.lock().unwrap();
                let server_only = state.server_only_studies.contains(&id)
                    || state.responses.get(&path).and_then(approved_study) == Some(id)
                    || state.remote_conflicts.get(&id).and_then(approved_study) == Some(id);
                if server_only {
                    return Err(format!("Утверждённый протокол открывается с сервера. {error}"));
                }
                drop(state);
                let cached = self.get(&path)?;
                if cached.pointer("/study/status").and_then(Value::as_str) == Some("approved") {
                    return Err(format!("Утверждённый протокол открывается с сервера. {error}"));
                }
                Ok(cached)
            }
        }
    }

    pub fn editor(&self, id: Uuid) -> Option<Value> {
        self.state.lock().unwrap().editors.get(&id).cloned()
    }
    pub fn journal(&self) -> Value {
        self.state.lock().unwrap().journal.clone()
    }
    pub(crate) fn journal_for_restore(&self) -> Value {
        let state = self.state.lock().unwrap();
        let detail = &state.journal["detail"];
        let study = detail.pointer("/study/id").and_then(Value::as_str)
            .and_then(|id| Uuid::parse_str(id).ok());
        if detail.pointer("/study/status").and_then(Value::as_str) == Some("approved")
            || study.is_some_and(|id| state.server_only_studies.contains(&id))
        {
            return Value::Null;
        }
        state.journal.clone()
    }
    /// Translate provisional local IDs after a backend receipt, without exposing credentials.
    pub fn canonicalize_local(&self, mut value: Value) -> Value {
        normalize(&mut value, &self.state.lock().unwrap().aliases);
        value
    }
    pub fn identifier_aliases(&self) -> BTreeMap<Uuid, Uuid> {
        self.state.lock().unwrap().aliases.clone()
    }
    pub fn patient_sexes(&self) -> BTreeMap<Uuid, PatientSex> {
        self.state.lock().unwrap().patient_sexes.clone()
    }
    /// Freeze the chosen demographics in the same durable operation as patient creation.
    /// Existing queued legacy requests remain unchanged.
    pub fn create_patient(
        &self,
        input: NewPatient,
        sex: Option<PatientSex>,
    ) -> Result<Patient, String> {
        let body = crate::patient_edit::create_body(&input, sex)?;
        serde_json::from_value(self.mutate("/api/v1/patients", "POST", body)?)
        .map_err(|_| "Неверный локальный ответ пациента".into())
    }
    pub fn information(&self) -> (bool, usize, bool, String) {
        let s = self.state.lock().unwrap();
        (
            s.online,
            s.pending.len(),
            s.durable_sequence >= s.sequence && s.blobs.values().all(|blob| blob.persisted),
            s.status_error(),
        )
    }
    pub fn checkpoint_ready(&self) -> bool {
        self.state.lock().unwrap().checkpoint_ready()
    }
    pub fn study_file_error(&self, id: Uuid) -> Option<String> {
        self.state.lock().unwrap().study_has_missing_file(id).then(||
            "Не удалось восстановить снимки этого обследования. Протокол и очередь сохранены; утверждение недоступно до восстановления файлов".into())
    }
    /// Cheap publication key: unchanged metadata need not clone the workspace.
    pub fn state_token(&self) -> (u64, u64, bool, String) {
        let state = self.state.lock().unwrap();
        (
            state.sequence,
            state.durable_sequence,
            state.online,
            state.status_error(),
        )
    }
    pub fn download(&self, id: Uuid) -> Result<Vec<u8>, String> {
        self.download_shared(id).map(|bytes| bytes.as_ref().clone())
    }
    /// The native image decoder can borrow bytes without copying a full original.
    pub fn download_shared(&self, id: Uuid) -> Result<Arc<Vec<u8>>, String> {
        self.prepare_asset(id)?
            .ok_or_else(|| "Снимок еще не загружен в локальный кеш".into())
    }
    pub fn cached_original_shared(&self, id: Uuid) -> Result<Option<Arc<Vec<u8>>>, String> {
        if let Some(bytes) = self.memory.lock().unwrap().get(&id) {
            self.performance
                .memory_hits
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(Some(bytes));
        }
        let blob = self.state.lock().unwrap().blobs.get(&id).cloned();
        if let Some(blob) = blob.filter(|b| b.persisted)
            && let Some(bytes) = self.store.read(&blob.file_id.simple().to_string())?
        {
            if digest(&bytes) != blob.sha256 {
                return Err("Поврежден локальный файл; он не удален".into());
            }
            self.performance
                .disk_hits
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let bytes = Arc::new(bytes);
            self.memory.lock().unwrap().insert(id, bytes.clone());
            self.trim_memory();
            return Ok(Some(bytes));
        }
        Ok(None)
    }
    pub fn prepare_asset(&self, id: Uuid) -> Result<Option<Arc<Vec<u8>>>, String> {
        if let Some(bytes) = self.cached_original_shared(id)? {
            return Ok(Some(bytes));
        }
        let id = self.canonical_asset_id(id);
        if let Some(bytes) = self.cached_original_shared(id)? {
            return Ok(Some(bytes));
        }
        if let Some(failure) = self.asset_failure(id) {
            return Err(failure.message());
        }
        self.enqueue_fetch(&format!("/api/v1/assets/{id}"), true, false);
        Ok(None)
    }
    pub fn asset_failure(&self, id: Uuid) -> Option<AssetFailure> {
        if self.stopping.load(std::sync::atomic::Ordering::SeqCst) {
            return Some(AssetFailure::Stopped);
        }
        if self.paused.load(std::sync::atomic::Ordering::SeqCst) {
            return Some(AssetFailure::SessionRequired);
        }
        let id = self.canonical_asset_id(id);
        self.asset_fetch
            .lock()
            .unwrap()
            .terminal
            .get(&format!("/api/v1/assets/{id}"))
            .copied()
    }
    fn canonical_asset_id(&self, mut id: Uuid) -> Uuid {
        let state = self.state.lock().unwrap();
        for _ in 0..state.aliases.len() {
            let Some(next) = state.aliases.get(&id) else {
                break;
            };
            id = *next;
        }
        id
    }
    pub fn clear_asset_failures(&self) {
        let mut queue = self.asset_fetch.lock().unwrap();
        queue.terminal.clear();
        queue.attempts.clear();
        queue.retry_after.clear();
    }
    pub fn thumbnail_shared(&self, id: Uuid) -> Result<Arc<Vec<u8>>, String> {
        if let Some(bytes) = self.read_thumbnail(id)? {
            return Ok(bytes);
        }
        self.enqueue_fetch(&format!("/api/v1/assets/{id}/thumbnail"), true, false);
        Err("Миниатюра еще не загружена в локальный кеш".into())
    }
    fn read_thumbnail(&self, id: Uuid) -> Result<Option<Arc<Vec<u8>>>, String> {
        if let Some(bytes) = self.thumbnails.lock().unwrap().get(&id) {
            self.performance
                .memory_hits
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(Some(bytes));
        }
        let Some(envelope) = self.store.read(&thumbnail_key(id))? else {
            return Ok(None);
        };
        if envelope.len() < 56
            || &envelope[..8] != THUMBNAIL_MAGIC
            || &envelope[8..24] != id.as_bytes()
        {
            return Err("Миниатюра не соответствует снимку; оригинал сохранен".into());
        }
        crate::connection::validate_thumbnail(
            &envelope[56..],
            Some(&hex::encode(&envelope[24..56])),
        )?;
        self.performance
            .disk_hits
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let bytes = Arc::new(envelope[56..].to_vec());
        self.remember_thumbnail(id, bytes.clone());
        Ok(Some(bytes))
    }
    fn remember_thumbnail(&self, id: Uuid, bytes: Arc<Vec<u8>>) {
        let mut cache = self.thumbnails.lock().unwrap();
        cache.insert(id, bytes);
        cache.trim(8 * 1024 * 1024, 256, |_| true);
    }
    fn add_operation(
        state: &mut LocalState,
        path: &str,
        method: &str,
        body: Value,
        entity_id: Uuid,
        version_id: Option<Uuid>,
        blob: Option<Uuid>,
    ) {
        state.change();
        state.touch_operation(path);
        let suffix = if path.contains("/assets?") {
            "assets"
        } else {
            path.rsplit('/').next().unwrap_or("")
        };
        let intent = state.activity_intents.remove(suffix);
        let operation_id = intent.unwrap_or_else(Uuid::new_v4);
        if intent.is_none()
            && let Some(journal) = &state.activity
        {
            let code = if path.ends_with("/approve") {
                Some("protocol.approve_clicked")
            } else if path.ends_with("/reopen") {
                Some("protocol.reopen_clicked")
            } else if path.ends_with("/protocol") {
                Some("protocol.field_changed")
            } else if path.contains("/assets?") {
                Some("image.import_completed")
            } else {
                None
            };
            if let Some(code) = code {
                let mut action = crate::activity::Action::new(code);
                action.study_id = study_id(path);
                action.entity_id = Some(entity_id);
                action.operation_id = Some(operation_id);
                journal.record_id(action, operation_id);
            }
        }
        state.pending.push_back(Operation {
            id: operation_id,
            entity_id,
            version_id,
            path: path.into(),
            method: method.into(),
            body,
            blob,
            started: false,
            blocked: None,
            sequence: state.sequence,
        });
    }
    pub fn mutate(&self, path: &str, method: &str, mut body: Value) -> Result<Value, String> {
        if path.ends_with("/logout") {
            self.paused.store(true, std::sync::atomic::Ordering::SeqCst);
            let raw = self.transport.lock().unwrap().clone();
            thread::spawn(move || { let _ = raw.exchange("POST", "/api/v1/logout", Some(&json!({})), None, None); });
            return Ok(json!({"ok":true}));
        }
        let _write = self.write_guard()?;
        if path.ends_with("/reports")
            && (method != "POST" || std::env::var("M53_LEGACY_LOGIN").as_deref() != Ok("1"))
        {
            return Err("Изменение и удаление сохраненных отчетов врачом недоступно".into());
        }
        self.require_edit_permission(matches!(path,"/api/v1/patients" | "/api/v1/studies"))?;
        let mut s = self.state.lock().unwrap();
        if self.stopping.load(std::sync::atomic::Ordering::SeqCst) { return Err("Рабочее место закрывается; введённое сохранено локально".into()); }
        normalize(&mut body, &s.aliases);
        if study_id(path).is_some_and(|id| s.has_local_revision_conflict(id)) {
            return Err("Локальный черновик расходится с серверной редакцией. Сравните версии перед отправкой".into());
        }
        if path.ends_with("/approve") && study_id(path).is_some_and(|id| s.study_has_missing_file(id)) {
            return Err(RECOVERY_ERROR.into());
        }
        let entity = Uuid::new_v4();
        let mut version_id = None;
        let response = if path == "/api/v1/patients" {
            let (p, wire_sex) = crate::patient_edit::create_input(&body)?;
            let value = json!(Patient { card_number: None,
                sex: wire_sex,
                id: entity,
                external_id: None,
                family_name: p.family_name,
                given_name: p.given_name,
                patronymic: p.patronymic,
                birth_date: p.birth_date
            });
            crate::patient_demographics::reconcile_legacy_sex(&mut s.patient_sexes, entity, &value);
            append(&mut s.responses, "/api/v1/patients", value.clone());
            value
        } else if path == "/api/v1/studies" {
            let mut encounter = body.clone();
            if let Some(model) = encounter.as_object_mut().and_then(|value| value.remove("colposcope_model"))
                && !model.is_null() && !model.is_string()
            {
                return Err("Некорректная модель кольпоскопа".into());
            }
            // Validate the legacy fields separately. The original request,
            // including the captured model, is frozen in the outbox below.
            let input: m53_contracts::continuity::NewEncounter =
                serde_json::from_value(encounter).map_err(|_| "Неверное исследование")?;
            let patient = s
                .responses
                .get("/api/v1/patients")
                .and_then(Value::as_array)
                .and_then(|values| {
                    values
                        .iter()
                        .find(|p| p.get("id") == Some(&json!(input.patient_id)))
                })
                .cloned()
                .ok_or("Пациент не в кеше")?;
            let study = json!(Study {
                id: entity,
                patient_id: input.patient_id,
                status: "draft".into(),
                row_version: 1,
                study_instance_uid: format!("2.25.{}", entity.as_u128())
            });
            append(&mut s.responses, "/api/v1/studies", study.clone());
            s.responses.insert(format!("/api/v1/studies/{entity}"),json!({"study":study,"patient":patient,"revision_id":null,"protocol":null,"annotations":[],"assets":[]}));
            study
        } else if path.ends_with("/protocol") {
            let id = study_id(path).ok_or("Неверный путь исследования")?;
            let input: SaveDraft =
                serde_json::from_value(body.clone()).map_err(|_| "Неверный черновик")?;
            let coalesce = s
                .pending
                .back()
                .filter(|op| op.path == path && !op.started)
                .cloned();
            let revision = coalesce.as_ref().map(|op| op.entity_id).unwrap_or(entity);
            let expected = coalesce
                .as_ref()
                .and_then(|op| op.body.get("expected_version"))
                .and_then(Value::as_i64)
                .unwrap_or(input.expected_version);
            let mut frozen = body.clone();
            frozen["expected_version"] = json!(expected);
            if let Some(last) = coalesce {
                let sequence = s.sequence + 1;
                let op = s.pending.back_mut().unwrap();
                assert_eq!(op.id, last.id);
                op.body = frozen;
                op.sequence = sequence;
                s.touch_operation(path);
                s.change();
            } else {
                Self::add_operation(&mut s, path, method, frozen, revision, None, None);
            }
            let key = format!("/api/v1/studies/{id}");
            let detail = s.responses.get_mut(&key).ok_or("Исследование не в кеше")?;
            detail["protocol"] = json!(input.protocol);
            detail["annotations"] = json!(input.annotations);
            detail["revision_id"] = json!(revision);
            detail["study"]["row_version"] = json!(expected + 1);
            self.wake_online(&s);
            return Ok(json!(SavedProtocol {
                study_id: id,
                revision_id: revision,
                row_version: expected + 1
            }));
        } else if path == "/api/v1/templates" {
            let layout: ReportLayout =
                serde_json::from_value(body.clone()).map_err(|_| "Неверный шаблон")?;
            let version = Uuid::new_v4();
            version_id = Some(version);
            let template = json!(Template {
                id: entity,
                version_id: version,
                version: 1,
                layout
            });
            append(&mut s.responses, path, template.clone());
            template
        } else if path.ends_with("/reports") {
            json!({"id":entity})
        } else if path.ends_with("/approve")
            || path.ends_with("/manual-identity-verification")
            || path.ends_with("/reopen")
        {
            let study = study_id(path).ok_or("Неверный путь исследования")?;
            if !s.online || s.study_pending_count(study) > 0 {
                return Err("Сначала дождитесь подтверждения синхронизации; редактирование и локальное сохранение доступны".into());
            }
            Self::add_operation(&mut s, path, method, body, entity, None, None);
            self.wake_online(&s);
            if path.ends_with("/reopen") {
                // This is a queue admission acknowledgement, not a server receipt.
                return Ok(json!({"queued":true}));
            }
            return Err(if path.ends_with("/manual-identity-verification") {"Ручная сверка сохраняется на сервере. После подтверждения нажмите «Утвердить»".into()} else {"Запрос отправляется в фоне; дождитесь подтверждения backend".into()});
        } else {
            return Err("Операция еще не поддержана автономным клиентом".into());
        };
        Self::add_operation(&mut s, path, method, body, entity, version_id, None);
        self.wake_online(&s);
        Ok(response)
    }
    fn wake_online(&self, state: &LocalState) {
        if state.online {
            self.wake.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    fn confirm_online(&self, state: &mut LocalState) {
        if !self.paused.load(std::sync::atomic::Ordering::SeqCst) {
            if !state.online {
                // A real successful GET/receipt releases only connection-outage delays.
                // Ordinary online GETs must not defeat per-operation HTTP retry limits.
                self.wake.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            state.online = true;
        }
    }
    fn require_edit_permission(&self, creating: bool) -> Result<(),String> {
        if self.stopping.load(std::sync::atomic::Ordering::SeqCst) { return Err("Рабочее место закрывается; дождитесь завершения выхода".into()); }
        let transport=self.transport.lock().unwrap();
        transport.require_offline_access()?;
        if transport.offline_permissions.as_ref().is_some_and(|p|
            if creating {!p.allow_new_manual_encounter} else {!p.allow_continue_open_encounter}) {
            return Err("Автономная политика врача запрещает это действие".into());
        }
        Ok(())
    }
    pub fn upload(
        &self,
        study: Uuid,
        kind: &str,
        parent: Option<Uuid>,
        bytes: Vec<u8>,
    ) -> Result<Asset, String> {
        let _write = self.write_guard()?;
        self.require_edit_permission(false)?;
        let bytes = if kind == "original" {
            crate::imaging::storage_jpeg(bytes)?
        } else {
            bytes
        };
        let sha = digest(&bytes);
        let existing = {
            let state = self.state.lock().unwrap();
            state
                .responses
                .get(&format!("/api/v1/studies/{study}"))
                .and_then(|d| d.get("assets"))
                .and_then(Value::as_array)
                .and_then(|assets| {
                    assets.iter().find(|a| {
                        a.get("sha256") == Some(&json!(sha)) && a.get("kind") == Some(&json!(kind))
                            // Asset DTOs do not carry parent identity. Only an exact
                            // local upload request proves a derived image's parent.
                            && (kind != "derived" || state.pending.iter().any(|op| {
                                a.get("id") == Some(&json!(op.entity_id))
                                    && op.scope() == format!("/api/v1/studies/{study}/assets")
                                    && op.body["kind"] == "derived"
                                    && op.body["sha256"] == sha
                                    && op.body["parent"] == json!(parent)
                            }))
                    })
                })
                .cloned()
        };
        if let Some(existing) = existing {
            return serde_json::from_value(existing).map_err(|_| "Invalid cached asset".into());
        }
        let id = Uuid::new_v4();
        let mime = if kind == "pdf" {
            "application/pdf"
        } else if kind == "html" {
            "text/html"
        } else if kind == "derived" {
            "application/x-m53-rgb8"
        } else if bytes.starts_with(b"\x89PNG") {
            "image/png"
        } else if bytes.starts_with(b"\xff\xd8") {
            "image/jpeg"
        } else if bytes.starts_with(b"BM") {
            "image/bmp"
        } else {
            "image/tiff"
        };
        let asset = Asset {
            id,
            study_id: study,
            kind: kind.into(),
            sha256: sha.clone(),
            byte_size: bytes.len() as i64,
            mime_type: mime.into(),
        };
        let bytes = Arc::new(bytes);
        self.memory.lock().unwrap().insert(id, bytes.clone());
        let mut s = self.state.lock().unwrap();
        if self.stopping.load(std::sync::atomic::Ordering::SeqCst) { return Err("Рабочее место закрывается; загрузка не поставлена в очередь".into()); }
        s.blobs.insert(
            id,
            Blob {
                file_id: id,
                sha256: sha.clone(),
                size: bytes.len(),
                persisted: false,
                backend_confirmed: false,
            },
        );
        let detail = s
            .responses
            .get_mut(&format!("/api/v1/studies/{study}"))
            .ok_or("Исследование не в кеше")?;
        detail["assets"]
            .as_array_mut()
            .ok_or("Неверный кеш")?
            .push(json!(asset));
        let path = format!(
            "/api/v1/studies/{study}/assets?kind={kind}&sha256={sha}{}",
            parent.map(|id| format!("&parent={id}")).unwrap_or_default()
        );
        Self::add_operation(
            &mut s,
            &path,
            "POST",
            json!({"kind":kind,"sha256":sha,"parent":parent}),
            id,
            None,
            Some(id),
        );
        self.wake_online(&s);
        drop(s);
        self.disk
            .send(DiskJob::Blob(id, bytes))
            .map_err(|_| "Cache writer stopped")?;
        Ok(asset)
    }
    fn invalidate_lists(&self) {
        self.refresh_registry();
        for path in ["/api/v1/me"] {
            self.fetch(path);
        }
        // Opening many examinations must not turn every heartbeat into an
        // archive-wide refresh and original-image prefetch.
        let foreground = *self.foreground.lock().unwrap();
        let active = foreground.or_else(|| {
            self.state
                .lock()
                .unwrap()
                .journal
                .pointer("/detail/study/id")
                .and_then(Value::as_str)
                .and_then(|id| Uuid::parse_str(id).ok())
        });
        if let Some(id) = active {
            self.fetch(&format!("/api/v1/studies/{id}"));
            self.fetch(&format!("/api/v1/studies/{id}/context"));
        }
    }
    fn reject_session(&self, rejected: &Connection) {
        // Requests may finish after a fresh login installed a different token.
        // Hold transport through the state transition so install_auth can unpause last.
        #[allow(unused_mut)]
        let mut current = self.transport.lock().unwrap();
        if current.session != rejected.session || current.key != rejected.key
            || current.base != rejected.base || current.user_id != rejected.user_id { return; }
        #[cfg(feature = "standalone")]
        {
            current.offline_deadline = Some((0, Instant::now()));
            #[cfg(any(windows, target_os = "linux"))]
            if let Some(user) = current.user_id {
                let _ = crate::offline_profile::deny_local_user(&current.storage_base, user);
            }
        }
        self.paused.store(true, std::sync::atomic::Ordering::SeqCst);
        let mut state = self.state.lock().unwrap();
        state.online = false;
        state.error = "Сеанс отозван или истек. Войдите снова; локальная работа сохранена".into();
    }
    fn start_events(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        self.workers.network(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                let mut failures = 0usize;
                loop {
                    let Some(this) = weak.upgrade() else {
                        break;
                    };
                    if this.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                        break;
                    }
                    if this.paused.load(std::sync::atomic::Ordering::SeqCst) {
                        drop(this);
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                    let raw = this.transport.lock().unwrap().clone();
                    if raw.session.is_empty() {
                        drop(this);
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                    let Ok(client) = raw.event_client() else {
                        break;
                    };
                    let response = client
                        .get(format!("{}/api/v1/events", raw.base))
                        .header("Accept", "text/event-stream")
                        .header("X-Workstation-Key", &raw.key)
                        .header("X-M53-Protocol-Version", "2")
                        .bearer_auth(&raw.session)
                        .send()
                        .await;
                    if let Ok(mut response) = response {
                        if matches!(response.status().as_u16(), 401 | 403) {
                            this.reject_session(&raw);
                            continue;
                        }
                        if response.status().is_success()
                            && response
                                .headers()
                                .get("content-type")
                                .and_then(|v| v.to_str().ok())
                                .is_some_and(|v| v.starts_with("text/event-stream"))
                        {
                            this.invalidate_lists();
                            this.fetch("/api/v1/templates");
                            failures = 0;
                            let mut parser = crate::events::Parser::default();
                            let mut last_byte = Instant::now();
                            loop {
                                if this.stopping.load(std::sync::atomic::Ordering::SeqCst) { break; }
                                let chunk = match tokio::time::timeout(
                                    Duration::from_millis(100),
                                    response.chunk(),
                                )
                                .await
                                {
                                    Ok(chunk) => chunk,
                                    Err(_) => {
                                        if last_byte.elapsed() >= Duration::from_secs(30)
                                            || this
                                                .stopping
                                                .load(std::sync::atomic::Ordering::SeqCst)
                                            || this.paused.load(std::sync::atomic::Ordering::SeqCst)
                                            || this.transport.lock().unwrap().session != raw.session
                                        {
                                            break;
                                        }
                                        continue;
                                    }
                                };
                                let Ok(Some(chunk)) = chunk else {
                                    break;
                                };
                                last_byte = Instant::now();
                                let Ok(signals) = parser.push(&chunk) else {
                                    break;
                                };
                                for signal in signals {
                                    match signal {
                                        crate::events::Signal::Refresh => this.invalidate_lists(),
                                        crate::events::Signal::TemplatesUpdated => this.fetch("/api/v1/templates"),
                                        crate::events::Signal::AuthRequired => {
                                            this.reject_session(&raw)
                                        }
                                    }
                                }
                                if this.paused.load(std::sync::atomic::Ordering::SeqCst) {
                                    break;
                                }
                            }
                        }
                    }
                    drop(this);
                    let delay = [2, 4, 8, 15][failures.min(3)];
                    failures += 1;
                    let jitter = (Uuid::new_v4().as_u128() % 500) as u64;
                    let until = Instant::now() + Duration::from_millis(delay * 1000 + jitter);
                    while Instant::now() < until {
                        if weak.upgrade().is_none_or(|this| this.stopping.load(std::sync::atomic::Ordering::SeqCst)) { return; }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            });
        });
    }
    fn start_asset_network(self: &Arc<Self>) {
        // Two large file transfers cannot occupy the metadata/outbox worker.
        // In-flight de-duplication keeps each encrypted file a single-writer job.
        for _ in 0..2 {
            let this = self.clone();
            self.workers.network(move || {
                loop {
                    if this.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                        break;
                    }
                    if this.paused.load(std::sync::atomic::Ordering::SeqCst) {
                        thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                    let path = this.asset_fetch.lock().unwrap().take();
                    let Some(path) = path else {
                        thread::sleep(Duration::from_millis(25));
                        continue;
                    };
                    let raw = this.transport.lock().unwrap().clone();
                    match this.fetch_one(&raw, &path) {
                        Ok(()) => this.asset_fetch.lock().unwrap().complete(&path),
                        Err(error) => {
                            this.asset_fetch.lock().unwrap().failed(&path);
                            if error != THUMBNAIL_PENDING {
                                this.state.lock().unwrap().error = error;
                            }
                        }
                    }
                }
            });
        }
    }
    fn start_network(self: &Arc<Self>, _raw: Connection) {
        for _ in 0..2 {
            let this = self.clone();
            self.workers.network(move || loop {
                if this.stopping.load(std::sync::atomic::Ordering::SeqCst) { break; }
                if this.paused.load(std::sync::atomic::Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(100)); continue;
                }
                let operation = {
                    let mut schedule = this.sync.lock().unwrap();
                    if this.wake.swap(false, std::sync::atomic::Ordering::SeqCst) { schedule.wake(); }
                    if !schedule.ready(Instant::now()) { None } else {
                        let deferred = schedule.deferred(Instant::now());
                        let mut state = this.state.lock().unwrap();
                        let index = crate::outbox::next_index(&state, &deferred);
                        let durable = state.durable_sequence;
                        let sequence = state.sequence + 1;
                        let mut changed = false;
                        let result = index.and_then(|i| {
                            let op = &mut state.pending[i];
                            if !op.started {
                                op.started = true; op.sequence = sequence; changed = true; None
                            } else if op.sequence <= durable && schedule.reserve(op.id) { Some(op.clone()) }
                            else { None }
                        });
                        if changed { state.change(); }
                        result
                    }
                };
                let Some(operation) = operation else { thread::sleep(Duration::from_millis(25)); continue; };
                let raw = this.transport.lock().unwrap().clone();
                match this.sync_one(&raw, &operation) {
                    Ok(()) => {
                        this.sync.lock().unwrap().succeeded(operation.id);
                        this.confirm_online(&mut this.state.lock().unwrap());
                    }
                    Err(error) => {
                        let outage = transport_outage(&error);
                        let same_auth = {
                            let current = this.transport.lock().unwrap();
                            current.session == raw.session && current.key == raw.key
                                && current.base == raw.base && current.user_id == raw.user_id
                        };
                        if this.paused.load(std::sync::atomic::Ordering::SeqCst) || !same_auth {
                            this.sync.lock().unwrap().release(operation.id);
                            if !same_auth { continue; }
                        } else {
                            this.sync.lock().unwrap().failed(operation.id, Instant::now(), outage);
                        }
                        let mut state = this.state.lock().unwrap();
                        if outage { state.online = false; }
                        state.error = error;
                    }
                }
            });
        }
    }
    fn record_metadata_failure(state: &mut LocalState, error: String) {
        if transport_outage(&error) {
            state.online = false;
            state.error = error;
        } else if state.online {
            state.error = error;
        }
    }
    fn start_metadata_network(self: &Arc<Self>) {
        let this = self.clone();
        self.workers.network(move || {
            let mut next_poll = Instant::now() + Duration::from_secs(15);
            loop {
                if this.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                if this.paused.load(std::sync::atomic::Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                if Instant::now() >= next_poll {
                    this.invalidate_lists();
                    if !cfg!(feature = "standalone") {
                        let studies = delivery_poll_studies(&this.state.lock().unwrap());
                        for id in studies {
                            this.fetch(&format!("/api/v1/studies/{id}/results/1c"));
                        }
                    }
                    next_poll = Instant::now() + Duration::from_secs(15);
                }
                let path = this.fetch.lock().unwrap().take();
                let Some(path) = path else {
                    thread::sleep(Duration::from_millis(25));
                    continue;
                };
                let raw = this.transport.lock().unwrap().clone();
                let result = this.fetch_one(&raw, &path);
                let mut state = this.state.lock().unwrap();
                match result {
                    Ok(()) => {
                        this.fetch.lock().unwrap().complete(&path);
                        this.confirm_online(&mut state);
                    }
                    Err(error) => {
                        if registry_path(&path)
                            && !this.registry_paths.lock().unwrap().contains(&path)
                        {
                            this.fetch.lock().unwrap().complete(&path);
                            continue;
                        }
                        this.fetch.lock().unwrap().failed(&path);
                        Self::record_metadata_failure(&mut state, error);
                    }
                }
            }
        });
    }
    pub fn claim_task(&self, id: Uuid, version: i64, model: Option<&str>) -> Result<Uuid, String> {
        let _write = self.write_guard()?;
        let raw = self.transport.lock().unwrap().clone();
        if raw.session.is_empty() {
            return Err("Захват задания требует онлайн-входа".into());
        }
        let mut request = json!({"expected_version":version});
        if let Some(model) = model {
            request["colposcope_model"] = json!(model);
        }
        let (status, bytes) = raw.exchange(
            "POST",
            &format!("/api/v1/tasks/{id}/claim"),
            Some(&request),
            None,
            None,
        )?;
        let reply: Value =
            serde_json::from_slice(&bytes).map_err(|_| "Неверный ответ захвата задания")?;
        if status != 200 {
            self.refresh_registry();
            return Err(format!(
                "Задание не открыто ({status}): {}",
                reply["code"].as_str().unwrap_or("invalid_response")
            ));
        }
        if reply["task_id"] != json!(id) {
            return Err("Ответ относится к другому заданию".into());
        }
        let study = reply["study_id"]
            .as_str()
            .and_then(|v| Uuid::parse_str(v).ok())
            .filter(|id| !id.is_nil())
            .ok_or("Нет обследования в ответе захвата")?;
        self.refresh_registry();
        self.fetch(&format!("/api/v1/studies/{study}"));
        Ok(study)
    }
    pub fn cached_render(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        let _write = self.write_guard()?;
        let metadata = self
            .state
            .lock()
            .unwrap()
            .responses
            .get(&format!("render/{key}"))
            .cloned();
        let Some(meta) = metadata else {
            return Ok(None);
        };
        let bytes = meta["file_id"]
            .as_str()
            .and_then(|v| Uuid::parse_str(v).ok())
            .and_then(|id| self.store.read(&id.simple().to_string()).ok().flatten())
            .filter(|bytes| json!(digest(bytes)) == meta["sha256"]);
        if let Some(bytes) = bytes {
            return Ok(Some(bytes));
        }
        // This is only a reproducible render cache. Keep immutable local PDFs,
        // receipts and their blobs intact, even when the derived copy is damaged.
        let mut state = self.state.lock().unwrap();
        let cache_key = format!("render/{key}");
        if state.responses.get(&cache_key) == Some(&meta) {
            state.responses.remove(&cache_key);
            state.change();
            drop(state);
            // Persist invalidation without hiding journal/disk write failures.
            self.flush_now()?;
        }
        Ok(None)
    }
    pub fn cache_render(&self, key: &str, bytes: &[u8]) -> Result<(), String> {
        let _write = self.write_guard()?;
        let id = Uuid::new_v4();
        self.store.write(&id.simple().to_string(), bytes)?;
        let mut s = self.state.lock().unwrap();
        s.responses.insert(
            format!("render/{key}"),
            json!({"file_id":id,"sha256":digest(bytes)}),
        );
        s.change();
        drop(s);
        self.flush_now()
    }
    /// Keep immutable PDF bytes locally. Only approved revisions enter the delivery queue.
    pub fn require_report_output_access(&self) -> Result<(), String> {
        if self.stopping.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("Рабочее место закрывается; подготовка отчета отменена".into());
        }
        let transport=self.transport.lock().unwrap();
        transport.require_offline_access()?;
        if transport.offline_permissions.as_ref().is_some_and(|p| !p.allow_local_print) {
            return Err("Локальная печать запрещена политикой врача".into());
        }
        Ok(())
    }

    pub fn store_report(
        &self,
        detail: &StudyDetail,
        template: Uuid,
        renderer: &str,
        bytes: &[u8],
    ) -> Result<(), String> {
        self.persist_report(detail, template, renderer, bytes, false)
    }
    pub fn enqueue_report(
        &self,
        detail: &StudyDetail,
        template: Uuid,
        renderer: &str,
        bytes: &[u8],
    ) -> Result<(), String> {
        crate::delivery::require_mis_task(detail)?;
        if detail.study.status != "approved" || detail.revision_id.is_none() {
            return Err("Передача в 1С требует утвержденной серверной ревизии".into());
        }
        let state = self.state.lock().unwrap();
        let jobs = state
            .responses
            .get(&format!("/api/v1/studies/{}/results/1c", detail.study.id));
        if let Some(job) = jobs.and_then(|jobs| {
            crate::delivery::matching(jobs, detail.revision_id.unwrap(), template, &digest(bytes))
        }) {
            if crate::delivery::prevents_resend(job) {
                return Err(format!("Эта версия: {}", crate::delivery::status(job)));
            }
        }
        drop(state);
        self.persist_report(detail, template, renderer, bytes, true)
    }
    fn persist_report(
        &self,
        detail: &StudyDetail,
        template: Uuid,
        renderer: &str,
        bytes: &[u8],
        send: bool,
    ) -> Result<(), String> {
        let _write = self.write_guard()?;
        self.require_report_output_access()?;
        let hash = digest(bytes);
        let id = Uuid::new_v4();
        let key = format!(
            "local-pdf/{}/{hash}/{:?}/{template}/{renderer}/{}",
            detail.study.id,
            detail.revision_id,
            format!("{}-{send}", detail.study.status)
        );
        if self.state.lock().unwrap().responses.contains_key(&key) {
            return self.flush_now();
        }
        // Encryption/fsync of a large PDF must not hold the editor-state lock.
        self.store.write(&id.simple().to_string(), bytes)?;
        let mut s = self.state.lock().unwrap();
        if s.responses.contains_key(&key) {
            drop(s);
            return self.flush_now();
        }
        s.blobs.insert(
            id,
            Blob {
                file_id: id,
                sha256: hash.clone(),
                size: bytes.len(),
                persisted: true,
                backend_confirmed: false,
            },
        );
        let metadata = json!({"sha256":hash,"file_id":id,"revision_id":detail.revision_id,"template_version_id":template,"renderer_version":renderer,"client_version":env!("CARGO_PKG_VERSION")});
        s.responses.insert(key, metadata.clone());
        if send
            && detail.study.status == "approved"
            && std::env::var("M53_LEGACY_LOGIN").as_deref() != Ok("1")
        {
            let revision = detail.revision_id.ok_or("Нет утвержденной ревизии")?;
            let path = format!(
                "/api/v1/studies/{}/results/1c/pdf?revision_id={revision}&template_version_id={template}&client_version={}&renderer_version={renderer}",
                detail.study.id,
                env!("CARGO_PKG_VERSION")
            );
            Self::add_operation(&mut s, &path, "POST", metadata, id, None, Some(id));
            self.wake_online(&s);
        } else {
            s.change();
        }
        // The durable state and bytes precede any network transmission.
        drop(s);
        self.flush_now()
    }
    fn sync_pdf(&self, raw: &Connection, op: &Operation) -> Result<(), String> {
        queued_index(&self.state.lock().unwrap(), op)?;
        let blob_id = op.blob.ok_or("Нет сохраненного PDF")?;
        let bytes = self
            .store
            .read(&blob_id.simple().to_string())?
            .ok_or("Локальный PDF отсутствует; очередь сохранена")?;
        if json!(digest(&bytes)) != op.body["sha256"] {
            return Err("Поврежден локальный PDF; очередь сохранена".into());
        }
        let (status, body) = raw.exchange_owned("POST", &op.path, None, Some(bytes), None)?;
        if matches!(status,401 | 403) {
            self.reject_session(raw);
            return Err(
                "Сессия истекла: нужен повторный вход; локальная работа сохраняется".into(),
            );
        }
        let receipt: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        if [400, 404, 409, 413, 415, 422].contains(&status) {
            let mut s = self.state.lock().unwrap();
            let index = queued_index(&s, op)?;
            s.pending[index].blocked = Some(format!(
                "PDF {status}: {}",
                receipt["code"].as_str().unwrap_or("invalid_response")
            ));
            s.change();
            return Ok(());
        }
        if status != 200
            || receipt["pdf_sha256"] != op.body["sha256"]
            || receipt["job_id"]
                .as_str()
                .and_then(|id| Uuid::parse_str(id).ok())
                .is_none()
            || !matches!(
                receipt["state"].as_str(),
                Some("pending" | "retry" | "sent")
            )
            || receipt["sequence"].as_u64().unwrap_or(0) == 0
            || receipt["payload_kind"] != "cache"
        {
            return Err("М53 не подтвердил загрузку PDF; файл остается в очереди".into());
        }
        let mut s = self.state.lock().unwrap();
        let index = queued_index(&s, op)?;
        s.responses
            .insert(format!("result-upload/{}", op.id), receipt);
        s.confirmed.insert(op.id, op.request_hash());
        s.pending.remove(index);
        s.touch_operation(&op.path);
        s.change();
        drop(s);
        if let Some(id) = study_id(&op.path) {
            self.fetch(&format!("/api/v1/studies/{id}/results/1c"));
        }
        Ok(())
    }
    fn sync_one(&self, raw: &Connection, op: &Operation) -> Result<(), String> {
        queued_index(&self.state.lock().unwrap(), op)?;
        if op.path.contains("/results/1c/pdf?") {
            return self.sync_pdf(raw, op);
        }
        let receipt_path = format!("/api/v1/operations/{}", op.id);
        let mut reply = raw.exchange("GET", &receipt_path, None, None, None)?;
        if matches!(reply.0, 401 | 403) {
            self.reject_session(raw);
            return Err(
                "Сессия истекла: нужен повторный вход; локальная работа сохраняется".into(),
            );
        }
        if reply.0 == 404 {
            let bytes = if let Some(id) = op.blob {
                let blob = self
                    .state
                    .lock()
                    .unwrap()
                    .blobs
                    .get(&id)
                    .cloned()
                    .ok_or("Missing local blob")?;
                Some(
                    self.store
                        .read(&blob.file_id.simple().to_string())?
                        .ok_or("Missing encrypted file; local operation retained")?,
                )
            } else {
                None
            };
            let body = bytes.is_none().then_some(&op.body);
            reply = raw.exchange_owned(&op.method, &op.path, body, bytes, Some(op))?;
            if matches!(reply.0, 401 | 403) {
                self.reject_session(raw);
                return Err(
                    "Сессия истекла: нужен повторный вход; локальная работа сохраняется".into(),
                );
            }
            if matches!(reply.0, 400 | 404 | 409 | 413 | 415 | 422) {
                if let Some(id)=study_id(&op.path) {
                    if op.path.ends_with("/manual-identity-verification") || op.path.ends_with("/approve") {
                        self.fetch(&format!("/api/v1/studies/{id}/context"));
                    }
                }
                let remote = study_id(&op.path).and_then(|id| {
                    raw.exchange("GET", &format!("/api/v1/studies/{id}"), None, None, None)
                        .ok()
                        .filter(|response| response.0 == 200)
                        .and_then(|r| serde_json::from_slice::<Value>(&r.1).ok())
                        .map(|value| (id, value))
                });
                let mut s = self.state.lock().unwrap();
                let index = queued_index(&s, op)?;
                s.pending[index].blocked = Some(format!(
                    "Backend {}: {}",
                    reply.0,
                    String::from_utf8_lossy(&reply.1)
                ));
                if let Some((id, remote)) = remote {
                    if op.path.ends_with("/reopen")
                        && let Ok(detail) = serde_json::from_value::<StudyDetail>(remote.clone())
                    {
                        s.remote_conflicts.insert(id, remote);
                        archive_stale_reopen(&mut s, id, &detail);
                    } else {
                        s.remote_conflicts.insert(id, remote);
                    }
                }
                // A rejected request belongs to its study, not to the entire account queue.
                s.change();
                archive_rejected_approval(&mut s);
                return Ok(());
            }
            if !(200..300).contains(&reply.0) {
                return Err(if reply.0 == 401 {
                    "Сессия истекла: нужен повторный вход; локальная работа сохраняется".into()
                } else {
                    format!("Backend временно недоступен ({})", reply.0)
                });
            }
            reply = raw.exchange("GET", &receipt_path, None, None, None)?;
        }
        if reply.0 != 200 {
            return Err(
                "Backend не подтвердил конкретную операцию; локальные данные остаются в очереди"
                    .into(),
            );
        }
        let receipt: Value =
            serde_json::from_slice(&reply.1).map_err(|_| "Неверная квитанция backend")?;
        if receipt.get("request_sha256") != Some(&json!(op.request_hash()))
            || receipt.get("scope") != Some(&json!(op.scope()))
            || receipt.get("operation_id") != Some(&json!(op.id))
        {
            return Err(
                "Квитанция не совпадает с операцией; удаление локальных данных запрещено".into(),
            );
        }
        let response = receipt
            .get("response")
            .cloned()
            .ok_or("Неполная квитанция")?;
        if op.path == "/api/v1/patients" {
            crate::patient_edit::check_create_ack(&response, &op.body)?;
        }
        let mut s = self.state.lock().unwrap();
        let index = queued_index(&s, op)?;
        if let Some(blob_id) = op.blob {
            if response.get("sha256") != op.body.get("sha256") {
                return Err("Backend не подтвердил SHA-256 файла".into());
            }
            if let Some(blob) = s.blobs.get_mut(&blob_id) {
                blob.backend_confirmed = true;
            }
            if let Some(server_id) = response
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| Uuid::parse_str(id).ok())
                && server_id != op.entity_id
            {
                remap(&mut s, op.entity_id, server_id, op.id);
            }
        }
        if op.path == "/api/v1/patients" {
            if let Some(values) = s
                .responses
                .get_mut("/api/v1/patients")
                .and_then(Value::as_array_mut)
            {
                for value in values {
                    if value.get("id") == response.get("id") {
                        *value = response.clone();
                    }
                }
            }
            let mut touched = Vec::new();
            for (path, value) in &mut s.responses {
                if value.pointer("/patient/id") == response.get("id") {
                    value["patient"] = response.clone();
                    touched.push(path.clone());
                }
            }
            for path in touched { s.touch_metadata(&path); }
            if s.journal.pointer("/detail/patient/id") == response.get("id") {
                s.journal["detail"]["patient"] = response.clone();
            }
        }
        let reopening = op.path.ends_with("/reopen");
        if (op.path.ends_with("/approve") || reopening)
            && let Some(id) = study_id(&op.path)
            && let Some(detail) = s.responses.get_mut(&format!("/api/v1/studies/{id}"))
            && let Some(version) = response["row_version"].as_i64()
            && version >= detail["study"]["row_version"].as_i64().unwrap_or(0)
            && approval_receipt_matches_revision(op, detail)
            && (reopening || response["revision_id"] == detail["revision_id"])
        {
            detail["study"]["row_version"] = json!(version);
            if reopening {
                detail["study"]["status"] = json!("draft");
                // Older APIs only return row_version. Require a fresh draft ID
                // before an approval can be submitted.
                detail["revision_id"] = response.get("revision_id")
                    .filter(|v| v.as_str().and_then(|id| Uuid::parse_str(id).ok()).is_some())
                    .cloned().unwrap_or(Value::Null);
                s.server_only_studies.remove(&id);
            } else {
                detail["study"]["status"] = json!("approved");
            }
        }
        if op.path.ends_with("/manual-identity-verification") {
            if let Some(id)=study_id(&op.path) {
                if let Some(context)=s.responses.get_mut(&format!("/api/v1/studies/{id}/context")) {
                    context["manual_identity_verification"]=response["manual_identity_verification"].clone();
                }
            }
        }
        s.confirmed.insert(op.id, op.request_hash());
        s.pending.remove(index);
        s.touch_operation(&op.path);
        s.error.clear();
        if op.path.ends_with("/protocol")
            && let Some(id) = study_id(&op.path)
            && s.study_blocked(id).is_none() && !s.has_local_revision_conflict(id)
        {
            s.remote_conflicts.remove(&id);
        }
        s.change();
        Ok(())
    }
    pub fn refresh_study_patient(&self, study: Uuid) {
        let state = self.state.lock().unwrap();
        let patient = state
            .responses
            .iter()
            .filter(|(key, _)| {
                key.as_str() == "/api/v1/studies" || key.starts_with("/api/v1/studies?patient_id=")
            })
            .filter_map(|(_, value)| value.as_array())
            .flatten()
            .find(|row| row.get("id") == Some(&json!(study)))
            .and_then(|row| row.get("patient_id"))
            .and_then(Value::as_str)
            .and_then(|id| Uuid::parse_str(id).ok());
        let linked = patient.is_some_and(|id| {
            state
                .responses
                .get("/api/v1/patients")
                .and_then(Value::as_array)
                .is_some_and(|rows| {
                    rows.iter().any(|row| {
                        row.get("id") == Some(&json!(id))
                            && row
                                .get("external_id")
                                .and_then(Value::as_str)
                                .is_some_and(|v| !v.is_empty())
                    })
                })
        });
        drop(state);
        if linked {
            self.fetch(&format!(
                "/api/v1/patients/{}/refresh-from-1c",
                patient.unwrap()
            ));
        }
    }
    fn fetch_one(&self, raw: &Connection, path: &str) -> Result<(), String> {
        let started = Instant::now();
        let generation = self.state.lock().unwrap().metadata_generation(path);
        let result = self.fetch_one_inner(raw, path, generation);
        if !path.starts_with("/api/v1/assets/") {
            self.performance
                .metadata_requests
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.performance.metadata_ms.fetch_add(
                started.elapsed().as_millis() as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
        }
        result
    }
    fn fetch_one_inner(&self, raw: &Connection, path: &str, generation: u64) -> Result<(), String> {
        if let Some(id) = path
            .strip_prefix("/api/v1/assets/")
            .and_then(|path| path.strip_suffix("/thumbnail"))
            .and_then(|id| Uuid::parse_str(id).ok())
        {
            if self.read_thumbnail(id)?.is_some() {
                return Ok(());
            }
            let started = Instant::now();
            let (status, bytes, expected) = raw.thumbnail_exchange(id)?;
            self.performance
                .asset_downloads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.performance
                .asset_download_bytes
                .fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
            self.performance.asset_download_ms.fetch_add(
                started.elapsed().as_millis() as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
            if matches!(status, 401 | 403) {
                self.reject_session(raw);
                return Err("Сеанс недоступен. Войдите снова; локальная работа сохранена".into());
            }
            if status == 404 {
                return Err(THUMBNAIL_PENDING.into());
            }
            if status != 200 {
                return Err(format!("Миниатюра пока недоступна ({status})"));
            }
            crate::connection::validate_thumbnail(&bytes, expected.as_deref())?;
            let bytes = Arc::new(bytes);
            self.remember_thumbnail(id, bytes.clone());
            self.disk
                .send(DiskJob::Thumbnail { asset: id, bytes })
                .map_err(|_| "Cache writer stopped")?;
            return Ok(());
        }
        if path.starts_with("/api/v1/patients/") && path.ends_with("/refresh-from-1c") {
            let (status, _) = raw.exchange("POST", path, None, None, None)?;
            if matches!(status, 401 | 403) {
                self.reject_session(raw);
            }
            self.refresh_registry();
            if status != 200 {
                return Err(format!(
                    "Сверка пациента с 1С недоступна ({status}); сохраненные данные оставлены"
                ));
            }
            return Ok(());
        }
        if path == "/api/v1/doctors" {
            let doctors = raw.doctor_directory()?;
            let mut s = self.state.lock().unwrap();
            if s.metadata_generation(path) != generation {
                drop(s);
                self.performance
                    .stale_responses
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.fetch(path);
                return Ok(());
            }
            if replace_response(&mut s.responses, path, json!(doctors)) {
                s.change();
            } else {
                self.performance
                    .unchanged_responses
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            return Ok(());
        }
        if let Some(id) = path
            .strip_prefix("/api/v1/assets/")
            .and_then(|s| Uuid::parse_str(s).ok())
        {
            if self.memory.lock().unwrap().contains_key(&id) {
                return Ok(());
            }
            let known = self.state.lock().unwrap().blobs.get(&id).cloned();
            if let Some(blob) = known.as_ref().filter(|b| b.persisted)
                && let Some(bytes) = self.store.read(&blob.file_id.simple().to_string())?
            {
                if digest(&bytes) != blob.sha256 {
                    return Err("Поврежден локальный снимок; кеш не удален".into());
                }
                self.memory.lock().unwrap().insert(id, Arc::new(bytes));
                return Ok(());
            }
            let started = Instant::now();
            let (status, bytes) = raw.exchange("GET", path, None, None, None)?;
            self.performance
                .asset_downloads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.performance
                .asset_download_bytes
                .fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
            self.performance.asset_download_ms.fetch_add(
                started.elapsed().as_millis() as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
            if matches!(status, 400 | 401 | 403 | 404 | 409 | 410 | 413 | 415 | 422) {
                self.asset_fetch
                    .lock()
                    .unwrap()
                    .reject(path, AssetFailure::Http(status));
            }
            if matches!(status, 401 | 403) {
                self.reject_session(raw);
                return Err("Сеанс недоступен. Войдите снова; локальная работа сохранена".into());
            }
            if status != 200 {
                return Err(format!("Файл пока недоступен ({status})"));
            }
            let expected = self
                .state
                .lock()
                .unwrap()
                .responses
                .values()
                .filter_map(|v| v.get("assets").and_then(Value::as_array))
                .flatten()
                .find(|a| a.get("id") == Some(&json!(id)))
                .and_then(|a| a.get("sha256").and_then(Value::as_str))
                .map(str::to_owned);
            let sha256 = digest(&bytes);
            if expected.as_deref() != Some(sha256.as_str()) {
                self.asset_fetch
                    .lock()
                    .unwrap()
                    .reject(path, AssetFailure::Integrity);
                return Err("Backend-file hash mismatch; no cache replacement".into());
            }
            let bytes = Arc::new(bytes);
            self.memory.lock().unwrap().insert(id, bytes.clone());
            {
                let mut s = self.state.lock().unwrap();
                s.blobs.insert(
                    id,
                    Blob {
                        file_id: id,
                        sha256,
                        size: bytes.len(),
                        persisted: false,
                        backend_confirmed: true,
                    },
                );
                // These verified bytes are now an active disk write, not an
                // unrecoverable attach-time failure exempted from close barriers.
                s.recovery_errors.remove(&id);
                s.change();
            }
            self.disk
                .send(DiskJob::Blob(id, bytes))
                .map_err(|_| "Cache writer stopped")?;
            return Ok(());
        }
        let (status, bytes) = raw.exchange("GET", path, None, None, None)?;
        if path == "/api/v1/studies/summaries" && status == 404 {
            return Ok(()); // Older servers do not provide the light projection yet.
        }
        if matches!(status, 401 | 403) {
            self.reject_session(raw);
            return Err("Сеанс недоступен. Войдите снова; локальная работа сохранена".into());
        }
        if status != 200 {
            if status == 404
                && (path.starts_with("/api/v1/patients/page?")
                    || path.starts_with("/api/v1/tasks/page?"))
            {
                return Err("Сервер не поддерживает новые списки. Требуется обновление API; локальная работа сохранена".into());
            }
            return Err(format!(
                "API пока недоступен ({status}); локальная работа сохранена"
            ));
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| "Неверный ответ API")?;
        let mut s = self.state.lock().unwrap();
        if s.metadata_generation(path) != generation {
            drop(s);
            self.performance
                .stale_responses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.fetch(path);
            return Ok(());
        }
        let mut demographics_changed = false;
        if path == "/api/v1/patients" || path.starts_with("/api/v1/patients/page?") {
            if let Some(rows) = value.as_array().or_else(|| value["items"].as_array()) {
                for row in rows {
                    let Some(id) = row
                        .get("id")
                        .and_then(Value::as_str)
                        .and_then(|v| Uuid::parse_str(v).ok())
                    else {
                        continue;
                    };
                    // A new local study needs its selected patient's identity,
                    // not a complete server directory. Keep loaded identities.
                    if path.starts_with("/api/v1/patients/page?") {
                        let pending = s.pending.iter().any(|op| op.entity_id == id);
                        if !pending {
                            let list = s
                                .responses
                                .entry("/api/v1/patients".into())
                                .or_insert(json!([]));
                            if let Some(list) = list.as_array_mut() {
                                if let Some(old) = list.iter_mut().find(|p| p["id"] == row["id"]) {
                                    if *old != *row {
                                        *old = row.clone();
                                        demographics_changed = true;
                                    }
                                } else {
                                    list.push(row.clone());
                                    demographics_changed = true;
                                }
                            }
                        }
                    }
                    demographics_changed |= crate::patient_demographics::reconcile_legacy_sex(
                        &mut s.patient_sexes, id, row,
                    );
                }
            }
        }
        let protected = s.pending.iter().any(|op| {
            op.scope() == path
                || study_id(&op.path).is_some_and(|id| path == format!("/api/v1/studies/{id}"))
        }) || s
            .journal
            .pointer("/detail/study/id")
            .and_then(Value::as_str)
            .is_some_and(|id| path == format!("/api/v1/studies/{id}"));
        let mut changed = demographics_changed;
        if protected && let Some(id) = study_id(path) {
            if s.remote_conflicts.get(&id) != Some(&value) {
                s.remote_conflicts.insert(id, value.clone());
                changed = true;
            }
        }
        if !protected {
            let mut merged = value.clone();
            if let Some(remote) = merged.as_array_mut()
                && let Some(local) = s.responses.get(path).and_then(Value::as_array)
            {
                for entity in local {
                    if s.pending
                        .iter()
                        .any(|op| entity.get("id") == Some(&json!(op.entity_id)))
                        && !remote.iter().any(|r| r.get("id") == entity.get("id"))
                    {
                        remote.push(entity.clone());
                    }
                }
            }
            changed |= replace_response(&mut s.responses, path, merged);
        }
        if changed {
            s.change();
        } else {
            self.performance
                .unchanged_responses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        drop(s);
        // Originals are requested explicitly by the selected preview/report.
        // Metadata refreshes must never download the entire image archive.
        Ok(())
    }
    pub fn install_auth(&self, raw: Connection) -> Result<(), String> {
        if self.stopping.load(std::sync::atomic::Ordering::SeqCst) { return Err("Рабочее место закрывается; дождитесь завершения выхода".into()); }
        let profile = self.state.lock().unwrap().profile.clone();
        if raw.user_id != Some(profile.user_id) || !crate::transport::same_server(&raw.storage_base, &profile.server) {
            return Err("Сначала завершите/синхронизируйте работу текущего врача; чужой кеш не объединяется".into());
        }
        *self.transport.lock().unwrap() = raw.clone();
        let mut cache=self.diagnosis_catalog.lock().unwrap();
        cache.rebind(&self.store,&raw);
        drop(cache);
        self.clear_asset_failures();
        self.paused
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.wake.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    pub fn remote(&self, id: Uuid) -> Option<Value> {
        self.state
            .lock()
            .unwrap()
            .remote_conflicts
            .get(&id)
            .cloned()
    }
    pub fn resolve_protocol(&self, detail: &StudyDetail) -> Result<(), String> {
        let _write = self.write_guard()?;
        self.require_edit_permission(false)?;
        let protocol = detail.protocol.clone().ok_or("Нет локального протокола")?;
        let mut s = self.state.lock().unwrap();
        let id = detail.study.id;
        if s.study_blocked(id).is_none() && !s.has_local_revision_conflict(id) {
            return Err("Нет конфликтующей версии этого обследования".into());
        }
        if s.pending
            .iter()
            .any(|op| study_id(&op.path) == Some(id) && op.started && op.blocked.is_none())
        {
            return Err(
                "Дождитесь подтверждения уже отправленной операции этого обследования".into(),
            );
        }
        let remote = s
            .remote_conflicts
            .get(&id)
            .cloned()
            .ok_or("Сначала загрузите и сравните серверную версию")?;
        let mut server: StudyDetail =
            serde_json::from_value(remote).map_err(|_| "Серверный снимок еще не готов")?;
        if server.study.id != id || server.study.patient_id != detail.study.patient_id {
            return Err("Несовпадение пациента/исследования".into());
        }
        let mut uploads = VecDeque::new();
        let mut unrelated = VecDeque::new();
        while let Some(op) = s.pending.pop_front() {
            if study_id(&op.path) == Some(id) {
                if op.blob.is_some() {
                    uploads.push_back(op);
                } else {
                    s.conflict_history.push(op);
                }
            } else {
                unrelated.push_back(op);
            }
        }
        if server.study.status == "approved" {
            let body = json!({"expected_version":server.study.row_version});
            Self::add_operation(
                &mut s,
                &format!("/api/v1/studies/{id}/reopen"),
                "POST",
                body,
                Uuid::new_v4(),
                None,
                None,
            );
            server.study.row_version += 1;
            server.study.status = "draft".into();
        }
        s.pending.extend(uploads);
        let input = SaveDraft {
            expected_version: server.study.row_version,
            protocol,
            annotations: detail.annotations.clone(),
        };
        let revision = Uuid::new_v4();
        Self::add_operation(
            &mut s,
            &format!("/api/v1/studies/{id}/protocol"),
            "PUT",
            json!(input),
            revision,
            None,
            None,
        );
        s.pending.extend(unrelated);
        if let Some(editor) = s.editors.get_mut(&id) {
            editor["revision_conflict"] = json!(false);
        }
        if s.journal.pointer("/detail/study/id") == Some(&json!(id)) {
            s.journal["revision_conflict"] = json!(false);
        }
        let mut merged = detail.clone();
        merged.study = server.study;
        merged.study.row_version += 1;
        merged.revision_id = Some(revision);
        for asset in server.assets {
            if !merged.assets.iter().any(|a| a.id == asset.id) {
                merged.assets.push(asset);
            }
        }
        s.responses
            .insert(format!("/api/v1/studies/{id}"), json!(merged));
        s.touch_metadata(&format!("/api/v1/studies/{id}"));
        s.error="Объединенный черновик поставлен в очередь; исходные конфликтующие операции сохранены в локальном архиве".into();
        s.change();
        self.wake_online(&s);
        Ok(())
    }
    pub fn shutdown(&self) -> Result<(), String> {
        self.workers.close();
        self.paused.store(true, std::sync::atomic::Ordering::SeqCst);
        self.stopping.store(true, std::sync::atomic::Ordering::SeqCst);
        if !self.workers.finish()? || !self.checkpoint_ready() || !self.store.try_release()? {
            return Err("Локальная запись еще не завершена".into());
        }
        Ok(())
    }
    pub fn revise_failed(&self, body: Value) -> Result<(), String> {
        let _write = self.write_guard()?;
        // Inspect without holding state across the transport permission lock.
        // Re-select by immutable ID afterwards, never by a possibly shifted index.
        let candidate = self.state.lock().unwrap().pending.iter()
            .find(|op| op.blocked.is_some()).cloned()
            .ok_or("Нет отклоненной операции")?;
        self.require_edit_permission(matches!(candidate.path.as_str(), "/api/v1/patients" | "/api/v1/studies"))?;
        let mut s = self.state.lock().unwrap();
        let index = s
            .pending
            .iter()
            .position(|op| op.id == candidate.id && op.path == candidate.path && op.blocked.is_some())
            .ok_or("Операция изменилась; повторите проверку конфликта")?;
        let old = s.pending[index].clone();
        if old.path.ends_with("/protocol") {
            return Err("Для протокола используйте сравнение и ручное объединение версий".into());
        }
        let mut revised = old.clone();
        revised.id = Uuid::new_v4();
        revised.body = body;
        revised.started = false;
        revised.blocked = None;
        revised.sequence = s.sequence + 1;
        s.conflict_history.push(old);
        s.pending[index] = revised;
        s.change();
        self.wake.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    pub fn conflict_for_study(&self, id: Uuid) -> Option<Value> {
        let state = self.state.lock().unwrap();
        if let Some(op) = state.study_blocked(id) {
            return Some(json!({"operation":op,"remote":study_id(&op.path).and_then(|study|state.remote_conflicts.get(&study).cloned())}));
        }
        state.has_local_revision_conflict(id).then(|| json!({
            "code":"local_revision_diverged","operation":null,"remote":state.remote_conflicts.get(&id)
        }))
    }
    pub fn conflict(&self) -> Option<Value> {
        let s = self.state.lock().unwrap();
        s.pending.iter().find(|op|op.blocked.is_some()).map(|op|json!({"operation":op,"remote":study_id(&op.path).and_then(|id|s.remote_conflicts.get(&id).cloned())}))
    }
}
fn replace_response(responses: &mut BTreeMap<String, Value>, path: &str, value: Value) -> bool {
    if responses.get(path) == Some(&value) {
        false
    } else {
        responses.insert(path.into(), value);
        true
    }
}
fn delivery_poll_studies(state: &LocalState) -> BTreeSet<Uuid> {
    state
        .responses
        .keys()
        .filter(|key| key.ends_with("-true"))
        .filter_map(|key| {
            key.strip_prefix("local-pdf/")?
                .split('/')
                .next()?
                .parse::<Uuid>()
                .ok()
        })
        .filter(|id| {
            state
                .responses
                .get(&format!("/api/v1/studies/{id}/results/1c"))
                .and_then(Value::as_array)
                .is_none_or(|jobs| {
                    jobs.is_empty()
                        || jobs.iter().any(|job| {
                            matches!(job["state"].as_str(), Some("pending" | "retry"))
                                || (job["state"] == "sent" && !crate::delivery::delivered(job))
                        })
                })
        })
        .collect()
}
fn append(responses: &mut BTreeMap<String, Value>, path: &str, value: Value) {
    responses
        .entry(path.into())
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .unwrap()
        .push(value);
}
fn study_id(path: &str) -> Option<Uuid> {
    path.strip_prefix("/api/v1/studies/")?
        .split('/')
        .next()
        .and_then(|id| Uuid::parse_str(id).ok())
}
/// Late replies must never acknowledge a different queue entry or a rewritten request.
fn queued_index(state: &LocalState, op: &Operation) -> Result<usize, String> {
    let index = state
        .pending
        .iter()
        .position(|current| current.id == op.id)
        .ok_or("Операция больше не ожидает подтверждения; очередь не изменена")?;
    let current = &state.pending[index];
    if current.path != op.path
        || current.method != op.method
        || current.blob != op.blob
        || current.request_hash() != op.request_hash()
    {
        return Err("Запрос изменился после отправки; подтверждение не применено".into());
    }
    Ok(index)
}
fn remap(s: &mut LocalState, old: Uuid, new: Uuid, current: Uuid) {
    s.aliases.insert(old, new);
    if let Some(sex) = s.patient_sexes.remove(&old) {
        s.patient_sexes.insert(new, sex);
    }
    fn replace(value: &mut Value, old: &str, new: &str) {
        match value {
            Value::String(s) if s == old => *s = new.into(),
            Value::Object(map) => {
                for value in map.values_mut() {
                    replace(value, old, new)
                }
            }
            Value::Array(values) => {
                for value in values {
                    replace(value, old, new)
                }
            }
            _ => {}
        }
    }
    let old_text = old.to_string();
    let new_text = new.to_string();
    let mut touched = Vec::new();
    for (path, value) in &mut s.responses {
        let before = value.clone();
        replace(value, &old_text, &new_text);
        if *value != before { touched.push(path.clone()); }
    }
    for path in touched { s.touch_metadata(&path); }
    replace(&mut s.journal, &old_text, &new_text);
    for editor in s.editors.values_mut() {
        replace(editor, &old_text, &new_text);
    }
    for op in s.pending.iter_mut() {
        if op.id == current || op.started {
            continue;
        }
        replace(&mut op.body, &old_text, &new_text);
        op.path = op.path.replace(&old_text, &new_text);
    }
    if let Some(blob) = s.blobs.get(&old).cloned() {
        s.blobs.insert(new, blob);
    }
}

fn normalize(value: &mut Value, aliases: &BTreeMap<Uuid, Uuid>) {
    match value {
        Value::String(text) => {
            for _ in 0..aliases.len() {
                if let Some(new) = Uuid::parse_str(text).ok().and_then(|id| aliases.get(&id)) {
                    *text = new.to_string();
                } else {
                    break;
                }
            }
        }
        Value::Object(map) => {
            for value in map.values_mut() {
                normalize(value, aliases);
            }
        }
        Value::Array(values) => {
            for value in values {
                normalize(value, aliases);
            }
        }
        _ => {}
    }
}

// An approval receipt can only approve its immutable revision, not newer local text.
fn approval_receipt_matches_revision(op: &Operation, detail: &Value) -> bool {
    !op.path.ends_with("/approve") || op.body.get("revision_id")
        .filter(|id|id.as_str().and_then(|s|Uuid::parse_str(s).ok()).is_some())
        .is_some_and(|id|Some(id)==detail.get("revision_id"))
}

#[cfg(test)]
#[path = "offline_performance_tests.rs"]
mod performance_tests;

#[cfg(test)]
mod pdf_transport_tests {
    #[test]
    fn persisted_approved_detail_is_evicted_without_losing_local_editor() {
        let (offline, _, _) = setup(200);
        let mut state = offline.state.lock().unwrap().clone();
        let study = Uuid::new_v4();
        let path = format!("/api/v1/studies/{study}");
        let approved = json!({"study":{"id":study,"status":"approved"},
            "revision_id":Uuid::new_v4(),"protocol":{"conclusion":"approved"}});
        let local = json!({"dirty":true,"detail":{"study":{"id":study,"status":"draft"},
            "protocol":{"conclusion":"unsent"}}});
        state.responses.insert(path.clone(), approved.clone());
        state.journal = json!({"dirty":false,"detail":approved,"protocol":{"conclusion":"approved"}});
        state.editors.insert(study, local.clone());
        assert!(state.evict_approved_protocols());
        assert!(!state.responses.contains_key(&path));
        assert!(state.journal.is_null());
        assert!(state.server_only_studies.contains(&study));
        assert_eq!(state.editors.get(&study), Some(&local));
        assert!(!state.evict_approved_protocols());
    }

    use super::*;
    use std::io::{Read, Write};
    fn setup(status: u16) -> (Arc<Offline>, Operation, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let bytes = b"%PDF-1.7 synthetic frozen bytes".to_vec();
        let hash = digest(&bytes);
        let study = Uuid::new_v4();
        let blob_id = Uuid::new_v4();
        let raw = Connection::new(&format!("http://{address}"), "synthetic-workstation").unwrap();
        let store = Store::new(
            std::env::temp_dir().join(format!("m53-pdf-contract-{}", Uuid::new_v4())),
            Arc::new(crate::cache::AesProtector([7; 32])),
        )
        .unwrap();
        store.write(&blob_id.simple().to_string(), &bytes).unwrap();
        let profile = Profile {
            server: raw.base.clone(),
            user_id: Uuid::new_v4(),
            doctor: "Synthetic".into(),
        };
        let op = Operation {
            id: Uuid::new_v4(),
            entity_id: blob_id,
            version_id: None,
            path: format!(
                "/api/v1/studies/{study}/results/1c/pdf?revision_id={}&template_version_id={}",
                Uuid::new_v4(),
                Uuid::new_v4()
            ),
            method: "POST".into(),
            body: json!({"sha256":hash}),
            blob: Some(blob_id),
            started: true,
            blocked: None,
            sequence: 1,
        };
        let state = LocalState {
            activity: None,
            activity_intents: BTreeMap::new(),
            format_version: 1,
            profile,
            responses: BTreeMap::new(),
            blobs: BTreeMap::new(),
            pending: VecDeque::from([op.clone()]),
            confirmed: BTreeMap::new(),
            journal: Value::Null,
            editors: BTreeMap::new(),
            server_only_studies: BTreeSet::new(),
            remote_conflicts: BTreeMap::new(),
            conflict_history: vec![],
            aliases: BTreeMap::new(),
            patient_sexes: BTreeMap::new(),
            sequence: 1,
            durable_sequence: 1,
            metadata_generations: BTreeMap::new(),
            recovery_errors: BTreeSet::new(),
            online: true,
            error: String::new(),
        };
        let (tx, _) = mpsc::channel();
        let offline = Arc::new(Offline {
            diagnosis_catalog: Mutex::new(crate::diagnosis_cache::State::empty()),
            state: Arc::new(Mutex::new(state)),
            store,
            memory: Mutex::new(MemoryCache::default()),
            thumbnails: Mutex::new(MemoryCache::default()),
            fetch: Mutex::new(FetchQueue::default()),
            asset_fetch: Mutex::new(FetchQueue::default()),
            foreground: Mutex::new(None),
            registry_paths: Mutex::new(vec![]),
            registry_epoch: std::sync::atomic::AtomicU64::new(0),
            performance: PerformanceCounters::default(),
            disk: tx,
            transport: Mutex::new(raw),
            wake: false.into(),
            paused: false.into(),
            stopping: false.into(),
            workers: Workers::default(),
            sync: Mutex::new(SyncSchedule::default()),
        });
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(4)))
                .unwrap();
            let mut request = vec![];
            let mut part = [0; 4096];
            let header_end = loop {
                let n = stream.read(&mut part).unwrap();
                assert!(n > 0);
                request.extend_from_slice(&part[..n]);
                if let Some(i) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]).to_lowercase();
            assert!(headers.starts_with("post /api/v1/studies/"));
            assert!(headers.contains("/results/1c/pdf?"));
            assert!(headers.contains("content-type: application/pdf"));
            assert!(headers.contains(&format!("x-content-sha256: {hash}")));
            assert!(!headers.contains("x-operation-id"));
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .unwrap()
                .trim()
                .parse::<usize>()
                .unwrap();
            while request.len() < header_end + length {
                let n = stream.read(&mut part).unwrap();
                request.extend_from_slice(&part[..n]);
            }
            assert_eq!(&request[header_end..], bytes.as_slice());
            let body=json!({"job_id":Uuid::new_v4(),"state":"pending","sequence":1,"payload_kind":"cache","cache_state":"ready","pdf_sha256":hash,"code":"result_snapshot_conflict"}).to_string();
            write!(stream,"HTTP/1.1 {status} Reply\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        });
        (offline, op, handle)
    }
    #[test]
    fn workflow_review_pdf_forbidden_keeps_request_for_reauthentication() {
        let (offline,op,server)=setup(403);
        let raw=offline.transport.lock().unwrap().clone();
        assert!(offline.sync_one(&raw,&op).is_err());server.join().unwrap();
        let state=offline.state.lock().unwrap();
        assert!(offline.paused.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(state.pending[0].request_hash(),op.request_hash());
        assert!(state.pending[0].blocked.is_none() && state.confirmed.is_empty());
    }
    #[test]
    fn pdf_uses_frozen_bytes_and_direct_job_receipt() {
        let (o, op, h) = setup(200);
        let raw = o.transport.lock().unwrap().clone();
        o.sync_one(&raw, &op).unwrap();
        h.join().unwrap();
        assert!(o.state.lock().unwrap().pending.is_empty());
        assert!(
            o.store
                .read(&op.blob.unwrap().simple().to_string())
                .unwrap()
                .is_some()
        );
    }
    #[test]
    fn metadata_http_error_preserves_connection_for_approval() {
        let mut state: LocalState = serde_json::from_value(json!({
            "format_version": 1,
            "profile": {"server": "http://127.0.0.1", "user_id": Uuid::new_v4(), "doctor": "Synthetic"},
            "responses": {}, "blobs": {}, "pending": [], "confirmed": {},
            "journal": null, "editors": {}, "remote_conflicts": {},
            "conflict_history": [], "sequence": 1
        })).unwrap();
        state.online = true;
        Offline::record_metadata_failure(&mut state, "API пока недоступен (404)".into());
        assert!(state.online);
        Offline::record_metadata_failure(
            &mut state,
            "Связь с API прервана; работа сохраняется локально".into(),
        );
        assert!(!state.online);
    }

    #[test]
    fn accepted_202_is_not_delivery_confirmation() {
        let (o, op, h) = setup(202);
        let raw = o.transport.lock().unwrap().clone();
        assert!(o.sync_one(&raw, &op).is_err());
        h.join().unwrap();
        assert_eq!(o.state.lock().unwrap().pending.len(), 1);
    }
    #[test]
    fn conflict_keeps_pdf_and_blocks_the_same_operation() {
        let (o, op, h) = setup(409);
        let raw = o.transport.lock().unwrap().clone();
        o.sync_one(&raw, &op).unwrap();
        h.join().unwrap();
        let state = o.state.lock().unwrap();
        assert_eq!(state.pending.front().unwrap().id, op.id);
        assert!(state.pending.front().unwrap().blocked.is_some());
    }
    #[test]
    fn nonfront_pdf_acknowledges_only_its_exact_operation() {
        let (o, op, h) = setup(200);
        let mut other = op.clone();
        other.id = Uuid::new_v4();
        other.path = format!("/api/v1/studies/{}/protocol", Uuid::new_v4());
        other.blocked = Some("protocol_validation_failed".into());
        let before = serde_json::to_value(&other).unwrap();
        o.state.lock().unwrap().pending.push_front(other);
        let raw = o.transport.lock().unwrap().clone();
        o.sync_one(&raw, &op).unwrap();
        h.join().unwrap();
        let state = o.state.lock().unwrap();
        assert_eq!(state.pending.len(), 1);
        assert_eq!(serde_json::to_value(&state.pending[0]).unwrap(), before);
        assert_eq!(state.confirmed.get(&op.id), Some(&op.request_hash()));
    }
    #[test]
    fn nonfront_pdf_rejection_blocks_only_its_exact_operation() {
        let (o, op, h) = setup(409);
        let mut other = op.clone();
        other.id = Uuid::new_v4();
        other.path = format!("/api/v1/studies/{}/protocol", Uuid::new_v4());
        let before = serde_json::to_value(&other).unwrap();
        o.state.lock().unwrap().pending.push_front(other);
        let raw = o.transport.lock().unwrap().clone();
        o.sync_one(&raw, &op).unwrap();
        h.join().unwrap();
        let state = o.state.lock().unwrap();
        assert_eq!(state.pending.len(), 2);
        assert_eq!(serde_json::to_value(&state.pending[0]).unwrap(), before);
        assert!(state.pending[1].blocked.is_some());
        assert!(state.confirmed.is_empty());
    }
}

/// One retry per attach of a historically rejected, now structurally valid draft.
/// Frozen id/hash/version/body are preserved; sync_one queries its receipt first.
/// A fresh rejection stays blocked until the next explicit application launch.
fn retry_validated_draft_rejections(state: &mut LocalState) {
    use m53_domain::{protocol::Protocol, validation::{Dictionary, ValidationMode, validate}};
    let mut changed = false;
    for op in &mut state.pending {
        let rejected = op.method == "PUT" && op.path.ends_with("/protocol") && op.started
            && op.blocked.as_deref().and_then(|m|m.strip_prefix("Backend 400: "))
                .and_then(|m|serde_json::from_str::<Value>(m).ok())
                .is_some_and(|v|v["code"] == "protocol_validation_failed");
        if !rejected { continue; }
        let Ok(protocol) = serde_json::from_value::<Protocol>(op.body["protocol"].clone()) else { continue; };
        let dictionary = state.responses.values().filter(|v|v.get("terms").is_some()).find_map(|v|serde_json::from_value::<Dictionary>(v.clone()).ok()
            .filter(|d|d.version_id==protocol.terminology_version_id));
        if dictionary.is_some_and(|d|validate(&protocol,&d,ValidationMode::Draft).is_empty()) {
            op.blocked = None;
            changed = true;
        }
    }
    if changed { state.change(); }
}

fn archive_stale_reopen(state: &mut LocalState, study: Uuid, confirmed: &StudyDetail) {
    if confirmed.study.id != study || confirmed.study.status != "draft"
        || confirmed.revision_id.is_none() || confirmed.protocol.is_none()
    {
        return;
    }
    let reopen_path = format!("/api/v1/studies/{study}/reopen");
    let mut retired = false;
    let mut index = 0;
    while index < state.pending.len() {
        let op = &state.pending[index];
        let superseded = op.method == "POST" && op.path == reopen_path
            && op.body["expected_version"].as_i64().is_some_and(|version| version < confirmed.study.row_version)
            && op.blocked.as_deref().and_then(|error| error.strip_prefix("Backend 409: "))
                .and_then(|body| serde_json::from_str::<Value>(body).ok())
                .is_some_and(|body| body["code"] == "version_conflict");
        if superseded {
            let op = state.pending.remove(index).unwrap();
            state.conflict_history.push(op);
            retired = true;
        } else {
            index += 1;
        }
    }
    if retired {
        if state.study_blocked(study).is_none() && !state.has_local_revision_conflict(study) {
            state.remote_conflicts.remove(&study);
        }
        state.change();
    }
}

fn archive_retired_publications(state: &mut LocalState) -> bool {
    let mut retained=VecDeque::new();
    let mut retired=false;
    while let Some(mut operation)=state.pending.pop_front() {
        let publication=operation.method=="POST" && operation.scope()
            .strip_prefix("/api/v1/studies/")
            .and_then(|path|path.split_once('/'))
            .is_some_and(|(id,tail)|tail=="publications" && Uuid::parse_str(id).is_ok());
        if publication {
            const NOTICE:&str="Передача в PACS удалена. Старая операция сохранена в архиве без подтверждения доставки";
            operation.blocked=Some(match operation.blocked.take() {
                Some(previous)=>format!("{NOTICE}; {previous}"),
                None=>NOTICE.into(),
            });
            state.conflict_history.push(operation);
            retired=true;
        } else {
            retained.push_back(operation);
        }
    }
    state.pending=retained;
    if retired {state.error="Старая очередь передачи в PACS сохранена в архиве без подтверждения доставки".into();}
    retired
}

fn archive_rejected_approval(state: &mut LocalState) {
    let mut retained = VecDeque::new();
    let mut studies = Vec::new();
    let mut identity_rejected = false;
    while let Some(operation) = state.pending.pop_front() {
        let rejected=operation.blocked.as_deref().is_some_and(|message| {
            let code=message.find('{').and_then(|i|serde_json::from_str::<Value>(&message[i..]).ok());
            let code=code.as_ref().and_then(|v|v["code"].as_str());
            (operation.path.ends_with("/approve") && ((message.starts_with("Backend 400:") && code==Some("protocol_validation_failed")) || (message.starts_with("Backend 409:") && code==Some("patient_identity_unconfirmed"))))
            || (operation.path.ends_with("/manual-identity-verification") && message.starts_with("Backend 409:") && matches!(code,Some("patient_version_conflict"|"patient_changed"|"manual_verification_not_applicable")))
        });
        if rejected {
            let identity=operation.blocked.as_deref().is_some_and(|m|m.starts_with("Backend 409:"));
            identity_rejected |= identity;
            if let Some(id) = study_id(&operation.path) {
                let proven_same_version=operation.body.get("expected_version").and_then(Value::as_i64).is_some_and(|expected| {
                    state.remote_conflicts.get(&id).and_then(|v|v.pointer("/study/row_version")).and_then(Value::as_i64)==Some(expected)
                });
                if !identity || proven_same_version { studies.push(id); }
            }
            state.conflict_history.push(operation);
        } else {
            retained.push_back(operation);
        }
    }
    state.pending = retained;
    if !studies.is_empty() || identity_rejected {
        for id in studies {
            if !state
                .pending
                .iter()
                .any(|op| study_id(&op.path) == Some(id))
                && !state.has_local_revision_conflict(id)
            {
                state.remote_conflicts.remove(&id);
            }
        }
        state.error = if identity_rejected { "Сервер отклонил утверждение: подтвердите личность пациента. Черновик сохранён; это не конфликт версий".into() } else { "Сервер отклонил утверждение: проверьте обязательные поля и готовность словаря. Черновик сохранен; это не конфликт версий".into() };
        state.change();
    }
}

#[cfg(test)]
mod rejected_approval_tests {
    use super::*;
    #[test]
    fn valid_historic_draft_retries_exact_frozen_request_without_acknowledging_it() {
        let mut s = state("protocol_validation_failed");
        let version = Uuid::new_v4();
        let protocol = crate::model::blank_protocol(version);
        s.responses.insert("dictionary".into(), json!({"version_id":version,"clinically_approved":false,"terms":[]}));
        s.pending[0].path = s.pending[0].path.replace("/approve", "/protocol");
        s.pending[0].method = "PUT".into();
        s.pending[0].body = json!({"expected_version":8,"protocol":protocol,"annotations":[]});
        let before = s.pending[0].clone();
        retry_validated_draft_rejections(&mut s);
        assert!(s.pending[0].blocked.is_none());
        assert_eq!(s.pending[0].id, before.id);
        assert_eq!(s.pending[0].request_hash(), before.request_hash());
        assert_eq!(s.pending[0].body, before.body);
        assert!(s.pending[0].started && s.confirmed.is_empty());
        assert_eq!(s.pending.len(), 1);
        // Ordinary scheduler does not call the recovery again after a fresh 400.
        s.pending[0].blocked = before.blocked;
        assert!(crate::outbox::next_index(&s, &BTreeSet::new()).is_none());
    }
    #[test]
    fn historic_retry_never_clears_conflicts_auth_failures_or_unknown_dictionaries() {
        for code in ["version_conflict", "forbidden", "protocol_validation_failed"] {
            let mut s = state(code);
            s.pending[0].path = s.pending[0].path.replace("/approve", "/protocol");
            s.pending[0].method = "PUT".into();
            s.pending[0].body = json!({"protocol":crate::model::blank_protocol(Uuid::new_v4())});
            let before = serde_json::to_value(&s.pending).unwrap();
            retry_validated_draft_rejections(&mut s);
            assert_eq!(serde_json::to_value(&s.pending).unwrap(), before);
        }
    }
    fn state(code: &str) -> LocalState {
        let study = Uuid::new_v4();
        let op = Operation {
            id: Uuid::new_v4(),
            entity_id: study,
            version_id: None,
            path: format!("/api/v1/studies/{study}/approve"),
            method: "POST".into(),
            body: json!({"expected_version":8}),
            blob: None,
            started: true,
            blocked: Some(format!(
                "Backend {}: {}",
                if code == "version_conflict" { 409 } else { 400 },
                json!({"code":code})
            )),
            sequence: 1,
        };
        serde_json::from_value(json!({"format_version":1,"profile":{"server":"http://127.0.0.1","user_id":Uuid::new_v4(),"doctor":"Synthetic"},"responses":{},"blobs":{},"pending":[op],"confirmed":{},"journal":{"important":"preserved"},"editors":{},"remote_conflicts":{},"conflict_history":[],"sequence":1})).unwrap()
    }
    #[test]
    fn delayed_approval_receipt_never_approves_newer_local_anamnesis() {
        let s=state("version_conflict");let mut op=s.pending[0].clone();
        let original=Uuid::new_v4();let newer=Uuid::new_v4();op.body["revision_id"]=json!(original);
        assert!(approval_receipt_matches_revision(&op,&json!({"revision_id":original})));
        assert!(!approval_receipt_matches_revision(&op,&json!({"revision_id":newer})));
        assert!(!approval_receipt_matches_revision(&op,&json!({})));
        op.body["revision_id"]=Value::Null;
        assert!(!approval_receipt_matches_revision(&op,&json!({"revision_id":null})));
        op.path=op.path.replace("/approve","/reopen");
        assert!(approval_receipt_matches_revision(&op,&json!({"revision_id":newer})));
    }
    #[test]
    fn rejected_approval_preserves_document_and_history_without_false_conflict() {
        let mut s = state("protocol_validation_failed");
        let journal = s.journal.clone();
        let id = s.pending[0].id;
        archive_rejected_approval(&mut s);
        assert!(s.pending.is_empty());
        assert_eq!(s.conflict_history[0].id, id);
        assert_eq!(s.journal, journal);
        assert!(s.error.contains("не конфликт версий"));
    }
    #[test]
    fn identity_rejection_archives_exact_request_and_clears_only_false_conflict() {
        let mut s=state("patient_identity_unconfirmed");
        s.pending[0].blocked=Some("Backend 409: {\"code\":\"patient_identity_unconfirmed\"}".into());
        let original=s.pending[0].clone();let study=study_id(&original.path).unwrap();
        s.remote_conflicts.insert(study,json!({"study":{"row_version":8}}));
        let journal=s.journal.clone();
        archive_rejected_approval(&mut s);
        assert!(s.pending.is_empty());assert!(!s.remote_conflicts.contains_key(&study));
        assert_eq!(s.conflict_history[0].request_hash(),original.request_hash());
        assert_eq!(s.journal,journal);assert!(s.confirmed.is_empty());
        assert!(s.error.contains("пациента"));
    }
    #[test]
    fn stale_manual_verification_preserves_request_and_requires_fresh_context() {
        let mut s=state("patient_identity_unconfirmed");
        s.pending[0].path=s.pending[0].path.replace("/approve","/manual-identity-verification");
        s.pending[0].blocked=Some("Backend 409: {\"code\":\"patient_version_conflict\"}".into());
        let study=study_id(&s.pending[0].path).unwrap();
        s.remote_conflicts.insert(study,json!({"study":{"row_version":8}}));
        let original=s.pending[0].clone();archive_rejected_approval(&mut s);
        assert!(s.pending.is_empty());assert_eq!(s.conflict_history[0].request_hash(),original.request_hash());
        assert!(!s.remote_conflicts.contains_key(&study));assert!(s.confirmed.is_empty());
    }
    #[test]
    fn identity_rejection_preserves_remote_with_newer_version() {
        let mut s=state("patient_identity_unconfirmed");
        s.pending[0].blocked=Some("Backend 409: {\"code\":\"patient_identity_unconfirmed\"}".into());
        let study=study_id(&s.pending[0].path).unwrap();
        s.remote_conflicts.insert(study,json!({"study":{"row_version":9}}));
        archive_rejected_approval(&mut s);
        assert!(s.remote_conflicts.contains_key(&study));
        assert!(s.confirmed.is_empty());
    }
    #[test]
    fn rejected_approval_preserves_a_separately_detected_local_revision_conflict() {
        for identity in [false,true] {
            let mut s=state("protocol_validation_failed");
            if identity {
                s.pending[0].blocked=Some("Backend 409: {\"code\":\"patient_identity_unconfirmed\"}".into());
            }
            let study=study_id(&s.pending[0].path).unwrap();
            let remote=json!({"study":{"row_version":8},"protocol":{"history":"Confirmed remote"}});
            s.remote_conflicts.insert(study,remote.clone());
            s.editors.insert(study,json!({"revision_conflict":true,"dirty":true}));
            let original=s.pending[0].clone();
            archive_rejected_approval(&mut s);
            assert!(s.pending.is_empty());
            assert_eq!(s.conflict_history[0].request_hash(),original.request_hash());
            assert!(s.has_local_revision_conflict(study));
            assert_eq!(s.remote_conflicts.get(&study),Some(&remote));
            assert!(s.confirmed.is_empty());
        }
    }
    #[test]
    fn genuine_version_conflict_is_never_removed() {
        let mut s = state("version_conflict");
        let id = s.pending[0].id;
        archive_rejected_approval(&mut s);
        assert_eq!(s.pending[0].id, id);
        assert!(s.conflict_history.is_empty());
    }
    #[test]
    fn rejected_nonfront_approval_archives_only_that_request() {
        let mut s = state("protocol_validation_failed");
        let approval = s.pending[0].clone();
        let mut other = approval.clone();
        other.id = Uuid::new_v4();
        other.path = format!("/api/v1/studies/{}/protocol", Uuid::new_v4());
        let before = serde_json::to_value(&other).unwrap();
        s.pending.push_front(other);
        archive_rejected_approval(&mut s);
        assert_eq!(s.pending.len(), 1);
        assert_eq!(serde_json::to_value(&s.pending[0]).unwrap(), before);
        assert_eq!(s.conflict_history[0].id, approval.id);
    }
    #[test]
    fn late_receipt_requires_matching_id_hash_path_and_method() {
        let mut s = state("version_conflict");
        let original = s.pending[0].clone();
        let mut other = original.clone();
        other.id = Uuid::new_v4();
        s.pending.push_front(other);
        assert_eq!(queued_index(&s, &original).unwrap(), 1);
        s.pending[1].body = json!({"changed":true});
        assert!(queued_index(&s, &original).is_err());
        s.pending[1] = original.clone();
        s.pending[1].path.push_str("?changed=true");
        assert!(queued_index(&s, &original).is_err());
        s.pending.remove(1);
        assert!(queued_index(&s, &original).is_err());
        assert!(s.confirmed.is_empty());
    }
    #[test]
    fn alias_remap_never_rewrites_a_started_retry_or_current_request() {
        let mut s = state("version_conflict");
        let current = s.pending[0].id;
        let old = Uuid::new_v4();
        let new = Uuid::new_v4();
        s.pending[0].body = json!({"asset_id":old});
        let first = s.pending[0].clone();
        let mut later = first.clone();
        later.id = Uuid::new_v4();
        later.started = false;
        s.pending.push_back(later);
        remap(&mut s, old, new, current);
        assert_eq!(s.pending[0].request_hash(), first.request_hash());
        assert_eq!(s.pending[1].body["asset_id"], json!(new));
    }
}

#[cfg(test)]
mod study_receipt_tests {
    use super::*;
    use crate::cache::AesProtector;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    fn fixture(replies: Vec<(u16, Value)>) -> (Offline, std::thread::JoinHandle<()>) {
        fixture_recording(replies,None)
    }
    fn fixture_recording(replies: Vec<(u16, Value)>, records:Option<mpsc::Sender<Vec<u8>>>) -> (Offline, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let raw = Connection::new(
            &format!("http://{}", listener.local_addr().unwrap()),
            "synthetic-key",
        )
        .unwrap();
        let store = Store::new(
            std::env::temp_dir().join(format!("m53-scoped-receipt-{}", Uuid::new_v4())),
            Arc::new(AesProtector([19; 32])),
        )
        .unwrap();
        let mut state:LocalState=serde_json::from_value(json!({"format_version":1,"profile":{"server":raw.base,"user_id":Uuid::new_v4(),"doctor":"Synthetic"},"responses":{},"blobs":{},"pending":[],"confirmed":{},"journal":{},"editors":{},"remote_conflicts":{},"conflict_history":[],"sequence":1})).unwrap();
        state.online = true;
        state.durable_sequence = 1;
        let (disk, _) = mpsc::channel();
        let offline = Offline {
            diagnosis_catalog: Mutex::new(crate::diagnosis_cache::State::empty()),
            state: Arc::new(Mutex::new(state)),
            store,
            memory: Mutex::new(MemoryCache::default()),
            thumbnails: Mutex::new(MemoryCache::default()),
            fetch: Mutex::new(FetchQueue::default()),
            asset_fetch: Mutex::new(FetchQueue::default()),
            foreground: Mutex::new(None),
            registry_paths: Mutex::new(vec![]),
            registry_epoch: std::sync::atomic::AtomicU64::new(0),
            performance: PerformanceCounters::default(),
            disk,
            transport: Mutex::new(raw),
            wake: false.into(),
            paused: false.into(),
            stopping: false.into(),
            workers: Workers::default(),
            sync: Mutex::new(SyncSchedule::default()),
        };
        let handle = thread::spawn(move || {
            for (status, body) in replies {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(4)))
                    .unwrap();
                let mut request = Vec::new();
                let mut chunk = [0u8; 4096];
                let end = loop {
                    let n = socket.read(&mut chunk).unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&chunk[..n]);
                    if let Some(i) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                let length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .and_then(|s| s.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                while request.len() < end + length {
                    let n = socket.read(&mut chunk).unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&chunk[..n]);
                }
                if let Some(records)=&records {records.send(request.clone()).unwrap();}
                let body = body.to_string();
                write!(socket,"HTTP/1.1 {status} Reply\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        (offline, handle)
    }
    fn operation(study: Uuid) -> Operation {
        Operation {
            id: Uuid::new_v4(),
            entity_id: Uuid::new_v4(),
            version_id: None,
            path: format!("/api/v1/studies/{study}/protocol"),
            method: "PUT".into(),
            body: json!({"expected_version":1}),
            blob: None,
            started: true,
            blocked: None,
            sequence: 1,
        }
    }
    fn receipt(op: &Operation) -> Value {
        json!({"operation_id":op.id,"scope":op.scope(),"request_sha256":op.request_hash(),"response":{"revision_id":op.entity_id,"row_version":2}})
    }
    #[test]
    fn manual_verification_queues_without_fabricating_ack_or_approval() {
        let (offline,server)=fixture(vec![]);server.join().unwrap();
        {let mut raw=offline.transport.lock().unwrap();raw.offline_deadline=Some((crate::one_c_auth::now()+60,Instant::now()+Duration::from_secs(60)));}
        offline.state.lock().unwrap().online=true;
        let id=Uuid::new_v4();let path=format!("/api/v1/studies/{id}/manual-identity-verification");
        let body=json!({"expected_version":8,"patient_id":Uuid::new_v4(),"expected_patient_version":2,"confirmed":true});
        assert!(offline.mutate(&path,"POST",body.clone()).is_err());
        let state=offline.state.lock().unwrap();
        assert_eq!(state.pending.len(),1);assert_eq!(state.pending[0].body,body);
        assert!(state.confirmed.is_empty());assert_eq!(state.pending[0].path,path);
    }
    #[test]
    fn workflow_review_valid_grant_allows_full_offline_creation_and_existing_draft() {
        let (offline,server)=fixture(vec![]);server.join().unwrap();
        {let mut raw=offline.transport.lock().unwrap();
            raw.offline_deadline=Some((crate::one_c_auth::now()+60,Instant::now()+Duration::from_secs(60)));
            raw.offline_permissions=Some(m53_contracts::access::OfflinePermissions{
                allow_new_manual_encounter:true,allow_continue_open_encounter:true,allow_local_print:true});}
        offline.state.lock().unwrap().online=false;
        let patient=offline.mutate("/api/v1/patients","POST",json!({"family_name":"Synthetic","given_name":"Patient"})).unwrap();
        let study=offline.mutate("/api/v1/studies","POST",json!({"patient_id":patient["id"],"colposcope_model":"Model A"})).unwrap();
        let protocol=crate::model::blank_protocol(Uuid::new_v4());
        let saved=offline.mutate(&format!("/api/v1/studies/{}/protocol",study["id"].as_str().unwrap()),"PUT",
            json!({"expected_version":study["row_version"],"protocol":protocol,"annotations":[]})).unwrap();
        assert!(saved["revision_id"].as_str().is_some());
        let state = offline.state.lock().unwrap();
        assert_eq!(state.pending.len(),3);
        assert_eq!(state.pending[1].body["colposcope_model"], "Model A");
    }
    #[test]
    fn workflow_review_expired_running_grant_stops_new_actions_but_preserves_input() {
        for clock_rolled_back in [false,true] {
            let (offline,server)=fixture(vec![]);server.join().unwrap();
            let now=crate::one_c_auth::now();let tick=Instant::now();
            offline.transport.lock().unwrap().offline_deadline=Some(if clock_rolled_back {
                (now+60,tick-Duration::from_secs(1))} else {(now-1,tick+Duration::from_secs(60))});
            let before=serde_json::to_value(&*offline.state.lock().unwrap()).unwrap();
            let result=offline.mutate("/api/v1/patients","POST",json!({"family_name":"Synthetic","given_name":"Patient"}));
            assert!(result.is_err(),"expired signed grant still permits new encounters in an open app");
            assert_eq!(serde_json::to_value(&*offline.state.lock().unwrap()).unwrap(),before);
            offline.checkpoint(json!({"recovery_text":"Typed input stays recoverable"}));
            assert_eq!(offline.journal()["recovery_text"],"Typed input stays recoverable");
        }
    }
    #[test]
    fn workflow_review_upload_respects_same_offline_policy_as_protocol_edits() {
        let (offline,server)=fixture(vec![]);server.join().unwrap();
        offline.transport.lock().unwrap().offline_permissions=Some(m53_contracts::access::OfflinePermissions {
            allow_new_manual_encounter:false,allow_continue_open_encounter:false,allow_local_print:false});
        let study=Uuid::new_v4();
        offline.state.lock().unwrap().responses.insert(format!("/api/v1/studies/{study}"),
            json!({"study":{"id":study,"status":"draft"},"assets":[]}));
        let before=serde_json::to_value(&*offline.state.lock().unwrap()).unwrap();
        assert!(offline.upload(study,"derived",None,vec![1,2,3]).is_err());
        assert_eq!(serde_json::to_value(&*offline.state.lock().unwrap()).unwrap(),before);
    }
    #[test]
    fn workflow_review_lost_ack_and_503_retry_keep_exact_operation_and_order() {
        for committed in [true,false] {
            let study=Uuid::new_v4();let op=operation(study);let frozen=op.request_hash();
            let replies=if committed {vec![(404,json!({})),(200,json!({})),(503,json!({})),(200,receipt(&op))]}
                else {vec![(503,json!({})),(404,json!({})),(200,json!({})),(200,receipt(&op))]};
            let (tx,rx)=mpsc::channel();let (offline,server)=fixture_recording(replies,Some(tx));
            offline.state.lock().unwrap().pending.push_back(op.clone());
            let raw=offline.transport.lock().unwrap().clone();
            assert!(offline.sync_one(&raw,&op).is_err());
            {let s=offline.state.lock().unwrap();assert!(s.confirmed.is_empty());
                assert_eq!(s.pending[0].request_hash(),frozen);assert_eq!(s.pending[0].id,op.id);}
            // A retry after availability returns uses the same receipt/operation ID.
            offline.sync_one(&raw,&op).unwrap();server.join().unwrap();
            let requests:Vec<_>=rx.into_iter().collect();
            let writes:Vec<_>=requests.iter().filter(|r|r.starts_with(b"PUT ")).collect();
            assert_eq!(writes.len(),1,"committed mutation was sent twice instead of looking up receipt");
            let write=writes[0];let end=write.windows(4).position(|w|w==b"\r\n\r\n").unwrap()+4;
            assert_eq!(serde_json::from_slice::<Value>(&write[end..]).unwrap(),op.body);
            assert!(String::from_utf8_lossy(&write[..end]).to_ascii_lowercase().contains(&op.id.to_string()));
            let s=offline.state.lock().unwrap();assert!(s.pending.is_empty());
            assert_eq!(s.confirmed.get(&op.id),Some(&frozen));
        }
    }
    #[test]
    fn workflow_review_reopen_receipt_moves_to_exact_new_revision() {
        for legacy in [false,true] {
            let study=Uuid::new_v4();let original=Uuid::new_v4();let reopened=Uuid::new_v4();
            let mut op=operation(study);op.path=format!("/api/v1/studies/{study}/reopen");op.method="POST".into();
            let mut reply=receipt(&op);reply["response"]=if legacy {json!({"row_version":2})}
                else {json!({"row_version":2,"revision_id":reopened})};
            let (offline,server)=fixture(vec![(200,reply)]);let path=format!("/api/v1/studies/{study}");
            {let mut s=offline.state.lock().unwrap();s.pending.push_back(op.clone());
                s.responses.insert(path.clone(),json!({"study":{"status":"approved","row_version":1},"revision_id":original}));}
            let raw=offline.transport.lock().unwrap().clone();offline.sync_one(&raw,&op).unwrap();server.join().unwrap();
            let s=offline.state.lock().unwrap();
            assert_eq!(s.responses[&path]["study"]["status"],"draft");
            assert_eq!(s.responses[&path]["revision_id"],if legacy {Value::Null} else {json!(reopened)});
            assert_eq!(s.confirmed.get(&op.id),Some(&op.request_hash()));
        }
    }
    #[test]
    fn stale_reopen_receipt_cannot_replace_newer_approved_revision() {
        let study = Uuid::new_v4();
        let newer = Uuid::new_v4();
        let path = format!("/api/v1/studies/{study}");
        let mut op = operation(study);
        op.path = format!("{path}/reopen");
        op.method = "POST".into();
        let (offline, server) = fixture(vec![(200, receipt(&op))]);
        let current = json!({"study":{"id":study,"status":"approved","row_version":3},
            "revision_id":newer,"protocol":{"conclusion":"newer"}});
        {
            let mut state = offline.state.lock().unwrap();
            state.responses.insert(path.clone(), current.clone());
            state.server_only_studies.insert(study);
            state.pending.push_back(op.clone());
        }
        let raw = offline.transport.lock().unwrap().clone();
        offline.sync_one(&raw, &op).unwrap();
        server.join().unwrap();
        let state = offline.state.lock().unwrap();
        assert_eq!(state.responses[&path], current);
        assert!(state.server_only_studies.contains(&study));
        assert!(state.pending.is_empty());
        assert_eq!(state.confirmed.get(&op.id), Some(&op.request_hash()));
    }

    #[test]
    fn workflow_review_old_401_cannot_revoke_new_session() {
        let (offline,server)=fixture(vec![(401,json!({"code":"expired_session"}))]);
        let mut old=offline.transport.lock().unwrap().clone();
        old.user_id=Some(offline.state.lock().unwrap().profile.user_id);old.session="old-synthetic-session".into();
        *offline.transport.lock().unwrap()=old.clone();let mut current=old.clone();
        current.session="new-synthetic-session".into();
        offline.install_auth(current).unwrap();
        assert!(offline.fetch_one(&old,"/api/v1/templates").is_err());server.join().unwrap();
        assert!(!offline.paused.load(std::sync::atomic::Ordering::SeqCst),"stale HTTP 401 paused a newer authenticated session");
    }
    #[test]
    fn actual_delayed_ack_keeps_newer_local_draft_and_exact_receipt() {
        for newer in [false,true] {
            let study=Uuid::new_v4();let original=Uuid::new_v4();let mut op=operation(study);
            op.path=format!("/api/v1/studies/{study}/approve");op.method="POST".into();
            op.body=json!({"revision_id":original,"expected_version":1});
            let local_revision=if newer {Uuid::new_v4()} else {original};
            let mut reply = receipt(&op);
            reply["response"]["revision_id"] = json!(original);
            let (offline,server)=fixture(vec![(200,reply)]);
            let path=format!("/api/v1/studies/{study}");
            let local=json!({"study":{"id":study,"status":"draft","row_version":if newer {3} else {1}},
                "revision_id":local_revision,"protocol":{"history":{"value":{"state":"assessed","value":"Local history"}}}});
            {let mut state=offline.state.lock().unwrap();state.pending.push_back(op.clone());
                state.responses.insert(path.clone(),local.clone());state.journal=json!({"detail":local});}
            let raw=offline.transport.lock().unwrap().clone();offline.sync_one(&raw,&op).unwrap();server.join().unwrap();
            let state=offline.state.lock().unwrap();
            assert!(state.pending.is_empty() && state.confirmed.contains_key(&op.id));
            assert_eq!(state.responses[&path]["revision_id"],json!(local_revision));
            assert_eq!(state.responses[&path]["protocol"],local["protocol"]);
            assert_eq!(state.responses[&path]["study"]["status"],if newer {"draft"} else {"approved"});
            assert_eq!(state.journal["detail"]["protocol"],local["protocol"]);
        }
    }
    #[test]
    fn ordinary_nonfront_receipt_keeps_rejected_other_study_unchanged() {
        let mut other = operation(Uuid::new_v4());
        other.blocked = Some("Backend 400: {\"code\":\"protocol_validation_failed\"}".into());
        let op = operation(Uuid::new_v4());
        let before = json!(other);
        let (o, handle) = fixture(vec![(200, receipt(&op))]);
        o.state.lock().unwrap().pending = VecDeque::from([other, op.clone()]);
        let raw = o.transport.lock().unwrap().clone();
        o.sync_one(&raw, &op).unwrap();
        handle.join().unwrap();
        let s = o.state.lock().unwrap();
        assert_eq!(s.pending.len(), 1);
        assert_eq!(json!(s.pending[0]), before);
        assert_eq!(s.confirmed.get(&op.id), Some(&op.request_hash()));
    }
    #[test]
    fn nonfront_cas_rejection_keeps_both_requests_and_scopes_the_conflict() {
        let other = operation(Uuid::new_v4());
        let b = Uuid::new_v4();
        let op = operation(b);
        let before = json!(other);
        let (o, handle) = fixture(vec![
            (404, json!({})),
            (409, json!({"code":"version_conflict"})),
            (200, json!({"study":{"id":b},"protocol":{}})),
        ]);
        o.state.lock().unwrap().pending = VecDeque::from([other, op.clone()]);
        let raw = o.transport.lock().unwrap().clone();
        o.sync_one(&raw, &op).unwrap();
        handle.join().unwrap();
        let s = o.state.lock().unwrap();
        assert_eq!(s.pending.len(), 2);
        assert_eq!(json!(s.pending[0]), before);
        assert!(
            s.pending[1]
                .blocked
                .as_ref()
                .unwrap()
                .contains("version_conflict")
        );
        assert!(s.confirmed.is_empty());
        assert!(s.study_blocked(b).is_some());
    }
    #[test]
    fn mismatched_receipt_never_acknowledges_another_queue_entry() {
        let other = operation(Uuid::new_v4());
        let op = operation(Uuid::new_v4());
        let (o, handle) = fixture(vec![(200, receipt(&other))]);
        o.state.lock().unwrap().pending = VecDeque::from([other, op.clone()]);
        let before = json!(o.state.lock().unwrap().pending);
        let raw = o.transport.lock().unwrap().clone();
        assert!(o.sync_one(&raw, &op).is_err());
        handle.join().unwrap();
        let s = o.state.lock().unwrap();
        assert_eq!(json!(s.pending), before);
        assert!(s.confirmed.is_empty());
    }
    #[test]
    fn approval_barrier_counts_only_current_study_and_dependencies() {
        let mut other = operation(Uuid::new_v4());
        other.blocked = Some("validation".into());
        let study = Uuid::new_v4();
        let (o, handle) = fixture(vec![]);
        handle.join().unwrap();
        o.state.lock().unwrap().pending.push_back(other);
        let path = format!("/api/v1/studies/{study}/approve");
        assert!(
            o.mutate(&path, "POST", json!({"expected_version":2}))
                .unwrap_err()
                .contains("в фоне")
        );
        assert_eq!(o.state.lock().unwrap().study_pending_count(study), 1);
        assert!(
            o.mutate(&path, "POST", json!({"expected_version":2}))
                .unwrap_err()
                .contains("дождитесь")
        );
        assert_eq!(o.state.lock().unwrap().study_pending_count(study), 1);
    }
}

#[cfg(test)]
#[path = "offline_priority_tests.rs"]
mod priority_tests;
