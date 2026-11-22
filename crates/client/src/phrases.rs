//! Physician-owned phrase library. Its encrypted outbox is independent of clinical work.
//!
//! Public mutations perform only a durable local write. HTTP runs on a separate thread.
//! This module never accepts a patient, study, physician name, or arbitrary actor identifier
//! in the API payload. The `/me` identity must match the encrypted cache owner before upload.
use crate::{cache::Store, connection::Connection, offline::Operation};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use uuid::Uuid;

const PREFIX: &str = "/api/v1/phrase-templates";
const PREFS: &str = "/api/v1/me/phrase-template-preferences";
const MAX_BYTES: usize = 16_384;
pub const FIELD_CODES: &[(&str, &str)] = &[
    ("conclusion", "Заключение врача"),
    ("diagnosis_note", "Дополнение к диагнозам"),
    ("extended.clock", "Соответствие с циферблатом"),
    ("extended.other", "Прочие данные расширенной кольпоскопии"),
    ("rectovaginal.description", "Ректо-вагинальный осмотр"),
    ("vulva.other_comment", "Вульва: различные находки"),
    ("vulva.cancer_comment", "Вульва: подозрение на рак"),
    (
        "cervix.adequacy_reason",
        "Причина неадекватности оценки шейки",
    ),
    (
        "vulva.adequacy_reason",
        "Причина неадекватности оценки вульвы",
    ),
    (
        "anus.adequacy_reason",
        "Причина неадекватности оценки анальной области",
    ),
    ("vulva.other.trauma.comment", "Дополнительно: травма"),
    (
        "vulva.other.malformation.comment",
        "Дополнительно: порок развития",
    ),
    ("vulva.other.hematoma.comment", "Дополнительно: гематома"),
    (
        "vulva.other.hemangioma.comment",
        "Дополнительно: гемангиома",
    ),
    (
        "vulva.cancer.neoplasia.comment",
        "Дополнительно: явная неоплазия",
    ),
    (
        "vulva.cancer.ulceration.comment",
        "Дополнительно: изъязвление",
    ),
    ("vulva.cancer.necrosis.comment", "Дополнительно: некроз"),
    (
        "vulva.cancer.bleeding.comment",
        "Дополнительно: кровоточивость",
    ),
    (
        "vulva.cancer.hyperkeratosis.comment",
        "Дополнительно: гиперкератоз",
    ),
    ("vulva.cancer.exophytic.comment", "Дополнительно: экзофит"),
];

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Field {
    pub field_code: String,
    pub name: String,
    pub target_kind: String,
    pub max_bytes: usize,
    pub enabled: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Template {
    pub id: Uuid,
    pub field_code: String,
    pub scope: String,
    pub title: String,
    pub body: String,
    pub row_version: u64,
    pub can_edit: bool,
    #[serde(default)]
    pub sync_state: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preferences {
    pub apply_mode: String,
    pub include_shared: bool,
    pub row_version: u64,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            apply_mode: "append".into(),
            include_shared: true,
            row_version: 0,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Conflict {
    pub operation_id: Uuid,
    pub entity_id: Uuid,
    pub field_code: String,
    pub code: String,
    pub message: String,
    pub can_keep_local: bool,
    pub local: Value,
    pub remote: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Snapshot {
    pub field_code: String,
    pub fields: Vec<Field>,
    pub templates: Vec<Template>,
    pub preferences: Preferences,
    pub pending: usize,
    pub preferences_pending: bool,
    pub conflicts: Vec<Conflict>,
    pub error: String,
    pub online: bool,
    pub loaded: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct Pending {
    id: Uuid,
    entity_id: Uuid,
    field_code: String,
    method: String,
    path: String,
    body: Value,
    after: Option<Uuid>,
    started: bool,
    blocked: Option<String>,
    remote: Value,
    #[serde(default)]
    current_version: Option<u64>,
}
impl Pending {
    fn transport(&self) -> Operation {
        Operation {
            id: self.id,
            entity_id: self.entity_id,
            version_id: None,
            path: self.path.clone(),
            method: self.method.clone(),
            body: self.body.clone(),
            blob: None,
            started: self.started,
            blocked: self.blocked.clone(),
            sequence: 0,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct State {
    format_version: u32,
    base: String,
    user_id: Uuid,
    fields: Vec<Field>,
    templates: BTreeMap<String, Vec<Template>>,
    preferences: Preferences,
    outbox: VecDeque<Pending>,
    receipts: BTreeMap<Uuid, u64>,
    /// Retained after explicit conflict resolution; never silently discard local work.
    resolved: Vec<Pending>,
}
struct Repo {
    store: Store,
    state: State,
    field: String,
    error: String,
    online: bool,
    recent_operations: BTreeMap<Uuid, Uuid>,
}
struct Shared {
    repo: Mutex<Repo>,
    connection: Mutex<Option<Connection>>,
    wake: AtomicBool,
    generation: std::sync::atomic::AtomicU64,
}
pub struct Library {
    shared: Arc<Shared>,
}

fn digest(base: &str) -> String {
    hex::encode(Sha256::digest(base.as_bytes()))
}
fn known_field(code: &str) -> bool {
    FIELD_CODES.iter().any(|(v, _)| *v == code)
}
fn default_fields() -> Vec<Field> {
    FIELD_CODES
        .iter()
        .map(|(code, name)| Field {
            field_code: (*code).into(),
            name: (*name).into(),
            target_kind: if code.contains(".other.") || code.contains(".cancer.") {
                "mark"
            } else {
                "protocol"
            }
            .into(),
            max_bytes: MAX_BYTES,
            enabled: true,
        })
        .collect()
}
fn raw(connection: &Connection) -> Connection {
    let mut raw = connection.clone();
    raw.offline = None;
    raw
}
fn load(store: Store, base: &str, user_id: Uuid) -> Result<Repo, String> {
    let state = match store.read("library")? {
        Some(bytes) => {
            let state: State = serde_json::from_slice(&bytes)
                .map_err(|_| "Кеш шаблонов повреждён; существующие файлы сохранены")?;
            if state.format_version != 1 || !crate::transport::same_server(&state.base, base) || state.user_id != user_id {
                return Err("Кеш шаблонов принадлежит другому врачу или серверу".into());
            }
            state
        }
        None => State {
            format_version: 1,
            base: base.into(),
            user_id,
            fields: default_fields(),
            templates: BTreeMap::new(),
            preferences: Preferences::default(),
            outbox: VecDeque::new(),
            receipts: BTreeMap::new(),
            resolved: Vec::new(),
        },
    };
    Ok(Repo {
        store,
        state,
        field: String::new(),
        error: String::new(),
        online: false,
        recent_operations: BTreeMap::new(),
    })
}
impl Repo {
    /// Persist a complete new state before making it visible. On disk failure keep old state.
    fn commit(&mut self, state: State) -> Result<(), String> {
        self.store.write(
            "library",
            &serde_json::to_vec(&state).map_err(|_| "Не удалось сохранить шаблоны")?,
        )?;
        self.state = state;
        Ok(())
    }
    fn visible_preferences(&self) -> Preferences {
        let mut prefs = self.state.preferences.clone();
        for op in &self.state.outbox {
            if op.path == PREFS {
                prefs.apply_mode = op.body["apply_mode"].as_str().unwrap_or("append").into();
                prefs.include_shared = op.body["include_shared"].as_bool().unwrap_or(true);
            }
        }
        prefs
    }
    fn visible_templates(&self, field: &str) -> Vec<Template> {
        let mut list: BTreeMap<Uuid, Template> = self
            .state
            .templates
            .get(field)
            .into_iter()
            .flatten()
            .cloned()
            .map(|mut t| {
                t.sync_state = "synced".into();
                (t.id, t)
            })
            .collect();
        for op in &self.state.outbox {
            if op.field_code != field || op.path == PREFS {
                continue;
            }
            let t = list.entry(op.entity_id).or_insert_with(|| Template {
                id: op.entity_id,
                field_code: field.into(),
                scope: op.body["scope"].as_str().unwrap_or("personal").into(),
                can_edit: true,
                ..Template::default()
            });
            t.title = op.body["title"].as_str().unwrap_or("").into();
            t.body = op.body["body"].as_str().unwrap_or("").into();
            t.sync_state = if op.blocked.is_some() {
                "conflict"
            } else {
                "pending"
            }
            .into();
        }
        // A newer draft can depend on an older operation which has since conflicted.
        // Keep the conflict badge visible on the whole entity branch.
        for blocked in self.state.outbox.iter().filter(|p| p.blocked.is_some()) {
            if let Some(template) = list.get_mut(&blocked.entity_id) {
                template.sync_state = "conflict".into();
            }
        }
        let mut list: Vec<_> = list.into_values().collect();
        list.sort_by(|a, b| {
            a.title
                .to_lowercase()
                .cmp(&b.title.to_lowercase())
                .then(a.id.cmp(&b.id))
        });
        list
    }
    fn snapshot(&self) -> Snapshot {
        let preferences = self.visible_preferences();
        let templates = self
            .visible_templates(&self.field)
            .into_iter()
            .filter(|t| preferences.include_shared || t.scope == "personal")
            .collect();
        Snapshot {
            field_code: self.field.clone(),
            fields: self.state.fields.clone(),
            templates,
            preferences,
            pending: self.state.outbox.len(),
            preferences_pending: self.state.outbox.iter().any(|p| p.path == PREFS),
            conflicts: self
                .state
                .outbox
                .iter()
                .filter_map(|op| {
                    op.blocked.as_ref().map(|code| Conflict {
                        operation_id: op.id,
                        entity_id: op.entity_id,
                        field_code: op.field_code.clone(),
                        code: code.clone(),
                        message: conflict_message(code).into(),
                        can_keep_local: can_reapply(op, &op.remote),
                        local: op.body.clone(),
                        remote: op.remote.clone(),
                    })
                })
                .collect(),
            error: self.error.clone(),
            online: self.online,
            loaded: self.state.templates.contains_key(&self.field),
        }
    }
    fn field(&self, code: &str) -> Result<&Field, String> {
        self.state
            .fields
            .iter()
            .find(|f| f.field_code == code && f.enabled && known_field(code))
            .ok_or_else(|| "Для этого поля шаблоны недоступны".into())
    }
    fn enqueue(&mut self, mut op: Pending) -> Result<(), String> {
        let mut state = self.state.clone();
        let entity = op.entity_id;
        if let Some(previous) = state
            .outbox
            .iter_mut()
            .rev()
            .find(|p| p.entity_id == op.entity_id && p.path == op.path)
        {
            if previous.blocked.is_some() {
                return Err("Сначала разрешите конфликт этого шаблона".into());
            }
            if !previous.started {
                previous.body["title"] = op.body["title"].clone();
                previous.body["body"] = op.body["body"].clone();
                if op.path == PREFS {
                    previous.body = op.body;
                }
                let id = previous.id;
                self.commit(state)?;
                self.recent_operations.insert(entity, id);
                return Ok(());
            }
            op.after = Some(previous.id);
        } else if let Some(previous) = state
            .outbox
            .iter()
            .rev()
            .find(|p| p.entity_id == op.entity_id)
        {
            if previous.blocked.is_some() {
                return Err("Сначала разрешите конфликт этого шаблона".into());
            }
            op.after = Some(previous.id);
        }
        let id = op.id;
        state.outbox.push_back(op);
        self.commit(state)?;
        self.recent_operations.insert(entity, id);
        Ok(())
    }
    fn prepare(&mut self) -> Result<Option<Pending>, String> {
        let mut state = self.state.clone();
        let candidate = state.outbox.iter().position(|p| {
            p.blocked.is_none() && p.after.is_none_or(|id| state.receipts.contains_key(&id))
        });
        let Some(index) = candidate else {
            return Ok(None);
        };
        let op = &mut state.outbox[index];
        if !op.started {
            if let Some(previous) = op.after {
                op.body["expected_version"] = json!(state.receipts[&previous]);
            }
            op.started = true;
            let prepared = op.clone();
            self.commit(state)?;
            Ok(Some(prepared))
        } else {
            Ok(Some(op.clone()))
        }
    }
    fn acknowledge(&mut self, op: &Pending, response: &Value) -> Result<(), String> {
        let mut state = self.state.clone();
        let current = state
            .outbox
            .iter()
            .find(|v| v.id == op.id)
            .ok_or("Операция шаблонов уже изменилась")?;
        if current.body != op.body || !current.started {
            return Err("Некорректная квитанция шаблонов".into());
        }
        let version = if op.path == PREFS {
            let value = response.get("preferences").unwrap_or(response);
            let prefs: Preferences = serde_json::from_value(value.clone())
                .map_err(|_| "Некорректный ответ настроек шаблонов")?;
            validate_preferences(&prefs.apply_mode)?;
            if prefs.apply_mode != op.body["apply_mode"]
                || prefs.include_shared != op.body["include_shared"]
                || prefs.row_version == 0
            {
                return Err("Квитанция не соответствует настройкам шаблонов".into());
            }
            let version = prefs.row_version;
            state.preferences = prefs;
            version
        } else {
            let value = response.get("template").unwrap_or(response);
            let mut template: Template = serde_json::from_value(value.clone())
                .map_err(|_| "Некорректный ответ библиотеки шаблонов")?;
            if template.id != op.entity_id
                || template.field_code != op.field_code
                || template.title != op.body["title"]
                || template.body != op.body["body"]
                || template.row_version == 0
                || !matches!(template.scope.as_str(), "personal" | "shared")
                || (op.method == "POST" && template.scope != op.body["scope"])
            {
                return Err("Квитанция не соответствует сохранённому шаблону".into());
            }
            template.sync_state = "synced".into();
            let version = template.row_version;
            let list = state
                .templates
                .entry(template.field_code.clone())
                .or_default();
            list.retain(|t| t.id != template.id);
            list.push(template);
            version
        };
        if op.body["expected_version"]
            .as_u64()
            .is_some_and(|expected| version <= expected)
        {
            return Err("Версия квитанции шаблонов не увеличилась".into());
        }
        state.outbox.retain(|p| p.id != op.id);
        state.receipts.insert(op.id, version);
        let dependencies: BTreeSet<_> = state.outbox.iter().filter_map(|p| p.after).collect();
        state.receipts.retain(|id, _| dependencies.contains(id));
        self.commit(state)
    }
    fn block(&mut self, op: &Pending, code: &str, response: &Value) -> Result<(), String> {
        let mut state = self.state.clone();
        if let Some(current) = state.outbox.iter_mut().find(|p| p.id == op.id) {
            current.blocked = Some(code.into());
            current.remote = response
                .get("current")
                .or_else(|| response.get("template"))
                .or_else(|| response.get("preferences"))
                .cloned()
                .unwrap_or(Value::Null);
            current.current_version = response["current_version"].as_u64();
        }
        self.commit(state)
    }
    fn remote(&mut self, op: &Pending, response: &Value) -> Result<(), String> {
        let value = if op.path == PREFS {
            let preferences: Preferences = serde_json::from_value(response.clone())
                .map_err(|_| "Некорректная серверная версия настроек шаблонов")?;
            validate_preferences(&preferences.apply_mode)?;
            serde_json::to_value(preferences).unwrap()
        } else {
            let template: Template = serde_json::from_value(response.clone())
                .map_err(|_| "Некорректная серверная версия шаблона")?;
            if template.id != op.entity_id
                || template.field_code != op.field_code
                || template.row_version == 0
            {
                return Err("Сервер вернул другой шаблон при разборе конфликта".into());
            }
            serde_json::to_value(template).unwrap()
        };
        let mut state = self.state.clone();
        if let Some(current) = state.outbox.iter_mut().find(|v| v.id == op.id) {
            if value["row_version"].as_u64().unwrap_or(0) < current.current_version.unwrap_or(0) {
                return Err("Текущая версия шаблона ещё недоступна; обновите список".into());
            }
            current.remote = value;
        }
        self.commit(state)
    }
}

impl Library {
    fn require_offline_access(&self) -> Result<(),String> {
        if let Some(connection)=self.shared.connection.lock().unwrap().as_ref() {
            connection.require_offline_access()?;
        }
        Ok(())
    }
    pub fn open(connection: &Connection) -> Result<Self, String> {
        let user = connection
            .user_id
            .ok_or("Для библиотеки шаблонов требуется вход врача")?;
        #[cfg(windows)]
        let store = {
            let path =
                std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA")?)
                    .join(crate::APP_DATA_DIR).join("phrases")
                    .join(digest(&connection.storage_base))
                    .join(user.to_string());
            Store::new(path, Arc::new(crate::cache::Dpapi))?
        };
        #[cfg(not(windows))]
        return Err("Для библиотеки шаблонов нужен защищённый кеш Windows".into());
        #[cfg(windows)]
        {
            let repo = load(store, &connection.storage_base, user)?;
            let shared = Arc::new(Shared {
                repo: Mutex::new(repo),
                connection: Mutex::new(Some(raw(connection))),
                wake: AtomicBool::new(true),
                generation: std::sync::atomic::AtomicU64::new(1),
            });
            let weak = Arc::downgrade(&shared);
            std::thread::Builder::new()
                .name("m53-phrase-library".into())
                .spawn(move || background(weak))
                .map_err(|_| "Не удалось запустить синхронизацию шаблонов")?;
            Ok(Self { shared })
        }
    }
    pub fn reconnect(&self, connection: &Connection) -> Result<(), String> {
        let mut repo = self.shared.repo.lock().unwrap();
        if !crate::transport::same_server(&repo.state.base, &connection.storage_base) || connection.user_id != Some(repo.state.user_id) {
            return Err("Нельзя отправить шаблоны другого врача или сервера".into());
        }
        repo.online = false;
        drop(repo);
        *self.shared.connection.lock().unwrap() = Some(raw(connection));
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        self.shared.wake.store(true, Ordering::SeqCst);
        Ok(())
    }
    pub fn pause(&self) {
        *self.shared.connection.lock().unwrap() = None;
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        self.shared.repo.lock().unwrap().online = false;
    }
    pub fn set_field(&self, code: &str) -> Result<(), String> {
        let mut repo = self.shared.repo.lock().unwrap();
        repo.field(code)?;
        if repo.field != code {
            repo.field = code.into();
            self.shared.wake.store(true, Ordering::SeqCst);
        }
        Ok(())
    }
    pub fn refresh(&self) {
        self.shared.wake.store(true, Ordering::SeqCst);
    }
    pub fn snapshot(&self) -> Snapshot {
        self.shared.repo.lock().unwrap().snapshot()
    }
    /// The actual frozen outbox operation used by the immediately preceding mutation.
    /// A recent ACK does not lose the link needed by the separate activity journal.
    pub fn operation_id(&self, entity: Option<Uuid>) -> Option<Uuid> {
        let repo = self.shared.repo.lock().unwrap();
        let id = entity.unwrap_or(repo.state.user_id);
        repo.state
            .outbox
            .iter()
            .rev()
            .find(|op| op.entity_id == id && (entity.is_some() || op.path == PREFS))
            .map(|op| op.id)
            .or_else(|| repo.recent_operations.get(&id).copied())
    }
    pub fn create(
        &self,
        field: &str,
        scope: &str,
        title: &str,
        body: &str,
    ) -> Result<Uuid, String> {
        self.require_offline_access()?;
        let mut repo = self.shared.repo.lock().unwrap();
        validate_text(title, body, repo.field(field)?.max_bytes)?;
        if !matches!(scope, "personal" | "shared") {
            return Err("Выберите личный или общий шаблон".into());
        }
        let id = Uuid::new_v4();
        repo.enqueue(Pending {
            id: Uuid::new_v4(), entity_id: id, field_code: field.into(), method: "POST".into(), path: PREFIX.into(),
            body: json!({"id":id,"field_code":field,"scope":scope,"title":title.trim(),"body":body}),
            after: None, started: false, blocked: None, remote: Value::Null, current_version: None,
        })?;
        self.shared.wake.store(true, Ordering::SeqCst);
        Ok(id)
    }
    pub fn edit(&self, id: Uuid, title: &str, body: &str) -> Result<(), String> {
        let field = self.shared.repo.lock().unwrap().field.clone();
        self.edit_for(&field, id, title, body)
    }
    /// Save to the editor's captured field even if protocol focus has moved elsewhere.
    pub fn edit_for(
        &self,
        field_code: &str,
        id: Uuid,
        title: &str,
        body: &str,
    ) -> Result<(), String> {
        self.require_offline_access()?;
        let mut repo = self.shared.repo.lock().unwrap();
        repo.field(field_code)?;
        let template = repo
            .visible_templates(field_code)
            .into_iter()
            .find(|t| t.id == id)
            .ok_or("Шаблон не найден в поле, для которого открыт редактор")?;
        if template.field_code != field_code {
            return Err("Нельзя переназначить шаблон другому полю".into());
        }
        if !template.can_edit {
            return Err("Редактирование этого шаблона недоступно".into());
        }
        validate_text(title, body, repo.field(&template.field_code)?.max_bytes)?;
        // A not-yet-attempted create can be changed in place without changing its request identity.
        if let Some(create) = repo
            .state
            .outbox
            .iter()
            .find(|p| p.entity_id == id && p.method == "POST" && !p.started)
        {
            let mut state = repo.state.clone();
            let op = state.outbox.iter_mut().find(|p| p.id == create.id).unwrap();
            op.body["title"] = json!(title.trim());
            op.body["body"] = json!(body);
            let operation = op.id;
            repo.commit(state)?;
            repo.recent_operations.insert(id, operation);
        } else {
            repo.enqueue(Pending {
                id: Uuid::new_v4(), entity_id: id, field_code: template.field_code, method: "PUT".into(), path: format!("{PREFIX}/{id}"),
                body: json!({"title":title.trim(),"body":body,"expected_version":template.row_version}),
                after: None, started: false, blocked: None, remote: Value::Null, current_version: None,
            })?;
        }
        self.shared.wake.store(true, Ordering::SeqCst);
        Ok(())
    }
    pub fn preferences(&self, apply_mode: &str, include_shared: bool) -> Result<(), String> {
        self.require_offline_access()?;
        validate_preferences(apply_mode)?;
        let mut repo = self.shared.repo.lock().unwrap();
        let version = repo.state.preferences.row_version;
        let owner = repo.state.user_id;
        repo.enqueue(Pending {
            id: Uuid::new_v4(), entity_id: owner, field_code: String::new(), method: "PUT".into(), path: PREFS.into(),
            body: json!({"apply_mode":apply_mode,"include_shared":include_shared,"expected_version":version}),
            after: None, started: false, blocked: None, remote: Value::Null, current_version: None,
        })?;
        self.shared.wake.store(true, Ordering::SeqCst);
        Ok(())
    }
    /// Explicitly discard this entity's pending branch after comparing both versions in the UI.
    /// Original local operations remain encrypted in the resolution archive.
    pub fn resolve_use_server(&self, operation: Uuid) -> Result<(), String> {
        self.resolve(operation, false)
    }
    /// Explicitly reapply the latest local draft with a new operation UUID and current server CAS.
    pub fn resolve_keep_local(&self, operation: Uuid) -> Result<(), String> {
        self.resolve(operation, true)
    }
    fn resolve(&self, operation: Uuid, keep: bool) -> Result<(), String> {
        if keep { self.require_offline_access()?; }
        let mut repo = self.shared.repo.lock().unwrap();
        let blocked = repo
            .state
            .outbox
            .iter()
            .find(|p| p.id == operation && p.blocked.is_some())
            .cloned()
            .ok_or("Конфликт шаблона не найден")?;
        let latest = repo
            .state
            .outbox
            .iter()
            .rev()
            .find(|p| p.entity_id == blocked.entity_id)
            .cloned()
            .unwrap();
        let cached = if blocked.path == PREFS {
            serde_json::to_value(&repo.state.preferences).unwrap()
        } else {
            repo.state
                .templates
                .get(&blocked.field_code)
                .into_iter()
                .flatten()
                .find(|t| t.id == blocked.entity_id)
                .and_then(|t| serde_json::to_value(t).ok())
                .unwrap_or(blocked.remote.clone())
        };
        let remote = if blocked.remote["row_version"].as_u64().unwrap_or(0)
            > cached["row_version"].as_u64().unwrap_or(0)
        {
            blocked.remote.clone()
        } else {
            cached
        };
        if !blocked.blocked.as_deref().is_some_and(version_conflict)
            || !remote_loaded(&blocked, &remote)
        {
            return Err("Серверная версия не подтверждена. Локальная правка сохранена; обновите список или обратитесь к администратору".into());
        }
        if keep && !can_reapply(&blocked, &remote) {
            return Err(
                "Обновите список и проверьте право редактирования перед повторной отправкой".into(),
            );
        }
        let mut state = repo.state.clone();
        // A conflict response is itself a current server snapshot. Selecting it must
        // not uncover a stale pre-conflict cache after removing the local overlay.
        if blocked.path == PREFS {
            if let Ok(preferences) = serde_json::from_value::<Preferences>(remote.clone()) {
                state.preferences = preferences;
            }
        } else if let Ok(template) = serde_json::from_value::<Template>(remote.clone()) {
            if template.id == blocked.entity_id && template.field_code == blocked.field_code {
                let list = state
                    .templates
                    .entry(template.field_code.clone())
                    .or_default();
                list.retain(|v| v.id != template.id);
                list.push(template);
            }
        }
        let retained: Vec<_> = state
            .outbox
            .iter()
            .filter(|p| p.entity_id == blocked.entity_id)
            .cloned()
            .collect();
        state.outbox.retain(|p| p.entity_id != blocked.entity_id);
        state.resolved.extend(retained);
        if keep {
            let mut replacement = latest;
            replacement.id = Uuid::new_v4();
            replacement.after = None;
            replacement.started = false;
            replacement.blocked = None;
            replacement.remote = Value::Null;
            replacement.current_version = None;
            replacement.body["expected_version"] = remote["row_version"].clone();
            if replacement.method == "POST" {
                replacement.method = "PUT".into();
                replacement.path = format!("{PREFIX}/{}", replacement.entity_id);
                replacement.body = json!({"title":replacement.body["title"],"body":replacement.body["body"],"expected_version":remote["row_version"]});
            }
            state.outbox.push_back(replacement);
        }
        repo.commit(state)?;
        self.shared.wake.store(true, Ordering::SeqCst);
        Ok(())
    }
}
impl Drop for Library {
    fn drop(&mut self) {
        self.pause();
    }
}

fn validate_preferences(mode: &str) -> Result<(), String> {
    if matches!(mode, "append" | "replace") {
        Ok(())
    } else {
        Err("Выберите добавление или замену текста".into())
    }
}
fn validate_text(title: &str, body: &str, max_bytes: usize) -> Result<(), String> {
    if title.contains('\0') || body.contains('\0') {
        return Err("Текст шаблона содержит недопустимый символ".into());
    }
    if title.trim().is_empty() || body.trim().is_empty() {
        return Err("Заполните наименование и текст шаблона".into());
    }
    if title.trim().chars().count() > 200 {
        return Err("Наименование шаблона: не более 200 символов".into());
    }
    if body.len() > max_bytes.min(MAX_BYTES) {
        return Err("Текст шаблона превышает допустимый размер поля".into());
    }
    Ok(())
}
/// Exact pre-insertion calculation, shared by native UI and tests; it does not change the form.
pub fn apply(current: &str, phrase: &str, mode: &str, max_bytes: usize) -> Result<String, String> {
    validate_preferences(mode)?;
    if phrase.contains('\0') {
        return Err("Текст шаблона содержит недопустимый символ".into());
    }
    let result = if mode == "replace" || current.is_empty() {
        phrase.to_owned()
    } else {
        format!(
            "{}\n{}",
            current.trim_end_matches(['\r', '\n']),
            phrase.trim_start_matches(['\r', '\n'])
        )
    };
    if result.len() > max_bytes.min(MAX_BYTES) {
        return Err("После вставки текст превысит допустимый размер поля".into());
    }
    Ok(result)
}
fn active(shared: &Shared, generation: u64) -> bool {
    shared.generation.load(Ordering::SeqCst) == generation
        && shared.connection.lock().unwrap().is_some()
}
fn version_conflict(code: &str) -> bool {
    matches!(
        code,
        "version_conflict"
            | "phrase_template_version_conflict"
            | "phrase_preferences_version_conflict"
    )
}
fn can_reapply(op: &Pending, remote: &Value) -> bool {
    op.blocked.as_deref().is_some_and(version_conflict)
        && remote_loaded(op, remote)
        && (op.path == PREFS || remote["can_edit"] == true)
}
fn remote_loaded(op: &Pending, remote: &Value) -> bool {
    remote["row_version"]
        .as_u64()
        .is_some_and(|v| v >= op.current_version.unwrap_or(0))
        && if op.path == PREFS {
            remote["apply_mode"]
                .as_str()
                .is_some_and(|v| matches!(v, "append" | "replace"))
                && remote["include_shared"].is_boolean()
        } else {
            remote["id"] == json!(op.entity_id)
                && remote["field_code"] == op.field_code
                && remote["can_edit"].is_boolean()
                && remote["title"].is_string()
                && remote["body"].is_string()
        }
}
fn conflict_message(code: &str) -> &'static str {
    if version_conflict(code) {
        "Шаблон уже изменён на сервере. Сравните обе версии."
    } else if code == "operation_id_conflict" {
        "Идентификатор операции уже связан с другим запросом. Отправка остановлена; локальный текст сохранён."
    } else if code == "phrase_template_id_conflict" {
        "Идентификатор шаблона уже занят. Локальный текст сохранён; создайте отдельный шаблон."
    } else {
        "Сервер не принял изменение. Локальный текст сохранён."
    }
}
fn rejection(status: u16, body: &Value) -> &'static str {
    match body["code"]
        .as_str()
        .or_else(|| body["error"]["code"].as_str())
    {
        Some("phrase_template_version_conflict") if status == 409 => {
            "phrase_template_version_conflict"
        }
        Some("phrase_preferences_version_conflict") if status == 409 => {
            "phrase_preferences_version_conflict"
        }
        Some("phrase_template_id_conflict") if status == 409 => "phrase_template_id_conflict",
        Some("operation_id_conflict") if status == 409 => "operation_id_conflict",
        _ if status == 409 => "request_conflict",
        _ => "validation_rejected",
    }
}
fn fetch(connection: &Connection, path: &str) -> Result<Value, String> {
    let (status, bytes) = connection.exchange("GET", path, None, None, None)?;
    if matches!(status, 401 | 403) {
        return Err("phrase_auth_required".into());
    }
    if status == 404 {
        return Err("Сервер пока не поддерживает библиотеку шаблонов".into());
    }
    if status != 200 {
        return Err(format!(
            "Библиотека шаблонов временно недоступна ({status})"
        ));
    }
    serde_json::from_slice(&bytes).map_err(|_| "Некорректный ответ библиотеки шаблонов".into())
}
fn refresh(shared: &Shared, connection: &Connection, generation: u64) -> Result<(), String> {
    let fields = fetch(connection, "/api/v1/phrase-template-fields")?;
    let fields: Vec<Field> = serde_json::from_value(
        fields
            .get("fields")
            .cloned()
            .ok_or("Некорректный реестр шаблонов")?,
    )
    .map_err(|_| "Некорректный реестр шаблонов")?;
    let preferences = fetch(connection, PREFS)?;
    let preferences: Preferences = serde_json::from_value(
        preferences
            .get("preferences")
            .unwrap_or(&preferences)
            .clone(),
    )
    .map_err(|_| "Некорректные настройки шаблонов")?;
    validate_preferences(&preferences.apply_mode)?;
    if !active(shared, generation) {
        return Ok(());
    }
    let field = shared.repo.lock().unwrap().field.clone();
    let templates = if !field.is_empty() {
        let result = fetch(
            connection,
            &format!("{PREFIX}?field_code={field}&visibility=all"),
        )?;
        let mut templates: Vec<Template> = serde_json::from_value(
            result
                .get("templates")
                .cloned()
                .ok_or("Некорректный список шаблонов")?,
        )
        .map_err(|_| "Некорректный список шаблонов")?;
        let mut ids = BTreeSet::new();
        if templates.iter().any(|t| {
            t.field_code != field
                || !matches!(t.scope.as_str(), "personal" | "shared")
                || t.row_version == 0
                || !ids.insert(t.id)
        }) {
            return Err("Сервер вернул шаблоны другого поля или некорректную версию".into());
        }
        for template in &mut templates {
            template.sync_state = "synced".into();
        }
        Some(templates)
    } else {
        None
    };
    if !active(shared, generation) {
        return Ok(());
    }
    let mut repo = shared.repo.lock().unwrap();
    let mut state = repo.state.clone();
    state.fields = fields
        .into_iter()
        .filter(|f| known_field(&f.field_code) && f.max_bytes > 0)
        .collect();
    state.preferences = preferences;
    if let Some(templates) = templates {
        state.templates.insert(field, templates);
    }
    // Populate the comparison even when a 409 contains only an error code.
    for pending in state.outbox.iter_mut().filter(|p| p.blocked.is_some()) {
        let remote = if pending.path == PREFS {
            serde_json::to_value(&state.preferences).unwrap()
        } else {
            state
                .templates
                .get(&pending.field_code)
                .into_iter()
                .flatten()
                .find(|t| t.id == pending.entity_id)
                .and_then(|t| serde_json::to_value(t).ok())
                .unwrap_or(Value::Null)
        };
        if remote["row_version"].as_u64().unwrap_or(0)
            >= pending.remote["row_version"].as_u64().unwrap_or(0)
        {
            pending.remote = remote;
        }
    }
    repo.commit(state)
}
fn sync(
    shared: &Shared,
    connection: &Connection,
    generation: u64,
    do_refresh: bool,
) -> Result<(), String> {
    let owner = shared.repo.lock().unwrap().state.user_id;
    if connection.user_id != Some(owner) || connection.session.is_empty() {
        return Ok(());
    }
    let me = fetch(connection, "/api/v1/me")?;
    if me["user_id"] != json!(owner) {
        return Err("phrase_actor_mismatch".into());
    }
    if !active(shared, generation) {
        return Ok(());
    }
    if do_refresh {
        refresh(shared, connection, generation)?;
    }
    // Recover a comparison fetch interrupted after the durable 409 quarantine.
    // This also handles a conflict from a field that is no longer selected.
    let unresolved: Vec<_> = shared
        .repo
        .lock()
        .unwrap()
        .state
        .outbox
        .iter()
        .filter(|op| {
            op.blocked.as_deref().is_some_and(version_conflict) && !remote_loaded(op, &op.remote)
        })
        .take(8)
        .cloned()
        .collect();
    for op in unresolved {
        if !active(shared, generation) {
            return Ok(());
        }
        let path = if op.path == PREFS {
            PREFS.to_owned()
        } else {
            format!("{PREFIX}/{}", op.entity_id)
        };
        let current = fetch(connection, &path)?;
        if active(shared, generation) {
            shared.repo.lock().unwrap().remote(&op, &current)?;
        }
    }
    // Bound a cycle so fresh navigation, logout and other library operations remain responsive.
    for _ in 0..8 {
        if !active(shared, generation) {
            return Ok(());
        }
        let prepared = shared.repo.lock().unwrap().prepare()?;
        let Some(op) = prepared else { break };
        if !active(shared, generation) {
            return Ok(());
        }
        let (status, bytes) = connection.exchange(
            &op.method,
            &op.path,
            Some(&op.body),
            None,
            Some(&op.transport()),
        )?;
        if !active(shared, generation) {
            return Ok(());
        }
        if matches!(status, 401 | 403) {
            return Err("phrase_auth_required".into());
        }
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if matches!(status, 200 | 201) {
            shared.repo.lock().unwrap().acknowledge(&op, &body)?;
        } else if matches!(status, 400 | 409 | 422) {
            let code = rejection(status, &body);
            shared.repo.lock().unwrap().block(&op, code, &body)?;
            if version_conflict(code) && active(shared, generation) {
                let path = if op.path == PREFS {
                    PREFS.to_owned()
                } else {
                    format!("{PREFIX}/{}", op.entity_id)
                };
                let current = fetch(connection, &path)?;
                if active(shared, generation) {
                    shared.repo.lock().unwrap().remote(&op, &current)?;
                }
            }
            shared.wake.store(true, Ordering::SeqCst);
        } else {
            return Err(format!("Шаблоны ожидают синхронизации ({status})"));
        }
    }
    Ok(())
}
fn background(weak: Weak<Shared>) {
    let mut last_refresh = Instant::now() - Duration::from_secs(120);
    let mut last_attempt = Instant::now() - Duration::from_secs(120);
    let mut last_generation = 0;
    let mut failures = 0u32;
    loop {
        let Some(shared) = weak.upgrade() else { break };
        let generation = shared.generation.load(Ordering::SeqCst);
        let connection = shared.connection.lock().unwrap().clone();
        let changed = generation != last_generation;
        let requested = shared.wake.swap(false, Ordering::SeqCst);
        let refresh = changed || requested || last_refresh.elapsed() >= Duration::from_secs(60);
        let delay = Duration::from_secs(2u64.saturating_pow(failures.min(5)).max(2));
        if let Some(connection) = connection.filter(|c| !c.session.is_empty()) {
            let pending = shared
                .repo
                .lock()
                .unwrap()
                .state
                .outbox
                .iter()
                .any(|p| p.blocked.is_none());
            if (refresh || pending) && (changed || last_attempt.elapsed() >= delay) {
                last_attempt = Instant::now();
                last_generation = generation;
                let result = sync(&shared, &connection, generation, refresh);
                if active(&shared, generation) {
                    let mut repo = shared.repo.lock().unwrap();
                    match result {
                        Ok(()) => {
                            repo.online = true;
                            repo.error.clear();
                            failures = 0;
                            if refresh {
                                last_refresh = Instant::now();
                            }
                        }
                        Err(error) => {
                            repo.online = false;
                            let auth =
                                error == "phrase_auth_required" || error == "phrase_actor_mismatch";
                            repo.error = if auth {
                                "Шаблоны сохранены; повторите вход для синхронизации".into()
                            } else {
                                error
                            };
                            failures = failures.saturating_add(1);
                            drop(repo);
                            if auth {
                                *shared.connection.lock().unwrap() = None;
                            }
                            shared.wake.store(true, Ordering::SeqCst);
                        }
                    }
                }
            } else if requested {
                shared.wake.store(true, Ordering::SeqCst);
            }
        }
        drop(shared);
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (Repo, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("m53-phrase-test-{}", Uuid::new_v4()));
        let store =
            Store::new(root.clone(), Arc::new(crate::cache::AesProtector([32; 32]))).unwrap();
        (
            load(store, "http://localhost:8790", Uuid::new_v4()).unwrap(),
            root,
        )
    }
    fn operation(id: Uuid, field: &str) -> Pending {
        Pending {
            id: Uuid::new_v4(),
            entity_id: id,
            field_code: field.into(),
            method: "POST".into(),
            path: PREFIX.into(),
            body: json!({"id":id,"field_code":field,"scope":"personal","title":"Название","body":"Тестовый текст"}),
            after: None,
            started: false,
            blocked: None,
            remote: Value::Null,
            current_version: None,
        }
    }
    fn remove(repo: Repo, path: std::path::PathBuf) {
        drop(repo);
        assert!(path.starts_with(std::env::temp_dir()));
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn encrypted_cache_reopens_only_for_exact_actor_and_server() {
        let (mut repo, path) = setup();
        let id = Uuid::new_v4();
        repo.enqueue(operation(id, "conclusion")).unwrap();
        let user = repo.state.user_id;
        let disk = std::fs::read(path.join("library.dpapi")).unwrap();
        assert!(!String::from_utf8_lossy(&disk).contains("Тестовый текст"));
        let store = repo.store.clone();
        drop(repo);
        assert!(load(store.clone(), "http://localhost:8790", Uuid::new_v4()).is_err());
        assert!(load(store.clone(), "http://localhost:8791", user).is_err());
        let repo = load(store, "http://localhost:8790", user).unwrap();
        assert_eq!(repo.state.outbox[0].entity_id, id);
        remove(repo, path);
    }
    #[test]
    fn frozen_retry_preserves_operation_and_payload_until_valid_ack() {
        let (mut repo, path) = setup();
        let id = Uuid::new_v4();
        repo.enqueue(operation(id, "conclusion")).unwrap();
        let first = repo.prepare().unwrap().unwrap();
        let second = repo.prepare().unwrap().unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(first.body, second.body);
        assert!(first.body.get("user_id").is_none());
        assert!(first.body.get("organization_id").is_none());
        assert!(
            repo.acknowledge(&first, &json!({"id":Uuid::new_v4()}))
                .is_err()
        );
        assert_eq!(repo.state.outbox.len(), 1);
        repo.acknowledge(&first, &json!({"id":id,"field_code":"conclusion","scope":"personal","title":"Название","body":"Тестовый текст","row_version":1,"can_edit":true})).unwrap();
        assert!(repo.state.outbox.is_empty());
        remove(repo, path);
    }
    #[test]
    fn conflict_preserves_local_branch_and_does_not_stop_other_entities() {
        let (mut repo, path) = setup();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        repo.enqueue(operation(a, "conclusion")).unwrap();
        repo.enqueue(operation(b, "extended.other")).unwrap();
        let first = repo.prepare().unwrap().unwrap();
        repo.block(
            &first,
            "version_conflict",
            &json!({"current":{"row_version":2}}),
        )
        .unwrap();
        assert_eq!(repo.prepare().unwrap().unwrap().entity_id, b);
        assert_eq!(repo.state.outbox.len(), 2);
        assert_eq!(repo.snapshot().conflicts.len(), 1);
        remove(repo, path);
    }
    #[test]
    fn dependent_preference_change_uses_own_receipt_not_remote_rebase() {
        let (mut repo, path) = setup();
        let owner = repo.state.user_id;
        let pending = |mode| Pending {
            id: Uuid::new_v4(),
            entity_id: owner,
            field_code: String::new(),
            method: "PUT".into(),
            path: PREFS.into(),
            body: json!({"apply_mode":mode,"include_shared":true,"expected_version":0}),
            after: None,
            started: false,
            blocked: None,
            remote: Value::Null,
            current_version: None,
        };
        repo.enqueue(pending("replace")).unwrap();
        let first = repo.prepare().unwrap().unwrap();
        repo.enqueue(pending("append")).unwrap();
        assert_eq!(repo.state.outbox[1].after, Some(first.id));
        repo.acknowledge(
            &first,
            &json!({"apply_mode":"replace","include_shared":true,"row_version":1}),
        )
        .unwrap();
        let second = repo.prepare().unwrap().unwrap();
        assert_eq!(second.body["expected_version"], 1);
        assert_ne!(first.id, second.id);
        assert_eq!(repo.visible_preferences().apply_mode, "append");
        remove(repo, path);
    }
    #[test]
    fn field_registry_and_utf8_append_limits_are_exact() {
        assert_eq!(FIELD_CODES.len(), 20);
        assert!(known_field("diagnosis_note"));
        assert!(default_fields().iter().any(|field| field.field_code == "diagnosis_note" && field.name == "Дополнение к диагнозам" && field.max_bytes == 16384));
        assert!(!known_field("history"));
        assert_eq!(
            apply("До\r\n", "\nПосле", "append", 100).unwrap(),
            "До\nПосле"
        );
        assert_eq!(apply("До", "После", "replace", 100).unwrap(), "После");
        assert!(apply("А", "Б", "append", 4).is_err());
        assert!(validate_text(" ", "Текст", MAX_BYTES).is_err());
        assert!(validate_text("Имя", &"я".repeat(8193), MAX_BYTES).is_err());
        assert!(validate_text("Имя\0", "Текст", MAX_BYTES).is_err());
        assert!(validate_text("Имя", "Те\0кст", MAX_BYTES).is_err());
        assert!(apply("", "Те\0кст", "append", MAX_BYTES).is_err());
    }
    #[test]
    fn actual_cas_codes_require_current_server_text_and_fatal_codes_cannot_rebase() {
        let (mut repo, path) = setup();
        repo.field = "conclusion".into();
        let id = Uuid::new_v4();
        repo.enqueue(operation(id, "conclusion")).unwrap();
        let op = repo.prepare().unwrap().unwrap();
        let code = rejection(
            409,
            &json!({"code":"phrase_template_version_conflict","current_version":3}),
        );
        assert!(version_conflict(code));
        repo.block(&op, code, &json!({"current_version":3}))
            .unwrap();
        assert!(!repo.snapshot().conflicts[0].can_keep_local);
        let remote = json!({"id":id,"field_code":"conclusion","scope":"personal","title":"Сервер","body":"Иная версия","row_version":3,"can_edit":true});
        let mut stale = remote.clone();
        stale["row_version"] = json!(2);
        assert!(repo.remote(&op, &stale).is_err());
        repo.remote(&op, &remote).unwrap();
        assert!(repo.snapshot().conflicts[0].can_keep_local);
        for code in ["operation_id_conflict", "phrase_template_id_conflict"] {
            assert_eq!(rejection(409, &json!({"code":code})), code);
            repo.block(&op, code, &json!({"current":remote})).unwrap();
            assert!(!repo.snapshot().conflicts[0].can_keep_local);
        }
        assert_eq!(
            rejection(409, &json!({"code":"phrase_preferences_version_conflict"})),
            "phrase_preferences_version_conflict"
        );
        remove(repo, path);
    }
    #[test]
    fn workflow_review_expired_profile_cannot_enqueue_template_changes() {
        let (repo,path)=setup();
        let mut raw=Connection::new("http://127.0.0.1:8790","synthetic-key").unwrap();
        raw.offline_deadline=Some((crate::one_c_auth::now()-1,std::time::Instant::now()));
        let library=Library{shared:Arc::new(Shared{repo:Mutex::new(repo),connection:Mutex::new(Some(raw)),
            wake:AtomicBool::new(false),generation:std::sync::atomic::AtomicU64::new(1)})};
        assert!(library.create("conclusion","personal","Synthetic","Text").is_err());
        assert!(library.shared.repo.lock().unwrap().state.outbox.is_empty());
        drop(library);let _=std::fs::remove_dir_all(path);
    }
    #[test]
    fn pause_reconnect_and_shared_filter_keep_private_drafts_bound_to_owner() {
        let (mut repo, path) = setup();
        repo.field = "conclusion".into();
        let owner = repo.state.user_id;
        let shared = Arc::new(Shared {
            repo: Mutex::new(repo),
            connection: Mutex::new(None),
            wake: AtomicBool::new(false),
            generation: std::sync::atomic::AtomicU64::new(1),
        });
        let library = Library {
            shared: shared.clone(),
        };
        let personal = library
            .create("conclusion", "personal", "Личный", "Личный текст")
            .unwrap();
        library
            .create("conclusion", "shared", "Общий", "Общий текст")
            .unwrap();
        assert_eq!(library.snapshot().templates.len(), 2);
        assert!(
            library
                .snapshot()
                .templates
                .iter()
                .all(|t| t.sync_state == "pending")
        );
        let operation = library.operation_id(Some(personal)).unwrap();
        assert!(
            shared
                .repo
                .lock()
                .unwrap()
                .state
                .outbox
                .iter()
                .any(|p| p.id == operation && p.entity_id == personal)
        );
        library.preferences("replace", false).unwrap();
        assert!(library.operation_id(None).is_some());
        assert_eq!(library.snapshot().templates.len(), 1);
        assert_eq!(library.snapshot().templates[0].id, personal);
        library.pause();
        assert_eq!(library.snapshot().pending, 3);
        let mut connection = Connection::new("http://localhost:8790", "test-key").unwrap();
        connection.user_id = Some(Uuid::new_v4());
        assert!(library.reconnect(&connection).is_err());
        assert!(shared.connection.lock().unwrap().is_none());
        connection.user_id = Some(owner);
        library.reconnect(&connection).unwrap();
        assert_eq!(library.snapshot().templates[0].scope, "personal");
        assert!(
            shared
                .connection
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .offline
                .is_none()
        );
        drop(library);
        let shared = Arc::try_unwrap(shared).ok().unwrap();
        remove(shared.repo.into_inner().unwrap(), path);
    }
    #[test]
    fn captured_editor_field_does_not_follow_later_protocol_focus() {
        let (mut repo, path) = setup();
        repo.field = "conclusion".into();
        let shared = Arc::new(Shared {
            repo: Mutex::new(repo),
            connection: Mutex::new(None),
            wake: AtomicBool::new(false),
            generation: std::sync::atomic::AtomicU64::new(1),
        });
        let library = Library {
            shared: shared.clone(),
        };
        let template = library
            .create("conclusion", "shared", "Старое имя", "Исходный текст")
            .unwrap();
        library.set_field("extended.other").unwrap();
        library
            .edit_for("conclusion", template, "Новое имя", "Изменённый текст")
            .unwrap();
        assert_eq!(library.snapshot().field_code, "extended.other");
        assert!(library.snapshot().templates.is_empty());
        assert!(
            library
                .edit_for("extended.other", template, "Нельзя", "Перенести")
                .is_err()
        );
        library.set_field("conclusion").unwrap();
        assert_eq!(library.snapshot().templates[0].body, "Изменённый текст");
        assert_eq!(library.snapshot().templates[0].scope, "shared");
        assert_eq!(library.snapshot().templates[0].field_code, "conclusion");
        drop(library);
        let shared = Arc::try_unwrap(shared).ok().unwrap();
        remove(shared.repo.into_inner().unwrap(), path);
    }
    #[test]
    fn resolving_without_server_copy_or_a_fatal_conflict_never_hides_local_work() {
        let (mut repo, path) = setup();
        repo.field = "conclusion".into();
        let id = Uuid::new_v4();
        repo.enqueue(operation(id, "conclusion")).unwrap();
        let op = repo.prepare().unwrap().unwrap();
        repo.block(
            &op,
            "phrase_template_version_conflict",
            &json!({"current_version":2}),
        )
        .unwrap();
        let shared = Arc::new(Shared {
            repo: Mutex::new(repo),
            connection: Mutex::new(None),
            wake: AtomicBool::new(false),
            generation: std::sync::atomic::AtomicU64::new(1),
        });
        let library = Library {
            shared: shared.clone(),
        };
        assert!(library.resolve_use_server(op.id).is_err());
        assert!(library.resolve_keep_local(op.id).is_err());
        let remote = json!({"id":id,"field_code":"conclusion","scope":"personal","title":"Сервер","body":"На сервере","row_version":2,"can_edit":true});
        for code in ["operation_id_conflict", "phrase_template_id_conflict"] {
            shared
                .repo
                .lock()
                .unwrap()
                .block(&op, code, &json!({"current":remote}))
                .unwrap();
            assert!(library.resolve_use_server(op.id).is_err());
            assert!(library.resolve_keep_local(op.id).is_err());
            assert_eq!(library.snapshot().pending, 1);
            assert_eq!(library.snapshot().templates[0].body, "Тестовый текст");
            assert!(shared.repo.lock().unwrap().state.resolved.is_empty());
        }
        drop(library);
        let shared = Arc::try_unwrap(shared).ok().unwrap();
        remove(shared.repo.into_inner().unwrap(), path);
    }
    #[test]
    fn different_me_identity_cannot_upload_the_cached_doctors_draft() {
        use std::io::{Read, Write};
        let (mut repo, path) = setup();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            let mut one = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut one).unwrap();
                request.push(one[0]);
            }
            let body = json!({"user_id":Uuid::new_v4()}).to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            String::from_utf8(request)
                .unwrap()
                .lines()
                .next()
                .unwrap()
                .to_owned()
        });
        repo.enqueue(operation(Uuid::new_v4(), "conclusion"))
            .unwrap();
        let mut connection = Connection::new(&format!("http://{address}"), "test-key").unwrap();
        connection.user_id = Some(repo.state.user_id);
        connection.session = "test-session".into();
        let shared = Shared {
            repo: Mutex::new(repo),
            connection: Mutex::new(Some(connection.clone())),
            wake: AtomicBool::new(false),
            generation: std::sync::atomic::AtomicU64::new(1),
        };
        assert_eq!(
            sync(&shared, &connection, 1, false).unwrap_err(),
            "phrase_actor_mismatch"
        );
        assert_eq!(server.join().unwrap(), "GET /api/v1/me HTTP/1.1");
        assert_eq!(shared.repo.lock().unwrap().state.outbox.len(), 1);
        remove(shared.repo.into_inner().unwrap(), path);
    }
    #[test]
    fn selecting_server_conflict_keeps_local_history_and_displays_current_copy() {
        let (mut repo, path) = setup();
        let id = Uuid::new_v4();
        repo.field = "conclusion".into();
        repo.enqueue(operation(id, "conclusion")).unwrap();
        let op = repo.prepare().unwrap().unwrap();
        let remote = json!({"id":id,"field_code":"conclusion","scope":"personal","title":"Сервер","body":"Изменено коллегой","row_version":3,"can_edit":true});
        repo.block(&op, "version_conflict", &json!({"current":remote}))
            .unwrap();
        let shared = Arc::new(Shared {
            repo: Mutex::new(repo),
            connection: Mutex::new(None),
            wake: AtomicBool::new(false),
            generation: std::sync::atomic::AtomicU64::new(1),
        });
        let library = Library {
            shared: shared.clone(),
        };
        library.resolve_use_server(op.id).unwrap();
        assert_eq!(library.snapshot().templates[0].title, "Сервер");
        assert_eq!(library.snapshot().pending, 0);
        assert_eq!(
            shared.repo.lock().unwrap().state.resolved[0].body["body"],
            "Тестовый текст"
        );
        drop(library);
        let shared = Arc::try_unwrap(shared).ok().unwrap();
        remove(shared.repo.into_inner().unwrap(), path);
    }
}
