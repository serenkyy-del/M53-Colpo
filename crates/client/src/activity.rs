//! Durable, actor-separated UI journal. Values and clinical content are not accepted by the API.
use crate::{cache::Store, connection::Connection, offline::Offline};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, Weak, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Action {
    pub action: String,
    pub study_id: Option<Uuid>,
    pub entity_id: Option<Uuid>,
    pub operation_id: Option<Uuid>,
    pub field_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phrase_template_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phrase_template_version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply_mode: Option<String>,
}
impl Action {
    pub fn new(code: &str) -> Self {
        Self {
            action: format!("client.{code}"),
            ..Default::default()
        }
    }
    pub fn study(mut self, id: Uuid) -> Self {
        self.study_id = Some(id);
        self
    }
    pub fn entity(mut self, id: Uuid) -> Self {
        self.entity_id = Some(id);
        self
    }
    pub fn field(mut self, code: impl Into<String>) -> Self {
        self.field_code = Some(code.into());
        self
    }
    pub fn valid(&self) -> bool {
        let metadata = self.phrase_template_id.is_some()
            || self.phrase_template_version.is_some()
            || self.apply_mode.is_some();
        if self.action == "client.phrase.applied" {
            if self.study_id.is_none()
                || self.field_code.is_none()
                || self.phrase_template_id.is_none_or(|id| id.is_nil())
                || self.phrase_template_version.is_none_or(|v| v == 0)
                || !matches!(self.apply_mode.as_deref(), Some("append" | "replace"))
            {
                return false;
            }
        } else if metadata {
            return false;
        }
        let catalog: Value =
            serde_json::from_str(include_str!("../../../config/activity-actions.json")).unwrap();
        catalog["actions"].as_array().is_some_and(|a| {
            a.iter()
                .any(|v| v["code"] == self.action && v["source"] == "client")
        }) && self.field_code.as_ref().is_none_or(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-[]".contains(&c))
        })
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Event {
    pub event_id: Uuid,
    #[serde(flatten)]
    pub action: Action,
    pub reported_at: u64,
    pub client_sequence: u64,
    pub reported_credential_id: Option<Uuid>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    event: Event,
    attempted: bool,
    blocked: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct Queue {
    user: Option<Uuid>,
    key_tag: String,
    station: Option<Uuid>,
    sequence: u64,
    events: Vec<Entry>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct Index {
    queues: BTreeSet<String>,
    identities: BTreeMap<String, Value>,
}
struct Repo {
    store: Store,
    index: Index,
    queues: BTreeMap<String, Queue>,
}
#[derive(Clone, Default)]
struct Actor {
    base: String,
    tag: String,
    user: Option<Uuid>,
}
#[derive(Clone)]
struct Context {
    raw: Connection,
    offline: Weak<Offline>,
    actor: Actor,
    epoch: u64,
}
#[derive(Clone, Serialize, Default)]
pub struct View {
    pub workstation: Value,
    pub pending: usize,
    pub error: String,
    pub safe_error_code: String,
    #[serde(skip)]
    disk_failed: bool,
}
struct Shared {
    context: Mutex<Option<Context>>,
    view: Mutex<View>,
}
#[derive(Clone)]
pub struct Journal {
    tx: mpsc::Sender<Message>,
    shared: Arc<Shared>,
}
enum Message {
    Context(Context),
    Record(Actor, Action, u64, Uuid),
    Metadata(Actor, Value, Value),
    Prepare(Context, mpsc::Sender<Option<Batch>>),
    Ack(String, String, Vec<Event>, Vec<Uuid>),
    Block(String, String),
    Flush(mpsc::Sender<()>),
}
struct Batch {
    base: String,
    name: String,
    events: Vec<Event>,
}
fn digest(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn station(v: &Value) -> Option<Uuid> {
    v["workstation_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
}
fn credential(v: &Value) -> Option<Uuid> {
    v["credential_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
}
fn queue_name(actor: &Actor) -> String {
    format!(
        "q-{}-{}",
        actor.tag,
        actor
            .user
            .map(|v| v.to_string())
            .unwrap_or("workstation".into())
    )
}
fn repo(base: &str) -> Result<Repo, String> {
    #[cfg(windows)]
    let cipher: Arc<dyn crate::cache::Protector> = Arc::new(crate::cache::Dpapi);
    #[cfg(not(windows))]
    return Err("activity_windows_dpapi_required".into());
    #[cfg(windows)]
    {
        let path =
            std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA")?)
                .join(crate::APP_DATA_DIR).join("activity")
                .join(digest(&crate::transport::cache_origin(base)));
        let store = Store::new(path, cipher)?;
        load_repo(store)
    }
}
fn load_repo(store: Store) -> Result<Repo, String> {
    let mut index = match store.read("index")? {
        Some(bytes) => serde_json::from_slice(&bytes).map_err(|_| "activity_index_damaged")?,
        None => Index::default(),
    };
    for entry in std::fs::read_dir(store.root()).map_err(|_| "activity_index_unavailable")? {
        let entry = entry.map_err(|_| "activity_index_unavailable")?;
        if let Some(name) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.strip_suffix(".dpapi"))
        {
            if name.starts_with("q-") {
                index.queues.insert(name.into());
            }
        }
    }
    let mut queues = BTreeMap::new();
    for name in &index.queues {
        let bytes = store.read(name)?.ok_or("activity_queue_missing")?;
        let queue = serde_json::from_slice(&bytes).map_err(|_| "activity_queue_damaged")?;
        queues.insert(name.clone(), queue);
    }
    Ok(Repo {
        store,
        index,
        queues,
    })
}

fn write_queue(repo: &mut Repo, name: &str, queue: Queue) -> Result<(), String> {
    repo.store
        .write(name, &serde_json::to_vec(&queue).unwrap())?;
    if repo.index.queues.insert(name.into()) {
        repo.store
            .write("index", &serde_json::to_vec(&repo.index).unwrap())?;
    }
    repo.queues.insert(name.into(), queue);
    Ok(())
}
pub fn acknowledged(sent: &[Event], ids: &[Uuid]) -> bool {
    let valid: BTreeSet<_> = sent.iter().map(|e| e.event_id).collect();
    let unique: BTreeSet<_> = ids.iter().copied().collect();
    !ids.is_empty() && unique.len() == ids.len() && unique.is_subset(&valid)
}
pub fn field(path: &str) -> Option<(String, Option<Uuid>)> {
    let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
    let (code, entity) = match parts.as_slice() {
        ["colpo", "fields", code, ..] => (code.to_string(), None),
        ["colpo", "marks", id, "properties", code, ..] => {
            (code.to_string(), Uuid::parse_str(id).ok())
        }
        _ => (parts.join("."), None),
    };
    Action::new("protocol.field_changed")
        .field(code.clone())
        .valid()
        .then_some((code, entity))
}
pub fn annotations(
    study: Uuid,
    before: &[m53_contracts::workflow::Annotation],
    after: &[m53_contracts::workflow::Annotation],
) -> Vec<Action> {
    use m53_contracts::workflow::AnnotationLayer;
    let mut out = vec![];
    for a in after {
        let previous = before.iter().find(|b| b.id == a.id);
        let code = if let Some(b) = previous {
            if serde_json::to_value(a).unwrap() == serde_json::to_value(b).unwrap() {
                continue;
            }
            match a.layer {
                AnnotationLayer::Target { .. } => "target.changed",
                AnnotationLayer::OutputRegion { .. } => "report_region.changed",
                _ => {
                    let old = serde_json::to_value(&b.layer).unwrap();
                    let new = serde_json::to_value(&a.layer).unwrap();
                    if a.label != b.label
                        || old["term_code"] != new["term_code"]
                        || old["full_name"] != new["full_name"]
                    {
                        "annotation.retagged"
                    } else if a.radius != b.radius {
                        "annotation.resized"
                    } else {
                        "annotation.moved"
                    }
                }
            }
        } else {
            match a.layer {
                AnnotationLayer::Target { .. } => "target.changed",
                AnnotationLayer::OutputRegion { .. } => "report_region.changed",
                _ => "annotation.added",
            }
        };
        out.push(Action::new(code).study(study).entity(a.id));
    }
    for b in before
        .iter()
        .filter(|b| !after.iter().any(|a| a.id == b.id))
    {
        out.push(Action::new("annotation.removed").study(study).entity(b.id));
    }
    out
}
impl Journal {
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            context: Mutex::new(None),
            view: Mutex::new(View::default()),
        });
        let disk_shared = shared.clone();
        std::thread::spawn(move || {
            let mut repos: BTreeMap<String, Repo> = BTreeMap::new();
            while let Ok(message) = rx.recv() {
                let result = (|| -> Result<(), String> {
                    match message {
                        Message::Context(ctx) => {
                            if !repos.contains_key(&ctx.actor.base) {
                                repos.insert(ctx.actor.base.clone(), repo(&ctx.actor.base)?);
                            }
                            let r = &repos[&ctx.actor.base];
                            if let Some(identity) = r.index.identities.get(&ctx.actor.tag) {
                                disk_shared.view.lock().unwrap().workstation = identity.clone();
                            }
                        }
                        Message::Record(actor, action, time, event_id) => {
                            let r = repos
                                .get_mut(&actor.base)
                                .ok_or("activity_store_unavailable")?;
                            let anonymous_blocked = actor.user.is_none()
                                && !matches!(
                                    action.action.as_str(),
                                    "client.app.started" | "client.app.closed"
                                )
                                || (actor.user.is_none()
                                    && (action.study_id.is_some()
                                        || action.entity_id.is_some()
                                        || action.field_code.is_some()));
                            let name = queue_name(&actor);
                            let identity = r
                                .index
                                .identities
                                .get(&actor.tag)
                                .cloned()
                                .unwrap_or(Value::Null);
                            let mut q = r.queues.get(&name).cloned().unwrap_or(Queue {
                                user: actor.user,
                                key_tag: actor.tag.clone(),
                                station: station(&identity),
                                sequence: 0,
                                events: vec![],
                            });
                            if q.events.iter().any(|e| e.event.event_id == event_id) {
                                return Ok(());
                            }
                            q.sequence = q
                                .sequence
                                .checked_add(1)
                                .ok_or("activity_counter_exhausted")?;
                            q.events.push(Entry {
                                event: Event {
                                    event_id,
                                    action,
                                    reported_at: time,
                                    client_sequence: q.sequence,
                                    reported_credential_id: credential(&identity),
                                },
                                attempted: false,
                                blocked: anonymous_blocked,
                            });
                            write_queue(r, &name, q)?;
                        }
                        Message::Metadata(actor, identity, catalog) => {
                            let r = repos
                                .get_mut(&actor.base)
                                .ok_or("activity_store_unavailable")?;
                            if station(&identity).is_none() || credential(&identity).is_none() {
                                return Err("activity_identity_invalid".into());
                            }
                            r.index
                                .identities
                                .insert(actor.tag.clone(), identity.clone());
                            r.store
                                .write("index", &serde_json::to_vec(&r.index).unwrap())?;
                            r.store
                                .write("actions", &serde_json::to_vec(&catalog).unwrap())?;
                            if disk_shared
                                .context
                                .lock()
                                .unwrap()
                                .as_ref()
                                .is_some_and(|c| {
                                    c.actor.base == actor.base && c.actor.tag == actor.tag
                                })
                            {
                                disk_shared.view.lock().unwrap().workstation = identity;
                            }
                        }
                        Message::Prepare(ctx, reply) => {
                            let result = (|| -> Result<Option<Batch>, String> {
                                let r = repos
                                    .get_mut(&ctx.actor.base)
                                    .ok_or("activity_store_unavailable")?;
                                let identity = r
                                    .index
                                    .identities
                                    .get(&ctx.actor.tag)
                                    .cloned()
                                    .unwrap_or(Value::Null);
                                let Some(current_station) = station(&identity) else {
                                    return Ok(None);
                                };
                                let aliases = ctx
                                    .offline
                                    .upgrade()
                                    .map(|o| o.identifier_aliases())
                                    .unwrap_or_default();
                                for name in r.index.queues.clone() {
                                    let mut q = r
                                        .queues
                                        .get(&name)
                                        .cloned()
                                        .ok_or("activity_queue_missing")?;
                                    if q.user != ctx.actor.user {
                                        continue;
                                    }
                                    let original = r
                                        .index
                                        .identities
                                        .get(&q.key_tag)
                                        .cloned()
                                        .unwrap_or(Value::Null);
                                    if !same_actor(
                                        q.user,
                                        q.station.or(station(&original)),
                                        ctx.actor.user,
                                        current_station,
                                    ) {
                                        continue;
                                    }
                                    let mut events = vec![];
                                    for entry in
                                        q.events.iter_mut().filter(|e| !e.blocked).take(100)
                                    {
                                        freeze_event(entry, &aliases, credential(&original));
                                        events.push(entry.event.clone());
                                    }
                                    if !events.is_empty() {
                                        q.station = Some(current_station);
                                        write_queue(r, &name, q)?;
                                        return Ok(Some(Batch {
                                            base: ctx.actor.base.clone(),
                                            name,
                                            events,
                                        }));
                                    }
                                }
                                Ok(None)
                            })();
                            let _ = reply.send(result?);
                        }
                        Message::Ack(base, name, sent, ids) => {
                            if !acknowledged(&sent, &ids) {
                                return Err("activity_ack_invalid".into());
                            }
                            let r = repos.get_mut(&base).ok_or("activity_store_unavailable")?;
                            let mut q = r
                                .queues
                                .get(&name)
                                .cloned()
                                .ok_or("activity_queue_missing")?;
                            q.events.retain(|e| !ids.contains(&e.event.event_id));
                            write_queue(r, &name, q)?;
                        }
                        Message::Block(base, name) => {
                            let r = repos.get_mut(&base).ok_or("activity_store_unavailable")?;
                            let mut q = r
                                .queues
                                .get(&name)
                                .cloned()
                                .ok_or("activity_queue_missing")?;
                            for e in q.events.iter_mut().filter(|e| e.attempted) {
                                e.blocked = true;
                            }
                            write_queue(r, &name, q)?;
                            return Err("activity_batch_requires_review".into());
                        }
                        Message::Flush(reply) => {
                            let _ = reply.send(());
                        }
                    }
                    Ok(())
                })();
                if result.is_err() {
                    disk_shared.view.lock().unwrap().disk_failed = true;
                    disk_shared.view.lock().unwrap().error =
                        "Журнал действий: ошибка записи или передачи; обследование доступно".into();
                }
                let ctx = disk_shared.context.lock().unwrap().clone();
                if let Some(c) = ctx {
                    disk_shared.view.lock().unwrap().pending = repos
                        .get(&c.actor.base)
                        .map(|r| {
                            r.queues
                                .values()
                                .filter(|q| q.user == c.actor.user)
                                .map(|q| q.events.len())
                                .sum()
                        })
                        .unwrap_or(0);
                }
            }
        });
        let net = shared.clone();
        let sender = tx.clone();
        std::thread::spawn(move || {
            let mut metadata = Instant::now() - Duration::from_secs(61);
            let mut seen = 0;
            let mut halted = None;
            let mut failures = 0u32;
            loop {
                std::thread::sleep(Duration::from_secs(2));
                let Some(ctx) = net.context.lock().unwrap().clone() else {
                    continue;
                };
                if ctx.epoch != seen {
                    metadata = Instant::now() - Duration::from_secs(61);
                    seen = ctx.epoch;
                }
                if halted == Some(ctx.epoch) {
                    continue;
                }
                let result = (|| -> Result<(), String> {
                    if metadata.elapsed() >= Duration::from_secs(60) {
                        let (status, bytes) =
                            ctx.raw
                                .exchange("GET", "/api/v1/workstation", None, None, None)?;
                        if matches!(status, 401 | 403) {
                            halted = Some(ctx.epoch);
                            return Err("activity_auth_required".into());
                        }
                        if status != 200 {
                            return Err("activity_workstation_unavailable".into());
                        }
                        let identity: Value = serde_json::from_slice(&bytes)
                            .map_err(|_| "activity_invalid_identity")?;
                        let (status, bytes) = ctx.raw.exchange(
                            "GET",
                            "/api/v1/activity-actions",
                            None,
                            None,
                            None,
                        )?;
                        if status != 200 {
                            return Err("activity_catalog_unavailable".into());
                        }
                        let catalog: Value = serde_json::from_slice(&bytes)
                            .map_err(|_| "activity_invalid_catalog")?;
                        sender
                            .send(Message::Metadata(ctx.actor.clone(), identity, catalog))
                            .map_err(|_| "activity_writer_stopped")?;
                        metadata = Instant::now();
                    }
                    if ctx.actor.user.is_some() {
                        let mut anonymous = ctx.clone();
                        anonymous.actor.user = None;
                        anonymous.raw.session.clear();
                        anonymous.raw.user_id = None;
                        let (tx, rx) = mpsc::channel();
                        sender
                            .send(Message::Prepare(anonymous.clone(), tx))
                            .map_err(|_| "activity_writer_stopped")?;
                        if let Some(batch) = rx
                            .recv_timeout(Duration::from_secs(2))
                            .map_err(|_| "activity_prepare_failed")?
                        {
                            let (status, bytes) = anonymous.raw.exchange(
                                "POST",
                                "/api/v1/workstation/activity-events",
                                Some(&json!({"events":batch.events})),
                                None,
                                None,
                            )?;
                            if matches!(status, 401 | 403) {
                                halted = Some(ctx.epoch);
                                return Err("activity_auth_required".into());
                            }
                            if matches!(status, 400 | 409 | 422) {
                                let _ = sender.send(Message::Block(batch.base, batch.name));
                                return Err("activity_anonymous_rejected".into());
                            }
                            if status != 200 {
                                return Err("activity_retry_later".into());
                            }
                            #[derive(Deserialize)]
                            struct AnonAck {
                                accepted_event_ids: Vec<Uuid>,
                            }
                            let ack: AnonAck = serde_json::from_slice(&bytes)
                                .map_err(|_| "activity_invalid_ack")?;
                            if !acknowledged(&batch.events, &ack.accepted_event_ids) {
                                return Err("activity_invalid_ack".into());
                            }
                            sender
                                .send(Message::Ack(
                                    batch.base,
                                    batch.name,
                                    batch.events,
                                    ack.accepted_event_ids,
                                ))
                                .map_err(|_| "activity_writer_stopped")?;
                        }
                    }
                    if ctx.actor.user.is_some() {
                        if ctx.raw.session.is_empty() {
                            return Ok(());
                        }
                        if ctx.offline.upgrade().is_some_and(|o| {
                            let (_, pending, durable, _) = o.information();
                            pending > 0 || !durable
                        }) {
                            return Ok(());
                        }
                    }
                    let (tx, rx) = mpsc::channel();
                    sender
                        .send(Message::Prepare(ctx.clone(), tx))
                        .map_err(|_| "activity_writer_stopped")?;
                    let Some(batch) = rx
                        .recv_timeout(Duration::from_secs(2))
                        .map_err(|_| "activity_prepare_failed")?
                    else {
                        return Ok(());
                    };
                    if ctx.actor.user.is_some() {
                        let (status, bytes) =
                            ctx.raw.exchange("GET", "/api/v1/me", None, None, None)?;
                        if matches!(status, 401 | 403) {
                            halted = Some(ctx.epoch);
                            return Err("activity_auth_required".into());
                        }
                        let me: Value =
                            serde_json::from_slice(&bytes).map_err(|_| "activity_invalid_me")?;
                        if status != 200 || me["user_id"] != json!(ctx.actor.user) {
                            halted = Some(ctx.epoch);
                            return Err("activity_actor_mismatch".into());
                        }
                    }
                    if net
                        .context
                        .lock()
                        .unwrap()
                        .as_ref()
                        .is_none_or(|c| c.epoch != ctx.epoch)
                    {
                        return Ok(());
                    }
                    let path = if ctx.actor.user.is_some() {
                        "/api/v1/activity-events"
                    } else {
                        "/api/v1/workstation/activity-events"
                    };
                    let body = json!({"events":batch.events});
                    if serde_json::to_vec(&body).unwrap().len() > 128 * 1024 {
                        return Err("activity_batch_too_large".into());
                    }
                    let (status, bytes) =
                        ctx.raw.exchange("POST", path, Some(&body), None, None)?;
                    if matches!(status, 401 | 403) {
                        halted = Some(ctx.epoch);
                        return Err("activity_auth_required".into());
                    }
                    if matches!(status, 400 | 409 | 422) {
                        let _ = sender.send(Message::Block(batch.base, batch.name));
                        return Err("activity_batch_rejected".into());
                    }
                    if status != 200 {
                        return Err("activity_retry_later".into());
                    }
                    #[derive(Deserialize)]
                    struct Ack {
                        accepted_event_ids: Vec<Uuid>,
                    }
                    let ack: Ack =
                        serde_json::from_slice(&bytes).map_err(|_| "activity_invalid_ack")?;
                    if !acknowledged(&batch.events, &ack.accepted_event_ids) {
                        return Err("activity_invalid_ack".into());
                    }
                    sender
                        .send(Message::Ack(
                            batch.base,
                            batch.name,
                            batch.events,
                            ack.accepted_event_ids,
                        ))
                        .map_err(|_| "activity_writer_stopped")?;
                    Ok(())
                })();
                if let Err(ref code) = result {
                    net.view.lock().unwrap().safe_error_code = if code.starts_with("activity_") {
                        code.clone()
                    } else {
                        "activity_transport_unavailable".into()
                    };
                    if !net.view.lock().unwrap().disk_failed {
                        net.view.lock().unwrap().error =
                            "Журнал действий ожидает передачи; локальная работа сохранена".into();
                    }
                    failures = (failures + 1).min(3);
                    std::thread::sleep(Duration::from_secs(1 << failures));
                } else {
                    failures = 0;
                    net.view.lock().unwrap().safe_error_code.clear();
                    if !net.view.lock().unwrap().disk_failed {
                        net.view.lock().unwrap().error.clear();
                    }
                }
            }
        });
        Self { tx, shared }
    }
    pub fn connect(&self, raw: &Connection) {
        let mut transport = raw.clone();
        let offline = transport
            .offline
            .as_ref()
            .map(Arc::downgrade)
            .unwrap_or_default();
        transport.offline = None;
        let mut context = self.shared.context.lock().unwrap();
        if context.as_ref().is_some_and(|c| {
            c.raw.base == raw.base
                && c.raw.key == raw.key
                && c.raw.session == raw.session
                && c.actor.user == raw.user_id
        }) {
            return;
        }
        let epoch = context.as_ref().map(|c| c.epoch + 1).unwrap_or(1);
        let value = Context {
            actor: Actor {
                base: raw.storage_base.clone(),
                tag: digest(&raw.key),
                user: raw.user_id,
            },
            raw: transport,
            offline,
            epoch,
        };
        let _ = self.tx.send(Message::Context(value.clone()));
        *context = Some(value);
    }
    pub fn sign_out(&self) {
        let context = self.shared.context.lock().unwrap().clone();
        if let Some(context) = context {
            let mut raw = context.raw;
            raw.user_id = None;
            raw.session.clear();
            raw.doctor.clear();
            self.connect(&raw);
        }
    }
    pub fn record(&self, action: Action) {
        self.record_id(action, Uuid::new_v4());
    }
    pub fn record_id(&self, action: Action, id: Uuid) {
        if !action.valid() {
            self.shared.view.lock().unwrap().error = "Некорректный код события аудита".into();
            return;
        }
        if let Some(ctx) = self.shared.context.lock().unwrap().as_ref() {
            let _ = self
                .tx
                .send(Message::Record(ctx.actor.clone(), action, now(), id));
        }
    }
    pub fn flush(&self) {
        let (tx, rx) = mpsc::channel();
        let _ = self.tx.send(Message::Flush(tx));
        let _ = rx.recv_timeout(Duration::from_secs(3));
    }
    pub fn snapshot(&self) -> View {
        self.shared.view.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event() -> Event {
        Event {
            event_id: Uuid::new_v4(),
            action: Action::new("protocol.field_changed").field("context.exam_date"),
            reported_at: 1,
            client_sequence: 1,
            reported_credential_id: None,
        }
    }
    #[test]
    fn completed_annotation_changes_have_one_semantic_event_without_values() {
        use m53_contracts::workflow::*;
        let study = Uuid::new_v4();
        let mark = Annotation {
            layer: AnnotationLayer::Mark,
            coordinate_system: AnnotationSpace::default(),
            id: Uuid::new_v4(),
            asset_id: Uuid::new_v4(),
            lesion_id: None,
            finding_id: None,
            x: 0.2,
            y: 0.3,
            radius: 0.1,
            label: "private label".into(),
        };
        assert!(annotations(study, &[mark.clone()], &[mark.clone()]).is_empty());
        let mut moved = mark.clone();
        moved.x = 0.7;
        let actions = annotations(study, &[mark.clone()], &[moved.clone()]);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].action, "client.annotation.moved");
        assert_eq!(actions[0].entity_id, Some(mark.id));
        assert!(
            !serde_json::to_string(&actions)
                .unwrap()
                .contains("private label")
        );
        moved.radius = 0.2;
        assert_eq!(
            annotations(study, &[mark.clone()], &[moved])[0].action,
            "client.annotation.resized"
        );
        assert_eq!(
            annotations(study, &[], &[mark.clone()])[0].action,
            "client.annotation.added"
        );
        assert_eq!(
            annotations(study, &[mark], &[])[0].action,
            "client.annotation.removed"
        );
        assert_eq!(
            field("/colpo/fields/sections[0].adequacy").unwrap().0,
            "sections[0].adequacy"
        );
    }
    #[test]
    fn actor_and_workstation_are_never_substituted() {
        let user = Uuid::new_v4();
        let station = Uuid::new_v4();
        assert!(same_actor(Some(user), Some(station), Some(user), station));
        assert!(!same_actor(
            Some(user),
            Some(station),
            Some(Uuid::new_v4()),
            station
        ));
        assert!(!same_actor(
            Some(user),
            Some(Uuid::new_v4()),
            Some(user),
            station
        ));
        assert!(!same_actor(None, Some(station), Some(user), station));
    }
    #[test]
    fn retry_keeps_uuid_bytes_and_original_credential_after_alias_change() {
        let local = Uuid::new_v4();
        let remote = Uuid::new_v4();
        let original = Uuid::new_v4();
        let mut entry = Entry {
            event: event(),
            attempted: false,
            blocked: false,
        };
        entry.event.action.study_id = Some(local);
        freeze_event(
            &mut entry,
            &BTreeMap::from([(local, remote)]),
            Some(original),
        );
        let frozen = serde_json::to_vec(&entry.event).unwrap();
        freeze_event(
            &mut entry,
            &BTreeMap::from([(remote, Uuid::new_v4())]),
            Some(Uuid::new_v4()),
        );
        assert_eq!(frozen, serde_json::to_vec(&entry.event).unwrap());
        assert_eq!(entry.event.action.study_id, Some(remote));
    }
    #[test]
    fn orphaned_encrypted_queue_survives_missing_index() {
        let path = std::env::temp_dir().join(format!("m53-activity-test-{}", Uuid::new_v4()));
        let cipher =
            || Arc::new(crate::cache::AesProtector([71; 32])) as Arc<dyn crate::cache::Protector>;
        let mut repo = load_repo(Store::new(path.clone(), cipher()).unwrap()).unwrap();
        let owner = Uuid::new_v4();
        let event = event();
        let id = event.event_id;
        let queue = Queue {
            user: Some(owner),
            key_tag: "abc".into(),
            station: Some(Uuid::new_v4()),
            sequence: 1,
            events: vec![Entry {
                event,
                attempted: false,
                blocked: true,
            }],
        };
        write_queue(&mut repo, "q-abc-user", queue).unwrap();
        drop(repo);
        assert!(
            !std::fs::read(path.join("q-abc-user.dpapi"))
                .unwrap()
                .windows(6)
                .any(|w| w == b"client")
        );
        std::fs::remove_file(path.join("index.dpapi")).unwrap();
        let recovered = load_repo(Store::new(path.clone(), cipher()).unwrap()).unwrap();
        assert_eq!(recovered.queues["q-abc-user"].user, Some(owner));
        assert_eq!(recovered.queues["q-abc-user"].events[0].event.event_id, id);
        assert!(recovered.queues["q-abc-user"].events[0].blocked);
        drop(recovered);
        assert_eq!(path.parent(), Some(std::env::temp_dir().as_path()));
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("m53-activity-test-")
        );
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn acknowledgement_only_removes_matching_committed_ids() {
        let a = event();
        let b = event();
        assert!(acknowledged(&[a.clone(), b], &[a.event_id]));
        assert!(!acknowledged(&[a.clone()], &[Uuid::new_v4()]));
        assert!(!acknowledged(&[a.clone()], &[a.event_id, a.event_id]));
        assert!(!acknowledged(&[a], &[]));
    }
    #[test]
    fn phrase_metadata_is_explicit_and_absent_from_legacy_wire() {
        let old = serde_json::to_value(event()).unwrap();
        for key in [
            "phrase_template_id",
            "phrase_template_version",
            "apply_mode",
        ] {
            assert!(old.get(key).is_none());
        }
        let mut action = Action::new("phrase.applied")
            .study(Uuid::new_v4())
            .field("conclusion");
        assert!(!action.valid());
        action.phrase_template_id = Some(Uuid::new_v4());
        action.phrase_template_version = Some(2);
        action.apply_mode = Some("append".into());
        assert!(action.valid());
        action.phrase_template_version = Some(0);
        assert!(!action.valid());
        action.phrase_template_version = Some(2);
        action.action = "client.protocol.field_changed".into();
        assert!(!action.valid());
    }
    #[test]
    fn wire_cannot_contain_user_credentials_or_values() {
        let wire = serde_json::to_value(event()).unwrap();
        assert!(wire.get("user_id").is_none());
        assert!(wire.get("doctor_id").is_none());
        assert!(wire.get("value").is_none());
        assert!(
            Action::new("protocol.field_changed")
                .field("sections[0].adequacy")
                .valid()
        );
        assert!(
            !Action::new("protocol.field_changed")
                .field("заключение пациента")
                .valid()
        );
        assert!(!Action::new("server.approved").valid());
    }
}

fn same_actor(
    user: Option<Uuid>,
    station: Option<Uuid>,
    current_user: Option<Uuid>,
    current_station: Uuid,
) -> bool {
    user == current_user && station == Some(current_station)
}
fn freeze_event(entry: &mut Entry, aliases: &BTreeMap<Uuid, Uuid>, credential: Option<Uuid>) {
    if entry.attempted {
        return;
    }
    let resolve = |id: Uuid| {
        let mut value = id;
        for _ in 0..32 {
            if let Some(next) = aliases.get(&value) {
                value = *next;
            } else {
                break;
            }
        }
        value
    };
    entry.event.action.study_id = entry.event.action.study_id.map(resolve);
    entry.event.action.entity_id = entry.event.action.entity_id.map(resolve);
    entry.event.reported_credential_id = credential;
    entry.attempted = true;
}
