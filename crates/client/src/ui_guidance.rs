//! Local interface help. Never controls clinical permissions, validation or network work.
use crate::{cache::Store, connection::Connection};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use uuid::Uuid;

const SCHEMA_VERSION: u32 = 1;

/// Non-secret identity retained only after successful verification of signed claims.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    pub organization_id: Uuid,
    pub user_id: Uuid,
    pub doctor_id: Uuid,
    pub workstation_id: Uuid,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub server_namespace: String,
    pub organization_id: Uuid,
    pub user_id: Uuid,
    pub workstation_id: Uuid,
}

impl Scope {
    fn from_connection(connection: &Connection) -> Result<(Self, Uuid), String> {
        let identity = connection
            .identity
            .as_ref()
            .ok_or("Не подтверждены врач и рабочее место для сохранения подсказок")?;
        if connection.user_id != Some(identity.user_id) {
            return Err("Настройки подсказок относятся к другому врачу".into());
        }
        let scope = Self {
            server_namespace: connection.storage_base.clone(),
            organization_id: identity.organization_id,
            user_id: identity.user_id,
            workstation_id: identity.workstation_id,
        };
        scope.validate(identity.doctor_id)?;
        Ok((scope, identity.doctor_id))
    }

    fn validate(&self, doctor_id: Uuid) -> Result<(), String> {
        if self.server_namespace.is_empty()
            || self.server_namespace.len() > 2048
            || [
                self.organization_id,
                self.user_id,
                self.workstation_id,
                doctor_id,
            ]
            .iter()
            .any(Uuid::is_nil)
        {
            return Err("Не подтверждены врач и рабочее место для сохранения подсказок".into());
        }
        Ok(())
    }

    fn storage_key(&self) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(
            serde_json::to_vec(self).expect("serializable guidance scope"),
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Patients,
    Study,
    Protocol,
    Report,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TipId {
    PatientsTask,
    StudyCapture,
    StudyPhotoSelection,
    StudyMarkup,
    StudyToProtocol,
    ProtocolSections,
    ProtocolPhrases,
    ProtocolRequired,
    ReportApproval,
    ReportDelivery,
}
impl TipId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PatientsTask => "patients.task.v1",
            Self::StudyCapture => "study.capture.v1",
            Self::StudyPhotoSelection => "study.photo_selection.v1",
            Self::StudyMarkup => "study.markup.v1",
            Self::StudyToProtocol => "study.to_protocol.v1",
            Self::ProtocolSections => "protocol.sections.v1",
            Self::ProtocolPhrases => "protocol.phrases.v1",
            Self::ProtocolRequired => "protocol.required.v1",
            Self::ReportApproval => "report.approval.v1",
            Self::ReportDelivery => "report.delivery.v1",
        }
    }
}
#[derive(Clone, Copy)]
pub struct Tip {
    pub id: TipId,
    pub stage: Stage,
    pub title: &'static str,
    pub body: &'static str,
}
pub const TIPS: &[Tip] = &[
    Tip {
        id: TipId::PatientsTask,
        stage: Stage::Patients,
        title: "Задание из 1С",
        body: "Выберите поступившее задание и откройте его штатной кнопкой. Создавать вместо него новый ручной приём не требуется.",
    },
    Tip {
        id: TipId::StudyCapture,
        stage: Stage::Study,
        title: "Снимок обследования",
        body: "Включите живое видео и сделайте снимок кнопкой или настроенной педалью. Готовые снимки также можно импортировать.",
    },
    Tip {
        id: TipId::StudyPhotoSelection,
        stage: Stage::Study,
        title: "Сохранённые снимки",
        body: "Нажмите миниатюру, чтобы открыть кадр. Флажок «В отчёт» определяет, какие снимки войдут в документ.",
    },
    Tip {
        id: TipId::StudyMarkup,
        stage: Stage::Study,
        title: "Разметка снимка",
        body: "Выберите инструмент, поставьте метку на сохранённом кадре и настройте её свойства. Используйте отмену для исправления действия.",
    },
    Tip {
        id: TipId::StudyToProtocol,
        stage: Stage::Study,
        title: "Далее — протокол",
        body: "При включённой камере «Провести осмотр» останавливает видео и возвращает к сохранённым снимкам. Затем «Заполнить протокол» сохраняет этап и открывает протокол.",
    },
    Tip {
        id: TipId::ProtocolSections,
        stage: Stage::Protocol,
        title: "Разделы протокола",
        body: "Раздел раскрывается по нажатию заголовка. Сворачивание сохраняет введённые значения.",
    },
    Tip {
        id: TipId::ProtocolPhrases,
        stage: Stage::Protocol,
        title: "Библиотека фраз",
        body: "При вводе в текстовое поле справа доступны личные и общие фразы. Перед вставкой выберите добавление или замену текста.",
    },
    Tip {
        id: TipId::ProtocolRequired,
        stage: Stage::Protocol,
        title: "Проверка перед утверждением",
        body: "Обязательные поля выделены рамкой с пояснением. Исправьте отмеченные значения перед утверждением; черновик сохраняется и без них.",
    },
    Tip {
        id: TipId::ReportApproval,
        stage: Stage::Report,
        title: "Утверждение ответа",
        body: "Просмотреть отчёт можно до утверждения. PDF и печать доступны после подтверждённого сервером утверждения ответа.",
    },
    Tip {
        id: TipId::ReportDelivery,
        stage: Stage::Report,
        title: "Печать и передача",
        body: "Печать и «Отправить в МИС» — отдельные действия. Постановка в очередь ещё не подтверждает получение результата в 1С; следите за статусом доставки.",
    },
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub schema_version: u32,
    pub enabled: bool,
    pub completed_tip_ids: BTreeSet<String>,
    pub dismissed_tip_ids: BTreeSet<String>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            enabled: true,
            completed_tip_ids: BTreeSet::new(),
            dismissed_tip_ids: BTreeSet::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextToken {
    pub scope: Scope,
    pub epoch: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    SetEnabled(bool),
    Complete(TipId),
    Dismiss(TipId),
    ResetProgress,
}
#[derive(Clone, Debug)]
pub struct Request {
    pub context: ContextToken,
    pub action: Action,
}
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub epoch: u64,
    pub context: Option<ContextToken>,
    pub preferences: Preferences,
    pub writable: bool,
    pub error: String,
}
impl Snapshot {
    pub fn request(&self, action: Action) -> Option<Request> {
        self.writable
            .then(|| self.context.clone())
            .flatten()
            .map(|context| Request { context, action })
    }
    pub fn should_show(&self, id: TipId) -> bool {
        self.writable
            && self.context.is_some()
            && self.preferences.enabled
            && !self.preferences.completed_tip_ids.contains(id.as_str())
            && !self.preferences.dismissed_tip_ids.contains(id.as_str())
    }
    /// A new login advances the epoch. Within one epoch, the full context must match.
    pub fn accepts(&self, incoming: &Self) -> bool {
        incoming
            .context
            .as_ref()
            .is_none_or(|context| context.epoch == incoming.epoch)
            && (incoming.epoch > self.epoch
                || (incoming.epoch == self.epoch && incoming.context == self.context))
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    scope: Scope,
    doctor_id: Uuid,
    preferences: Preferences,
}
struct Repository {
    store: Store,
    envelope: Envelope,
}
impl Repository {
    fn load(store: Store, scope: Scope, doctor_id: Uuid) -> Result<Self, String> {
        scope.validate(doctor_id)?;
        let envelope = match store.read("preferences")? {
            Some(bytes) => {
                let envelope: Envelope = serde_json::from_slice(&bytes)
                    .map_err(|_| "Настройки подсказок повреждены; исходный файл сохранён")?;
                if envelope.scope != scope || envelope.doctor_id != doctor_id {
                    return Err("Настройки подсказок принадлежат другому врачу, рабочему месту или серверу; исходный файл сохранён".into());
                }
                validate_preferences(&envelope.preferences)?;
                envelope
            }
            None => Envelope {
                scope,
                doctor_id,
                preferences: Preferences::default(),
            },
        };
        Ok(Self { store, envelope })
    }
    fn change(&mut self, action: Action) -> Result<(), String> {
        let mut next = self.envelope.clone();
        match action {
            Action::SetEnabled(enabled) => next.preferences.enabled = enabled,
            Action::Complete(id) => {
                next.preferences
                    .completed_tip_ids
                    .insert(id.as_str().into());
            }
            Action::Dismiss(id) => {
                next.preferences
                    .dismissed_tip_ids
                    .insert(id.as_str().into());
            }
            Action::ResetProgress => {
                next.preferences.completed_tip_ids.clear();
                next.preferences.dismissed_tip_ids.clear();
            }
        }
        if next.preferences == self.envelope.preferences {
            return Ok(());
        }
        validate_preferences(&next.preferences)?;
        let bytes =
            serde_json::to_vec(&next).map_err(|_| "Не удалось подготовить настройки подсказок")?;
        self.store.write("preferences", &bytes)?;
        self.envelope = next;
        Ok(())
    }
}
fn validate_preferences(preferences: &Preferences) -> Result<(), String> {
    if preferences.schema_version != SCHEMA_VERSION {
        return Err(
            "Эта версия настроек подсказок не поддерживается; исходный файл сохранён".into(),
        );
    }
    for ids in [
        &preferences.completed_tip_ids,
        &preferences.dismissed_tip_ids,
    ] {
        if ids.len() > 256
            || ids.iter().any(|id| {
                id.is_empty()
                    || id.len() > 128
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            })
        {
            return Err(
                "Настройки подсказок содержат недопустимые идентификаторы; исходный файл сохранён"
                    .into(),
            );
        }
    }
    Ok(())
}

fn open_store(scope: &Scope) -> Result<Store, String> {
    #[cfg(windows)]
    {
        let root = std::path::PathBuf::from(
            std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA недоступен")?,
        )
        .join(crate::APP_DATA_DIR)
        .join("ui-guidance")
        .join(scope.storage_key());
        Store::new(root, std::sync::Arc::new(crate::cache::Dpapi))
    }
    #[cfg(not(windows))]
    {
        let _ = scope;
        Err("Настройки подсказок сохраняются в защищённом профиле Windows".into())
    }
}

/// Owned by the clinical bridge worker; no I/O is performed by snapshots or GPUI renders.
#[derive(Default)]
pub struct Session {
    epoch: u64,
    active: Option<Repository>,
    context: Option<ContextToken>,
    error: String,
}
impl Session {
    pub fn attach(&mut self, connection: &Connection) -> Snapshot {
        self.advance();
        match Scope::from_connection(connection) {
            Ok((scope, doctor_id)) => {
                self.context = Some(ContextToken {
                    scope: scope.clone(),
                    epoch: self.epoch,
                });
                // Reauthentication to the same scope reuses its already-open Store lock.
                if !self.active.as_ref().is_some_and(|repo| {
                    repo.envelope.scope == scope && repo.envelope.doctor_id == doctor_id
                }) {
                    self.active = None;
                    self.install(
                        open_store(&scope)
                            .and_then(|store| Repository::load(store, scope, doctor_id)),
                    );
                }
            }
            Err(error) => {
                self.active = None;
                self.context = None;
                self.error = error;
            }
        }
        self.snapshot()
    }
    pub fn detach(&mut self) -> Snapshot {
        self.advance();
        self.active = None;
        self.context = None;
        self.snapshot()
    }
    /// Stale UI requests are harmless. Persistence errors stay in this independent channel.
    pub fn apply(&mut self, request: Request) -> Snapshot {
        if self.context.as_ref() != Some(&request.context) {
            return self.snapshot();
        }
        if let Some(repository) = self.active.as_mut() {
            self.error = match repository.change(request.action) {
                Ok(()) => String::new(),
                Err(error) => format!("Не удалось запомнить выбор подсказок: {error}"),
            };
        }
        self.snapshot()
    }
    fn advance(&mut self) {
        self.epoch = self.epoch.checked_add(1).expect("guidance epoch exhausted");
        self.error.clear();
    }
    fn install(&mut self, opened: Result<Repository, String>) {
        match opened {
            Ok(repository) => self.active = Some(repository),
            Err(error) => {
                self.active = None;
                self.error = error;
            }
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            epoch: self.epoch,
            context: self.context.clone(),
            preferences: self
                .active
                .as_ref()
                .map(|r| r.envelope.preferences.clone())
                .unwrap_or_default(),
            writable: self.active.is_some(),
            error: self.error.clone(),
        }
    }
    #[cfg(test)]
    fn attach_store(&mut self, store: Store, scope: Scope, doctor_id: Uuid) -> Snapshot {
        self.advance();
        self.active = None;
        self.install(Repository::load(store, scope.clone(), doctor_id));
        self.context = Some(ContextToken {
            scope,
            epoch: self.epoch,
        });
        self.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{AesProtector, Protector};
    use std::{
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("m53-guidance-test-{}", Uuid::new_v4())))
        }
        fn store(&self, scope: &Scope) -> Store {
            Store::new(
                self.0.join(scope.storage_key()),
                Arc::new(AesProtector([53; 32])),
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn scope(user: u128, place: u128) -> Scope {
        Scope {
            server_namespace: "https://synthetic.invalid".into(),
            organization_id: Uuid::from_u128(1),
            user_id: Uuid::from_u128(user),
            workstation_id: Uuid::from_u128(place),
        }
    }
    fn doctor() -> Uuid {
        Uuid::from_u128(20)
    }

    #[test]
    fn physician_workstation_org_and_server_are_separate_and_persisted() {
        let f = Fixture::new();
        let a1 = scope(2, 3);
        let mut repo = Repository::load(f.store(&a1), a1.clone(), doctor()).unwrap();
        repo.change(Action::SetEnabled(false)).unwrap();
        drop(repo);
        let repo = Repository::load(f.store(&a1), a1.clone(), doctor()).unwrap();
        assert!(!repo.envelope.preferences.enabled);
        drop(repo);
        let mut other_org = a1.clone();
        other_org.organization_id = Uuid::from_u128(10);
        let mut other_server = a1.clone();
        other_server.server_namespace = "https://other.invalid".into();
        for separate in [scope(4, 3), scope(2, 5), other_org, other_server] {
            let repo = Repository::load(f.store(&separate), separate, doctor()).unwrap();
            assert_eq!(repo.envelope.preferences, Preferences::default());
        }
    }

    #[test]
    fn completion_dismissal_and_explicit_reset_survive_off_on_and_reopen() {
        let f = Fixture::new();
        let s = scope(2, 3);
        let mut session = Session::default();
        let mut snap = session.attach_store(f.store(&s), s.clone(), doctor());
        assert!(snap.should_show(TipId::StudyCapture));
        for action in [
            Action::Complete(TipId::StudyCapture),
            Action::Dismiss(TipId::ProtocolSections),
            Action::SetEnabled(false),
            Action::SetEnabled(true),
        ] {
            snap = session.apply(snap.request(action).unwrap());
        }
        assert!(!snap.should_show(TipId::StudyCapture));
        assert!(!snap.should_show(TipId::ProtocolSections));
        drop(session);
        let mut session = Session::default();
        snap = session.attach_store(f.store(&s), s, doctor());
        assert!(!snap.should_show(TipId::StudyCapture));
        snap = session.apply(snap.request(Action::ResetProgress).unwrap());
        assert!(snap.should_show(TipId::StudyCapture));
        assert!(snap.should_show(TipId::ProtocolSections));
    }

    #[test]
    fn corrupt_foreign_and_future_schema_files_are_preserved_without_default_write() {
        let f = Fixture::new();
        let s = scope(2, 3);
        let mut future = Preferences::default();
        future.schema_version = 99;
        let samples = [
            b"not json".to_vec(),
            serde_json::to_vec(&Envelope {
                scope: scope(4, 3),
                doctor_id: doctor(),
                preferences: Preferences::default(),
            })
            .unwrap(),
            serde_json::to_vec(&Envelope {
                scope: s.clone(),
                doctor_id: Uuid::new_v4(),
                preferences: Preferences::default(),
            })
            .unwrap(),
            serde_json::to_vec(&Envelope {
                scope: s.clone(),
                doctor_id: doctor(),
                preferences: future,
            })
            .unwrap(),
        ];
        for bytes in samples {
            let store = f.store(&s);
            store.write("preferences", &bytes).unwrap();
            let original = std::fs::read(store.root().join("preferences.dpapi")).unwrap();
            assert!(Repository::load(store.clone(), s.clone(), doctor()).is_err());
            assert_eq!(
                std::fs::read(store.root().join("preferences.dpapi")).unwrap(),
                original
            );
        }
    }

    struct Failable {
        failed: Arc<AtomicBool>,
    }
    impl Protector for Failable {
        fn protect(&self, b: &[u8]) -> Result<Vec<u8>, String> {
            if self.failed.load(Ordering::SeqCst) {
                Err("synthetic disk failure".into())
            } else {
                AesProtector([53; 32]).protect(b)
            }
        }
        fn unprotect(&self, b: &[u8]) -> Result<Vec<u8>, String> {
            AesProtector([53; 32]).unprotect(b)
        }
    }
    #[test]
    fn failed_write_keeps_last_durable_preferences_and_reports_only_guidance_failure() {
        let f = Fixture::new();
        let s = scope(2, 3);
        let failed = Arc::new(AtomicBool::new(false));
        let store = Store::new(
            f.0.join(s.storage_key()),
            Arc::new(Failable {
                failed: failed.clone(),
            }),
        )
        .unwrap();
        let mut session = Session::default();
        let snap = session.attach_store(store, s.clone(), doctor());
        let snap = session.apply(snap.request(Action::Dismiss(TipId::StudyCapture)).unwrap());
        failed.store(true, Ordering::SeqCst);
        let snap = session.apply(snap.request(Action::SetEnabled(false)).unwrap());
        assert!(snap.preferences.enabled);
        assert!(!snap.error.is_empty());
        drop(session);
        let repo = Repository::load(f.store(&s), s, doctor()).unwrap();
        assert!(repo.envelope.preferences.enabled);
        assert!(
            repo.envelope
                .preferences
                .dismissed_tip_ids
                .contains(TipId::StudyCapture.as_str())
        );
    }

    #[test]
    fn stale_command_and_snapshot_cannot_change_another_login_even_for_same_scope() {
        let f = Fixture::new();
        let a = scope(2, 3);
        let b = scope(4, 3);
        let mut session = Session::default();
        let a_snap = session.attach_store(f.store(&a), a.clone(), doctor());
        let old = a_snap.request(Action::SetEnabled(false)).unwrap();
        let detached = session.detach();
        assert!(!detached.accepts(&a_snap));
        let b_snap = session.attach_store(f.store(&b), b, doctor());
        let mut wrong_context = b_snap.clone();
        wrong_context.context.as_mut().unwrap().scope = a.clone();
        assert!(!b_snap.accepts(&wrong_context));
        let after = session.apply(old);
        assert_eq!(after, b_snap);
        assert!(!after.accepts(&a_snap));
        let old_b = after.request(Action::SetEnabled(false)).unwrap();
        session.detach();
        let fresh = session.attach_store(f.store(&a), a, doctor());
        assert!(!fresh.accepts(&a_snap));
        assert_eq!(session.apply(old_b), fresh);
    }

    #[test]
    fn invalid_load_is_unwritable_and_cannot_be_reset_over_the_original_file() {
        let f = Fixture::new();
        let s = scope(2, 3);
        let store = f.store(&s);
        store.write("preferences", b"broken").unwrap();
        let mut session = Session::default();
        let snap = session.attach_store(store.clone(), s, doctor());
        assert!(!snap.writable);
        assert!(!snap.error.is_empty());
        assert!(snap.request(Action::ResetProgress).is_none());
        assert!(!snap.should_show(TipId::StudyCapture));
        assert_eq!(store.read("preferences").unwrap().unwrap(), b"broken");
    }

    #[test]
    fn unknown_future_tip_ids_are_retained_during_known_changes() {
        let f = Fixture::new();
        let s = scope(2, 3);
        let store = f.store(&s);
        let mut prefs = Preferences::default();
        prefs.dismissed_tip_ids.insert("future.action.v2".into());
        store
            .write(
                "preferences",
                &serde_json::to_vec(&Envelope {
                    scope: s.clone(),
                    doctor_id: doctor(),
                    preferences: prefs,
                })
                .unwrap(),
            )
            .unwrap();
        let mut repo = Repository::load(store, s, doctor()).unwrap();
        repo.change(Action::SetEnabled(false)).unwrap();
        assert!(
            repo.envelope
                .preferences
                .dismissed_tip_ids
                .contains("future.action.v2")
        );
    }

    #[test]
    fn store_lock_excludes_second_writer_for_same_pair() {
        let f = Fixture::new();
        let s = scope(2, 3);
        let store = f.store(&s);
        assert!(Store::new(store.root().to_path_buf(), Arc::new(AesProtector([53; 32]))).is_err());
    }

    #[test]
    fn guidance_off_does_not_change_clinical_errors_or_thirty_day_deadline() {
        let f = Fixture::new();
        let s = scope(2, 3);
        let mut session = Session::default();
        let snap = session.attach_store(f.store(&s), s, doctor());
        let mut connection = Connection::new("https://synthetic.invalid", "synthetic").unwrap();
        connection.offline_deadline =
            Some((crate::one_c_auth::now() - 1, std::time::Instant::now()));
        let before = connection.require_offline_access().unwrap_err();
        let snap = session.apply(snap.request(Action::SetEnabled(false)).unwrap());
        assert!(!snap.should_show(TipId::ReportApproval));
        assert_eq!(connection.require_offline_access().unwrap_err(), before);
        assert_eq!(
            m53_contracts::access::OFFLINE_GRANT_SECONDS,
            30 * 24 * 60 * 60
        );
    }

    #[test]
    fn verified_identity_uses_stable_storage_namespace_and_not_rotating_credential() {
        let mut connection = Connection::new("https://synthetic.invalid", "synthetic").unwrap();
        assert!(Scope::from_connection(&connection).is_err());
        connection.user_id = Some(Uuid::from_u128(2));
        connection.identity = Some(Identity {
            organization_id: Uuid::from_u128(1),
            user_id: Uuid::from_u128(2),
            doctor_id: doctor(),
            workstation_id: Uuid::from_u128(3),
        });
        connection.storage_base = "http://synthetic-old.invalid".into();
        let (scope, _) = Scope::from_connection(&connection).unwrap();
        connection.key = "rotated synthetic".into();
        connection.session = "new synthetic session".into();
        connection.doctor = "Renamed".into();
        connection.base = "https://synthetic-upgraded.invalid".into();
        assert_eq!(Scope::from_connection(&connection).unwrap().0, scope);
        assert_eq!(scope.server_namespace, "http://synthetic-old.invalid");
        connection.user_id = Some(Uuid::new_v4());
        assert!(Scope::from_connection(&connection).is_err());
    }

    #[test]
    fn claims_identity_and_reauthentication_reuse_store_without_extending_deadline() {
        use m53_contracts::access::*;
        let f = Fixture::new();
        let s = scope(2, 3);
        let mut session = Session::default();
        let snap = session.attach_store(f.store(&s), s.clone(), doctor());
        let snap = session.apply(snap.request(Action::SetEnabled(false)).unwrap());
        let stale = snap.request(Action::SetEnabled(true)).unwrap();
        let now = crate::one_c_auth::now();
        let mut connection = Connection::new(&s.server_namespace, "synthetic").unwrap();
        connection
            .apply_offline_claims(OfflineClaims {
                auth_provider: "1c_web".into(),
                external_user_guid: Some(Uuid::from_u128(40)),
                schema_version: 1,
                iss: OFFLINE_ISSUER.into(),
                aud: OFFLINE_AUDIENCE.into(),
                purpose: OFFLINE_PURPOSE.into(),
                jti: Uuid::new_v4(),
                sub: s.user_id,
                organization_id: s.organization_id,
                doctor_id: doctor(),
                workstation_id: s.workstation_id,
                credential_id: Uuid::new_v4(),
                auth_version: 1,
                display_name: "Synthetic".into(),
                external_doctor_ids: vec![],
                roles: vec!["doctor".into()],
                permissions: OfflinePermissions {
                    allow_new_manual_encounter: true,
                    allow_continue_open_encounter: true,
                    allow_local_print: true,
                },
                iat: now,
                nbf: now,
                exp: now + OFFLINE_GRANT_SECONDS,
            })
            .unwrap();
        connection.user_id = Some(s.user_id);
        let deadline = connection.offline_deadline;
        assert_eq!(Scope::from_connection(&connection).unwrap(), (s, doctor()));
        let reconnected = session.attach(&connection);
        assert!(reconnected.writable); // Reuses synthetic Store; never opens the real Windows profile.
        assert!(!reconnected.preferences.enabled);
        assert!(!reconnected.accepts(&snap));
        assert_eq!(session.apply(stale), reconnected);
        assert_eq!(connection.offline_deadline, deadline);
        assert_eq!(connection.clone().offline_deadline, deadline);
    }
}
