//! UI-independent workspace. HTML has no network credentials or filesystem authority.
use crate::{connection::Connection, model::blank_protocol};
use m53_contracts::{ApproveProtocolRequest, SavedProtocol, workflow::*};
use m53_domain::validation::{Dictionary, ValidationMode, validate};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use uuid::Uuid;

// Local clinical text stays local; approval metadata belongs to an exact revision.
fn refresh_detail_metadata(local: &mut StudyDetail, cached: StudyDetail) {
    if local.study.status == "approved" && cached.study.status != "approved" {
        return; // Open the new draft explicitly; background metadata cannot unlock this revision.
    }
    local.patient = cached.patient;
    if cached.study.status != "approved" || (local.revision_id == cached.revision_id
        && local.protocol == cached.protocol && json!(local.annotations) == json!(cached.annotations)) {
        local.study = cached.study;
        local.revision_id = cached.revision_id;
    }
    local.assets = cached.assets;
}

struct BackgroundOpen {
    id: Uuid,
    reopen: bool,
    source: Connection,
    receiver: std::sync::mpsc::Receiver<Result<Value, String>>,
    ready: Option<Value>,
    active: std::sync::Arc<std::sync::Mutex<bool>>,
}

impl Drop for BackgroundOpen {
    fn drop(&mut self) {
        *self.active.lock().unwrap() = false;
    }
}

impl BackgroundOpen {
    fn matches(&self, id: Uuid, reopen: bool, current: &Connection) -> bool {
        self.id == id && self.reopen == reopen && self.source.base == current.base
            && self.source.key == current.key && self.source.session == current.session
            && self.source.user_id == current.user_id
    }
}

#[derive(Default)]
pub struct Workspace {
    pub connection: Option<Connection>,
    pub detail: Option<StudyDetail>,
    pub dictionary: Option<Dictionary>,
    pub journal: Value,
    pub dirty: bool,
    changed: Option<Instant>,
    attempted: bool,
    refreshed: Option<(Uuid, u64)>,
    pub message: String,
    background_open: Option<BackgroundOpen>,
}
impl Workspace {
    pub fn attach(&mut self, connection: Connection) {
        if self.background_open.as_ref().is_some_and(|pending| !pending.matches(pending.id,pending.reopen,&connection)) {
            self.cancel_background_open();
        }
        if self.connection.is_none() {
            self.journal = connection
                .offline
                .as_ref()
                .map(|o| o.journal_for_restore())
                .unwrap_or(Value::Null);
            self.detail = serde_json::from_value(self.journal["detail"].clone()).ok();
            self.dictionary = serde_json::from_value(self.journal["dictionary"].clone()).ok();
            self.dirty = self.journal["dirty"].as_bool().unwrap_or(false);
            self.changed = self.dirty.then(Instant::now);
        }
        self.connection = Some(connection);
        self.message = "Рабочее место открыто".into();
    }
    pub fn connection(&self) -> Result<&Connection, String> {
        self.connection
            .as_ref()
            .ok_or("Сначала войдите в рабочее место".into())
    }
    pub fn checkpoint(&mut self, mut journal: Value) -> Result<(), String> {
        let offline = self.connection.as_ref().and_then(|c| c.offline.clone());
        let _write = offline.as_ref().map(|offline| offline.write_guard()).transpose()?;
        if !journal.is_object() {
            return Err("Неверный локальный журнал".into());
        }
        if let Some(incoming) = journal.get("detail").filter(|v| !v.is_null()) {
            let incoming: StudyDetail =
                serde_json::from_value(incoming.clone()).map_err(|_| "Неверная структура формы")?;
            let current = self.detail.as_mut().ok_or("Нет открытого исследования")?;
            if incoming.study.id != current.study.id || incoming.patient.id != current.patient.id {
                return Err(
                    "Форма принадлежит другому исследованию; текущая работа сохранена".into(),
                );
            }
            let edited = incoming.protocol != current.protocol
                || json!(incoming.annotations) != json!(current.annotations);
            if edited && current.study.status == "approved" {
                return Err("Утвержденный протокол неизменяем; откройте новую редакцию".into());
            }
            if edited {
                current.protocol = incoming.protocol;
                current.annotations = incoming.annotations;
                self.dirty = true;
                self.changed = Some(Instant::now());
                self.attempted = false;
            }
        }
        // These fields are owned by Rust, never by the web document.
        journal["detail"] = json!(self.detail);
        journal["dictionary"] = json!(self.dictionary);
        journal["dirty"] = json!(self.dirty);
        if self.has_revision_conflict() {
            journal["revision_conflict"] = json!(true);
        } else {
            journal.as_object_mut().unwrap().remove("revision_conflict");
        }
        self.journal = journal;
        self.persist_admitted(offline.as_deref());
        Ok(())
    }
    pub fn persist(&mut self) {
        let offline = self.connection.as_ref().and_then(|c| c.offline.clone());
        let Ok(_write) = offline.as_ref().map(|offline| offline.write_guard()).transpose() else { return; };
        self.persist_admitted(offline.as_deref());
    }
    fn persist_admitted(&mut self, offline: Option<&crate::offline::Offline>) {
        if !self.journal.is_object() {
            self.journal = json!({});
        }
        self.journal["detail"] = json!(self.detail);
        self.journal["dictionary"] = json!(self.dictionary);
        self.journal["dirty"] = json!(self.dirty);
        if self.has_revision_conflict() {
            self.journal["revision_conflict"] = json!(true);
        } else {
            self.journal.as_object_mut().unwrap().remove("revision_conflict");
        }
        if let Some(offline) = offline {
            offline.checkpoint_admitted(self.journal.clone());
        }
    }
    fn has_revision_conflict(&self) -> bool {
        self.journal["revision_conflict"] == true || self.detail.as_ref().is_some_and(|detail| {
            self.connection.as_ref().and_then(|connection| connection.offline.as_ref())
                .is_some_and(|offline| offline.state.lock().unwrap().has_local_revision_conflict(detail.study.id))
        })
    }
    pub fn open_background(&mut self, id: Uuid, version: Option<Uuid>, reopen: bool) -> Result<(), String> {
        let connection = self.connection()?.clone();
        if self.background_open.as_ref().is_none_or(|pending| !pending.matches(id, reopen, &connection)) {
            self.cancel_background_open();
            let (sender, receiver) = std::sync::mpsc::channel();
            let worker_connection = connection.clone();
            let previous = self.detail.clone();
            let active = std::sync::Arc::new(std::sync::Mutex::new(true));
            let worker_active = active.clone();
            std::thread::spawn(move || {
                let result = (|| {
                    if reopen {
                        let mut worker = Workspace::default();
                        worker.connection = Some(worker_connection.clone());
                        worker.detail = previous.filter(|detail|detail.study.id == id);
                        worker.reopen_if_active(Some(&worker_active))?;
                    }
                    if let Some(offline) = &worker_connection.offline {
                        offline.study_for_open(id)
                    } else {
                        worker_connection.get(&format!("/api/v1/studies/{id}"))
                    }
                })();
                let _ = sender.send(result);
            });
            self.background_open = Some(BackgroundOpen { id, reopen, source:connection, receiver, ready:None, active });
        }
        let pending = self.background_open.as_mut().unwrap();
        if pending.ready.is_none() {
            match pending.receiver.try_recv() {
                Ok(Ok(value)) => pending.ready = Some(value),
                Ok(Err(error)) => {
                    self.background_open = None;
                    return Err(error);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) =>
                    return Err("Данные еще не в локальном кеше; загружаются в фоне".into()),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.background_open = None;
                    return Err("Загрузка обследования прервана. Откройте его повторно".into());
                }
            }
        }
        let detail = pending.ready.as_ref().unwrap().clone();
        let result = self.open_detail(id, version, Some(detail));
        if result.as_ref().err().is_none_or(|error| !opening_pending(error)) {
            self.background_open = None;
        }
        result
    }

    pub fn cancel_background_open(&mut self) {
        self.background_open = None;
    }

    pub fn open(&mut self, id: Uuid, version: Option<Uuid>) -> Result<(), String> {
        self.cancel_background_open();
        self.open_detail(id, version, None)
    }

    fn open_detail(&mut self, id: Uuid, version: Option<Uuid>, prepared: Option<Value>) -> Result<(), String> {
        self.persist();
        if let Some(offline) = self.connection()?.offline.as_ref() {
            // Per-study journals preserve even invalid/incomplete input. Switching
            // studies must not depend on clinical validation or backend delivery.
            if !offline.checkpoint_ready() {
                return Err(
                    "Локальная запись еще не завершена; обследование откроется после сохранения"
                        .into(),
                );
            }
        } else if self.dirty {
            // Without a durable per-study journal, retain the original safeguard.
            self.save()?;
        }
        let connection = self.connection()?;
        let mut detail: StudyDetail = if let Some(prepared) = prepared {
            serde_json::from_value(prepared).map_err(|_| "Некорректные данные обследования")?
        } else {
            match &connection.offline {
                Some(offline) => serde_json::from_value(offline.study_for_open(id)?)
                    .map_err(|_| "Некорректные данные обследования")?,
                None => connection.get(&format!("/api/v1/studies/{id}"))?,
            }
        };
        if detail.study.id != id {
            return Err("Сервер вернул другое обследование".into());
        }
        let approved = detail.study.status == "approved";
        let local_editor = connection.offline.as_ref().and_then(|o| o.editor(id));
        let preserved_local = approved && local_editor.as_ref().is_some_and(journal_needs_upload);
        let restored = local_editor.filter(|_| !approved);
        let restored_dirty = restored.as_ref().is_some_and(|j| j["dirty"] == true);
        if restored_dirty
            && let Some(local) = restored
                .as_ref()
                .and_then(|j| serde_json::from_value::<StudyDetail>(j["detail"].clone()).ok())
            && local.study.id == id
        {
            let same_input = local.protocol == detail.protocol
                && json!(local.annotations) == json!(detail.annotations);
            if !same_input || detail.study.row_version < local.study.row_version {
                detail = local;
            } else {
                for asset in local.assets {
                    if !detail.assets.iter().any(|remote| remote.id == asset.id) {
                        detail.assets.push(asset);
                    }
                }
            }
        }

        if detail.protocol.is_none() && version.is_none() {
            if let Some(offline) = &connection.offline {
                if offline.cached("/api/v1/terminology").is_none()
                    && offline
                        .cached("/api/v1/terminology/versions")
                        .as_ref()
                        .and_then(Value::as_array)
                        .is_some_and(|versions| versions.is_empty())
                {
                    return Err("На сервере не подготовлен словарь обследования. Требуется загрузить справочник и нажать «Обновить»; данные пациента сохранены".into());
                }
            }
        }
        let dictionary: Dictionary = connection.get(&match detail
            .protocol
            .as_ref()
            .map(|p| p.terminology_version_id)
            .or(version)
        {
            Some(version) => format!("/api/v1/terminology?version_id={version}"),
            None => "/api/v1/terminology".into(),
        })?;
        if detail.protocol.is_none() {
            detail.protocol = Some(blank_protocol(dictionary.version_id));
        }
        if let Some(editor) = restored {
            self.journal = editor;
        } else {
            self.journal = json!({"screen":"study","selected_photos":detail.assets.iter().filter(|a|a.kind=="original").map(|a|a.id).collect::<Vec<_>>()});
        }
        self.dictionary = Some(dictionary);
        self.detail = Some(detail);
        self.dirty = restored_dirty;
        self.changed = restored_dirty.then(Instant::now);
        self.attempted = false;
        self.message = if preserved_local {
            "Открыт утверждённый протокол с сервера. Локальные правки сохранены отдельно"
        } else if restored_dirty {
            "Восстановлен локальный черновик; проверка перед отправкой выполняется отдельно"
        } else {
            "Обследование открыто"
        }
        .into();
        self.persist();
        Ok(())
    }
    pub fn save(&mut self) -> Result<(), String> {
        let result = self.save_draft();
        self.journal["server_save_error"] = result.as_ref().err().map(|error| json!(error)).unwrap_or(Value::Null);
        if let Err(error) = &result { self.message = error.clone(); }
        self.persist();
        result
    }
    fn save_draft(&mut self) -> Result<(), String> {
        if self.has_revision_conflict() {
            return Err("Локальный черновик расходится с серверной редакцией. Сравните версии перед сохранением".into());
        }
        check_unfinished_input(&self.journal)?;
        let detail = self.detail.as_ref().ok_or("Нет исследования")?;
        if !self.dirty && detail.revision_id.is_some() {
            return Ok(());
        }
        let protocol = detail.protocol.clone().ok_or("Нет протокола")?;
        let dictionary = self.dictionary.as_ref().ok_or("Словарь еще не загружен")?;
        let errors = validate(&protocol, dictionary, ValidationMode::Draft);
        if let Some(error) = errors.first() {
            if error.code == "unexamined_section_has_observations" {
                return Err("В разделе «Не исследовано» есть находки. Откройте протокол: выберите «Исследовано» или удалите ошибочно добавленные находки. Ввод сохранен на компьютере.".into());
            }
            return Err(format!(
                "{}: {}. Ввод сохранен локально, исправьте поле перед отправкой.",
                error.path, error.code
            ));
        }
        let result: SavedProtocol = self.connection()?.put(
            &format!("/api/v1/studies/{}/protocol", detail.study.id),
            &SaveDraft {
                expected_version: detail.study.row_version,
                protocol,
                annotations: detail.annotations.clone(),
            },
        )?;
        let current = self.detail.as_mut().unwrap();
        current.study.row_version = result.row_version;
        current.revision_id = Some(result.revision_id);
        self.dirty = false;
        self.message = "Черновик сохранен локально; сервер подтверждает отдельно".into();
        Ok(())
    }
    pub fn tick(&mut self) {
        if self.has_revision_conflict() { return; }
        if self.dirty
            && !self.attempted
            && self
                .changed
                .is_some_and(|t| t.elapsed() > Duration::from_millis(800))
        {
            self.attempted = true;
            if let Err(error) = self.save() {
                self.message = error;
            }
        }
        if let (Some(connection), Some(detail)) = (&self.connection, &self.detail)
            && let Some(offline) = &connection.offline
        {
            let revision = (detail.study.id, offline.state_token().0);
            if self.refreshed == Some(revision) {
                return;
            }
            self.refreshed = Some(revision);
        }
        if let (Some(connection), Some(detail)) = (&self.connection, &mut self.detail)
            && let Ok(cached) =
                connection.get::<StudyDetail>(&format!("/api/v1/studies/{}", detail.study.id))
        {
            refresh_detail_metadata(detail,cached);
        }
    }
    pub fn snapshot(&self) -> Value {
        let Some(connection) = &self.connection else {
            return json!({"authenticated":false,"message":self.message});
        };
        let get = |path: &str| {
            connection
                .offline
                .as_ref()
                .and_then(|o| o.cached(path))
                .unwrap_or(Value::Null)
        };
        let (online, pending, durable, error) = connection
            .offline
            .as_ref()
            .map(|o| o.information())
            .unwrap_or((false, 0, false, String::new()));
        let remote = self.detail.as_ref().and_then(|d| {
            connection
                .offline
                .as_ref()
                .and_then(|o| o.remote(d.study.id))
        });
        let (current_pending, other_blocked, conflict, local_unsent, current_durable) = connection
            .offline
            .as_ref()
            .map(|o| {
                let state = o.state.lock().unwrap();
                let own = self
                    .detail
                    .as_ref()
                    .map(|d| crate::outbox::study_indices(&state, d.study.id))
                    .unwrap_or_default();
                let others = state
                    .pending
                    .iter()
                    .enumerate()
                    .filter(|(i, op)| op.blocked.is_some() && !own.contains(i))
                    .count();
                let local_unsent = state.editors.iter().filter(|(id, journal)| {
                    self.detail.as_ref().is_none_or(|detail| {
                        detail.study.id != **id || detail.study.status == "approved"
                    }) && journal_needs_upload(journal)
                }).count();
                let current_durable=self.detail.as_ref().is_some_and(|detail|state.study_durable(detail.study.id));
                drop(state);
                let conflict = self
                    .detail
                    .as_ref()
                    .and_then(|d| o.conflict_for_study(d.study.id));
                (own.len(), others, conflict, local_unsent, current_durable)
            })
            .unwrap_or((0, 0, None, 0, false));
        let unmaterialized = self.detail.as_ref().is_some_and(|detail| detail.revision_id.is_none());
        let mut server_status = server_save_status(self.detail.is_some(), self.dirty || unmaterialized || journal_needs_upload(&self.journal), if self.detail.is_some() {current_durable} else {durable}, online, current_pending, pending, conflict.is_some(), !error.is_empty() || other_blocked > 0);
        if local_unsent > 0 {
            let unsent = format!("Не переданы локальные черновики: {local_unsent} · откройте обследование и проверьте поля");
            server_status = if self.detail.is_none() && pending == 0 { unsent } else { format!("{server_status} · {unsent}") };
        }
        let checkpoint_ready=connection.offline.as_ref().is_some_and(|offline|offline.checkpoint_ready());
        json!({"authenticated":true,"doctor":connection.doctor,"templates":get("/api/v1/templates"),"versions":get("/api/v1/terminology/versions"),"detail":self.detail,"dictionary":self.dictionary,"journal":self.journal,"dirty":self.dirty,"message":self.message,"sync":{"online":online,"pending":pending,"current_pending":current_pending,"other_blocked":other_blocked,"local_unsent":local_unsent,"durable":durable,"checkpoint_ready":checkpoint_ready,"error":error,"server_status":server_status},"conflict":conflict,"remote":remote})
    }
    pub fn approval_problems(&self) -> Vec<String> {
        let Some(detail) = &self.detail else {
            return vec!["Нет открытого обследования".into()];
        };
        if let Some(error)=self.connection.as_ref().and_then(|connection|connection.offline.as_ref())
            .and_then(|offline|offline.study_file_error(detail.study.id)) {
            return vec![error];
        }
        let Some(protocol) = &detail.protocol else {
            return vec!["Протокол еще не заполнен".into()];
        };
        let Some(dictionary) = &self.dictionary else {
            return vec!["Словарь еще не загружен".into()];
        };
        approval_problems(protocol, dictionary)
    }
    pub fn approve(&self) -> Result<(), String> {
        self.approve_with_photos(None)
    }
    pub fn approve_with_photos(&self, selected_photo_ids: Option<&[Uuid]>) -> Result<(), String> {
        let problems = self.approval_problems();
        if !problems.is_empty() {
            return Err(format!("Утверждение недоступно: {}", problems.join("; ")));
        }
        if self.dirty {
            return Err("Сначала сохраните черновик".into());
        }
        let d = self.detail.as_ref().ok_or("Нет исследования")?;
        let mut request = json!(ApproveProtocolRequest {
            study_id: d.study.id,
            revision_id: d.revision_id.ok_or("Сначала сохраните протокол")?,
            expected_version: d.study.row_version,
        });
        if let Some(selected) = selected_photo_ids {
            request["selected_photo_ids"] = json!(selected);
        }
        let _: SavedProtocol = self.connection()?.post(
            &format!("/api/v1/studies/{}/approve", d.study.id), &request,
        )?;
        Ok(())
    }
    pub fn reopen(&self) -> Result<(), String> {
        self.reopen_if_active(None)
    }
    fn reopen_if_active(&self, active: Option<&std::sync::Mutex<bool>>) -> Result<(), String> {
        let d = self.detail.as_ref().ok_or("Нет исследования")?;
        let connection = self.connection()?;
        let mut expected_version = d.study.row_version;
        if let Some(offline) = &connection.offline {
            let live = match offline.study_for_open(d.study.id) {
                Err(error) if error == "Ожидается подтверждение новой редакции" => return Ok(()),
                result => result?,
            };
            let remote: StudyDetail = serde_json::from_value(live)
                .map_err(|_| "Некорректные данные обследования")?;
            if remote.study.status == "draft" && remote.revision_id.is_some() {
                return Ok(());
            }
            expected_version = remote.study.row_version;
        }
        // Cancellation and local outbox admission share one linearization point.
        // Once admitted, a frozen operation must never be removed by navigation.
        let admission = active.map(|active| active.lock().unwrap());
        if admission.as_deref() == Some(&false) {
            return Err("Открытие новой редакции отменено".into());
        }
        // Direct transports have no outbox: an HTTP request is admitted here.
        // Never hold the cancellation mutex through a blocking network exchange.
        let _admission = if connection.offline.is_some() { admission } else { drop(admission); None };
        let _: Value = connection.post(
            &format!("/api/v1/studies/{}/reopen", d.study.id),
            &ExpectedVersion { expected_version },
        )?;
        Ok(())
    }
    pub fn resolve(&mut self) -> Result<(), String> {
        let detail = self
            .detail
            .as_ref()
            .ok_or("Откройте конфликтующее исследование")?;
        if detail.study.status == "approved" {
            return Err("Открыт утверждённый протокол с сервера. Для продолжения работы с локальными правками сначала откройте новую редакцию".into());
        }
        let protocol = detail.protocol.as_ref().ok_or("Нет локального протокола")?;
        let dictionary = self.dictionary.as_ref().ok_or("Нет словаря протокола")?;
        check_unfinished_input(&self.journal)?;
        if !validate(protocol, dictionary, ValidationMode::Draft).is_empty() {
            return Err("Исправьте незавершённые поля перед отправкой объединённой версии".into());
        }
        self.connection()?
            .offline
            .as_ref()
            .ok_or("Нет локального журнала")?
            .resolve_protocol(detail)?;
        self.journal["revision_conflict"] = json!(false);
        self.detail = Some(
            self.connection()?
                .get(&format!("/api/v1/studies/{}", detail.study.id))?,
        );
        self.dirty = false;
        self.persist();
        Ok(())
    }
    pub fn ensure_asset(&self, id: Uuid) -> Result<Asset, String> {
        self.detail
            .as_ref()
            .and_then(|d| d.assets.iter().find(|a| a.id == id))
            .cloned()
            .ok_or("Файл не относится к открытому исследованию".into())
    }
    pub fn logout(&mut self) -> Result<(), String> {
        self.cancel_background_open();
        self.persist();
        if let Some(o) = self.connection()?.offline.as_ref() {
            o.shutdown()?;
        }
        let _: Result<Value, _> = self.connection()?.post("/api/v1/logout", &json!({}));
        *self = Self::default();
        Ok(())
    }
    /// Close the editor without ending the account session or dropping per-study drafts.
    pub fn close_study(&mut self) -> Result<(), String> {
        self.cancel_background_open();
        self.persist();
        let offline = self
            .connection()?
            .offline
            .as_ref()
            .ok_or("Нет защищенного локального кеша")?;
        if !offline.checkpoint_ready() {
            return Err("Локальная запись еще не завершена".into());
        }
        self.detail = None;
        self.dictionary = None;
        self.journal = json!({});
        self.dirty = false;
        self.changed = None;
        self.attempted = false;
        self.message.clear();
        self.persist();
        Ok(())
    }
}

/// Durable editor input can exist without an outbox operation (e.g. incomplete numbers).
pub(crate) fn journal_needs_upload(journal: &Value) -> bool {
    journal["dirty"] == true
        || journal["revision_conflict"] == true
        || journal["advanced_edited"] == true
        || journal["native_editor_rejected"].is_object()
        || journal["unfinished_fields"].as_object().is_some_and(|fields| !fields.is_empty())
        || journal["server_save_error"].as_str().is_some_and(|error| !error.is_empty())
}

pub fn render_report(
    connection: &Connection,
    detail: &StudyDetail,
    template: &Template,
    dictionary: &Dictionary,
    selected: &[Uuid],
) -> Result<Vec<u8>, String> {
    render_report_with_equipment(connection, detail, template, dictionary, selected, None)
}
pub fn render_report_with_equipment(
    connection: &Connection,
    detail: &StudyDetail,
    template: &Template,
    dictionary: &Dictionary,
    selected: &[Uuid],
    equipment: Option<&str>,
) -> Result<Vec<u8>, String> {
    render_report_cancellable(connection, detail, template, dictionary, selected, equipment, || false)
}
pub fn render_report_cancellable(
    connection: &Connection,
    detail: &StudyDetail,
    template: &Template,
    dictionary: &Dictionary,
    selected: &[Uuid],
    equipment: Option<&str>,
    cancelled: impl Fn() -> bool,
) -> Result<Vec<u8>, String> {
    connection.require_offline_access()?;
    let ensure_active = || {
        if cancelled() { Err("Подготовка отчета отменена".to_string()) }
        else { Ok(()) }
    };
    ensure_active()?;
    let trusted: Vec<Template> = connection.get("/api/v1/templates")?;
    if !trusted
        .iter()
        .any(|t| t.version_id == template.version_id && json!(t.layout) == json!(template.layout))
    {
        return Err("Сначала сохраните шаблон".into());
    }
    let (report_context, _) = crate::report_identity::load(connection, detail)?;
    let (mut report_detail,performer) = crate::report_identity::bind(detail,&report_context)?;
    let approved_selection = if detail.study.status == "approved" {
        crate::report_identity::approved_assets(&report_context, detail.study.id)?
    } else { None };
    let approved_ids;
    let selected = if let Some(assets) = approved_selection {
        approved_ids = assets.iter().map(|asset| asset.id).collect::<Vec<_>>();
        report_detail.assets = assets;
        approved_ids.as_slice()
    } else { selected };
    let equipment = crate::report_identity::equipment(&report_context, equipment).map(str::to_owned);
    let mut report_context = report_context;
    // Equipment has its own render-key field, including for older cached PDFs.
    if let Some(context) = report_context.as_object_mut() {
        context.remove("colposcope_model");
        context.remove("report_assets");
        context.remove("report_assets_source");
    }
    let detail = &report_detail;
    use sha2::{Digest, Sha256};
    let render_key=hex::encode(Sha256::digest(m53_contracts::canonical_json::to_vec(&json!({"detail":detail,"template":template,"selected":selected,"equipment":equipment,"report_context":report_context,"renderer":crate::report::RENDERER_ID})).map_err(|_|"Ошибка snapshot PDF")?));
    ensure_active()?;
    if let Some(offline) = &connection.offline {
        if let Some(bytes) = offline.cached_render(&render_key)? {
            ensure_active()?;
            offline.store_report(detail, template.version_id, crate::report::RENDERER_ID, &bytes)?;
            return Ok(bytes);
        }
    }
    let photos = crate::report_assets::load_until(connection, &detail.assets, selected, Instant::now() + Duration::from_secs(45), &cancelled)?;
    ensure_active()?;
    let rendered = crate::report::with_equipment(
        detail,
        &template.layout,
        &photos,
        &performer,
        &dictionary.terms,
        equipment.as_deref(),
        false,
    )?;
    ensure_active()?;
    if std::env::var("M53_LEGACY_LOGIN").as_deref() == Ok("1") {
        // Historical test backend only. Target API never receives permanent HTML/PDF assets.
    }
    if let Some(offline) = &connection.offline {
        ensure_active()?;
        offline.cache_render(&render_key, &rendered.pdf)?;
        ensure_active()?;
        offline.store_report(
            detail,
            template.version_id,
            crate::report::RENDERER_ID,
            &rendered.pdf,
        )?;
    }
    ensure_active()?;
    Ok(rendered.pdf)
}

/// Raw incomplete input remains in the journal and cannot silently commit an older numeric value.
pub fn check_unfinished_input(journal: &Value) -> Result<(), String> {
    if journal
        .get("unfinished_fields")
        .and_then(Value::as_object)
        .is_some_and(|v| !v.is_empty())
    {
        return Err(
            "Незавершенные числовые поля сохранены локально; заполните их перед отправкой".into(),
        );
    }
    if journal.get("advanced_edited").and_then(Value::as_bool) == Some(true) {
        return Err(
            "Незавершенная форма сохранена локально; примените ее в окне протокола перед отправкой"
                .into(),
        );
    }
    Ok(())
}

/// A cache miss/disk flush is transient; structural and setup errors require an explicit retry.
pub fn opening_pending(error: &str) -> bool {
    error.starts_with("Данные еще не в локальном кеше")
        || error.starts_with("Локальная запись еще не завершена")
        || error == "Ожидается подтверждение новой редакции"
}

#[cfg(test)]
mod opening_tests {
    #[test]
    fn only_loading_is_retried() {
        assert!(super::opening_pending(
            "Данные еще не в локальном кеше; загружаются в фоне"
        ));
        assert!(super::opening_pending("Локальная запись еще не завершена"));
        assert!(!super::opening_pending(
            "На сервере не подготовлен словарь обследования"
        ));
        assert!(!super::opening_pending(
            "Кеш: структура данных еще не готова"
        ));
    }
}

/// Server approval rules presented before a command enters the outbox.
pub fn approval_problems(
    protocol: &m53_domain::protocol::Protocol,
    dictionary: &Dictionary,
) -> Vec<String> {
    let mut problems = vec![];
    for e in validate(protocol, dictionary, ValidationMode::Approval) {
        let message=match e.code.as_str(){
 "dictionary_not_approved"=>"На сервере установлен неутвержденный draft-словарь. Требуется клиническое утверждение справочника ответственным специалистом".into(),
 "conclusion_required"=>"Заполните заключение врача".into(),
 "adequacy_required" | "required_colpo_field" | "transformation_zone_required"=>approval_error_message(protocol,&e),
 "annotation_requires_physician_confirmation"=>"Подтвердите находки, отмеченные на снимках".into(),
 "unexamined_section_has_observations"=>"В разделе «Не исследовано» есть находки".into(),
 _=>format!("Проверьте поле {}: {}",e.path,e.code)};
        if !problems.contains(&message) {
            problems.push(message);
        }
    }
    problems
}

#[cfg(test)]
mod approval_tests {
    #[test]
    fn delayed_approved_metadata_preserves_local_history_and_revision() {
        use super::*;
        let study=Uuid::new_v4();let patient=Uuid::new_v4();let revision=Uuid::new_v4();
        let protocol=crate::model::blank_protocol(Uuid::new_v4());
        let original:StudyDetail=serde_json::from_value(json!({"study":{"id":study,"patient_id":patient,
            "status":"draft","row_version":1,"study_instance_uid":"1.2.3"},"patient":{"id":patient,
            "family_name":"Synthetic","given_name":"Example","patronymic":null,"birth_date":null},
            "revision_id":revision,"protocol":protocol,"annotations":[],"assets":[]})).unwrap();
        let mut approved=original.clone();approved.study.status="approved".into();approved.study.row_version=2;
        let mut open_approved = approved.clone();
        let mut new_draft = original.clone();
        new_draft.study.row_version = 3;
        new_draft.revision_id = Some(Uuid::new_v4());
        refresh_detail_metadata(&mut open_approved, new_draft);
        assert_eq!(open_approved.study.status, "approved");
        assert_eq!(open_approved.revision_id, approved.revision_id);

        let mut same=original.clone();refresh_detail_metadata(&mut same,approved.clone());
        assert_eq!(same.study.status,"approved");
        let mut edited=original.clone();
        edited.protocol.as_mut().unwrap().history.value=m53_domain::protocol::Assessment::Assessed("New local history".into());
        let expected=json!(edited.protocol);refresh_detail_metadata(&mut edited,approved.clone());
        assert_eq!(edited.study.status,"draft");assert_eq!(json!(edited.protocol),expected);
        let mut newer=original;newer.revision_id=Some(Uuid::new_v4());let id=newer.revision_id;
        refresh_detail_metadata(&mut newer,approved);
        assert_eq!(newer.study.status,"draft");assert_eq!(newer.revision_id,id);
    }
    #[test]
    fn draft_dictionary_and_missing_conclusion_are_explained_before_queueing() {
        let dictionary = m53_domain::validation::Dictionary {
            version_id: uuid::Uuid::new_v4(),
            clinically_approved: false,
            terms: vec![],
        };
        let protocol = crate::model::blank_protocol(dictionary.version_id);
        let before = serde_json::to_value(&protocol).unwrap();
        let errors = super::approval_problems(&protocol, &dictionary);
        assert!(errors.iter().any(|e| e.contains("draft-словарь")));
        assert!(errors.iter().any(|e| e.contains("заключение")));
        assert_eq!(serde_json::to_value(protocol).unwrap(), before);
    }
}

/// Match legacy dotted paths and JSON Pointers to actual controls.
pub fn validation_pointer(path: &str) -> String {
    if path.starts_with('/') {
        return path.to_string();
    }
    if let Some(key) = path.strip_prefix("colpo.fields.") {
        return format!(
            "/colpo/fields/{}",
            key.replace('~', "~0").replace('/', "~1")
        );
    }
    format!(
        "/{}",
        path.replace('[', "/").replace(']', "").replace('.', "/")
    )
}
pub fn approval_error_message(
    protocol: &m53_domain::protocol::Protocol,
    error: &m53_domain::validation::ValidationError,
) -> String {
    let pointer = validation_pointer(&error.path);
    let region = pointer
        .strip_prefix("/sections/")
        .and_then(|s| s.split('/').next())
        .and_then(|s| s.parse::<usize>().ok())
        .and_then(|i| protocol.sections.get(i))
        .map(|s| match s.region {
            m53_domain::protocol::Region::Cervix => "шейки матки",
            m53_domain::protocol::Region::Vagina => "влагалища",
            m53_domain::protocol::Region::Vulva => "вульвы",
            m53_domain::protocol::Region::Anus => "анальной области",
        });
    match error.code.as_str() {
        "required_colpo_field" => {
            let path = validation_pointer(&error.path);
            let label = path
                .strip_prefix("/colpo/fields/")
                .and_then(|id| m53_domain::colpo::FIELDS.iter().find(|f| f.id == id))
                .map(|f| f.label)
                .unwrap_or("Обязательное поле кольпоскопии");
            format!("Заполните поле «{label}»")
        }
        "transformation_zone_required" => "Укажите тип зоны трансформации".into(),
        "adequacy_required" => format!(
            "Укажите адекватность оценки {}",
            region.unwrap_or("области обследования")
        ),
        "conclusion_required" => "Заполните заключение врача".into(),
        "inadequacy_reason_required" | "inadequate_reason_required" | "reason_required" => {
            "Укажите причину неадекватности оценки".into()
        }
        "annotation_requires_physician_confirmation" => "Подтвердите находку на снимке".into(),
        _ => "Заполните обязательное поле или исправьте его значение".into(),
    }
}
pub fn approval_field_problem(
    protocol: &m53_domain::protocol::Protocol,
    dictionary: &Dictionary,
    path: &str,
) -> Option<String> {
    validate(protocol, dictionary, ValidationMode::Approval)
        .into_iter()
        .find(|error| {
            let target = validation_pointer(&error.path);
            path == target || path.starts_with(&format!("{target}/"))
        })
        .map(|error| approval_error_message(protocol, &error))
}

#[cfg(test)]
mod required_field_tests {
    use super::*;
    use m53_domain::{ExaminationState, protocol::*};
    #[test]
    fn required_control_highlights_and_clears_without_changing_optional_fields() {
        let dictionary = Dictionary {
            version_id: uuid::Uuid::new_v4(),
            clinically_approved: true,
            terms: vec![],
        };
        let mut protocol = crate::model::blank_protocol(dictionary.version_id);
        protocol.sections[0].state = crate::model::field(ExaminationState::Examined);
        let before = json!(protocol);
        assert!(
            approval_field_problem(&protocol, &dictionary, "/sections/0/adequacy/value")
                .unwrap()
                .contains("шейки матки")
        );
        assert!(approval_field_problem(&protocol, &dictionary, "/conclusion/value").is_some());
        assert!(
            approval_field_problem(&protocol, &dictionary, "/colpo/fields/context.date").is_none()
        );
        assert_eq!(json!(protocol), before);
        protocol.sections[0].adequacy =
            crate::model::field(Assessment::Assessed(Adequacy::Adequate));
        assert!(
            approval_field_problem(&protocol, &dictionary, "/sections/0/adequacy/value").is_none()
        );
        protocol.conclusion = crate::model::field(Assessment::Assessed("Synthetic".into()));
        assert!(approval_field_problem(&protocol, &dictionary, "/conclusion/value").is_none());
    }
    #[test]
    fn literal_colpo_keys_keep_their_dot_and_use_field_names() {
        let protocol = crate::model::blank_protocol(uuid::Uuid::new_v4());
        for id in [
            "cervix.shape",
            "cervix.os",
            "cervix.surface",
            "cervix.color",
            "simple.vessels",
        ] {
            let pointer = format!("/colpo/fields/{id}");
            assert_eq!(validation_pointer(&pointer), pointer);
            assert_eq!(validation_pointer(&format!("colpo.fields.{id}")), pointer);
            let error = m53_domain::validation::ValidationError {
                path: pointer,
                code: "required_colpo_field".into(),
            };
            let label = m53_domain::colpo::FIELDS
                .iter()
                .find(|f| f.id == id)
                .unwrap()
                .label;
            let message = approval_error_message(&protocol, &error);
            assert!(message.contains(label));
            assert!(!message.contains("colpo.fields"));
        }
    }
    #[test]
    fn region_labels_use_region_identity_even_when_sections_are_reordered() {
        let dictionary = Dictionary {
            version_id: uuid::Uuid::new_v4(),
            clinically_approved: true,
            terms: vec![],
        };
        let mut protocol = crate::model::blank_protocol(dictionary.version_id);
        protocol.sections.swap(0, 2);
        protocol.sections[0].state = crate::model::field(ExaminationState::Examined);
        assert!(
            approval_field_problem(&protocol, &dictionary, "/sections/0/adequacy/value")
                .unwrap()
                .contains("вульвы")
        );
        assert_eq!(
            validation_pointer("sections[2].adequacy"),
            "/sections/2/adequacy"
        );
    }
}


/// A reachable API alone never confirms a saved draft. Pending operations remain
/// queued until their matching ACK; dirty editor data have not been queued yet.
fn server_save_status(open: bool, dirty: bool, durable: bool, online: bool, current: usize, total: usize, conflict: bool, error: bool) -> String {
    if open {
        if conflict { return "Не сохранено на сервере: требуется устранить конфликт".into(); }
        if dirty || !durable { return "Изменения ещё не переданы на сервер".into(); }
        if current > 0 {
            let reason = if !online { "нет связи" } else if error { "ошибка передачи" } else { "ожидается подтверждение" };
            return format!("Ожидает сохранения на сервере: {current} операций · {reason}");
        }
        "Сохранено на сервере".into()
    } else if total > 0 {
        let reason = if !online { "нет связи" } else if error { "есть ошибка передачи" } else { "передача в фоне" };
        format!("Ожидает подтверждения сервера: {total} операций · {reason}")
    } else {
        "Очередь передачи на сервер пуста".into()
    }
}

#[cfg(test)]
mod server_save_status_tests {
    use super::server_save_status as status;
    #[test]
    fn failed_save_retains_actionable_error_in_durable_journal() {
        let mut workspace = super::Workspace::default();
        workspace.dirty = true;
        workspace.journal = serde_json::json!({"unfinished_fields":{"/synthetic":"-"}});
        let error = workspace.save().unwrap_err();
        assert_eq!(workspace.journal["server_save_error"], error);
        assert_eq!(workspace.message, error);
        assert!(workspace.dirty);
    }
    #[test]
    fn pending_zero_does_not_hide_incomplete_or_rejected_editor_input() {
        use serde_json::json;
        for journal in [json!({"dirty":true}), json!({"unfinished_fields":{"/synthetic":"-"}}),
            json!({"advanced_edited":true}), json!({"server_save_error":"synthetic rejected"})] {
            assert!(super::journal_needs_upload(&journal));
        }
        assert!(!super::journal_needs_upload(&json!({"dirty":false,"unfinished_fields":{},"server_save_error":null})));
    }
    #[test]
    fn reachable_api_is_not_an_ack_and_dirty_editor_is_not_saved() {
        assert!(status(true,false,true,true,2,2,false,false).starts_with("Ожидает"));
        assert!(status(true,true,true,true,0,0,false,false).starts_with("Изменения"));
        assert!(status(true,false,false,true,0,0,false,false).starts_with("Изменения"));
        assert!(status(true,false,true,true,0,0,true,false).contains("конфликт"));
    }
    #[test]
    fn acknowledged_study_is_independent_of_other_queues_and_connection() {
        for online in [false,true] {
            assert_eq!(status(true,false,true,online,0,5,false,true),"Сохранено на сервере");
        }
        assert!(status(false,false,true,true,0,5,false,true).contains("5 операций"));
        assert_eq!(status(false,false,true,true,0,0,false,false),"Очередь передачи на сервер пуста");
        assert!(status(true,false,true,false,1,1,false,false).contains("нет связи"));
        assert!(status(true,false,true,true,1,1,false,true).contains("ошибка передачи"));
    }
}
