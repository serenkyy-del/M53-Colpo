//! The clinical worker owns Workspace. GPUI never performs network or disk I/O in render callbacks.
use gpui_kit::RenderImage;
use m53_client::activity::Action as Audit;
use m53_client::care_path::{self, Action as WorkflowAction, Status as WorkflowStatus};
use m53_client::{connection::Connection, patient_demographics::PatientSex, workspace::Workspace};
use m53_contracts::workflow::*;
use m53_domain::validation::Dictionary;
use m53_gpui_editor::{Session, integration};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, mpsc},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use uuid::Uuid;
#[path = "approval_flow.rs"]
mod approval_flow;
use approval_flow::{Notice as ApprovalNotice, Pending as PendingApproval, Step as ApprovalStep};

#[cfg(windows)]
pub use m53_client::offline_profile::LoginProfile;
// Headless Linux render fixture has no DPAPI/authentication implementation.
#[cfg(not(windows))]
#[derive(Clone)]
pub struct LoginProfile {
    pub login:String,
    pub display_name:String,
    pub target:Option<m53_client::one_c_auth::Profile>,
}
#[path = "state_publisher.rs"]
mod state_publisher;

pub enum ReportOutput {
    Export(PathBuf),
    Print,
}
pub enum ReportCompletion {
    Exported(PathBuf),
    Printed(bool),
    Enqueued,
}

fn verify_output_identity(revision: Option<Uuid>, template: Uuid, bytes: &[u8], expected: &(Uuid, Uuid, String)) -> Result<(), String> {
    if revision == Some(expected.0) && template == expected.1 && m53_client::delivery::pdf_hash(bytes) == expected.2 { Ok(()) }
    else { Err("Отчёт изменился. Обновите предпросмотр перед выводом".into()) }
}

pub enum Command {
    VisitTemplatesList { request: Uuid, after: Option<Uuid> },
    VisitTemplateGet { request: Uuid, id: Uuid },
    VisitTemplateSave { request: Uuid, id: Uuid, expected_version: Option<i64>, title: String, protocol: m53_domain::visit_template::VisitTemplate },
    Registry(crate::registry_browser::Request),
    Activity(m53_client::activity::Action),
    CloseAudit,
    Guidance(m53_client::ui_guidance::Request),
    SaveDiagnosisPriority { request: Uuid, base: m53_client::diagnosis_priority::Group, ids: Vec<Uuid> },
    RefreshDiagnosisPriority,
    PhraseField(String),
    PhraseSave{id:Option<Uuid>,field_code:String,scope:String,title:String,body:String},
    PhrasePreferences{replace:bool,include_shared:bool},
    PhraseResolve{operation:Uuid,keep_local:bool},
    Colposcope(Option<String>),
    LoginProfiles(String, String),
    Login {
        url: String,
        key: String,
        login: String,
        password: String,
    },
    Refresh,
    ClaimTask(Uuid, i64),
    SendResult {
        request: Uuid,
        study: Uuid,
        selected: Vec<Uuid>,
        template: Template,
        expected: (Uuid, Uuid, String),
    },
    Open(Uuid),
    PhotoDemand { study: Uuid, selection: crate::photo_loader::Selection },
    NewPatient(NewPatient, Option<PatientSex>),
    CheckNewPatient { request: Uuid, input: NewPatient },
    UseMatchedPatient { patient: Patient, query: String },
    ManualPatient {request:Uuid,patient:Uuid,body:Option<Value>},
    ProtocolHistory {request:Uuid,study:Uuid,revision:Option<Uuid>,before:Option<i64>},
    #[cfg(windows)]
    HistoricalPdf {request:Uuid,study:Uuid,revision:Uuid,document:Value,dictionary:Dictionary,template:Template},
    #[cfg(windows)]
    HistoricalOutput {request:Uuid,study:Uuid,bytes:Arc<Vec<u8>>,output:ReportOutput},
    NewStudy(Uuid),
    Workflow {
        study: Uuid,
        request: Uuid,
        action: WorkflowAction,
        close_preview: bool,
    },
    Editor {
        study: Uuid,
        version: u64,
        session: Session,
    },
    Form {
        study: Uuid,
        version: u64,
        value: Value,
        base: Value,
        unfinished: BTreeMap<String, String>,
    },
    Capture {
        study: Uuid,
        bytes: Vec<u8>,
    },
    Import {
        study: Uuid,
        paths: Vec<PathBuf>,
    },
    Save,
    ConfirmManualIdentity,
    Approve,
    Reopen,
    Resolve,
    Preview {
        study: Uuid,
        request: Uuid,
        selected: Vec<Uuid>,
        template: Template,
    },
    CancelPreview(Uuid),
    Pdf {
        request: Uuid,
        study: Uuid,
        selected: Vec<Uuid>,
        template: Template,
        expected: Option<(Uuid, Uuid, String)>,
        output: ReportOutput,
    },
    ExportPhotos {
        study: Uuid,
        photos: Vec<(m53_gpui_editor::Photo, PathBuf)>,
    },
    Logout,
}
pub enum Event {
    NewPatientMatches { request: Uuid, result: Result<Vec<Patient>,String> },
    ManualPatient {request:Uuid,patient:Uuid,saved:bool,result:Result<Value,String>},
    ProtocolHistory {request:Uuid,study:Uuid,revision:Option<Uuid>,result:Result<(Value,Option<Dictionary>),String>},
    #[cfg(windows)]
    HistoricalPdf {request:Uuid,study:Uuid,revision:Uuid,result:Result<crate::protocol_history::HistoricalPdf,String>},
    VisitTemplatesList { request: Uuid, result: Result<Vec<m53_client::offline::VisitTemplateSummary>, String> },
    VisitTemplateGet { request: Uuid, result: Result<m53_client::offline::VisitTemplateDetail, String> },
    VisitTemplateSaved { request: Uuid, result: Result<m53_client::offline::VisitTemplateDetail, String> },
    AuditClosed,
    Guidance(m53_client::ui_guidance::Snapshot),
    DiagnosisPrioritySaved { request: Uuid, result: Result<(), String> },
    Phrases(Value),
    PhraseSaved(Result<(),String>),
    ApprovalState(ApprovalNotice),
    Workflow {
        study: Uuid,
        request: Uuid,
        action: WorkflowAction,
        result: Result<WorkflowStatus, String>,
    },
    LoginProfiles(Vec<LoginProfile>),
    TransportAddress(String),
    State {
        snapshot: Value,
        diagnoses: m53_client::diagnosis_cache::Snapshot,
        patients: Vec<Patient>,
        studies: Vec<Study>,
        templates: Vec<Template>,
    },
    Opened {
        detail: Box<StudyDetail>,
        dictionary: Dictionary,
        journal: Value,
    },
    Texture {
        study: Uuid,
        asset: Uuid,
        width: u32,
        height: u32,
        image: Arc<RenderImage>,
    },
    Thumbnail { study: Uuid, asset: Uuid, image: Arc<RenderImage> },
    PhotoAdded { study: Uuid, asset: Uuid, width: u32, height: u32, captured: bool },
    PhotoFinished { study: Uuid, error: Option<String> },
    PhotoFailed { study: Uuid, message: String },
    Ack {
        study: Uuid,
        version: u64,
        form: bool,
        aliases: Value,
        problem: Option<String>,
    },
    Preview {
        study: Uuid,
        request: Uuid,
        lines: Vec<String>,
        photos: Vec<(Uuid, Arc<RenderImage>)>,
        pages: Vec<Arc<RenderImage>>,
        identity: Option<(Uuid, Uuid, String)>,
    },
    PreviewFailed { study: Uuid, request: Uuid, error: String },
    FileDone(Option<PathBuf>),
    ReportOutput {request:Uuid,result:Result<ReportCompletion,String>},
    Error(String),
    LoggedOut,
}
pub fn start() -> (mpsc::Sender<Command>, mpsc::Receiver<Event>) {
    let (tx, rx) = mpsc::channel();
    let (events, out) = mpsc::channel();
    std::thread::spawn(move || run(rx, events));
    (tx, out)
}
#[cfg(all(any(windows, target_os = "linux"), feature = "standalone"))]
fn connect(url: &str, key: &str, login: &str, password: &str) -> Result<Connection, String> {
    let mut raw = Connection::new(url, key)?;
    match raw.login(login.into(), password.into()) {
        Ok(_) => {
            let connected = m53_client::offline_profile::connect_local(raw, login, password)?;
            m53_client::credentials::save(url, key)?;
            Ok(connected)
        }
        Err(error) if error.starts_with("Сервис недоступен:")
            || ["502", "503", "504"].iter().any(|code| error.starts_with(&format!("Ошибка сервиса {code} "))) =>
        {
            m53_client::offline_profile::resume_local(url, key, login, password)
        }
        Err(error) => {
            if error.starts_with("Ошибка сервиса 401 ") {
                m53_client::offline_profile::deny_local(url, login)?;
            }
            Err(error)
        }
    }
}

#[cfg(not(all(any(windows, target_os = "linux"), feature = "standalone")))]
fn connect(url: &str, key: &str, login: &str, password: &str) -> Result<Connection, String> {
    #[cfg(windows)]
    if let Err(error) = m53_client::transport::migrate(url, key) {
        if !m53_client::one_c_auth::unavailable(&error) { return Err(error); }
        let id = Uuid::parse_str(login).map_err(|_| "Выберите врача")?;
        let selected = m53_client::offline_profile::target_profiles(url)?.into_iter()
            .find(|p| p.doctor_id == id).ok_or("Первый вход требует доступного HTTPS API")?;
        return m53_client::offline_profile::resume_1c(url, key, &selected, password);
    }
    let mut raw = Connection::new(url, key)?;
    if !cfg!(feature = "standalone") && std::env::var("M53_LEGACY_LOGIN").as_deref() != Ok("1") {
        #[cfg(windows)]
        {
            let id = Uuid::parse_str(login).map_err(|_| "Выберите врача из каталога 1С")?;
            let cached = m53_client::offline_profile::target_profiles(url)?;
            let selected = match raw.auth_profiles() {
                Ok(profiles) => profiles
                    .into_iter()
                    .find(|p| p.doctor_id == id)
                    .ok_or("Врач отсутствует в текущем каталоге 1С")?,
                Err(error) if m53_client::one_c_auth::unavailable(&error) => {
                    let p = cached
                        .into_iter()
                        .find(|p| p.doctor_id == id)
                        .ok_or("Первый вход требует доступного API")?;
                    return m53_client::offline_profile::resume_1c(url, key, &p, password);
                }
                Err(error) => return Err(error),
            };
            match raw.login_1c(selected.clone(), password) {
                Ok(prepared) => {
                    let connected =
                        m53_client::offline_profile::connect_1c(raw, password, prepared)?;
                    m53_client::credentials::save(url, key)?;
                    return Ok(connected);
                }
                Err(error) if m53_client::one_c_auth::unavailable(&error) => {
                    return m53_client::offline_profile::resume_1c(url, key, &selected, password);
                }
                Err(error) => {
                    m53_client::offline_profile::deny_target(url, id)?;
                    return Err(error);
                }
            }
        }
        #[cfg(not(windows))]
        return Err("Целевой вход 1С проверяется на Windows".into());
    }
    let result = raw.login(login.into(), password.into());
    #[cfg(windows)]
    {
        match result {
            Ok(_) => {
                let connected = m53_client::offline_profile::connect(raw, login, password, None)?;
                m53_client::credentials::save(url, key)?;
                Ok(connected)
            }
            Err(error)
                if error.starts_with("Сервис недоступен:")
                    || ["502", "503", "504"]
                        .iter()
                        .any(|code| error.starts_with(&format!("Ошибка сервиса {code}"))) =>
            {
                m53_client::offline_profile::resume_for(url, login, password).map_err(|_| error)
            }
            Err(error) => Err(error),
        }
    }
    #[cfg(not(windows))]
    {
        result?;
        // Linux is a test host. Use an explicit independent native account store.
        let store = crate::storage::open_store()?;
        let profile = m53_client::offline::Profile {
            server: raw.base.clone(),
            user_id: raw.user_id.ok_or("Нет врача")?,
            doctor: raw.doctor.clone(),
        };
        raw.offline = Some(m53_client::offline::Offline::attach(
            raw.clone(),
            store,
            profile,
        )?);
        Ok(raw)
    }
}
fn require(workspace: &Workspace, id: Uuid) -> Result<(), String> {
    if workspace.detail.as_ref().is_some_and(|d| d.study.id == id) {
        Ok(())
    } else {
        Err("Операция относится к другому обследованию".into())
    }
}
fn run(receiver: mpsc::Receiver<Command>, events: mpsc::Sender<Event>) {
    let mut colposcope_model: Option<String> = None;
    let mut fresh_manual_model: Option<String> = None;
    let mut workspace = Workspace::default();
    let audit = m53_client::activity::Journal::start();
    let mut guidance = m53_client::ui_guidance::Session::default();
    let mut phrases:Option<m53_client::phrases::Library>=None;
    let mut phrase_owner:Option<(String,Uuid)>=None;
    let mut paused_phrases:BTreeMap<(String,Uuid),m53_client::phrases::Library>=BTreeMap::new();
    let mut phrase_state=Value::Null;
    let mut audit_started = false;
    let mut audit_sync_active = false;
    let mut audit_sync_failed = false;
    let mut audit_conflicts = BTreeSet::new();
    let mut pending_open = None;
    let mut pending_reopen = false;
    let mut pending_approval: Option<PendingApproval> = None;
    let mut approval_started = Instant::now();
    let mut fresh_study = None;
    let mut fresh_date = String::new();
    let mut logout_requested = false;
    let mut pending_workflow: Option<(Uuid, Uuid, WorkflowAction)> = None;
    let photos = crate::photo_loader::Loader::start();
    let report_epoch=Arc::new(AtomicU64::new(0));
    let mut active_preview=None;
    struct ImageBatch { study: Uuid, done: bool, error: Option<String>, assets: Vec<Uuid> }
    let mut photo_batches: BTreeMap<Uuid, ImageBatch> = BTreeMap::new();
    let mut publisher = state_publisher::StatePublisher::default();
    let mut browser = crate::registry_browser::Browser::default();
    let mut applied_aliases = BTreeMap::new();
    let mut last = Instant::now();
    loop {
        while let Ok(output) = photos.incoming.try_recv() {
            use crate::photo_loader::Output;
            match output {
                Output::Full {study,asset,width,height,image} => {let _=events.send(Event::Texture {study,asset,width,height,image});}
                Output::Thumbnail {study,asset,image} => {let _=events.send(Event::Thumbnail {study,asset,image});}
                Output::Failed {study,message} => {let _=events.send(Event::PhotoFailed {study,message});}
                Output::Added {batch,study,asset,width,height,captured} => {
                    let study=canonical_id(&workspace,study);
                    if photo_batches.contains_key(&batch) && workspace.detail.as_ref().is_some_and(|d|d.study.id==study) {
                        let asset: Asset=serde_json::from_value(canonical_value(&workspace,json!(asset))).unwrap();
                        photo_batches.get_mut(&batch).unwrap().assets.push(asset.id);
                        if let Some(detail)=workspace.detail.as_mut() {
                            if !detail.assets.iter().any(|a|a.id==asset.id) {detail.assets.push(asset.clone());}
                        }
                        workspace.persist();
                        if captured {audit.record(Audit::new("image.capture_completed").study(study).entity(asset.id));}
                        let _=events.send(Event::PhotoAdded {study,asset:asset.id,width,height,captured});
                    }
                }
                Output::Finished {batch,error} => {if let Some(pending)=photo_batches.get_mut(&batch) {pending.done=true;pending.error=error;}}
            }
        }
        let completed:Vec<_>=photo_batches.iter().filter_map(|(batch,pending)| {
            let study=canonical_id(&workspace,pending.study);
            let durable=workspace.connection.as_ref().and_then(|c|c.offline.as_ref()).is_none_or(|o|o.state.lock().unwrap().study_durable(study));
            let recorded=pending.assets.iter().all(|id|workspace.journal["native_editor"]["photos"].as_array().is_some_and(|photos|photos.iter().any(|p|p["id"]==json!(canonical_id(&workspace,*id)))));
            (pending.done&&durable&&recorded).then_some((*batch,study))
        }).collect();
        for (batch,study) in completed {
            let pending=photo_batches.remove(&batch).unwrap();
            if let Some(error)=&pending.error {let mut action=Audit::new("image.import_failed").study(study);action.operation_id=Some(batch);audit.record(action);workspace.message=error.clone();}
            if let Some(o)=workspace.connection.as_ref().and_then(|c|c.offline.as_ref()) {o.state.lock().unwrap().activity_intents.remove("assets");}
            let _=events.send(Event::PhotoFinished {study,error:pending.error});
        }
        let command = match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(c) => Some(c),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(_) => {
                workspace.persist();
                break;
            }
        };
        if let Some(command) = command {
            let invalidates=matches!(&command,Command::Preview {..}|Command::Open(_)|Command::Logout|Command::Login {..}|Command::ClaimTask(..)|Command::NewStudy(_))
                || matches!(&command,Command::CancelPreview(request) if active_preview==Some(*request));
            if invalidates {
                report_epoch.fetch_add(1,Ordering::AcqRel); active_preview=None;
                #[cfg(windows)] m53_client::report::chromium::cancel_pending();
            }
            canonicalize_workspace(&mut workspace, &mut applied_aliases);
            use m53_client::activity::Action as Audit;
            let study = workspace.detail.as_ref().map(|d| d.study.id);
            let code = match &command {
                Command::Save => Some("protocol.save_clicked"), Command::Approve => Some("protocol.approve_clicked"),
                Command::Reopen => Some("protocol.reopen_clicked"), Command::Logout => Some("auth.logout_requested"),
                Command::Refresh => Some("tasks.refresh_requested"), Command::SendResult{..} => Some("report.send_clicked"),
                Command::Capture{..} => Some("image.capture_requested"), Command::Import{..} => Some("image.import_started"),
                Command::Resolve => Some("sync.conflict_resolved"), _=>None
            };
            if let Some(code)=code {
                let mut action=Audit::new(code); action.study_id=study;
                let suffix=match &command {Command::Save=>Some("protocol"),Command::Approve=>Some("approve"),Command::Reopen=>Some("reopen"),Command::Capture{..}=>Some("assets"),_=>None};
                if let Some(suffix)=suffix {if let Some(offline)=workspace.connection.as_ref().and_then(|c|c.offline.as_ref()) {let id=Uuid::new_v4();offline.state.lock().unwrap().activity_intents.insert(suffix.into(),id);action.operation_id=Some(id);}}
                audit.record(action);
            }
            let import_requested = matches!(&command,Command::Import{..});
            let photo_requested = match &command {Command::Import {study,..}|Command::Capture {study,..}=>Some(*study),_=>None};
            let preview_requested = match &command {Command::Preview {study,request,..}=>Some((*study,*request)),_=>None};
            let output_requested = match &command {
                Command::Pdf {request,..}|Command::SendResult {request,..}=>Some(*request),
                #[cfg(windows)] Command::HistoricalOutput {request,..}=>Some(*request),
                _=>None,
            };
            #[cfg(windows)]
            let historical_pdf_requested=match &command {Command::HistoricalPdf {request,study,revision,..}=>Some((*request,*study,*revision)),_=>None};
            let approval_requested = matches!(&command, Command::Approve);
            let cleanup_intent = match &command {Command::Save=>Some("protocol"),Command::Capture{..}=>Some("assets"),Command::Reopen=>Some("reopen"),Command::Approve=>Some("approve"),_=>None};
            let workflow_request = match &command {
                Command::Workflow {
                    study,
                    request,
                    action,
                    ..
                } => Some((*study, *request, *action)),
                _ => None,
            };
            let result = (|| -> Result<(), String> {
                match command {
                    Command::Guidance(request) => {
                        let before = guidance.snapshot();
                        let field = match request.action {
                            m53_client::ui_guidance::Action::SetEnabled(_) => "guidance.enabled",
                            m53_client::ui_guidance::Action::ResetProgress => "guidance.reset",
                            _ => "guidance.progress",
                        };
                        let snapshot = guidance.apply(request);
                        if snapshot.error.is_empty() && before.context == snapshot.context
                            && before.preferences != snapshot.preferences {
                            audit.record(Audit::new("settings.changed").field(field));
                        }
                        let _ = events.send(Event::Guidance(snapshot));
                    },
                    Command::Registry(request) => {
                        if let Some(offline)=workspace.connection.as_ref().and_then(|c|c.offline.as_ref()) {
                            browser.request(request, offline);publisher.invalidate();
                        }
                    },
                    Command::PhraseField(field)=>{if let Some(library)=&phrases {library.set_field(&field)?;library.refresh();}},
                    Command::PhraseSave{id,field_code,scope,title,body}=>{
                        let result=phrases.as_ref().ok_or("Библиотека шаблонов недоступна".to_string()).and_then(|library|if let Some(id)=id {library.edit_for(&field_code,id,&title,&body).map(|_|id)}else{library.create(&field_code,&scope,&title,&body)});
                        let mut action=Audit::new("phrase.save_requested").field(field_code);action.entity_id=result.as_ref().ok().copied().or(id);action.operation_id=result.as_ref().ok().and_then(|id|phrases.as_ref().and_then(|l|l.operation_id(Some(*id))));audit.record(action);
                        let _=events.send(Event::PhraseSaved(result.map(|_|())));
                    },
                    Command::PhrasePreferences{replace,include_shared}=>{if let Some(library)=&phrases {library.preferences(if replace{"replace"}else{"append"},include_shared)?;let mut action=Audit::new("phrase.preferences_changed");action.operation_id=library.operation_id(None);audit.record(action);}},
                    Command::PhraseResolve{operation,keep_local}=>{if let Some(library)=&phrases {if keep_local {library.resolve_keep_local(operation)?;}else{library.resolve_use_server(operation)?;}}},
                    Command::Activity(action) => audit.record(action),
                    Command::CloseAudit => { audit.record(Audit::new("app.closed")); audit.flush(); let _=events.send(Event::AuditClosed); },
                    Command::Colposcope(model) => colposcope_model = model,
                    Command::Workflow {
                        study,
                        request,
                        action,
                        close_preview,
                    } => {
                        if !photo_batches.is_empty() {return Err("Дождитесь локального сохранения снимков".into());}
                        let study = canonical_id(&workspace, study);
                        require(&workspace, study)?;
                        if pending_workflow.is_some() {
                            return Err("Дождитесь сохранения предыдущего действия".into());
                        }
                        if action == WorkflowAction::SaveAndExit {
                            pending_open = None;
                            pending_reopen = false;
                            workspace.cancel_background_open();
                        }
                        if close_preview && action == WorkflowAction::SaveAndExit {
                            // Closing a historical preview must not save its underlying draft.
                            pending_workflow = Some((study, request, action));
                            return Ok(());
                        }
                        if workspace.journal["native_editor_rejected"].is_object() {
                            return Err("Разметка не принята. Исправьте ошибку перед выходом; исходный ввод сохранен на компьютере".into());
                        }
                        if action == WorkflowAction::FinishProtocol {
                            if !care_path::status(
                                workspace.detail.as_ref().unwrap(),
                                &workspace.journal,
                            )
                            .examination_complete
                            {
                                return Err("Сначала завершите этап «Обследование»".into());
                            }
                            workspace.save()?;
                        } else if action == WorkflowAction::SaveAndExit {
                            // Valid drafts join the outbox; incomplete input still has its durable journal.
                            let _ = workspace.save();
                        }
                        let mut journal = workspace.journal.clone();
                        care_path::complete(
                            workspace.detail.as_ref().unwrap(),
                            &mut journal,
                            action,
                        )?;
                        workspace.checkpoint(journal)?;
                        workspace.persist();
                        pending_workflow = Some((study, request, action));
                        workspace.message = "Сохранение обследования на компьютере…".into();
                    }
                    Command::VisitTemplatesList { request, after } => {
                        let offline=workspace.connection.as_ref().and_then(|c|c.offline.as_ref()).cloned();
                        let events=events.clone(); std::thread::spawn(move || {
                            let result=offline.ok_or_else(||"Для работы с шаблонами войдите в программу".to_owned()).and_then(|o| {
                                let mut cursor=after; let mut all=Vec::new();
                                loop { let page=o.list_visit_templates(cursor)?; all.extend(page.templates);
                                    if all.len()>10000 {return Err("Слишком много шаблонов; обратитесь к администратору".into());}
                                    match page.next_after { Some(next)=>cursor=Some(next), None=>return Ok(all) }
                                }
                            });
                            let _=events.send(Event::VisitTemplatesList {request,result});
                        });
                    }
                    Command::VisitTemplateGet { request, id } => {
                        let offline=workspace.connection.as_ref().and_then(|c|c.offline.as_ref()).cloned();
                        let events=events.clone(); std::thread::spawn(move || {
                            let result=offline.ok_or_else(||"Для работы с шаблонами войдите в программу".to_owned()).and_then(|o|o.get_visit_template(id));
                            let _=events.send(Event::VisitTemplateGet {request,result});
                        });
                    }
                    Command::VisitTemplateSave { request, id, expected_version, title, protocol } => {
                        let offline=workspace.connection.as_ref().and_then(|c|c.offline.as_ref()).cloned();
                        let events=events.clone(); std::thread::spawn(move || {
                            let result=offline.ok_or_else(||"Для работы с шаблонами войдите в программу".to_owned()).and_then(|o|match expected_version {
                                Some(version)=>o.update_visit_template(id,version,title,protocol),
                                None=>o.create_visit_template(id,title,protocol),
                            });
                            let _=events.send(Event::VisitTemplateSaved {request,result});
                        });
                    }
                    Command::SaveDiagnosisPriority { request, base, ids } => {
                        let offline = workspace.connection.as_ref().and_then(|c| c.offline.as_ref()).cloned();
                        let events = events.clone();
                        std::thread::spawn(move || {
                            let result = offline.ok_or_else(|| "Для изменения группы войдите в программу".to_owned())
                                .and_then(|offline| offline.save_diagnosis_priorities(base, ids));
                            let _ = events.send(Event::DiagnosisPrioritySaved { request, result });
                        });
                    }
                    Command::RefreshDiagnosisPriority => { if let Some(offline) = workspace.connection.as_ref().and_then(|c| c.offline.as_ref()) { offline.refresh_diagnosis_priorities(); } }
                    Command::LoginProfiles(url, key) => {
                        #[cfg(feature = "standalone")]
                        if url.is_empty() || key.is_empty() {
                            let _ = events.send(Event::LoginProfiles(Vec::new()));
                            return Ok(());
                        }
                        #[cfg(all(windows, not(feature = "standalone")))]
                        match m53_client::transport::migrate(&url, &key) {
                            Ok(effective) => if effective != url { let _ = events.send(Event::TransportAddress(effective)); },
                            Err(error) if m53_client::one_c_auth::unavailable(&error) => (),
                            Err(error) => return Err(error),
                        }
                        let raw = workspace.connection.clone().unwrap_or(Connection::new(&url,&key)?);
                        audit.connect(&raw);
                        if !audit_started {audit.record(Audit::new("app.started"));audit_started=true;}
                        #[cfg(windows)]
                        let profiles = if cfg!(feature = "standalone") || std::env::var("M53_LEGACY_LOGIN").as_deref() == Ok("1") {
                            m53_client::offline_profile::saved_profiles(&url)?
                        } else {
                            let raw = Connection::new(&url, &key)?;
                            let items = match raw.auth_profiles() {
                                Ok(items) => items,
                                Err(error) if m53_client::one_c_auth::unavailable(&error) => {
                                    m53_client::offline_profile::target_profiles(&url)?
                                }
                                Err(error) => return Err(error),
                            };
                            items
                                .into_iter()
                                .map(|p| m53_client::offline_profile::LoginProfile {
                                    user_id: p.doctor_id,
                                    login: p.doctor_id.to_string(),
                                    display_name: p.display_name.clone(),
                                    target: Some(p),
                                })
                                .collect()
                        };
                        #[cfg(all(target_os = "linux", feature = "standalone"))]
                        let profiles = m53_client::offline_profile::saved_profiles(&url)?;
                        #[cfg(not(any(windows, all(target_os = "linux", feature = "standalone"))))]
                        let profiles = {
                            let _ = (url, key);
                            vec![]
                        };
                        let _ = events.send(Event::LoginProfiles(profiles));
                    }
                    Command::Login {
                        url,
                        key,
                        login,
                        password,
                    } => {
                        let connected=connect(&url, &key, &login, &password)?;
                        let _ = events.send(Event::Guidance(guidance.attach(&connected)));
                        audit.connect(&connected);
                        if let Some(o)=&connected.offline {o.state.lock().unwrap().activity=Some(audit.clone());}
                        if connected.session.is_empty() {audit.record(Audit::new("auth.offline_login"));}
                        if let Some(library)=phrases.take() {library.pause();if let Some(owner)=phrase_owner.take(){paused_phrases.insert(owner,library);}}
                        if let Some(user)=connected.user_id {
                            let owner=(connected.base.clone(),user);
                            let opened=if let Some(library)=paused_phrases.remove(&owner) {library.reconnect(&connected).map(|_|library)}else{m53_client::phrases::Library::open(&connected)};
                            phrases=match opened {Ok(library)=>{phrase_owner=Some(owner);Some(library)},Err(error)=>{let _=events.send(Event::Phrases(json!({"error":error})));None}};
                        }
                        phrase_state=Value::Null;
                        workspace.attach(connected);
                        #[cfg(windows)] {
                            std::thread::spawn(|| { let _ = m53_client::report::chromium::warm_up(); });
                        }
                        fetch(&workspace);
                    }
                    Command::SendResult {
                        request,
                        study,
                        selected,
                        template,
                        expected,
                    } => {
                        if cfg!(feature = "standalone") {
                            return Err("Передача в МИС отключена".into());
                        }
                        require(&workspace, study)?;
                        let detail = integration::visible_detail(
                            workspace.detail.as_ref().ok_or("Нет обследования")?,
                            &workspace.journal,
                        );
                        m53_client::delivery::require_mis_task(&detail)?;
                        if detail.study.status != "approved" {
                            return Err("Сначала утвердите протокол на сервере".into());
                        }
                        let connection = workspace.connection()?.clone();
                        let dictionary = workspace.dictionary.clone().ok_or("Нет словаря")?;
                        let equipment = workspace.journal["native_study_meta"]["colposcope_model"]
                            .as_str()
                            .map(str::to_owned);
                        let events = events.clone();
                        std::thread::spawn(move || {
                            let result = (|| {
                                let bytes = m53_client::workspace::render_report_with_equipment(
                                    &connection,
                                    &detail,
                                    &template,
                                    &dictionary,
                                    &selected,
                                    equipment.as_deref(),
                                )?;
                                verify_output_identity(detail.revision_id, template.version_id, &bytes, &expected)?;
                                connection
                                    .offline
                                    .as_ref()
                                    .ok_or("Нет локальной очереди")?
                                    .enqueue_report(
                                        &detail,
                                        template.version_id,
                                        m53_client::report::RENDERER_ID,
                                        &bytes,
                                    )
                            })();
                            let _ = events.send(Event::ReportOutput {request,result:result.map(|()|ReportCompletion::Enqueued)});
                        });
                    }
                    Command::ClaimTask(id, version) => {
                        if cfg!(feature = "standalone") {
                            return Err("Задания из МИС отключены".into());
                        }
                        let offline = workspace
                            .connection()?
                            .offline
                            .as_ref()
                            .ok_or("Нет кеша рабочего места")?;
                        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
                        let study = offline.claim_task(id, version, colposcope_model.as_deref())?;
                        fresh_manual_model = None;
                        fresh_study = Some(study);
                        fresh_date = today;
                        offline.refresh_study_patient(study);
                        offline.fetch_study_foreground(study);
                        pending_open = Some(study);
                        pending_reopen = false;
                        fetch(&workspace);
                    }
                    Command::Refresh => {
                        if let Some(offline)=workspace.connection.as_ref().and_then(|c|c.offline.as_ref()) {offline.clear_asset_failures();}
                        if let Some(offline)=workspace.connection.as_ref().and_then(|c|c.offline.as_ref()) {offline.refresh_diagnoses();}
                        if let Some(offline)=workspace.connection.as_ref().and_then(|c|c.offline.as_ref()) {browser.request(crate::registry_browser::Request::Refresh,offline);publisher.invalidate();}
                        photos.refresh();
                        fetch(&workspace);
                    },
                    Command::Open(id) => {
                        if !photo_batches.is_empty() {return Err("Дождитесь локального сохранения снимков".into());}
                        photos.clear();
                        if pending_workflow.is_some() {
                            return Err("Дождитесь сохранения обследования".into());
                        }
                        fresh_study = None;
                        let id = canonical_id(&workspace, id);
                        if let Some(o) = workspace.connection()?.offline.as_ref() {
                            o.fetch_study_foreground(id);
                            o.refresh_study_patient(id);
                            o.fetch(&format!("/api/v1/studies/{id}/context"));
                            if !cfg!(feature = "standalone") {
                                o.fetch(&format!("/api/v1/studies/{id}/results/1c"));
                            }
                        }
                        pending_open = Some(id);
                        pending_reopen = false;
                    }
                    Command::ManualPatient {request,patient,body} => {
                        let offline=workspace.connection.as_ref().and_then(|c|c.offline.as_ref()).cloned();
                        let events=events.clone();std::thread::spawn(move||{
                            let saved=body.is_some();
                            let result=offline.ok_or_else(||"Войдите в программу для работы с карточкой пациента".to_owned()).and_then(|o|match body {Some(body)=>o.update_manual_patient(patient,body),None=>o.patient_edit_context(patient)});
                            let _=events.send(Event::ManualPatient {request,patient,saved,result});
                        });
                    }
                    Command::ProtocolHistory {request,study,revision,before} => {
                        let offline=workspace.connection.as_ref().and_then(|c|c.offline.as_ref()).cloned();let events=events.clone();
                        std::thread::spawn(move||{
                            let result=(||{
                                let offline=offline.ok_or("Войдите для просмотра истории")?;
                                let value=offline.protocol_history_document(study,revision,before)?;
                                let dictionary=if revision.is_some(){
                                    if value["study_id"]!=json!(study)||value["revision_id"]!=json!(revision)||value["readonly"]!=true{return Err("Сервер вернул другую редакцию или разрешил изменение исторических данных".into());}
                                    let protocol:m53_domain::protocol::Protocol=serde_json::from_value(value["protocol"].clone()).map_err(|_|"Некорректный исторический протокол")?;
                                    Some(offline.protocol_history_dictionary(protocol.terminology_version_id)?)
                                }else{None};
                                Ok((value,dictionary))
                            })();let _=events.send(Event::ProtocolHistory {request,study,revision,result});
                        });
                    }
                    #[cfg(windows)]
                    Command::HistoricalPdf {request,study,revision,document,dictionary,template} => {
                        require(&workspace,study)?;
                        let connection=workspace.connection()?.clone();
                        let events=events.clone();
                        std::thread::spawn(move||{
                            let result=crate::protocol_history::historical_pdf(study,revision,&document,&dictionary,&template,&connection);
                            let _=events.send(Event::HistoricalPdf{request,study,revision,result});
                        });
                    }
                    #[cfg(windows)]
                    Command::HistoricalOutput {request,study,bytes,output} => {
                        require(&workspace,study)?;
                        workspace.connection()?.offline.as_ref().ok_or("Нет рабочего сеанса")?.require_report_output_access()?;
                        let events=events.clone();
                        std::thread::spawn(move||{
                            let result=finish_report(output,&bytes,m53_client::preview::print_pdf_bytes);
                            let _=events.send(Event::ReportOutput {request,result});
                        });
                    }
                    Command::CheckNewPatient {request,input} => {
                        let offline=workspace.connection.as_ref().and_then(|c|c.offline.as_ref()).cloned();
                        let events=events.clone();
                        std::thread::spawn(move||{
                            let result=offline.ok_or_else(||"Локальный кеш не подключён".to_owned()).and_then(|o|o.matching_patients(&input));
                            let _=events.send(Event::NewPatientMatches{request,result});
                        });
                    }
                    Command::UseMatchedPatient{patient,query}=>{
                        let offline=workspace.connection()?.offline.as_ref().ok_or("Локальный кеш не подключён")?;
                        offline.remember_matched_patient(&patient)?;
                        browser.request(crate::registry_browser::Request::Query(query),offline);
                        browser.request(crate::registry_browser::Request::Patient(patient.id),offline);
                        publisher.invalidate();
                    }
                    Command::NewPatient(input, sex) => {
                        let connection = workspace.connection()?;
                        let offline = connection
                            .offline
                            .as_ref()
                            .ok_or("Локальный кеш не подключен")?;
                        offline.create_patient(input, sex)?;
                        fetch(&workspace);
                    }
                    Command::NewStudy(patient) => {
                        if pending_workflow.is_some() {
                            return Err("Дождитесь сохранения обследования".into());
                        }
                        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
                        let mut request = json!({
                            "patient_id":patient,"order_id":null,"encounter_date":today,
                        });
                        if let Some(model) = &colposcope_model {
                            request["colposcope_model"] = json!(model);
                        }
                        let study: Study = workspace.connection()?.post("/api/v1/studies", &request)?;
                        fresh_manual_model = colposcope_model.clone();
                        pending_open = Some(study.id);
                        pending_reopen = false;
                        if let Some(o) = workspace.connection.as_ref().and_then(|c| c.offline.as_ref()) {
                            o.fetch_study_foreground(study.id);
                        }
                        fresh_study = Some(study.id);
                        fresh_date = today;
                        fetch(&workspace);
                    }
                    Command::Editor {
                        study,
                        version,
                        session,
                    } => {
                        let study = canonical_id(&workspace, study);
                        require(&workspace, study)?;
                        let session =
                            serde_json::from_value(canonical_value(&workspace, json!(session)))
                                .map_err(|_| "Ошибка идентификаторов редактора")?;
                        let previous: Session=serde_json::from_value(workspace.journal["native_editor"].clone()).unwrap_or_default();
                        integration::checkpoint(&mut workspace, study, &session)?;
                        audit_editor(&audit,study,&previous,&session);
                        let _ = events.send(Event::Ack {
                            study,
                            version,
                            form: false,
                            problem: None,
                            aliases: workspace
                                .connection
                                .as_ref()
                                .and_then(|c| c.offline.as_ref())
                                .map(|o| json!(o.identifier_aliases()))
                                .unwrap_or(json!({})),
                        });
                    }
                    Command::Form {
                        study,
                        version,
                        value,
                        base,
                        mut unfinished,
                    } => {
                        let study = canonical_id(&workspace, study);
                        require(&workspace, study)?;
                        let mut journal = workspace.journal.clone();
                        let value = canonical_value(&workspace, value);
                        let base = canonical_value(&workspace, base);
                        match m53_gpui_editor::state::form(
                            workspace.detail.as_ref().unwrap(),
                            &base,
                            &value,
                        ) {
                            Ok(detail) => {
                                unfinished.remove("schema");
                                m53_gpui_editor::state::set_detail(&mut journal, &detail);
                            }
                            Err(error) => {
                                journal["native_protocol_draft"] =
                                    json!({"base":base,"value":value});
                                unfinished.insert("schema".into(), error);
                            }
                        }
                        journal["unfinished_fields"] = json!(&unfinished);
                        workspace.checkpoint(journal)?;
                        let _ = events.send(Event::Ack {
                            study,
                            version,
                            form: true,
                            problem: unfinished.get("schema").cloned(),
                            aliases: workspace
                                .connection
                                .as_ref()
                                .and_then(|c| c.offline.as_ref())
                                .map(|o| json!(o.identifier_aliases()))
                                .unwrap_or(json!({})),
                        });
                    }
                    Command::Capture { study, bytes } => {
                        let study = canonical_id(&workspace, study);
                        require(&workspace, study)?;
                        if workspace.detail.as_ref().unwrap().study.status != "draft" {
                            return Err("Снимки доступны только в черновике обследования".into());
                        }
                        if !photo_batches.is_empty() {return Err("Дождитесь локального сохранения снимков".into());}
                        let batch=Uuid::new_v4();
                        photos.upload(batch,study,workspace.connection()?.clone(),crate::photo_loader::Source::Capture(bytes))?;
                        photo_batches.insert(batch,ImageBatch {study,done:false,error:None,assets:vec![]});
                    }
                    Command::Import { study, paths } => {
                        let study = canonical_id(&workspace, study);
                        require(&workspace, study)?;
                        if workspace.detail.as_ref().unwrap().study.status == "approved" {
                            return Err("Откройте новую редакцию".into());
                        }
                        if !photo_batches.is_empty() {return Err("Дождитесь локального сохранения снимков".into());}
                        let batch=Uuid::new_v4();
                        photos.upload(batch,study,workspace.connection()?.clone(),crate::photo_loader::Source::Import(paths))?;
                        photo_batches.insert(batch,ImageBatch {study,done:false,error:None,assets:vec![]});
                    }
                    Command::PhotoDemand {study,mut selection} => {
                        let study=canonical_id(&workspace,study);
                        if workspace.detail.as_ref().is_some_and(|d|d.study.id==study) {
                            for id in selection.full.iter_mut().chain(selection.thumbnails.iter_mut()) {*id=canonical_id(&workspace,*id);}
                            photos.demand(workspace.connection()?.clone(),study,selection);
                        }
                    }
                    Command::Save => {
                        workspace.save()?;
                    }
                    Command::ConfirmManualIdentity => {
                        let detail=workspace.detail.as_ref().ok_or("Нет открытого обследования")?;
                        let id=detail.study.id;
                        let context:Value=workspace.connection()?.get(&format!("/api/v1/studies/{id}/context"))?;
                        if context["patient"]["id"]!=json!(detail.patient.id) {return Err("Пациент изменился. Откройте обследование повторно и сверьте данные".into());}
                        let _:Value=workspace.connection()?.post(&format!("/api/v1/studies/{id}/manual-identity-verification"),&json!({"expected_version":detail.study.row_version,"patient_id":detail.patient.id,"expected_patient_version":context["patient"]["row_version"],"confirmed":true,"patient_data":{"family_name":detail.patient.family_name,"given_name":detail.patient.given_name,"patronymic":detail.patient.patronymic,"birth_date":detail.patient.birth_date,"sex":detail.patient.sex}}))?;
                        workspace.message="Ручная сверка подтверждена сервером. Можно запросить утверждение протокола".into();
                    }
                    Command::Approve => {
                        if !photo_batches.is_empty() {return Err("Дождитесь локального сохранения снимков".into());}
                        let study = workspace.detail.as_ref().ok_or("Нет открытого обследования")?.study.id;
                        if let Some(pending) = &pending_approval {
                            // A repeated click must never append a second approval operation.
                            let notice = pending.last_notice.clone().unwrap_or_else(ApprovalNotice::saving);
                            publish_approval(&mut workspace, &events, notice);
                        } else if let Some(operation) = queued_approval(&workspace, study) {
                            // A previous attempt may already be durable/in flight after a reconnect.
                            let mut pending = PendingApproval::new(study, operation, true);
                            pending.draft = Some(approval_draft(&workspace));
                            pending_approval = Some(pending);
                            approval_started = Instant::now();
                            publish_approval(&mut workspace, &events, ApprovalNotice {busy:false, pending:true, message:"Запрос утверждения уже находится в очереди. Проверяем подтверждение сервера…".into()});
                        } else {
                            let problems = workspace.approval_problems();
                            if !problems.is_empty() {
                                return Err(format!("Утверждение недоступно: {}", problems.join("; ")));
                            }
                            workspace.save()?;
                            let operation = workspace.connection.as_ref().and_then(|c|c.offline.as_ref())
                                .and_then(|o|o.state.lock().unwrap().activity_intents.get("approve").copied())
                                .unwrap_or_else(Uuid::new_v4);
                            let mut pending = PendingApproval::new(study, operation, false);
                            pending.draft = Some(approval_draft(&workspace));
                            pending_approval = Some(pending);
                            approval_started = Instant::now();
                            publish_approval(&mut workspace, &events, ApprovalNotice::saving());
                        }
                    }
                    Command::Resolve => {
                        workspace.resolve()?;
                        workspace.message="Объединенная локальная версия поставлена в очередь; требуется подтверждение сервера".into();
                    }
                    Command::Reopen => {
                        if let Some(d) = &workspace.detail {
                            pending_open = Some(d.study.id);
                            pending_reopen = true;
                            workspace.cancel_background_open();
                            workspace.message = "Открываем новую редакцию…".into();
                        } else {
                            return Err("Нет открытого обследования".into());
                        }
                    }
                    Command::Preview {
                        study,
                        request,
                        selected,
                        template,
                    } => {
                        active_preview=Some(request);
                        let study = canonical_id(&workspace, study);
                        require(&workspace, study)?;
                        care_path::require_report(
                            workspace.detail.as_ref().unwrap(),
                            &workspace.journal,
                        )?;
                        m53_client::workspace::check_unfinished_input(&workspace.journal)?;
                        prepare_report(&workspace, selected, template, None, Some(request), Some((report_epoch.clone(),report_epoch.load(Ordering::Acquire))), None, None, events.clone())?;
                    }
                    Command::CancelPreview(_)=>{},
                    Command::Pdf {
                        request,
                        study,
                        selected,
                        template,
                        expected,
                        output,
                    } => {
                        #[cfg(windows)]
                        if expected.is_none() { return Err("Обновите предпросмотр перед выводом".into()); }
                        let study = canonical_id(&workspace, study);
                        require(&workspace, study)?;
                        care_path::require_print(workspace.detail.as_ref().ok_or("Нет обследования")?)?;
                        care_path::require_report(
                            workspace.detail.as_ref().unwrap(),
                            &workspace.journal,
                        )?;
                        workspace.save()?;
                        prepare_report(&workspace, selected, template, Some(output), None, None, expected, Some(request), events.clone())?;
                    }
                    Command::ExportPhotos { study, mut photos } => {
                        let study = canonical_id(&workspace, study);
                        require(&workspace, study)?;
                        if photos.iter().any(|(_, path)| path.exists()) {
                            return Err("В выбранной папке уже есть файл с таким именем. Выберите другую папку или имя".into());
                        }
                        for (photo, _) in &mut photos {
                            photo.id = canonical_id(&workspace, photo.id);
                            workspace.ensure_asset(photo.id)?;
                        }
                        let connection = workspace.connection()?.clone();
                        let events = events.clone();
                        std::thread::spawn(move || {
                            let result = (|| {
                                for (photo, path) in photos {
                                    let image = m53_client::imaging::decode(&connection.download(photo.id)?)?;
                                    let image = if photo.annotations.is_empty() { image } else {
                                        m53_client::imaging::annotated(&image, &photo.annotations)
                                    };
                                    let bytes = m53_client::imaging::png(&image)?;
                                    write_new(&path, &bytes)?;
                                }
                                Ok(())
                            })();
                            match result {
                                Ok(()) => {
                                    let _ = events.send(Event::FileDone(None));
                                }
                                Err(e) => {
                                    let _ = events.send(Event::Error(e));
                                }
                            }
                        });
                    }
                    Command::Logout => {
                        workspace.cancel_background_open();
                        if !photo_batches.is_empty() {return Err("Дождитесь локального сохранения снимков".into());}
                        photos.clear();
                        workspace.persist();
                        logout_requested = true;
                        pending_open = None;
                        pending_reopen = false;
                        workspace.message = "Сохранение локальной работы перед выходом…".into();
                    }
                }
                Ok(())
            })();
            if let Some(suffix)=cleanup_intent {if (suffix!="assets" || photo_batches.is_empty()) && (suffix!="approve" || result.is_err() || pending_approval.as_ref().is_none_or(|p|p.submitted)) {if let Some(o)=workspace.connection.as_ref().and_then(|c|c.offline.as_ref()) {o.state.lock().unwrap().activity_intents.remove(suffix);}}}
            if let Err(error) = result {
                if approval_requested && pending_approval.is_none() {
                    publish_approval(&mut workspace, &events, ApprovalNotice::failure(error.clone()));
                }
                if import_requested {let mut action=Audit::new("image.import_failed");action.study_id=study;audit.record(action);}
                if let Some((study, request, action)) = workflow_request {
                    let _ = events.send(Event::Workflow {
                        study,
                        request,
                        action,
                        result: Err(error),
                    });
                } else if let Some((study,request))=preview_requested {
                    let _=events.send(Event::PreviewFailed {study:canonical_id(&workspace,study),request,error});
                } else if let Some(request)=output_requested {
                    let _=events.send(Event::ReportOutput {request,result:Err(error)});
                } else if let Some(study)=photo_requested {
                    let _=events.send(Event::PhotoFinished {study:canonical_id(&workspace,study),error:Some(error)});
                } else {
                    #[cfg(windows)]
                    if let Some((request,study,revision))=historical_pdf_requested {
                        let _=events.send(Event::HistoricalPdf {request,study,revision,result:Err(error)});
                        continue;
                    }
                    let _ = events.send(Event::Error(error));
                }
            }
            send_state(&workspace, &events, &audit, &mut publisher, &mut browser);
        }
        if let Some((study, request, action)) = pending_workflow
            && workspace
                .connection
                .as_ref()
                .and_then(|c| c.offline.as_ref())
                .is_some_and(|o| o.checkpoint_ready())
        {
            let status = workspace
                .detail
                .as_ref()
                .map(|d| care_path::status(d, &workspace.journal))
                .unwrap_or_default();
            if action == WorkflowAction::SaveAndExit {
                if let Err(error) = workspace.close_study() {
                    workspace.message = error;
                    continue;
                }
                audit.record(Audit::new("study.closed").study(study));
                if let Some(o) = workspace.connection.as_ref().and_then(|c|c.offline.as_ref()) { o.clear_foreground(); }
                photos.clear();
            }
            pending_workflow = None;
            if action != WorkflowAction::SaveAndExit {
                workspace.message = match action {
                    WorkflowAction::FinishExamination => "Этап обследования завершен",
                    WorkflowAction::FinishProtocol => "Протокол готов к отчету",
                    WorkflowAction::SaveAndExit => unreachable!(),
                }
                .into();
            }
            let _ = events.send(Event::Workflow {
                study,
                request,
                action,
                result: Ok(status),
            });
            send_state(&workspace, &events, &audit, &mut publisher, &mut browser);
        }
        if last.elapsed() > Duration::from_millis(500) {
            last = Instant::now();
            canonicalize_workspace(&mut workspace, &mut applied_aliases);
            if workspace.connection.is_some() {
                workspace.tick();
            }
            // Recover an already durable request after reopening the app/study, without
            // creating a second operation. Local edits form a new draft; ACK stays revision-scoped.
            if pending_approval.is_none() {
                if let Some(study) = workspace.detail.as_ref().map(|d|d.study.id) {
                    if let Some(operation) = queued_approval(&workspace, study) {
                        let mut pending = PendingApproval::new(study, operation, true);
                        pending.draft = Some(approval_draft(&workspace));
                        pending_approval = Some(pending);
                        approval_started = Instant::now();
                    }
                }
            }
            if let Some(mut pending) = pending_approval.take() {
                pending.study = canonical_id(&workspace, pending.study);
                let queue = approval_queue(&workspace, &pending);
                match pending.inspect(&queue, approval_started.elapsed()) {
                    ApprovalStep::Wait(notice) => {
                        if pending.last_notice.as_ref() != Some(&notice) {
                            publish_approval(&mut workspace, &events, notice.clone());
                            pending.last_notice = Some(notice);
                        }
                        pending_approval = Some(pending);
                    }
                    ApprovalStep::Submit => {
                        // Freeze the original click's ID into the actual outbox operation.
                        if let Some(o) = workspace.connection.as_ref().and_then(|c|c.offline.as_ref()) {
                            o.state.lock().unwrap().activity_intents.insert("approve".into(),pending.operation);
                        }
                        let selected = serde_json::from_value::<Vec<Uuid>>(
                            canonical_value(&workspace, workspace.journal["selected_photos"].clone())
                        ).unwrap_or_else(|_|workspace.detail.as_ref().map(|detail| {
                            detail.assets.iter().filter(|asset|asset.kind=="original").map(|asset|asset.id).collect()
                        }).unwrap_or_default());
                        match workspace.approve_with_photos(Some(&selected)) {
                            Ok(()) => {
                                pending.submitted = true;
                                pending.last_notice = None;
                                approval_started = Instant::now();
                                pending_approval = Some(pending);
                                publish_approval(&mut workspace, &events, ApprovalNotice {busy:true,pending:true,message:"Ожидаем подтверждения утверждения сервером…".into()});
                            }
                            Err(error) if error.starts_with("Запрос отправляется в фоне") => {
                                pending.submitted = true;
                                pending.last_notice = None;
                                approval_started = Instant::now();
                                pending_approval = Some(pending);
                                publish_approval(&mut workspace, &events, ApprovalNotice {busy:true,pending:true,message:"Ожидаем подтверждения утверждения сервером…".into()});
                            }
                            Err(error) => {
                                clear_approval_intent(&workspace);
                                publish_approval(&mut workspace, &events, ApprovalNotice::failure(error));
                            }
                        }
                    }
                    ApprovalStep::Finish(notice) => {
                        clear_approval_intent(&workspace);
                        publish_approval(&mut workspace, &events, notice);
                    }
                }
            }
            if let Some(id) = pending_open {
                let id = canonical_id(&workspace, id);
                match workspace.open_background(id, preferred_version(&workspace), pending_reopen) {
                    Ok(()) => {
                        let mut detail = workspace.detail.clone().unwrap();
                        if m53_gpui_editor::state::use_current_form(&mut detail) {
                            let mut journal = workspace.journal.clone();
                            m53_gpui_editor::state::set_detail(&mut journal, &detail);
                            if let Err(error) = workspace.checkpoint(journal) {
                                let _ = events.send(Event::Error(error));
                            }
                        }
                        if fresh_study.is_some_and(|fresh| canonical_id(&workspace, fresh) == id) {
                            let mut detail = workspace.detail.clone().unwrap();
                            let mut journal = workspace.journal.clone();
                            let connection = workspace.connection.as_ref();
                            m53_gpui_editor::registry::initialize_new_study(
                                &mut detail,
                                &mut journal,
                                &fresh_date,
                                connection.and_then(|c| c.user_id),
                                connection.map(|c| c.doctor.as_str()).unwrap_or(""),
                            );
                            if let Some(model) = fresh_manual_model.take() {
                                journal["native_study_meta"]["colposcope_model"] = json!(model);
                            }
                            m53_gpui_editor::state::set_detail(&mut journal, &detail);
                            if let Err(error) = workspace.checkpoint(journal) {
                                let _ = events.send(Event::Error(error));
                            }
                            fresh_study = None;
                        }
                        audit.record(Audit::new("study.opened").study(id));
                        pending_open = None;
                        pending_reopen = false;
                        photos.clear();
                        let _ = events.send(Event::Opened {
                            detail: Box::new(integration::visible_detail(
                                workspace.detail.as_ref().unwrap(),
                                &workspace.journal,
                            )),
                            dictionary: workspace.dictionary.clone().unwrap(),
                            journal: workspace.journal.clone(),
                        });
                    }
                    Err(e) => {
                        workspace.message = e.clone();
                        if !m53_client::workspace::opening_pending(&e) {
                            pending_open = None;
                            pending_reopen = false;
                            if let Some(o) = workspace.connection.as_ref().and_then(|c| c.offline.as_ref()) {
                                o.clear_foreground();
                                if let Some(detail) = &workspace.detail { o.fetch_study_foreground(detail.study.id); }
                            }
                            let _ = events.send(Event::Error(e));
                        }
                    }
                }
            }
            if logout_requested {
                match workspace.logout() {
                    Ok(()) => {
                        let _ = events.send(Event::Guidance(guidance.detach()));
                        audit.sign_out();
                        if let Some(library)=phrases.take() {library.pause();if let Some(owner)=phrase_owner.take(){paused_phrases.insert(owner,library);}}
                        phrase_state=Value::Null;let _=events.send(Event::Phrases(json!({})));
                        logout_requested = false;
                        browser=Default::default();publisher.invalidate();
                        photos.clear();
                        #[cfg(windows)] m53_client::report::chromium::shutdown();
                        let _ = events.send(Event::LoggedOut);
                    }
                    Err(error) => {
                        if error != "Локальная запись еще не завершена"
                        {
                            logout_requested = false;
                            let _ = events.send(Event::Error(error));
                        }
                    }
                }
            }
            if let Some(o)=workspace.connection.as_ref().and_then(|c|c.offline.as_ref()) {
                let (online,pending,_,_)=o.information();
                if pending>0 && !audit_sync_active {audit.record(Audit::new("sync.started"));audit_sync_active=true;}
                if pending==0 && audit_sync_active {audit.record(Audit::new("sync.finished"));audit_sync_active=false;}
                if pending>0 && !online && !audit_sync_failed {audit.record(Audit::new("sync.failed"));audit_sync_failed=true;}
                if online {audit_sync_failed=false;}
                let conflicts:Vec<_>=o.state.lock().unwrap().pending.iter().filter(|op|op.blocked.is_some()).map(|op|op.id).collect();
                for id in conflicts {if audit_conflicts.insert(id) {let mut action=Audit::new("sync.conflict_detected");action.operation_id=Some(id);audit.record(action);}}
            }
            send_state(&workspace, &events, &audit, &mut publisher, &mut browser);
        }
        if let Some(library)=&phrases {let snapshot=json!(library.snapshot());if snapshot!=phrase_state {phrase_state=snapshot.clone();let _=events.send(Event::Phrases(snapshot));}}
    }
}
fn publish_approval(workspace: &mut Workspace, events: &mpsc::Sender<Event>, notice: ApprovalNotice) {
    workspace.message = notice.message.clone();
    let _ = events.send(Event::ApprovalState(notice));
}
fn clear_approval_intent(workspace: &Workspace) {
    if let Some(o) = workspace.connection.as_ref().and_then(|c|c.offline.as_ref()) {
        o.state.lock().unwrap().activity_intents.remove("approve");
    }
}
fn queued_approval(workspace: &Workspace, study: Uuid) -> Option<Uuid> {
    let offline = workspace.connection.as_ref()?.offline.as_ref()?;
    let state=offline.state.lock().unwrap();
    if state.study_blocked(study).is_some() {return None;}
    state.pending.iter().find(|op|op.scope()==format!("/api/v1/studies/{study}/approve")).map(|op|op.id)
}
fn approval_draft(workspace: &Workspace) -> Value {
    workspace.detail.as_ref().map(|d|json!({
        "protocol":d.protocol,"annotations":d.annotations,"selected_photos":workspace.journal["selected_photos"]
    })).unwrap_or(Value::Null)
}
fn approval_queue(workspace: &Workspace, pending: &PendingApproval) -> approval_flow::Queue {
    let mut queue = approval_flow::Queue {
        study: workspace.detail.as_ref().map(|d|d.study.id),
        approved: workspace.detail.as_ref().is_some_and(|d|d.study.status == "approved"),
        draft_changed: pending.draft.as_ref().is_some_and(|draft|*draft != approval_draft(workspace)),
        ..Default::default()
    };
    let Some(offline) = workspace.connection.as_ref().and_then(|c|c.offline.as_ref()) else {
        queue.network_error = Some("Нет локального журнала передачи: откройте обследование повторно.".into());
        return queue;
    };
    let state = offline.state.lock().unwrap();
    queue.online = state.online;
    queue.durable = state.study_durable(pending.study);
    queue.pending_count = state.study_pending_count(pending.study);
    queue.network_error = approval_flow::transport_problem(state.online,&state.error);
    queue.approval_queued = state.pending.iter().any(|op|op.id == pending.operation);
    queue.approval_confirmed = state.confirmed.contains_key(&pending.operation);
    queue.approval_rejected = state.conflict_history.iter().find(|op|op.id == pending.operation)
        .and_then(|op|op.blocked.as_ref()).map(|message|approval_flow::Block {own_study:true,code:approval_flow::rejection_code(message)});
    queue.blocked = state.study_blocked(pending.study).map(|op|approval_flow::Block {
        own_study:true,
        code:op.blocked.as_deref().and_then(approval_flow::rejection_code)
    });
    queue
}
fn fetch(workspace: &Workspace) {
    if let Ok(c) = workspace.connection() {
        for p in [
            "/api/v1/templates",
            "/api/v1/terminology",
            "/api/v1/terminology/versions",
        ] {
            if let Some(o) = &c.offline {
                o.fetch(p);
            }
        }
    }
}
fn send_state(workspace: &Workspace, events: &mpsc::Sender<Event>, audit: &m53_client::activity::Journal, publisher: &mut state_publisher::StatePublisher, browser: &mut crate::registry_browser::Browser) {
    if let Some(patient)=browser.patient {
        let canonical=canonical_id(workspace,patient);
        if canonical!=patient {
            if let Some(offline)=workspace.connection.as_ref().and_then(|c|c.offline.as_ref()) {
                browser.request(crate::registry_browser::Request::Patient(canonical),offline);publisher.invalidate();
            }
        }
    }
    if let Some(offline)=workspace.connection.as_ref().and_then(|c|c.offline.as_ref()) {if browser.sync_refresh(offline) {publisher.invalidate();}}
    if !publisher.changed(workspace,audit) { return; }
    let get = |path: &str| {
        workspace
            .connection
            .as_ref()
            .and_then(|c| c.offline.as_ref())
            .and_then(|o| o.cached(path))
            .unwrap_or(json!([]))
    };
    let registry = workspace.connection.as_ref().and_then(|c|c.offline.as_ref()).map(|o|{browser.fetch(o);browser.snapshot(o)}).unwrap_or(json!({}));
    let (study_path, summary_path)=browser.study_paths().unwrap_or_default();
    let mut studies: Vec<Study> = serde_json::from_value(get(&study_path)).unwrap_or_default();
    let local:Vec<Study>=serde_json::from_value(get("/api/v1/studies")).unwrap_or_default();
    for study in local.into_iter().filter(|s|Some(s.patient_id)==browser.patient) {if !studies.iter().any(|s|s.id==study.id){studies.push(study);}}
    let mut summaries = m53_gpui_editor::registry::StudySummary::server_rows(&get(&summary_path));
    if let Some(offline) = workspace
        .connection
        .as_ref()
        .and_then(|c| c.offline.as_ref())
    {
        let state = offline.state.lock().unwrap();
        let pending: BTreeSet<Uuid> = state.pending.iter().filter_map(|op| {
            if op.scope() == "/api/v1/studies" { return Some(op.entity_id); }
            op.scope().strip_prefix("/api/v1/studies/")?.split('/').next()?.parse().ok()
        }).collect();
        for study in &studies {
            let current = workspace.detail.as_ref().filter(|d| d.study.id == study.id);
            let journal = if current.is_some() { &workspace.journal }
                else { state.editors.get(&study.id).unwrap_or(&Value::Null) };
            let dirty = journal["dirty"] == true || (current.is_some() && workspace.dirty);
            let cached = state.responses.get(&format!("/api/v1/studies/{}", study.id));
            let current_json = current.filter(|_| dirty || cached.is_none()).map(|d| json!(d));
            let detail = current_json.as_ref()
                .or_else(|| journal.get("detail").filter(|v| dirty && v.is_object()))
                .or(cached);
            if let Some(detail) = detail {
                let summary = summaries.entry(study.id).or_default();
                if summary.needs_overlay(detail, dirty || pending.contains(&study.id)) {
                    summary.overlay_local(detail, journal);
                }
            }
        }
    }
    let _ = events.send(Event::State {
        diagnoses: workspace.connection.as_ref()
            .and_then(|c| c.offline.as_ref())
            .map(|offline| offline.diagnoses_snapshot())
            .unwrap_or_default(),
        snapshot: {
            let mut snapshot = workspace.snapshot();
            snapshot["native_activity"] = json!(audit.snapshot());
            snapshot["native_tasks"] = registry["tasks"].clone();
            snapshot["native_registry"] = registry.clone();
            if let Some(detail) = workspace.detail.as_ref() {
                if !cfg!(feature = "standalone") {
                    snapshot["native_result_jobs"] =
                        get(&format!("/api/v1/studies/{}/results/1c", detail.study.id));
                }
                snapshot["manual_identity_context"]=get(&format!("/api/v1/studies/{}/context", detail.study.id));
                let query=detail.revision_id.map(|id|format!("?revision_id={id}")).unwrap_or_default();
                let metadata=get(&format!("/api/v1/studies/{}/report-context{query}",detail.study.id));
                if m53_client::report_identity::performer(&metadata,detail.study.id,detail.revision_id).is_ok(){snapshot["protocol_metadata"]=metadata;}
                if workspace.message.contains("Ручная сверка сохраняется") && snapshot["manual_identity_context"]["manual_identity_verification"].is_object() {
                    snapshot["message"]=json!("Сверка пациента подтверждена сервером. Теперь можно нажать «Утвердить»");
                }
            }
            if let Some(detail) = workspace.detail.as_ref() {
                snapshot["detail"] = json!(integration::visible_detail(detail, &workspace.journal));
            }
            snapshot["native_aliases"] = workspace
                .connection
                .as_ref()
                .and_then(|c| c.offline.as_ref())
                .map(|o| json!(o.identifier_aliases()))
                .unwrap_or(json!({}));
            snapshot["native_study_summaries"] = json!(summaries);
            snapshot["native_workflow"] = workspace
                .detail
                .as_ref()
                .map(|d| json!(care_path::status(d, &workspace.journal)))
                .unwrap_or(Value::Null);
            snapshot["native_patient_sexes"] = workspace
                .connection
                .as_ref()
                .and_then(|c| c.offline.as_ref())
                .map(|o| json!(o.patient_sexes()))
                .unwrap_or(json!({}));
            snapshot
        },
        patients: serde_json::from_value(registry["patients"].clone()).unwrap_or_default(),
        studies,
        templates: serde_json::from_value(get("/api/v1/templates")).unwrap_or_default(),
    });
}
fn write_new(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| "Выберите новое имя файла")?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| "Ошибка записи файла".into())
}

fn finish_report(
    output: ReportOutput,
    bytes: &[u8],
    print: impl FnOnce(&[u8]) -> Result<bool, String>,
) -> Result<ReportCompletion, String> {
    match output {
        ReportOutput::Print => print(bytes).map(ReportCompletion::Printed),
        ReportOutput::Export(path) => {
            // The native Save dialog confirms replacement; publish a fully fsynced PDF atomically.
            if !bytes.starts_with(b"%PDF-") { return Err("Некорректный PDF".into()); }
            let temporary = path.with_file_name(format!(".m53-export-{}.tmp", Uuid::new_v4()));
            let result = write_new(&temporary, bytes).and_then(|_| {
                std::fs::rename(&temporary, &path).map_err(|_| "Не удалось сохранить PDF в выбранную папку".into())
            });
            if result.is_err() { let _ = std::fs::remove_file(&temporary); }
            result?;
            Ok(ReportCompletion::Exported(path))
        }
    }
}

#[cfg(test)]
mod report_output_tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    #[ignore = "synthetic bundled Chromium verification"]
    fn repeated_synthetic_pdf_uses_same_bytes_for_identity_guard() {
        let html = b"<!doctype html><html><head></head><body>Synthetic report identity</body></html>";
        let preview = m53_client::report::chromium::pdf(html).unwrap();
        let output = m53_client::report::chromium::pdf(html).unwrap();
        let revision=Uuid::new_v4(); let template=Uuid::new_v4();
        let expected=(revision,template,m53_client::delivery::pdf_hash(&preview));
        assert_eq!(preview,output);
        assert!(verify_output_identity(Some(revision),template,&output,&expected).is_ok());
    }
    #[test]
    fn report_output_rejects_bytes_from_a_different_preview() {
        let revision = Uuid::new_v4();
        let template = Uuid::new_v4();
        let expected = (revision, template, m53_client::delivery::pdf_hash(b"preview"));
        assert!(verify_output_identity(Some(revision), template, b"preview", &expected).is_ok());
        assert!(verify_output_identity(Some(revision), template, b"different", &expected).is_err());
        assert!(verify_output_identity(Some(Uuid::new_v4()), template, b"preview", &expected).is_err());
        assert!(verify_output_identity(Some(revision), Uuid::new_v4(), b"preview", &expected).is_err());
    }
    #[test]
    fn print_uses_pdf_bytes_and_cancel_does_not_export_a_file() {
        let bytes = b"%PDF-synthetic";
        let event = finish_report(ReportOutput::Print, bytes, |document| {
            assert_eq!(document, bytes);
            Ok(false)
        }).unwrap();
        assert!(matches!(event, ReportCompletion::Printed(false)));
    }
    #[test]
    fn export_atomically_replaces_a_confirmed_path_without_printing() {
        let directory = std::env::temp_dir().join(format!("m53-report-output-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("report.pdf");
        std::fs::write(&path, b"previous").unwrap();
        let bytes = b"%PDF-synthetic";
        let event = finish_report(ReportOutput::Export(path.clone()), bytes, |_| panic!("Export must not print")).unwrap();
        assert!(matches!(event, ReportCompletion::Exported(saved) if saved == path));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(finish_report(ReportOutput::Export(path.clone()), b"invalid", |_| panic!()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}

fn prepare_report(
    workspace: &Workspace,
    selected: Vec<Uuid>,
    template: Template,
    output: Option<ReportOutput>,
    preview_request: Option<Uuid>,
    preview_epoch: Option<(Arc<AtomicU64>,u64)>,
    expected: Option<(Uuid, Uuid, String)>,
    output_request: Option<Uuid>,
    events: mpsc::Sender<Event>,
) -> Result<(), String> {
    let selected: Vec<Uuid> = serde_json::from_value(canonical_value(workspace, json!(selected)))
        .map_err(|_| "Ошибка выбора фото")?;
    let connection = workspace.connection()?.clone();
    let detail = integration::visible_detail(
        workspace.detail.as_ref().ok_or("Нет исследования")?,
        &workspace.journal,
    );
    if output.is_some() { care_path::require_print(&detail)?; }
    let dictionary = workspace.dictionary.clone().ok_or("Нет словаря")?;
    let equipment = workspace.journal["native_study_meta"]["colposcope_model"]
        .as_str()
        .map(str::to_owned);
    let audit=workspace.connection.as_ref().and_then(|c|c.offline.as_ref()).and_then(|o|o.state.lock().unwrap().activity.clone());
    std::thread::spawn(move || {
        let cancelled=||preview_epoch.as_ref().is_some_and(|(epoch,generation)|epoch.load(Ordering::Acquire)!=*generation);
        let check_cancelled=||if cancelled() {Err("Предпросмотр отменен".to_string())} else {Ok(())};
        let result = (|| -> Result<(), String> {
            check_cancelled()?;
            if let Some(output) = output {
                let bytes = m53_client::workspace::render_report_with_equipment(
                    &connection,
                    &detail,
                    &template,
                    &dictionary,
                    &selected,
                    equipment.as_deref(),
                )?;
                if let Some(expected) = &expected { verify_output_identity(detail.revision_id, template.version_id, &bytes, expected)?; }
                if let Some(audit)=&audit {audit.record(Audit::new("report.pdf_generated").study(detail.study.id));}
                let event = finish_report(output, &bytes, |bytes| {
                    #[cfg(windows)] { m53_client::preview::print_pdf_bytes(bytes) }
                    #[cfg(all(target_os = "linux", feature = "standalone"))] { crate::linux_output::print(bytes) }
                    #[cfg(not(any(windows, all(target_os = "linux", feature = "standalone"))))]
                    { let _ = bytes; Err("Печать доступна в Windows-клиенте".into()) }
                })?;
                let _ = events.send(Event::ReportOutput {request:output_request.ok_or("Нет идентификатора вывода отчёта")?,result:Ok(event)});
            } else {
                if let Some(audit)=&audit {audit.record(m53_client::activity::Action::new("report.previewed").study(detail.study.id));}
                let (report_context, _) = m53_client::report_identity::load(&connection, &detail)?;
                let (report_detail,performer)=m53_client::report_identity::bind(&detail,&report_context)?;
                let mut lines = m53_client::report::native_lines_for_photos(
                    &report_detail,
                    &template.layout,
                    &performer,
                    &dictionary.terms,
                    &selected,
                )?;
                if let Some(model) = m53_client::report_identity::equipment(&report_context, equipment.as_deref()) {
                    lines.insert(1, format!("Кольпоскоп: {model}"));
                }
                let request=preview_request.ok_or("Нет идентификатора предпросмотра")?;
                #[cfg(any(windows, all(target_os = "linux", feature = "standalone")))]
                if detail.protocol.as_ref().is_some_and(|p| p.colpo.is_some()) {
                    let pdf = m53_client::workspace::render_report_cancellable(
                        &connection,
                        &detail,
                        &template,
                        &dictionary,
                        &selected,
                        equipment.as_deref(),
                        cancelled,
                    )?;
                    check_cancelled()?;
                    let mut pages = vec![];
                    #[cfg(windows)]
                    let rendered_pages = m53_client::report::chromium::pages(&pdf)?;
                    #[cfg(all(target_os = "linux", feature = "standalone"))]
                    let rendered_pages = crate::linux_output::pages(&pdf)?;
                    for page in rendered_pages {
                        check_cancelled()?;
                        pages.push(crate::storage::texture(&page)?.2);
                    }
                    check_cancelled()?;
                    let _ = events.send(Event::Preview {
                        study: detail.study.id,
                        request,
                        lines,
                        photos: vec![],
                        pages,
                        identity: detail.revision_id.map(|r| (r, template.version_id, m53_client::delivery::pdf_hash(&pdf))),
                    });
                    return Ok(());
                }
                let mut photos = vec![];
                for photo in m53_client::report_assets::load_until(&connection,&detail.assets,&selected,Instant::now()+Duration::from_secs(45),&cancelled)? {
                    check_cancelled()?;
                    let image = m53_client::imaging::decode(&photo.bytes)?;
                    check_cancelled()?;
                    let annotations = m53_client::report::annotations_for_report(&detail, photo.id);
                    let bytes = m53_client::imaging::png(
                        &m53_client::imaging::annotated(&image, &annotations).thumbnail(1800, 2400),
                    )?;
                    photos.push((photo.id, crate::storage::texture(&bytes)?.2));
                }
                check_cancelled()?;
                let _ = events.send(Event::Preview {
                    study: detail.study.id,
                    request,
                    lines,
                    photos,
                    pages: vec![],
                    identity: None,
                });
            }
            Ok(())
        })();
        if let Err(e) = result {
            let _ = events.send(match preview_request {
                Some(request)=>Event::PreviewFailed {study:detail.study.id,request,error:e},
                None=>match output_request {
                    Some(request)=>Event::ReportOutput {request,result:Err(e)},
                    None=>Event::Error(e),
                },
            });
        }
    });
    Ok(())
}

fn canonical_value(workspace: &Workspace, value: Value) -> Value {
    workspace
        .connection
        .as_ref()
        .and_then(|c| c.offline.as_ref())
        .map(|o| o.canonicalize_local(value.clone()))
        .unwrap_or(value)
}
fn canonical_id(workspace: &Workspace, id: Uuid) -> Uuid {
    serde_json::from_value(canonical_value(workspace, json!(id))).unwrap_or(id)
}
fn canonicalize_workspace(workspace: &mut Workspace, applied: &mut BTreeMap<Uuid,Uuid>) {
    let aliases = workspace.connection.as_ref().and_then(|c|c.offline.as_ref())
        .map(|o|o.identifier_aliases()).unwrap_or_default();
    if aliases.is_empty() || *applied == aliases { return; }
    *applied = aliases;
    if let Some(detail) = &workspace.detail {
        workspace.detail = serde_json::from_value(canonical_value(workspace, json!(detail))).ok();
    }
    workspace.journal = canonical_value(workspace, workspace.journal.clone());
}

#[cfg(not(windows))]
pub fn smoke_api(credentials: &str) -> anyhow::Result<()> {
    fn wait<T>(
        rx: &mpsc::Receiver<Event>,
        mut select: impl FnMut(Event) -> Option<T>,
    ) -> anyhow::Result<T> {
        let deadline = Instant::now() + Duration::from_secs(45);
        let mut last = String::new();
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Event::Error(error)) => anyhow::bail!("Native worker: {error}"),
                Ok(event) => {
                    if let Event::State { snapshot, .. } = &event {
                        last = format!(
                            "message={} sync_error={} pending={}",
                            snapshot["message"],
                            snapshot["sync"]["error"],
                            snapshot["sync"]["pending"]
                        );
                    }
                    if let Some(v) = select(event) {
                        return Ok(v);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => (),
                Err(_) => anyhow::bail!("Native worker disconnected"),
            }
        }
        anyhow::bail!("Native worker timed out: {last}")
    }
    let credentials: Value = serde_json::from_slice(&std::fs::read(credentials)?)?;
    anyhow::ensure!(
        credentials["login"] == "demo",
        "Only the synthetic demo account is allowed"
    );
    let (tx, rx) = start();
    tx.send(Command::Login {
        url: "http://127.0.0.1:8787".into(),
        key: credentials["workstation_key"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing test key"))?
            .into(),
        login: "demo".into(),
        password: credentials["password"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing test password"))?
            .into(),
    })?;
    let report_template = wait(&rx, |e| match e {
        Event::State { snapshot, templates, .. } if snapshot["authenticated"] == true => templates.into_iter().next(),
        _ => None,
    })?;
    let suffix = Uuid::new_v4().simple().to_string();
    let family = format!("GPUI-SYNTHETIC-{}", &suffix[..10]);
    tx.send(Command::NewPatient(
        NewPatient {
            family_name: family.clone(),
            given_name: "Синтетическая".into(),
            patronymic: None,
            birth_date: None,
        },
        None,
    ))?;
    println!("Native smoke: waiting for synthetic patient");
    let patient = wait(&rx, |e| match e {
        Event::State { patients, .. } => patients.into_iter().find(|p| p.family_name == family),
        _ => None,
    })?;
    tx.send(Command::NewStudy(patient.id))?;
    println!("Native smoke: waiting for synthetic study");
    let detail = wait(&rx, |e| match e {
        Event::Opened { detail, .. } => Some(detail),
        _ => None,
    })?;
    let root = std::env::temp_dir().join(format!("m53-native-smoke-{suffix}"));
    std::fs::create_dir_all(&root)?;
    let path = root.join("synthetic.png");
    let pixels = image::RgbImage::from_pixel(240, 160, image::Rgb([210, 220, 230]));
    pixels.save(&path)?;
    tx.send(Command::Import {
        study: detail.study.id,
        paths: vec![path],
    })?;
    println!("Native smoke: waiting for imported image");
    let (asset, width, height) = wait(&rx, |e| match e {
        Event::PhotoAdded {
            asset,
            width,
            height,
            ..
        } => Some((asset, width, height)),
        _ => None,
    })?;
    let mut photo = m53_gpui_editor::Photo::new(width, height);
    photo.id = asset;
    photo.add_callout(7, 0.2, 0.3);
    tx.send(Command::Editor {
        study: detail.study.id,
        version: 1,
        session: Session {
            photos: vec![photo],
            selected: 0,
        },
    })?;
    wait(&rx, |e| match e {
        Event::Ack {
            version: 1,
            form: false,
            ..
        } => Some(()),
        _ => None,
    })?;
    let current = wait(&rx, |e| match e {
        Event::State { snapshot, .. }
            if snapshot["detail"]["annotations"]
                .as_array()
                .is_some_and(|a| !a.is_empty()) =>
        {
            serde_json::from_value::<StudyDetail>(snapshot["detail"].clone()).ok()
        }
        _ => None,
    })?;
    let protocol = current.protocol.as_ref().unwrap();
    anyhow::ensure!(
        protocol.sections[0].findings.len() == 1,
        "Annotation must create exactly one clinical finding"
    );
    anyhow::ensure!(
        current.annotations[0].finding_id == Some(protocol.sections[0].findings[0].id),
        "Finding link must persist"
    );
    anyhow::ensure!(
        protocol.schema_version == 2 && protocol.colpo.is_some(),
        "New native study must use the versioned colposcopy protocol"
    );
    let mut value = json!(protocol);
    value["colpo"]["fields"]["extended.size"] = json!({"kind":"choice","value":">¾ цервикса"});
    value["colpo"]["marks"][current.annotations[0].id.to_string()]["properties"]["property.fine_mosaic"] =
        json!({"kind":"checked","value":true});
    value["conclusion"] = json!({"origin":{"kind":"physician"},"value":{"state":"assessed","value":"Синтетический тест нативного Rust-протокола"}});
    tx.send(Command::Form {
        study: current.study.id,
        version: 1,
        base: json!(protocol),
        value,
        unfinished: BTreeMap::new(),
    })?;
    wait(&rx, |e| match e {
        Event::Ack {
            version: 1,
            form: true,
            ..
        } => Some(()),
        _ => None,
    })?;
    tx.send(Command::Save)?;
    println!("Native smoke: waiting for backend receipts");
    let confirmed = wait(&rx, |e| match e {
        Event::State { snapshot, .. }
            if snapshot["sync"]["pending"] == 0 && snapshot["sync"]["durable"] == true =>
        {
            serde_json::from_value::<StudyDetail>(snapshot["detail"].clone()).ok()
        }
        _ => None,
    })?;
    let originals: Vec<_> = confirmed
        .assets
        .iter()
        .filter(|a| a.kind == "original")
        .collect();
    anyhow::ensure!(
        originals.len() == 1,
        "Original asset must survive alias normalization"
    );
    let selected = vec![originals[0].id];
    for action in [
        WorkflowAction::FinishExamination,
        WorkflowAction::FinishProtocol,
    ] {
        let request = Uuid::new_v4();
        tx.send(Command::Workflow {
            study: confirmed.study.id,
            request,
            action,
            close_preview: false,
        })?;
        let result = wait(&rx, |e| match e {
            Event::Workflow {
                request: id,
                result,
                ..
            } if id == request => Some(result),
            _ => None,
        })?;
        result.map_err(anyhow::Error::msg)?;
    }
    tx.send(Command::Preview {
        study: confirmed.study.id,
        request: Uuid::new_v4(),
        selected: selected.clone(),
        template: report_template.clone(),
    })?;
    let (lines, photos, identity) = wait(&rx, |e| match e {
        Event::Preview { lines, photos, identity, .. } => Some((lines, photos, identity)),
        _ => None,
    })?;
    anyhow::ensure!(
        photos.len() == 1 && lines.iter().any(|s| s.contains("Ацетобелый")),
        "Native report must include selected photo and full defect name"
    );
    let pdf = root.join("native-report.pdf");
    tx.send(Command::Pdf {
        request: Uuid::new_v4(),
        study: confirmed.study.id,
        selected,
        template: report_template,
        expected: identity,
        output: ReportOutput::Export(pdf.clone()),
    })?;
    wait(&rx, |e| match e {
        Event::ReportOutput {result:Ok(ReportCompletion::Exported(path)),..} => Some(path),
        _ => None,
    })?;
    anyhow::ensure!(
        std::fs::read(&pdf)?.starts_with(b"%PDF"),
        "PDF must be a real Rust-rendered PDF"
    );
    wait(&rx, |e| match e {
        Event::State { snapshot, .. }
            if snapshot["sync"]["pending"] == 0 && snapshot["sync"]["durable"] == true =>
        {
            Some(())
        }
        _ => None,
    })?;
    tx.send(Command::Logout)?;
    wait(&rx, |e| match e {
        Event::LoggedOut => Some(()),
        _ => None,
    })?;
    println!(
        "Native API smoke passed: login, patient, study, import, alias normalization, clinical callout, native form, preview, PDF, backend receipts, logout"
    );
    Ok(())
}

fn preferred_version(workspace: &Workspace) -> Option<Uuid> {
    let versions: Vec<TerminologyVersion> = workspace
        .connection
        .as_ref()?
        .offline
        .as_ref()?
        .cached("/api/v1/terminology/versions")
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    versions
        .iter()
        .find(|v| v.code == "IFCPC-2011-DRAFT-v1")
        .map(|v| v.id)
}

fn audit_editor(audit:&m53_client::activity::Journal,study:Uuid,before:&Session,after:&Session) {
    if !before.photos.is_empty() && before.photos.get(before.selected).map(|p|p.id)!=after.photos.get(after.selected).map(|p|p.id) {
        if let Some(photo)=after.photos.get(after.selected) {audit.record(Audit::new("image.selected").study(study).entity(photo.id));}
    }
    for photo in &after.photos {
        if let Some(old)=before.photos.iter().find(|p|p.id==photo.id) {
            for action in m53_client::activity::annotations(study,&old.annotations,&photo.annotations) {audit.record(action);}
            for id in old.hidden.iter().chain(photo.hidden.iter()).filter(|id|old.hidden.contains(id)!=photo.hidden.contains(id)) {audit.record(Audit::new("protocol.field_changed").study(study).entity(*id).field("annotation.visibility"));}
            for (changed,code) in [(old.in_report!=photo.in_report,"image.in_report"),(old.removed!=photo.removed,"image.removed")] {
                if changed {audit.record(Audit::new("protocol.field_changed").study(study).entity(photo.id).field(code));}
            }
        }
    }
}
