#[path = "header_view.rs"]
mod header_view;
#[path = "guidance_view.rs"]
mod guidance_view;
#[path = "report_ui.rs"]
mod report_ui;
#[path = "registry_ui.rs"]
mod registry_ui;
#[path = "history_ui.rs"]
mod history_ui;
#[cfg(not(windows))]
#[path = "diagnosis_proof.rs"]
mod diagnosis_proof;
#[cfg(not(windows))]
#[path = "readonly_form_proof.rs"]
mod readonly_form_proof;
use crate::{
    Editor,
    clinic::{self, Command, Event, ReportOutput, ReportCompletion, LoginProfile},
    native_form::NativeForm,
    phrase_panel::{PhrasePanel,PhrasePanelEvent},
    visit_template_panel::{VisitTemplatePanel,VisitTemplatePanelEvent},
    patient_table::PatientsTable,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    date_picker::{DatePicker, DatePickerState},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu, PopupMenuItem},
    table::{DataTable, TableEvent, TableState},
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::{prelude::*, *};
use m53_client::care_path::{self, Action as WorkflowAction, Status as WorkflowStatus};
use m53_client::patient_demographics::{PatientSex, display_sex};
use m53_client::pedal::{
    self, Config as PedalConfig, Connection as PedalConnection, Latch as PedalLatch,
};
use m53_client::video;
use m53_contracts::workflow::*;
use m53_domain::validation::Dictionary;
use m53_gpui_editor::registry::{StudySummary, display_date, patient_name, patient_rows};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::mpsc, time::Duration};
use uuid::Uuid;

#[derive(Clone, Copy, PartialEq)]
pub enum Screen {
    Patients,
    Study,
    Protocol,
    Report,
    Settings,
}
fn accepts_preview(expected: Option<Uuid>, current_study: Option<Uuid>, study: Uuid, request: Uuid) -> bool {
    expected==Some(request) && current_study==Some(study)
}
fn update_stale_video_note(note: &mut String) -> bool {
    const STALE: &str = "Нет свежих кадров — съемка недоступна";
    if note == STALE { return false; }
    *note = STALE.into();
    true
}
fn report_photo_state(session: &m53_gpui_editor::Session) -> Vec<Value> {
    session.photos.iter().map(|photo| {
        json!([photo.id, photo.in_report, photo.removed, photo.annotations, photo.hidden])
    }).collect()
}
fn report_operation_active(preview: Option<Uuid>, output: Option<Uuid>) -> bool {
    preview.is_some() || output.is_some()
}
fn checkpoint_ready(state: &Value) -> bool {
    state["sync"].get("checkpoint_ready").unwrap_or(&state["sync"]["durable"]) == true
}
fn visible_protocol_form<'a,T>(historical:Option<&'a T>,remote:Option<&'a T>,current:Option<&'a T>)->Option<&'a T> {
    historical.or(remote).or(current)
}

#[cfg(test)]
#[::core::prelude::v1::test]
fn report_photo_state_ignores_view_selection_and_loaded_dimensions() {
    let mut session = m53_gpui_editor::Session::default();
    session.photos.push(m53_gpui_editor::Photo::new(1, 1));
    session.photos.push(m53_gpui_editor::Photo::new(1, 1));
    let initial = report_photo_state(&session);
    session.selected = 1;
    session.photos[0].width = 1000;
    assert_eq!(report_photo_state(&session), initial);
    session.photos[0].in_report = !session.photos[0].in_report;
    assert_ne!(report_photo_state(&session), initial);
}

fn accepts_visit_application(expected_study:Uuid, current_study:Option<Uuid>, expected_generation:u64, current_generation:Option<u64>, editable:bool) -> bool {
    editable && current_study==Some(expected_study) && current_generation==Some(expected_generation)
}
fn protocol_notice(approved:bool, problems:&[String]) -> (String,bool) {
    if approved {return ("Утверждён".into(),true);}
    if problems.is_empty() {return ("Протокол готов к утверждению".into(),true);}
    let fields:Vec<_>=problems.iter().map(|problem| {
        problem.strip_prefix("Заполните поле «").and_then(|p|p.strip_suffix('»'))
            .unwrap_or(problem).replace(['\n','\r']," ")
    }).collect();
    (format!("Перед утверждением: {}",fields.join(", ")),false)
}
#[cfg(test)]
mod preview_tests {
    use super::accepts_preview;
    use uuid::Uuid;
    #[test]
    fn recovered_checkpoint_allows_close_without_claiming_missing_files_saved() {
        assert!(super::checkpoint_ready(&serde_json::json!({"sync":{"checkpoint_ready":true,"durable":false,"error":"missing original"}})));
        assert!(!super::checkpoint_ready(&serde_json::json!({"sync":{"checkpoint_ready":false,"durable":true}})));
        assert!(super::checkpoint_ready(&serde_json::json!({"sync":{"durable":true}})));
    }
    #[test]
    fn independent_io_completion_preserves_the_active_pdf_operation() {
        let output=Uuid::new_v4();
        assert!(super::report_operation_active(None,Some(output)));
        assert!(super::report_operation_active(Some(Uuid::new_v4()),None));
        assert!(!super::report_operation_active(None,None));
    }
    #[test]
    fn mark_navigation_consumes_the_visible_protocol_form() {
        let current=1;let historical=2;let remote=3;
        assert_eq!(super::visible_protocol_form(Some(&historical),Some(&remote),Some(&current)),Some(&historical));
        assert_eq!(super::visible_protocol_form(None,Some(&remote),Some(&current)),Some(&remote));
        assert_eq!(super::visible_protocol_form(None,None,Some(&current)),Some(&current));
    }
    #[test]
    fn stale_video_status_notifies_only_on_transition() {
        let mut note = "Видеосигнал поступает".to_owned();
        assert!(super::update_stale_video_note(&mut note));
        assert!(!super::update_stale_video_note(&mut note));
    }
    #[test]
    fn old_success_or_failure_cannot_replace_a_new_preview_of_the_same_study() {
        let study=Uuid::new_v4(); let first=Uuid::new_v4(); let latest=Uuid::new_v4();
        assert!(!accepts_preview(Some(latest),Some(study),study,first));
        assert!(accepts_preview(Some(latest),Some(study),study,latest));
        assert!(!accepts_preview(None,Some(study),study,latest));
        assert!(!accepts_preview(Some(latest),None,study,latest));
        assert!(!accepts_preview(Some(latest),Some(Uuid::new_v4()),study,latest));
    }
    #[test]
    fn visit_template_response_cannot_overwrite_another_or_edited_study() {
        let study=Uuid::new_v4();
        assert!(super::accepts_visit_application(study,Some(study),4,Some(4),true));
        assert!(!super::accepts_visit_application(study,Some(Uuid::new_v4()),4,Some(4),true));
        assert!(!super::accepts_visit_application(study,Some(study),4,Some(5),true));
        assert!(!super::accepts_visit_application(study,Some(study),4,Some(4),false));
    }
    #[test]
    fn approval_notice_keeps_every_problem_in_one_line_and_distinguishes_receipt() {
        let problems=(1..=8).map(|i|format!("Заполните поле «Поле {i}»")).collect::<Vec<_>>();
        let (text,ready)=super::protocol_notice(false,&problems);
        assert!(!ready);assert!(text.contains("Поле 1, Поле 2"));assert!(text.ends_with("Поле 8"));assert!(!text.contains('\n'));
        assert_eq!(super::protocol_notice(false,&[]),("Протокол готов к утверждению".into(),true));
        assert_eq!(super::protocol_notice(true,&problems),("Утверждён".into(),true));
    }
}
/// Keep transport and audit failures visible alongside transient UI notices.
fn footer_message(ui: &str, worker: &str, sync_error: &str, activity_error: &str) -> String {
    let mut notices = Vec::new();
    for notice in [if ui.is_empty() { worker } else { ui }, sync_error, activity_error] {
        if !notice.is_empty() && !notices.contains(&notice) {
            notices.push(notice);
        }
    }
    notices.join(" · ")
}
#[cfg(test)]
mod footer_message_tests {
    use super::footer_message;

    #[test]
    fn local_success_notice_does_not_hide_transport_failure() {
        assert_eq!(
            footer_message("Этап обследования сохранен", "", "Сервер отклонил запись: конфликт", ""),
            "Этап обследования сохранен · Сервер отклонил запись: конфликт"
        );
    }
    #[test]
    fn cleared_close_notice_leaves_authoritative_status_and_both_failures_visible() {
        // The server-save status is rendered separately; no inferred delivery notice here.
        assert_eq!(footer_message("", "", "", ""), "");
        assert_eq!(
            footer_message("", "", "Нет связи с сервером", "Журнал действий не записан"),
            "Нет связи с сервером · Журнал действий не записан"
        );
    }
    #[test]
    fn same_failure_from_ui_and_snapshot_is_displayed_once() {
        let failure = "Не удалось сохранить локальную запись";
        assert_eq!(footer_message(failure, "Другой статус", failure, failure), failure);
        assert_eq!(footer_message("", failure, failure, ""), failure);
        assert_eq!(footer_message("Ошибка ввода", "", "", ""), "Ошибка ввода");
    }
}
pub struct Workplace {
    editor: Entity<Editor>,
    form: Option<Entity<NativeForm>>,
    remote_form: Option<Entity<NativeForm>>,
    phrase_panel: Entity<PhrasePanel>,
    visit_panel: Entity<VisitTemplatePanel>,
    visit_study: Option<Uuid>,
    visit_request: Option<(Uuid, Uuid, u64, Option<i64>)>,
    visit_busy: bool,
    visit_confirmation: Option<(Uuid,u64)>,
    viewing_remote: bool,
    worker: mpsc::Sender<Command>,
    events: mpsc::Receiver<Event>,
    state: Value,
    patients: Vec<Patient>,
    patient_table: Entity<TableState<PatientsTable>>,
    patient_editor: Option<Entity<crate::patient_editor::PatientEditor>>,
    patient_editor_pending: Option<Value>,
    patient_editor_subscription: Option<Subscription>,
    patient_edit_request: Option<(Uuid,Uuid)>,
    patient_edit_access: Option<(Uuid,Uuid,bool)>,
    history: Option<crate::protocol_history::HistoryState>,
    study_summaries: BTreeMap<Uuid, StudySummary>,
    patient_sexes: BTreeMap<Uuid, PatientSex>,
    preview_study: Option<Uuid>,
    doctors: Vec<LoginProfile>,
    manual_login: bool,
    colposcope: pedal::ColposcopeConfig,
    colposcope_custom: Entity<InputState>,
    video_worker: video::Worker,
    video_devices: Vec<video::Device>,
    video_config: Option<video::Config>,
    video_running: bool,
    video_pending: bool,
    video_generation: u64,
    video_epoch: u64,
    video_note: String,
    video_image: Option<std::sync::Arc<RenderImage>>,
    video_painted: Option<std::sync::Arc<RenderImage>>,
    pedal_tx: mpsc::Sender<pedal::Request>,
    pedal_events: mpsc::Receiver<pedal::Event>,
    pedal_draft: PedalConfig,
    pedal_applied: PedalConfig,
    pedal_connection: PedalConnection,
    pedal_checked: std::time::Instant,
    pedal_latch: PedalLatch,
    pedal_test_latch: PedalLatch,
    pedal_testing: bool,
    pedal_saving: bool,
    pedal_counts: (u64, u64, u64),
    pedal_note: String,
    pedal_focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
    studies: Vec<Study>,
    templates: Vec<Template>,
    detail: Option<StudyDetail>,
    dictionary: Option<Dictionary>,
    patient: Option<Uuid>,
    screen: Screen,
    settings_return: Screen,
    workflow: WorkflowStatus,
    workflow_request: Option<(Uuid, WorkflowAction)>,
    opening: bool,
    opening_registry_draft: bool,
    url: Entity<InputState>,
    transport_address: Option<String>,
    key: Entity<InputState>,
    login: Entity<InputState>,
    password: Entity<InputState>,
    query: Entity<InputState>,
    query_due: Option<std::time::Instant>,
    task_date: chrono::NaiveDate,
    family: Entity<InputState>,
    given: Entity<InputState>,
    patronymic: Entity<InputState>,
    birth: Entity<DatePickerState>,
    sex: Option<PatientSex>,
    show_new: bool,
    new_patient_check: Option<(Uuid,NewPatient,Option<PatientSex>)>,
    new_patient_matches: Vec<Patient>,
    auth_busy: bool,
    logging_out: bool,
    message: String,
    sent_form: u64,
    form_ack: u64,
    report_lines: Vec<String>,
    report_pages: Vec<std::sync::Arc<RenderImage>>,
    report_photos: Vec<(Uuid, std::sync::Arc<RenderImage>)>,
    report_busy: bool,
    report_output_request: Option<Uuid>,
    preview_request: Option<Uuid>,
    study_edited: bool,
    opened_photo_state: Vec<Value>,
    audit_close_requested: bool,
    audit_close_ready: bool,
    audit_camera_active: bool,
    report_identity: Option<(Uuid, Uuid, String)>,
    delivery_seen: BTreeMap<Uuid, bool>,
    approval_busy: bool,
    approval_pending: bool,
    approval_message: String,
    preview_mode: bool,
    guidance: m53_client::ui_guidance::Snapshot,
    diagnoses: m53_client::diagnosis_cache::Snapshot,
    priority_draft: Option<m53_client::diagnosis_priority::Draft>,
    priority_search: Entity<InputState>,
    priority_query: String,
    priority_busy: bool,
    priority_request: Option<Uuid>,
    priority_message: String,
    guidance_pending: bool,
    guidance_closed: bool,
    viewport: Size<Pixels>,
    report_view: crate::report_view::ReportView,
    report_scroll: ScrollHandle,
    report_history_expanded: bool,
    _appearance: Subscription,
}
impl Workplace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (worker, events) = clinic::start();
        let (pedal_tx, pedal_events) = pedal::start();
        let video_worker = video::start();
        let _ = video_worker.tx.send(video::Request::Enumerate);
        #[cfg(all(any(windows, target_os = "linux"), feature = "standalone"))]
        let settings = m53_client::credentials::load().unwrap_or_default();
        #[cfg(all(windows, not(feature = "standalone")))]
        let settings = m53_client::credentials::load()
            .unwrap_or_else(|_| (m53_client::transport::DEFAULT_ORIGIN.into(), String::new()));
        #[cfg(not(any(windows, all(target_os = "linux", feature = "standalone"))))]
        let settings: (String, String) = ("http://127.0.0.1:8787".into(), String::new());
        let url = cx.new(|cx| InputState::new(window, cx).default_value(settings.0));
        let key = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .default_value(settings.1)
        });
        let login = cx.new(|cx| InputState::new(window, cx).placeholder("Логин врача"));
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Пароль")
        });
        let query = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Поиск по ФИО, дате рождения или внешнему ID")
        });
        let family = cx.new(|cx| InputState::new(window, cx).placeholder("Фамилия"));
        let given = cx.new(|cx| InputState::new(window, cx).placeholder("Имя"));
        let patronymic = cx.new(|cx| InputState::new(window, cx).placeholder("Отчество"));
        let birth = cx.new(|cx| m53_gpui_editor::calendar_locale::date_state(window, cx));
        let editor = cx.new(|cx| Editor::new(window, cx, None));
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            crate::appearance::sync_system(Some(window), cx);
            cx.notify();
        });
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                if view.update(cx, |s, cx| s.poll(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        let patient_table = cx.new(|cx| {
            TableState::new(PatientsTable::with_worker(worker.clone()), window, cx)
                .row_selectable(true)
                .col_selectable(false)
        });
        let phrase_panel=cx.new(|cx|PhrasePanel::new(window,cx));
        let visit_panel=cx.new(|cx|VisitTemplatePanel::new(window,cx));
        let subscriptions = vec![
            cx.subscribe_in(&phrase_panel,window,|s,_,event:&PhrasePanelEvent,window,cx|s.phrase_event(event.clone(),window,cx)),
            cx.subscribe_in(&visit_panel,window,|s,_,event:&VisitTemplatePanelEvent,_,cx|s.visit_event(event.clone(),cx)),
            cx.observe_window_activation(window, |s, window, _| {
                if !window.is_window_active() {
                    s.pedal_latch.reset();
                    s.pedal_test_latch.reset();
                }
            }),
            cx.subscribe_in(&password, window, |s, _, event: &InputEvent, window, cx| {
                if matches!(
                    event,
                    InputEvent::PressEnter {
                        secondary: false,
                        shift: false
                    }
                ) {
                    s.authenticate(window, cx);
                }
            }),
            cx.subscribe(&query, |s, _, event: &InputEvent, _cx| {
                if matches!(event, InputEvent::Change) {
                    s.query_due=Some(std::time::Instant::now()+Duration::from_millis(300));
                }
            }),
            cx.subscribe(&patient_table, |s, table, event: &TableEvent, cx| {
                if let TableEvent::SelectRow(index) = event {
                    if s.patient_registry_locked(){return;}
                    let id = table.read(cx).delegate().rows.get(*index).map(|r| r.id);
                    if let Some(id) = id {
                        table.update(cx, |t, _| t.delegate_mut().selected = Some(id));
                        if s.patient != Some(id) {
                            s.preview_study = None;
                            s.patient_edit_access = None;
                        }
                        if s.patient != Some(id) {s.send(Command::Activity(m53_client::activity::Action::new("patient.selected").entity(id)));}
                        s.patient = Some(id);
                        s.send(Command::Registry(crate::registry_browser::Request::Patient(id)));
                        cx.notify();
                    }
                }
            }),
        ];
        let _ = worker.send(Command::LoginProfiles(
            url.read(cx).value().to_string(),
            key.read(cx).value().to_string(),
        ));
        Self {
            editor,
            form: None,
            remote_form: None,
            phrase_panel,
            visit_panel,
            visit_study: None,
            visit_request: None,
            visit_busy: false,
            visit_confirmation: None,
            viewing_remote: false,
            worker,
            events,
            state: json!({}),
            patients: vec![],
            patient_table,
            study_summaries: BTreeMap::new(),
            patient_sexes: BTreeMap::new(),
            preview_study: None,
            doctors: vec![],
            manual_login: cfg!(feature = "standalone"),
            colposcope: pedal::ColposcopeConfig::default(),
            colposcope_custom: cx
                .new(|cx| InputState::new(window, cx).placeholder("Другая модель кольпоскопа")),
            video_worker,
            video_devices: vec![],
            video_config: None,
            video_running: false,
            video_pending: false,
            video_generation: 0,
            video_epoch: 0,
            video_note: "Выберите устройство видеозахвата".into(),
            video_image: None,
            video_painted: None,
            pedal_tx,
            pedal_events,
            pedal_draft: PedalConfig::default(),
            pedal_applied: PedalConfig::default(),
            pedal_connection: PedalConnection::Checking,
            pedal_checked: std::time::Instant::now(),
            pedal_latch: PedalLatch::default(),
            pedal_test_latch: PedalLatch::default(),
            pedal_testing: false,
            pedal_saving: false,
            pedal_counts: (0, 0, 0),
            pedal_note: String::new(),
            pedal_focus: cx.focus_handle(),
            _subscriptions: subscriptions,
            studies: vec![],
            templates: vec![],
            detail: None,
            dictionary: None,
            patient: None,
            screen: Screen::Patients,
            settings_return: Screen::Patients,
            workflow: WorkflowStatus::default(),
            workflow_request: None,
            opening: false,
            opening_registry_draft: false,
            url,
            key,
            login,
            password,
            query,
            query_due: None,
            task_date: chrono::Local::now().date_naive(),
            family,
            given,
            patronymic,
            birth,
            sex: None,
            show_new: false,
            new_patient_check: None,
            new_patient_matches: Vec::new(),
            auth_busy: false,
            logging_out: false,
            message: String::new(),
            sent_form: 0,
            form_ack: 0,
            report_lines: vec![],
            report_pages: vec![],
            report_photos: vec![],
            report_busy: false,
            report_output_request: None,
            preview_request: None,
            study_edited: false,
            opened_photo_state: Vec::new(),
            audit_close_requested: false,
            audit_close_ready: false,
            audit_camera_active: false,
            report_identity: None,
            delivery_seen: BTreeMap::new(),
            approval_busy: false,
            approval_pending: false,
            approval_message: String::new(),
            preview_mode: false,
            transport_address: None,
            guidance: Default::default(),
            diagnoses: Default::default(),
            priority_draft: None,
            priority_search: cx.new(|cx| InputState::new(window, cx).placeholder("Код МКБ‑10 или название")),
            priority_query: String::new(),
            priority_busy: false,
            priority_request: None,
            patient_editor: None,
            patient_editor_pending: None,
            patient_editor_subscription: None,
            patient_edit_request: None,
            patient_edit_access: None,
            history: None,
            priority_message: String::new(),
            guidance_pending: false,
            guidance_closed: false,
            viewport: window.viewport_size(),
            report_view: Default::default(),
            report_scroll: ScrollHandle::new(),
            report_history_expanded: false,
            _appearance: appearance,
        }
    }
    fn normalize_ids(&mut self, aliases: &Value, cx: &mut Context<Self>) {
        if aliases.as_object().is_none_or(|a| a.is_empty())
            || self.state["native_aliases"] == *aliases { return; }
        fn normalize(value: &mut Value, aliases: &Value) {
            match value {
                Value::String(s) => {
                    for _ in 0..aliases.as_object().map_or(0, |a| a.len()) {
                        if let Some(new) = aliases.get(s.as_str()).and_then(Value::as_str) {
                            *s = new.into();
                        } else {
                            break;
                        }
                    }
                }
                Value::Array(a) => {
                    for v in a {
                        normalize(v, aliases);
                    }
                }
                Value::Object(a) => {
                    for v in a.values_mut() {
                        normalize(v, aliases);
                    }
                }
                _ => (),
            }
        }
        let map_id = |id: Uuid| {
            let mut v = json!(id);
            normalize(&mut v, aliases);
            serde_json::from_value::<Uuid>(v).unwrap_or(id)
        };
        if let Some(d) = &self.detail {
            let mut value = json!(d);
            normalize(&mut value, aliases);
            self.detail = serde_json::from_value(value).ok();
        }
        self.patient = self.patient.map(map_id);
        if let Some(snapshot)=self.history.as_mut().and_then(|history|history.editor_snapshot.as_mut()) {
            let mut value=json!(snapshot.session);
            normalize(&mut value,aliases);
            if let Ok(session)=serde_json::from_value(value) {snapshot.session=session;}
            snapshot.selected=snapshot.selected.map(map_id);
        }
        self.editor.update(cx, |e, _| {
            let mut value = json!(e.session);
            normalize(&mut value, aliases);
            if let Ok(session) = serde_json::from_value(value) {
                e.session = session;
            }
            if let Some((id, _)) = &mut e.clinic {
                *id = map_id(*id);
            }
            let old = std::mem::take(&mut e.images);
            e.images = old
                .into_iter()
                .map(|(id, image)| (map_id(id), image))
                .collect();
            e.thumbnails=std::mem::take(&mut e.thumbnails).into_iter().map(|(id,image)|(map_id(id),image)).collect();
            e.photo_demand=None;
        });
        if let Some(form) = &self.form {
            form.update(cx, |f, _| {
                normalize(&mut f.value, aliases);
                normalize(&mut f.base, aliases);
                f.invalidate_validation();
            });
        }
    }
    fn update_phrase_target(&mut self,cx:&mut Context<Self>) {
        let target=if self.screen==Screen::Protocol && !self.viewing_remote && !self.approved_history() {self.form.as_ref().and_then(|f|f.read(cx).active_phrase_target())}else{None};
        let allowed=self.detail.as_ref().is_some_and(|d|d.study.status!="approved")&&!self.viewing_remote&&!self.logging_out&&!self.approved_history();
        let changed=self.phrase_panel.update(cx,|p,cx|p.set_target(target.clone(),allowed,cx));
        if changed {if let Some(target)=target {self.send(Command::PhraseField(target.field_code));}}
    }
    fn phrase_event(&mut self,event:PhrasePanelEvent,window:&mut Window,cx:&mut Context<Self>) {
        match event {
            PhrasePanelEvent::Apply{target,template,version,confirmed,text,replace}=>{
                if self.screen!=Screen::Protocol||self.viewing_remote||self.approved_history()||self.detail.as_ref().is_none_or(|d|d.study.status=="approved") {self.message="Этот протокол доступен только для чтения".into();cx.notify();return;}
                let result=self.form.as_ref().ok_or("Нет активной формы".to_string()).and_then(|form|form.update(cx,|f,cx|f.apply_phrase(&target,&text,replace,window,cx)));
                match result {Ok(result)=>{if result.changed {self.flush_form(cx,true);self.message=if replace{"Текст поля заменён шаблоном"}else{"Фраза добавлена в активное поле"}.into();
                    let operation=Uuid::new_v4();
                    let mut action=m53_client::activity::Action::new("protocol.field_changed").field(result.field_code.clone());action.study_id=self.detail.as_ref().map(|d|d.study.id);action.entity_id=result.mark_id;action.operation_id=Some(operation);self.send(Command::Activity(action));
                    let mut applied=m53_client::activity::Action::new(if confirmed{"phrase.applied"}else{"phrase.local_applied"}).field(result.field_code);
                    applied.study_id=self.detail.as_ref().map(|d|d.study.id);applied.operation_id=Some(operation);
                    if confirmed {applied.entity_id=result.mark_id;applied.phrase_template_id=Some(template);applied.phrase_template_version=Some(version);applied.apply_mode=Some(if replace{"replace"}else{"append"}.into());}else{applied.entity_id=Some(template);}
                    if applied.valid(){self.send(Command::Activity(applied));}
                }},Err(error)=>self.message=error};cx.notify();
            },
            PhrasePanelEvent::EditorOpened{field_code,template}=>{let mut action=m53_client::activity::Action::new("phrase.editor_opened").field(field_code);action.entity_id=template;action.operation_id=Some(Uuid::new_v4());self.send(Command::Activity(action));},
            PhrasePanelEvent::Save{id,field_code,scope,title,body}=>self.send(Command::PhraseSave{id,field_code,scope,title,body}),
            PhrasePanelEvent::Preferences{replace,include_shared}=>self.send(Command::PhrasePreferences{replace,include_shared}),
            PhrasePanelEvent::Resolve{operation,keep_local}=>self.send(Command::PhraseResolve{operation,keep_local}),
        }
    }
    fn send(&mut self, command: Command) {
        if self.worker.send(command).is_err() {
            self.message = "Не работает локальный обработчик".into();
        }
    }
    fn send_report_output(&mut self, request:Uuid, command:Command) {
        self.report_output_request=Some(request);
        self.report_busy=true;
        if self.worker.send(command).is_err() {
            self.report_output_request=None;
            self.report_busy=self.preview_request.is_some();
            self.message="Не работает локальный обработчик отчёта".into();
        }
    }
    fn request_approval(&mut self, cx: &mut Context<Self>) {
        if self.history.is_some(){self.message="Историческую редакцию нельзя утверждать повторно. Вернитесь к текущему приёму".into();cx.notify();return;}
        if self.approval_busy || self.approval_pending || self.workflow_request.is_some()
            || self.detail.as_ref().is_none_or(|d|d.study.status=="approved") { return; }
        self.approval_busy = true;
        self.approval_message = "Проверяем обязательные поля и сохраняем протокол…".into();
        self.message = self.approval_message.clone();
        self.flush_form(cx, true);
        self.send(Command::Approve);
        cx.notify();
    }
    fn visit_editable(&self, cx:&Context<Self>) -> bool {
        self.screen==Screen::Protocol && !self.approved_history() && !self.viewing_remote && !self.approval_busy && !self.logging_out && self.workflow_request.is_none()
            && self.detail.as_ref().is_some_and(|d| d.study.status!="approved") && self.form.as_ref().is_some_and(|f| !f.read(cx).readonly)
    }
    fn visit_notice(&mut self, text: impl Into<String>, error: bool, cx:&mut Context<Self>) {
        self.visit_busy=false;
        self.visit_panel.update(cx,|p,cx| {p.set_state(false,false,cx);p.set_message(text,error,cx)});
    }
    fn visit_event(&mut self, event:VisitTemplatePanelEvent, cx:&mut Context<Self>) {
        if matches!(event,VisitTemplatePanelEvent::Confirming) {
            self.visit_confirmation=self.detail.as_ref().and_then(|d|self.form.as_ref().map(|f|(d.study.id,f.read(cx).generation)));
            return;
        }
        if matches!(event,VisitTemplatePanelEvent::Refresh) {
            if self.visit_busy {return;}
            if let Some(study)=self.visit_study {let request=Uuid::new_v4();self.visit_request=Some((request,study,0,None));self.visit_busy=true;self.send(Command::VisitTemplatesList {request,after:None});}
            return;
        }
        if self.visit_busy || !self.visit_editable(cx) {self.visit_notice("Изменение протокола сейчас недоступно",true,cx);return;}
        let Some(study)=self.detail.as_ref().map(|d|d.study.id) else {return;};
        let Some(form)=self.form.clone() else {return;};
        if self.visit_confirmation.take()!=Some((study,form.read(cx).generation)) {self.visit_notice("Протокол изменился. Повторите действие и подтверждение",true,cx);return;}
        self.flush_form(cx,true);
        let generation=form.read(cx).generation;
        match event {
            VisitTemplatePanelEvent::Refresh | VisitTemplatePanelEvent::Confirming => (),
            VisitTemplatePanelEvent::Clear => {
                match form.update(cx,|f,cx|f.replace_visit_fields(None,cx)) {
                    Ok(())=>{self.flush_form(cx,true);self.visit_notice("Поля приёма очищены",false,cx);},
                    Err(error)=>self.visit_notice(error,true,cx),
                }
            },
            VisitTemplatePanelEvent::Apply(selected) => {
                let request=Uuid::new_v4();self.visit_request=Some((request,study,generation,Some(selected.row_version)));self.visit_busy=true;
                self.send(Command::VisitTemplateGet {request,id:selected.id});
            },
            VisitTemplatePanelEvent::Save {title,overwrite} => {
                if !form.read(cx).unfinished.is_empty() {self.visit_notice("Завершите ввод в полях перед сохранением шаблона",true,cx);return;}
                let value=form.read(cx).value.clone();
                let template=serde_json::from_value::<m53_domain::protocol::Protocol>(value).map_err(|_|"Не удалось прочитать протокол".to_string()).and_then(|p|m53_domain::visit_template::VisitTemplate::capture(&p));
                match template {
                    Ok(protocol)=>{let request=Uuid::new_v4();let id=overwrite.as_ref().map_or_else(Uuid::new_v4,|s|s.id);let expected_version=overwrite.map(|s|s.row_version);
                        self.visit_request=Some((request,study,generation,expected_version));self.visit_busy=true;
                        self.send(Command::VisitTemplateSave {request,id,expected_version,title,protocol});},
                    Err(error)=>self.visit_notice(error,true,cx),
                }
            }
        }
    }
    fn study_changed(&self, cx: &Context<Self>) -> bool {
        self.study_edited
            || self.form.as_ref().is_some_and(|form| form.read(cx).generation > 0)
            || (self.editor.read(cx).version > 0
                && report_photo_state(&self.editor.read(cx).session) != self.opened_photo_state)
    }

    fn flush_form(&mut self, cx: &mut Context<Self>, force: bool) {
        if self.approved_history() { return; }
        if let (Some(detail),Some(form))=(&self.detail,&self.form) {
            let study=detail.study.id;
            let (fields,sections)=form.update(cx,|f,_| {let fields=if force || f.changed.elapsed()>Duration::from_millis(600) {std::mem::take(&mut f.audit_fields)} else {Default::default()};(fields,std::mem::take(&mut f.audit_sections))});
            for path in fields {if let Some((code,entity))=m53_client::activity::field(&path) {let mut a=m53_client::activity::Action::new("protocol.field_changed").study(study).field(code);a.entity_id=entity;self.send(Command::Activity(a));}}
            for code in sections {self.send(Command::Activity(m53_client::activity::Action::new("protocol.section_opened").study(study).field(code)));}
        }
        if let (Some(detail), Some(form)) = (&self.detail, &self.form) {
            let form = form.read(cx);
            if form.generation > self.sent_form
                && (force || form.changed.elapsed() > Duration::from_millis(35))
            {
                let command = Command::Form {
                    study: detail.study.id,
                    version: form.generation,
                    value: form.value.clone(),
                    base: form.base.clone(),
                    unfinished: form.unfinished.clone(),
                };
                let generation = form.generation;
                let exam_date = form.value["colpo"]["fields"]["context.exam_date"]["value"]
                    .as_str().unwrap_or("").to_owned();
                self.editor.update(cx, |editor, _| {
                    if let Some((_, date)) = &mut editor.export_patient { *date=exam_date; }
                });
                self.study_edited = true;
                self.sent_form = generation;
                if let Some(entity) = &self.form {
                    entity.update(cx, |form, _| form.base = form.value.clone());
                }
                self.send(command);
                cx.notify();
            }
        }
    }
    fn patient_registry_locked(&self)->bool {
        self.patient_edit_request.is_some() || self.patient_editor_pending.is_some() || self.patient_editor.is_some()
    }
    fn can_open_patient_editor(&self)->bool {
        !self.patient_registry_locked() && !self.opening && !self.show_new
            && self.state["sync"]["online"]==true
            && self.patient_edit_access.is_some_and(|(patient,_,allowed)| allowed && self.patient==Some(patient))
    }
    fn poll(&mut self, cx: &mut Context<Self>) {
        if self.preview_mode { return; }
        let study=if self.screen==Screen::Protocol {self.detail.as_ref().map(|d|d.study.id)} else {None};
        if study!=self.visit_study {
            self.visit_study=study;self.visit_request=None;self.visit_confirmation=None;self.visit_busy=false;
            self.visit_panel.update(cx,|p,cx|{p.clear_pending(cx);p.set_templates(Vec::new(),cx);});
            if let Some(study)=study { let request=Uuid::new_v4();self.visit_request=Some((request,study,0,None));self.visit_busy=true;self.send(Command::VisitTemplatesList {request,after:None}); }
        }
        let readonly=!self.visit_editable(cx);
        self.visit_panel.update(cx,|p,cx|p.set_state(self.visit_busy,readonly,cx));
        self.update_phrase_target(cx);
        if self.query_due.is_some_and(|due| std::time::Instant::now()>=due) {
            self.query_due=None;
            self.send(Command::Registry(crate::registry_browser::Request::Query(self.query.read(cx).value().to_string())));
        }
        self.poll_video(cx);
        while let Ok(event) = self.pedal_events.try_recv() {
            match event {
                pedal::Event::Video(result) => {
                    match result {
                        Ok(config) => {
                            self.video_config = config;
                            if !self.video_running {
                                self.video_note =
                                    "Настройки видеозахвата загружены / сохранены".into();
                            }
                        }
                        Err(e) => self.video_note = e,
                    };
                    cx.notify();
                }
                pedal::Event::Colposcope(result) => {
                    match result {
                        Ok(config) => {
                            self.colposcope = config;
                            self.send(Command::Colposcope(self.colposcope.selected.clone()));
                        }
                        Err(e) => self.pedal_note = e,
                    };
                    cx.notify();
                }
                pedal::Event::Loaded(result) | pedal::Event::Saved(result) => {
                    if self.pedal_saving {self.send(Command::Activity(m53_client::activity::Action::new("settings.changed").field("pedal")));}
                    self.pedal_saving = false;
                    match result {
                        Ok(config) => {
                            self.pedal_draft = config;
                            self.pedal_applied = config;
                            self.pedal_latch.reset();
                            self.pedal_note =
                                "Настройки педали загружены / сохранены на этом компьютере".into();
                        }
                        Err(error) => self.pedal_note = error,
                    }
                }
                pedal::Event::Status(status) => {
                    self.pedal_checked = std::time::Instant::now();
                    if status == self.pedal_connection {
                        continue;
                    }
                    self.pedal_latch.reset();
                    self.pedal_test_latch.reset();
                    self.pedal_connection = status;
                }
            }
            cx.notify();
        }
        if self.pedal_checked.elapsed() > Duration::from_secs(15)
            && !matches!(self.pedal_connection, PedalConnection::Unknown(_))
        {
            self.pedal_connection =
                PedalConnection::Unknown("Проверка связи не завершилась вовремя".into());
            self.pedal_latch.reset();
            cx.notify();
        }
        self.flush_form(cx, false);
        self.navigate_requested_mark(cx);
        while let Ok(event) = self.events.try_recv() {
            self.receive(event, cx);
        }
        if self.state["authenticated"]!=true || self.state["sync"]["online"]!=true {
            if self.patient_edit_access.take().is_some(){cx.notify();}
        } else if self.screen==Screen::Patients && !self.patient_registry_locked() {
            if let Some(patient)=self.patient.filter(|id|self.patient_edit_access.is_none_or(|(p,_,_)|p!=*id)){
                let request=Uuid::new_v4();
                self.patient_edit_access=Some((patient,request,false));
                self.send(Command::ManualPatient{request,patient,body:None});
                cx.notify();
            }
        }
    }
    fn navigate_requested_mark(&mut self,cx:&mut Context<Self>) {
        let show=visible_protocol_form(
            self.history.as_ref().and_then(|history|history.form.as_ref()),
            self.remote_form.as_ref().filter(|_|self.history.is_none() && self.viewing_remote),
            self.form.as_ref().filter(|_|self.history.is_none() && !self.viewing_remote),
        ).and_then(|form|form.update(cx,|form,_|form.show_mark.take()));
        if let Some(id)=show {
            if self.history.is_none() {self.flush_form(cx,true);}
            let found=self.editor.update(cx, |e, cx| {
                if let Some(index) = e
                    .session
                    .photos
                    .iter()
                    .position(|p| p.annotations.iter().any(|a| a.id == id))
                {
                    e.session.selected = index;
                    e.cancel_marker_pick();
                    e.fit();
                    e.selected = Some(id);
                    cx.notify();
                    return true;
                }
                false
            });
            if found {
                if let Some(history)=self.history.as_mut().filter(|history|!history.prefer_approved) {
                    history.show_photo=true;
                } else {self.screen=Screen::Study;}
            } else if let Some(history)=self.history.as_mut() {
                history.note="Снимок этой метки недоступен в сохранённом обследовании".into();
            }
            cx.notify();
        }
    }
    fn receive(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::NewPatientMatches{request,result}=>{
                if !self.show_new || !self.new_patient_check.as_ref().is_some_and(|(id,_,_)|*id==request){return;}
                let (_,input,sex)=self.new_patient_check.take().unwrap();
                if self.screen!=Screen::Patients || self.state["authenticated"]!=true {cx.notify();return;}
                match result {
                    Ok(matches) if matches.is_empty()=>{self.show_new=false;self.send(Command::NewPatient(input,sex));},
                    Ok(matches)=>{self.new_patient_matches=matches;self.message="Найдены совпадающие карточки пациента".into();},
                    Err(error)=>self.message=format!("{error}. Введённые данные сохранены в форме"),
                }
            },
            Event::ProtocolHistory {request,study,revision,result}=>self.receive_history(request,study,revision,result,cx),
            #[cfg(windows)]
            Event::HistoricalPdf {request,study,revision,result}=>{
                if let Some(history)=self.history.as_mut().filter(|history| history.study==study
                    && history.selected==Some(revision) && history.pdf_request==Some(request)) {
                    history.pdf_request=None;
                    history.pdf_pages=Some(result);
                    cx.notify();
                } else if let Ok(pdf)=result {
                    for image in pdf.pages {cx.drop_image(image,None);}
                }
            },
            Event::ManualPatient {request,patient,saved,result} => {
                if !saved && self.patient_edit_access.is_some_and(|(id,probe,_)|id==patient && probe==request) {
                    if self.patient==Some(patient) && self.state["authenticated"]==true {
                        let allowed=result.as_ref().is_ok_and(|context|m53_client::patient_edit::edit_version(context,patient).is_ok());
                        self.patient_edit_access=Some((patient,request,allowed));
                    }
                    cx.notify();
                    return;
                }
                if self.patient_edit_request!=Some((request,patient)){return;}
                self.patient_edit_request=None;
                match result {
                    Ok(_context) if saved=>{
                        self.patient_edit_access=None;
                        self.patient_editor=None;self.patient_editor_subscription=None;
                        self.message="Карточка пациента сохранена на сервере. Перед утверждением выполните повторную сверку".into();
                        self.send(Command::Refresh);
                    },
                    Ok(mut context)=>{
                        if self.patient!=Some(patient){return;}
                        match m53_client::patient_edit::edit_version(&context,patient) {
                            Ok(_) => {
                                m53_client::patient_edit::prefill_manual_sex(
                                    &mut context, self.patient_sexes.get(&patient).copied());
                                self.patient_editor_pending=Some(context);
                            }
                            Err(e)=>{self.patient_edit_access=Some((patient,request,false));self.message=e;}
                        }
                    },
                    Err(error)=>{self.message=error.clone();if let Some(panel)=&self.patient_editor{panel.update(cx,|p,cx|p.set_result(error,cx));}},
                }
            },
            Event::VisitTemplatesList {request,result} => {
                if self.visit_request.is_none_or(|(id,study,_,_)| id!=request || self.visit_study!=Some(study)) {return;}
                self.visit_request=None;self.visit_busy=false;
                match result {Ok(items)=>{self.visit_panel.update(cx,|p,cx|{p.set_templates(items,cx);p.set_message("",false,cx);});},Err(error)=>self.visit_notice(error,true,cx)}
            },
            Event::VisitTemplateGet {request,result} => {
                let Some((expected,study,generation,version))=self.visit_request else {return;};
                if expected!=request {return;} self.visit_request=None;
                if !accepts_visit_application(study,self.visit_study,generation,self.form.as_ref().map(|f|f.read(cx).generation),self.visit_editable(cx)) {
                    self.visit_notice("Протокол изменился. Повторите выбор и применение шаблона",true,cx);return;
                }
                match result {
                    Ok(detail) if Some(detail.metadata.row_version)==version => {
                        let result=self.form.as_ref().unwrap().update(cx,|f,cx|f.replace_visit_fields(Some(&detail.protocol),cx));
                        match result {Ok(())=>{self.flush_form(cx,true);self.visit_notice("Шаблон применён",false,cx);},Err(error)=>self.visit_notice(error,true,cx)}
                    },
                    Ok(_)=>self.visit_notice("Шаблон изменился. Обновите список и повторите",true,cx),
                    Err(error)=>self.visit_notice(error,true,cx),
                }
            },
            Event::VisitTemplateSaved {request,result} => {
                let Some((expected,study,_,_))=self.visit_request else {return;};
                if expected!=request {return;} self.visit_request=None;self.visit_busy=false;
                if self.visit_study!=Some(study) {return;}
                match result {
                    Ok(_)=>{self.visit_panel.update(cx,|p,cx|p.save_confirmed(cx));self.visit_notice("Общий шаблон сохранён на сервере",false,cx);
                        let request=Uuid::new_v4();self.visit_request=Some((request,study,0,None));self.visit_busy=true;self.send(Command::VisitTemplatesList {request,after:None});},
                    Err(error)=>self.visit_notice(error,true,cx),
                }
            },
            Event::Guidance(snapshot) => {
                if self.guidance.accepts(&snapshot) {
                    if self.guidance.context != snapshot.context {
                        self.guidance_closed = false;
                        self.priority_draft = None;
                        self.priority_query.clear();
                        self.priority_message.clear();
                        self.priority_busy = false;
                        self.priority_request = None;
                    }
                    self.guidance = snapshot;
                    self.guidance_pending = false;
                }
            }
            Event::Phrases(snapshot) => self.phrase_panel.update(cx,|p,cx|p.set_snapshot(snapshot,cx)),
            Event::PhraseSaved(result) => self.phrase_panel.update(cx,|p,cx|p.saved(result,cx)),
            Event::DiagnosisPrioritySaved { request, result } => {
                if self.priority_request != Some(request) { return; }
                self.priority_busy = false;
                self.priority_request = None;
                match result {
                    Ok(()) => { self.priority_draft = None; self.priority_message = "Общая группа сохранена на сервере".into(); },
                    Err(error) => self.priority_message = error,
                }
            }
            Event::AuditClosed => {self.audit_close_ready=true; if self.audit_close_requested && self.can_close(cx) {cx.quit();}},
            Event::Workflow {
                study,
                request,
                action,
                result,
            } => {
                if self.workflow_request != Some((request, action)) {
                    return;
                }
                self.workflow_request = None;
                if self.detail.as_ref().is_none_or(|d| d.study.id != study) {
                    self.message = "Ответ сохранения относится к другому обследованию".into();
                    cx.notify();
                    return;
                }
                match result {
                    Ok(status) => {
                        self.workflow = status;
                        self.message = match action {
                            WorkflowAction::FinishExamination => {
                                self.guidance_closed = false;
                                "Этап обследования сохранен".into()
                            }
                            WorkflowAction::FinishProtocol => {
                                self.guidance_closed = false;
                                "Протокол сохранен; отчет готовится".into()
                            }
                            WorkflowAction::SaveAndExit => String::new(),
                        };
                        match action {
                            WorkflowAction::FinishExamination => {
                                self.navigate(Screen::Protocol, cx)
                            }
                            WorkflowAction::FinishProtocol => self.navigate(Screen::Report, cx),
                            WorkflowAction::SaveAndExit => {
                                #[cfg(windows)] self.release_history_pages(cx);
                                self.history = None;
                                self.guidance_closed = false;
                                self.preview_study = self.detail.as_ref().map(|d| d.study.id);
                                self.detail = None;
                                self.form = None;
                                self.remote_form = None;
                                self.dictionary = None;
                                self.workflow = WorkflowStatus::default();
                                self.sent_form = 0;
                                self.form_ack = 0;
                                self.report_lines.clear();
                                self.release_report_images(cx);
                                self.editor.update(cx, |e, cx| {
                                    e.clinic = None;
                                    e.ready = false;
                                    e.session = Default::default();
                                    e.clear_photo_cache(cx);
                                    e.selected = None;
                                    e.version = 0;
                                    e.saved = 0;
                                    cx.notify();
                                });
                                self.screen = Screen::Patients;
                                // Subsequent snapshots own the live server-save status. A static
                                // local-save notice would stay here even after the matching ACK.
                                self.message.clear();
                            }
                        }
                    }
                    Err(error) => self.message = error,
                }
                if let Some(jobs) = self.state["native_result_jobs"].as_array() {
                    for job in jobs {
                        if let Some(id) = job["job_id"].as_str().and_then(|v| Uuid::parse_str(v).ok()) {
                            let delivered = m53_client::delivery::delivered(job);
                            let before = self.delivery_seen.get(&id).copied();
                            self.delivery_seen.entry(id).and_modify(|value| *value |= delivered).or_insert(delivered);
                            if before == Some(false) && delivered {
                                self.message = format!("Доставлен в МИС · Приём № {}", job["receipt"]["reception_number"].as_str().unwrap_or("—"));
                            }
                        }
                    }
                }
                let readonly = self
                    .detail
                    .as_ref()
                    .is_some_and(|d| d.study.status == "approved")
                    || self.approval_busy;
                self.editor.update(cx, |e, _| e.readonly = readonly || e.historical_readonly);
                if let Some(form) = &self.form {
                    form.update(cx, |f, _| f.readonly = readonly);
                }
            }
            Event::ApprovalState(notice) => {
                self.approval_busy = notice.busy;
                self.approval_pending = notice.pending;
                self.approval_message = notice.message.clone();
                let readonly = self.detail.as_ref().is_some_and(|d|d.study.status == "approved")
                    || notice.editing_locked() || self.logging_out || self.workflow_request.is_some();
                self.message = notice.message;
                self.editor.update(cx, |e, _| e.readonly = readonly || e.historical_readonly);
                if let Some(form) = &self.form { form.update(cx, |f, _| f.readonly = readonly); }
            }
            Event::LoginProfiles(profiles) => {
                self.manual_login = profiles.is_empty();
                self.doctors = profiles;
            }
            Event::TransportAddress(url) => {
                self.transport_address = Some(url);
            }
            Event::State {
                snapshot,
                patients,
                studies,
                templates,
                diagnoses,
            } => {
                self.diagnoses = diagnoses;
                let template_changed = crate::report_view::default_template_changed(&self.templates,&templates,|t|t.version_id);
                let registry_changed = self.state["native_study_summaries"] != snapshot["native_study_summaries"]
                    || self.state["native_registry"] != snapshot["native_registry"]
                    || self.state["native_patient_sexes"] != snapshot["native_patient_sexes"]
                    || json!(self.patients) != json!(patients) || json!(self.studies) != json!(studies);
                self.normalize_ids(&snapshot["native_aliases"], cx);
                self.state = snapshot;
                self.workflow = serde_json::from_value(self.state["native_workflow"].clone())
                    .unwrap_or_default();
                if let Some(id)=self.patient {
                    let selected=|items:&[Patient]| items.iter().find(|p|p.id==id).map(|p|json!(p));
                    if selected(&self.patients)!=selected(&patients){self.patient_edit_access=None;}
                }
                self.patients = patients;
                self.studies = studies;
                self.templates = templates;
                if template_changed {
                    self.abandon_preview();
                    self.release_report_images(cx);
                    self.report_lines.clear();
                    self.report_identity = None;
                    if self.templates.is_empty() {
                        self.message = "Шаблон протокола недоступен".into();
                    } else if self.screen == Screen::Report && !self.report_busy && self.detail.is_some() {
                        self.preview(cx);
                    }
                }
                self.study_summaries =
                    serde_json::from_value(self.state["native_study_summaries"].clone())
                        .unwrap_or_default();
                self.patient_sexes =
                    serde_json::from_value(self.state["native_patient_sexes"].clone())
                        .unwrap_or_default();
                if registry_changed { self.refresh_patient_table(cx); }
                let remote = if self.state["remote"]["protocol"].is_object() {
                    self.state["remote"]["protocol"].clone()
                } else {
                    self.state["conflict"]["remote"]["protocol"].clone()
                };
                if self.state["conflict"].is_object() && remote.is_object() {
                    if let Some(dictionary) = &self.dictionary
                        && self
                            .remote_form
                            .as_ref()
                            .is_none_or(|f| f.read(cx).value != remote)
                    {
                        self.remote_form = Some(cx.new(|_| {
                            NativeForm::new(remote, dictionary.clone(), BTreeMap::new(), true)
                        }));
                    }
                } else {
                    self.remote_form = None;
                    self.viewing_remote = false;
                }

                if self.state["authenticated"] == true {
                    self.auth_busy = false;
                }
                if let Ok(detail) =
                    serde_json::from_value::<StudyDetail>(self.state["detail"].clone())
                    && self
                        .detail
                        .as_ref()
                        .is_some_and(|d| d.study.id == detail.study.id)
                {
                    self.editor.update(cx, |e, _| {
                        e.readonly = detail.study.status == "approved" || e.historical_readonly
                            || self.approval_busy
                            || self.logging_out
                            || self.workflow_request.is_some()
                    });
                    if let Some(form) = &self.form {
                        let can_sync = form.read(cx).generation == self.sent_form
                            && self.form_ack >= self.sent_form
                            && self.editor.read(cx).saved >= self.editor.read(cx).version
                            && form.read(cx).unfinished.is_empty();
                        let value = json!(detail.protocol);
                        if can_sync
                            && form.read(cx).value != value
                            && let Some(dictionary) = &self.dictionary
                        {
                            let mut new = NativeForm::new(
                                value,
                                dictionary.clone(),
                                BTreeMap::new(),
                                detail.study.status == "approved",
                            );
                            new.photo_order = detail
                                .assets
                                .iter()
                                .filter(|a| a.kind == "original")
                                .map(|a| a.id)
                                .collect();
                            form.update(cx, |f, cx| {
                                *f = new;
                                cx.notify();
                            });
                            self.sent_form = 0;
                            self.form_ack = 0;
                        }
                        form.update(cx, |f, _| {
                            f.readonly = detail.study.status == "approved"
                                || self.approval_busy
                                || self.logging_out
                                || self.workflow_request.is_some()
                        });
                    }
                    let just_approved = self.detail.as_ref().is_some_and(|old|old.study.status != "approved")
                        && detail.study.status == "approved";
                    self.study_edited |= just_approved;
                    let metadata = &self.state["protocol_metadata"];
                    if detail.study.status == "approved" && metadata["revision_id"] == json!(detail.revision_id)
                        && let Ok(Some(assets))=m53_client::report_identity::approved_assets(metadata,detail.study.id)
                    {
                        let selected: std::collections::BTreeSet<_> = assets.iter().map(|asset|asset.id).collect();
                        if self.editor.read(cx).session.photos.iter()
                            .any(|photo| photo.in_report != selected.contains(&photo.id))
                        {
                            self.editor.update(cx, |editor, cx| {
                                for photo in &mut editor.session.photos {photo.in_report=selected.contains(&photo.id);}
                                cx.notify();
                            });
                            if !self.study_edited {
                                self.opened_photo_state=report_photo_state(&self.editor.read(cx).session);
                            }
                        }
                    }
                    self.detail = Some(detail);
                    if just_approved && self.screen == Screen::Report { self.preview(cx); }
                }
            }
            Event::Opened {
                detail,
                dictionary,
                journal,
            } => {
                let opened_from_registry = self.opening;
                let selected_draft = opened_from_registry && std::mem::take(&mut self.opening_registry_draft);
                #[cfg(windows)] self.release_history_pages(cx);
                self.history = None;
                self.abandon_preview();
                let detail = *detail;
                let lookup_approved = opened_from_registry && !selected_draft
                    && detail.study.status == "draft" && detail.revision_id.is_some();
                if self.detail.as_ref().is_none_or(|old|old.study.id != detail.study.id) {
                    self.report_view.page = 0;
                    self.report_history_expanded = false;
                    self.report_scroll.set_offset(point(px(0.),px(0.)));
                    self.guidance_closed = false;
                    self.approval_busy = false;
                    self.approval_pending = false;
                    self.approval_message.clear();
                }
                self.opening = false;
                self.workflow = care_path::status(&detail, &journal);
                self.patient = Some(detail.patient.id);
                self.send(Command::Registry(crate::registry_browser::Request::Patient(detail.patient.id)));
                let id = detail.study.id;
                self.editor.update(cx, |e, cx| {
                    e.cancel_marker_pick();
                    e.session = serde_json::from_value(journal["native_editor"].clone())
                        .unwrap_or_default();
                    e.session.photos.retain(|p| {
                        p.removed
                            || detail
                                .assets
                                .iter()
                                .any(|a| a.id == p.id && a.kind == "original")
                    });
                    for asset in detail.assets.iter().filter(|a| a.kind == "original") {
                        if !e.session.photos.iter().any(|p| p.id == asset.id) {
                            let mut p = m53_gpui_editor::Photo::new(1, 1);
                            p.id = asset.id;
                            p.in_report = journal["selected_photos"]
                                .as_array()
                                .is_none_or(|ids| ids.contains(&json!(asset.id)));
                            e.session.photos.push(p);
                        }
                    }
                    e.session.selected = e
                        .session
                        .selected
                        .min(e.session.photos.len().saturating_sub(1));
                    e.session.ensure_visible_selection();
                    e.photo_selecting = false;
                    e.photo_checked.clear();
                    for p in &mut e.session.photos {
                        p.annotations = detail
                            .annotations
                            .iter()
                            .filter(|a| a.asset_id == p.id)
                            .cloned()
                            .collect();
                    }
                    e.clear_photo_cache(cx);
                    e.clinic = Some((id, self.worker.clone()));
                    let date = json!(detail.protocol)["colpo"]["fields"]["context.exam_date"]["value"]
                        .as_str().unwrap_or("").to_owned();
                    e.export_patient=Some((patient_name(&detail.patient),date));
                    e.historical_readonly = lookup_approved;
                    e.readonly = detail.study.status == "approved" || e.historical_readonly || self.logging_out;
                    e.version = 0;
                    e.saved = 0;
                    e.ready = true;
                    e.fit();
                    e.undo.clear();
                    cx.notify();
                });
                let continuing = !opened_from_registry && self.detail.as_ref().is_some_and(|old| old.study.id == id);
                self.study_edited = (continuing && self.study_edited) || journal["dirty"] == true
                    || journal["unfinished_fields"].as_object().is_some_and(|fields| !fields.is_empty())
                    || journal["server_save_error"].as_str().is_some_and(|error| !error.is_empty());
                self.opened_photo_state = report_photo_state(&self.editor.read(cx).session);
                let canonical = json!(detail.protocol);
                let value = m53_gpui_editor::state::restore_form(&detail, &journal);
                let unfinished = serde_json::from_value(journal["unfinished_fields"].clone())
                    .unwrap_or_default();
                self.form = Some(cx.new(|_| {
                    let mut form = NativeForm::new(
                        value,
                        dictionary.clone(),
                        unfinished,
                        detail.study.status == "approved",
                    );
                    form.base = canonical;
                    form.photo_order = detail
                        .assets
                        .iter()
                        .filter(|a| a.kind == "original")
                        .map(|a| a.id)
                        .collect();
                    form
                }));
                self.sent_form = 0;
                self.form_ack = 0;
                self.dictionary = Some(dictionary);
                let reopened = !opened_from_registry && detail.study.status == "draft"
                    && self.detail.as_ref().is_some_and(|previous| {
                        previous.study.id == id && previous.study.status == "approved"
                    });
                self.screen = if opened_from_registry && detail.study.status == "approved" {
                    Screen::Report
                } else if reopened || (selected_draft && detail.study.status == "draft") {
                    Screen::Protocol
                } else {
                    Screen::Study
                };
                self.detail = Some(detail);
                if lookup_approved {
                    self.screen = Screen::Report;
                    let mut history = crate::protocol_history::HistoryState::new(id);
                    history.prefer_approved = true;
                    self.history = Some(history);
                    self.request_history(None, None, cx);
                }
                self.report_lines.clear();
                self.release_report_images(cx);
                self.message.clear();
                if self.screen == Screen::Report {
                    self.report_view.page = 0;
                    self.report_scroll.set_offset(point(px(0.), px(0.)));
                    self.preview(cx);
                }
            }
            Event::PhotoAdded {study,asset,width,height,captured} => {
                if self.detail.as_ref().is_some_and(|d|d.study.id==study) {
                    self.editor.update(cx,|e,cx| {
                        if e.session.photos.iter().any(|p|p.id==asset&&p.removed) {return;}
                        if let Some(photo)=e.session.photos.iter_mut().find(|p|p.id==asset) {
                            photo.width=width; photo.height=height;
                        } else {
                            let mut photo=m53_gpui_editor::Photo::new(width,height); photo.id=asset;
                            e.session.photos.push(photo);
                        }
                        if captured {
                            e.session.selected=e.session.photos.iter().position(|p|p.id==asset).unwrap_or(0);
                            e.cancel_marker_pick();
                            e.fit();
                        }
                        e.photo_demand=None;
                        let wanted=e.photo_selection();
                        e.retain_photo_cache(&wanted,cx);
                        e.persist(); cx.notify();
                    });
                }
            }
            Event::Texture {study,asset,width,height,image} => {
                if self.detail.as_ref().is_some_and(|d|d.study.id==study) {
                    self.editor.update(cx,|e,cx| {
                        if !e.photo_selection().full.contains(&asset) {return;}
                        e.retain_photo_cache(&e.photo_selection(),cx);
                        if let Some(photo)=e.session.photos.iter_mut().find(|p|p.id==asset&&!p.removed) {
                            photo.width=width; photo.height=height;
                            if let Some(old)=e.images.insert(asset,image) {cx.drop_image(old,None);}
                            cx.notify();
                        }
                    });
                }
            }
            Event::Thumbnail {study,asset,image} => {
                if self.detail.as_ref().is_some_and(|d|d.study.id==study) {
                    self.editor.update(cx,|e,cx| {
                        if e.photo_selection().thumbnails.contains(&asset) {
                            e.retain_photo_cache(&e.photo_selection(),cx);
                            if let Some(old)=e.thumbnails.insert(asset,image) {cx.drop_image(old,None);} cx.notify();
                        }
                    });
                }
            }
            Event::PhotoFinished {study,error} => {
                if self.detail.as_ref().is_some_and(|d|d.study.id==study) {
                    self.video_pending=false;
                    self.editor.update(cx,|e,_| {e.photo_pending=e.photo_pending.saturating_sub(1);e.io_pending=e.io_pending.saturating_sub(1).max(e.photo_pending);});
                    self.message=error.unwrap_or_else(||"Готово; снимки сохранены на компьютере".into());
                }
            }
            Event::PhotoFailed {study,message} => {
                if self.detail.as_ref().is_some_and(|d|d.study.id==study) {self.message=message;}
            }
            Event::Ack {
                study,
                version,
                form,
                aliases,
                problem,
            } => {
                self.normalize_ids(&aliases, cx);
                if self.detail.as_ref().is_some_and(|d| d.study.id == study) {
                    if form {
                        self.form_ack = self.form_ack.max(version);
                        if let Some(error) = problem {
                            self.message = error.clone();
                            if let Some(form) = &self.form {
                                form.update(cx, |f, _| {
                                    f.unfinished.insert("schema".into(), error);
                                });
                            }
                        } else if let Some(form) = &self.form {
                            form.update(cx, |f, _| {
                                f.unfinished.remove("schema");
                            });
                        }
                    } else {
                        self.editor
                            .update(cx, |e, _| e.saved = e.saved.max(version));
                    }
                }
            }
            Event::Preview {
                study,
                request,
                lines,
                photos,
                pages,
                identity,
            } => {
                if accepts_preview(self.preview_request,self.detail.as_ref().map(|d|d.study.id),study,request) {
                    self.preview_request=None;
                    self.release_report_images(cx);
                    self.report_identity = identity;
                    self.report_lines = lines;
                    self.report_pages = pages;
                    self.report_photos = photos;
                    let count = if self.report_pages.is_empty() {self.report_lines.len().div_ceil(53)+self.report_photos.len()} else {self.report_pages.len()};
                    self.report_view.set_page(self.report_view.page,count);
                    self.report_busy = false;
                }
            }
            Event::PreviewFailed {study,request,error} => {
                if accepts_preview(self.preview_request,self.detail.as_ref().map(|d|d.study.id),study,request) {
                    self.preview_request=None; self.report_busy=false; self.message=error;
                }
            }
            Event::FileDone(path) => {
                if self.editor.read(cx).photo_pending==0 {self.video_pending = false;}
                self.report_busy = report_operation_active(self.preview_request,self.report_output_request);
                self.editor
                    .update(cx, |e, _| e.io_pending = e.io_pending.saturating_sub(1).max(e.photo_pending));
                self.message = "Готово; локальные данные сохранены".into();
                if path.is_some() { self.message = "PDF сохранён".into(); }
            }
            Event::ReportOutput {request,result} => {
                if self.report_output_request!=Some(request) {return;}
                self.report_output_request=None;
                self.report_busy=self.preview_request.is_some();
                self.message=match result {
                    Ok(ReportCompletion::Printed(true))=>"Задание принято принтером".into(),
                    Ok(ReportCompletion::Printed(false))=>"Печать отменена".into(),
                    Ok(ReportCompletion::Exported(_))=>"PDF сохранён".into(),
                    Ok(ReportCompletion::Enqueued)=>"Документ сохранён в очереди передачи".into(),
                    Err(error)=>error,
                };
            }
            Event::Error(error) => {
                if self.editor.read(cx).photo_pending==0 {self.video_pending = false;}
                self.opening = false;
                self.opening_registry_draft = false;
                self.auth_busy = false;
                self.logging_out = false;
                self.report_busy = report_operation_active(self.preview_request,self.report_output_request);
                self.editor
                    .update(cx, |e, _| e.io_pending = e.io_pending.saturating_sub(1).max(e.photo_pending));
                self.message = error;
            }
            Event::LoggedOut => {
                self.report_output_request=None;
                self.report_busy=false;
                #[cfg(windows)] self.release_history_pages(cx);
                self.history=None;
                self.show_new=false;self.new_patient_check=None;self.new_patient_matches.clear();
                // The worker already advanced the epoch. Never accept an old login response.
                self.guidance.context = None;
                self.guidance.writable = false;
                self.guidance_pending = false;
                self.guidance_closed = false;
                self.abandon_preview(); self.release_report_images(cx); self.report_lines.clear(); self.report_identity=None;
                self.editor.update(cx,|e,cx| {
                    e.clinic=None; e.session=Default::default(); e.clear_photo_cache(cx);
                    e.ready=false; e.io_pending=0; e.photo_pending=0;
                });
                self.approval_busy = false;
                self.approval_pending = false;
                self.approval_message.clear();
                self.phrase_panel.update(cx,|p,cx|p.reset(cx));
                self.logging_out = false;
                self.detail = None;
                self.workflow = WorkflowStatus::default();
                self.workflow_request = None;
                self.dictionary = None;
                self.form = None;
                self.state = json!({});
                self.patient = None;
                self.patient_edit_access = None;
                self.preview_study = None;
                self.patients.clear();
                self.patient_sexes.clear();
                self.studies.clear();
                self.study_summaries.clear();
                self.refresh_patient_table(cx);
                self.send(Command::LoginProfiles(
                    self.url.read(cx).value().to_string(),
                    self.key.read(cx).value().to_string(),
                ));
                self.screen = Screen::Patients;
                self.message = "Вы вышли. Локальная работа сохранена.".into();
            }
        }
        cx.notify();
    }
    fn release_report_images(&mut self, cx: &mut Context<Self>) {
        for image in self.report_pages.drain(..) { cx.drop_image(image, None); }
        for (_, image) in self.report_photos.drain(..) { cx.drop_image(image, None); }
    }
    fn abandon_preview(&mut self) {
        if let Some(request)=self.preview_request.take() {
            let _=self.worker.send(Command::CancelPreview(request));
            #[cfg(windows)] m53_client::report::chromium::cancel_pending();
            self.report_busy=false;
        }
    }
    fn navigate(&mut self, screen: Screen, cx: &mut Context<Self>) {
        if (self.patient_editor.is_some() || self.patient_edit_request.is_some() || self.patient_editor_pending.is_some()) && screen!=Screen::Patients {self.message="Дождитесь загрузки карточки; сохраните или отмените её изменение".into();cx.notify();return;}
        if self.history.as_ref().is_none_or(|history| !history.prefer_approved) {
            self.close_history(cx);
        }
        if screen==Screen::Settings {self.send(Command::Activity(m53_client::activity::Action::new("settings.opened")));}
        if self.workflow_request.is_some() {
            return;
        }
        if self.screen == Screen::Settings
            && (screen == Screen::Settings || screen == self.settings_return)
        {
            self.screen = match self.settings_return {
                Screen::Settings => {
                    if self.detail.is_some() {
                        Screen::Study
                    } else {
                        Screen::Patients
                    }
                }
                previous => previous,
            };
            self.pedal_testing = false;
            self.pedal_test_latch.reset();
            self.pedal_latch.reset();
            if self.screen==Screen::Report && !self.report_busy {self.preview(cx);}
            cx.notify();
            return;
        }
        if screen != Screen::Settings {
            self.pedal_testing = false;
            self.pedal_test_latch.reset();
        }
        self.pedal_latch.reset();
        if screen == Screen::Patients && self.detail.is_some() {
            self.request_workflow(WorkflowAction::SaveAndExit, cx);
            return;
        }
        if matches!(screen, Screen::Study | Screen::Protocol | Screen::Report)
            && !self.step_allowed(screen, cx)
        {
            self.message = "Сначала завершите предыдущий этап обследования".into();
            cx.notify();
            return;
        }
        if screen == Screen::Settings {
            self.settings_return = self.screen;
        }
        self.flush_form(cx, true);
        if screen!=Screen::Report {self.abandon_preview();}
        self.guidance_closed = false;
        self.screen = screen;
        if screen == Screen::Report {
            self.preview(cx);
        }
        cx.notify();
    }
    fn step_allowed(&self, screen: Screen, cx: &Context<Self>) -> bool {
        let Some(detail) = &self.detail else {
            return false;
        };
        if screen == Screen::Study {
            return true;
        }
        let editor = self.editor.read(cx);
        let examination = self.workflow.study_id == detail.study.id
            && self.workflow.examination_complete
            && editor.saved >= editor.version
            && editor.io_pending == 0;
        match screen {
            Screen::Protocol => examination,
            Screen::Report => {
                examination
                    && self.workflow.protocol_complete
                    && self.form.as_ref().is_some_and(|f| {
                        let f = f.read(cx);
                        f.unfinished.is_empty() && self.form_ack >= f.generation
                    })
            }
            _ => true,
        }
    }
    fn request_workflow(&mut self, action: WorkflowAction, cx: &mut Context<Self>) {
        if self.patient_editor.is_some()||self.patient_edit_request.is_some()||self.patient_editor_pending.is_some(){self.message="Сначала сохраните или отмените изменение карточки пациента".into();cx.notify();return;}
        if action==WorkflowAction::SaveAndExit {self.abandon_preview();}
        if self.workflow_request.is_some() || self.detail.is_none() {
            return;
        }
        if self.report_busy || self.editor.read(cx).io_pending > 0 {
            self.message = "Дождитесь завершения обработки файлов".into();
            cx.notify();
            return;
        }
        if self.viewing_remote && action != WorkflowAction::SaveAndExit {
            self.message = "Вернитесь к локальному протоколу".into();
            cx.notify();
            return;
        }
        let close_preview = action == WorkflowAction::SaveAndExit
            && self.history.as_ref().is_some_and(|history| history.prefer_approved);
        if !close_preview {
            self.editor.update(cx, |e, cx| {
                if e.gesture.is_some() {
                    e.finish(cx);
                }
            });
            self.flush_form(cx, true);
        }
        let request = Uuid::new_v4();
        let study = self.detail.as_ref().unwrap().study.id;
        self.workflow_request = Some((request, action));
        self.editor.update(cx, |e, _| e.readonly = true);
        if let Some(form) = &self.form {
            form.update(cx, |f, _| f.readonly = true);
        }
        self.message = if close_preview {"Закрытие протокола…"} else {"Сохранение на компьютере…"}.into();
        self.send(Command::Workflow {
            study,
            request,
            action,
            close_preview,
        });
        cx.notify();
    }
    fn open_study(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if self.patient_editor.is_some() || self.patient_edit_request.is_some() || self.patient_editor_pending.is_some(){self.message="Сохраните или отмените изменение карточки пациента".into();cx.notify();return;}
        if self.opening || self.workflow_request.is_some() {
            return;
        }
        self.flush_form(cx, true);
        self.abandon_preview();
        self.opening = true;
        self.opening_registry_draft = self.studies.iter()
            .any(|study| study.id == id && study.status == "draft");
        self.send(Command::Open(id));
        self.message = "Открытие обследования…".into();
        cx.notify();
    }
    fn approved_history(&self) -> bool {
        self.history.as_ref().is_some_and(|history| history.prefer_approved)
    }

    fn report_output_available(&self) -> bool {
        if self.approved_history() {
            #[cfg(windows)]
            { return self.history.as_ref().and_then(|history|history.pdf_pages.as_ref()).is_some_and(Result::is_ok); }
            #[cfg(not(windows))]
            { return false; }
        }
        self.detail.as_ref().is_some_and(|detail| care_path::require_print(detail).is_ok())
    }

    fn report_output_identity(&self) -> Option<(Uuid, Option<Uuid>)> {
        if self.approved_history() {
            self.history.as_ref().map(|history|(history.study,history.selected))
        } else {
            self.detail.as_ref().map(|detail|(detail.study.id,detail.revision_id))
        }
    }

    fn preview(&mut self, cx: &mut Context<Self>) {
        if self.approved_history() {
            if let Some(revision)=self.history.as_ref().and_then(|history|history.selected) {
                self.request_history(Some(revision),None,cx);
            }
            return;
        }
        if self.history.is_some() { return; }
        if self.report_busy && self.preview_request.is_none() {return;}
        self.abandon_preview();
        #[cfg(windows)] m53_client::report::chromium::cancel_pending();
        self.report_identity = None;
        if self.templates.first().is_none() {
            self.send(Command::Refresh);
            self.message = "Шаблон печати пока недоступен. Обновляем каталог сервера…".into();
            cx.notify();
            return;
        }
        self.flush_form(cx, true);
        if let Some(d) = &self.detail {
            let request=Uuid::new_v4(); self.preview_request=Some(request);
            let selected = self
                .editor
                .read(cx)
                .session
                .photos
                .iter()
                .filter(|p| p.in_report && !p.removed)
                .map(|p| p.id)
                .collect();
            if self.worker.send(Command::Preview {
                study: d.study.id,
                request,
                selected,
                template: self.templates[0].clone(),
            }).is_err() {
                self.preview_request=None;self.report_busy=false;self.message="Не работает локальный обработчик отчета".into();cx.notify();
            } else {self.report_busy = true;}
        }
    }
    fn output_report(&mut self, output: ReportOutput, cx: &mut Context<Self>) {
        if self.report_busy || self.preview_request.is_some() { return; }
        if self.approved_history() {
            #[cfg(windows)]
            if let Some(history)=&self.history
                && let Some(Ok(pdf))=&history.pdf_pages
            {
                let request=Uuid::new_v4();
                let command=Command::HistoricalOutput{request,study:history.study,bytes:pdf.bytes.clone(),output};
                self.send_report_output(request,command);
                cx.notify();
            }
            return;
        }
        let Some(template) = self.templates.first().cloned() else { self.message = "Шаблон протокола недоступен".into(); cx.notify(); return; };
        self.flush_form(cx, true);
        let Some(detail) = &self.detail else { return; };
        if let Err(message) = care_path::require_print(detail) { self.message = message; cx.notify(); return; }
        let Some(expected) = self.report_identity.clone().filter(|identity| Some(identity.0)==detail.revision_id && identity.1==template.version_id) else { self.message="Обновите предпросмотр перед выводом".into();cx.notify();return; };
        let selected = self.editor.read(cx).session.photos.iter()
            .filter(|photo| !photo.removed && photo.in_report).map(|photo| photo.id).collect();
        self.message = match &output { ReportOutput::Print => if cfg!(target_os="linux") { "Отправляем на принтер по умолчанию…" } else { "Подготовка диалога печати…" }, ReportOutput::Export(_) => "Сохранение PDF…" }.into();
        let request=Uuid::new_v4();
        let command=Command::Pdf {request,study:detail.study.id,selected,template,expected:Some(expected),output};
        self.send_report_output(request,command);
        cx.notify();
    }
    fn export_pdf(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.report_busy || self.preview_request.is_some() { return; }
        if !self.report_output_available() { return; }
        let Some(identity)=self.report_output_identity() else { return; };
        let request=Uuid::new_v4();
        self.report_output_request=Some(request);
        self.report_busy = true;
        self.message = "Выберите папку и имя PDF".into();
        #[cfg(windows)]
        let path = cx.background_executor().spawn(async {
            Ok::<_, String>(Ok::<_, String>(rfd::FileDialog::new()
                .set_title("Экспорт отчёта в PDF")
                .set_directory(m53_client::preview::documents_directory())
                .set_file_name("Протокол-кольпоскопии.pdf")
                .add_filter("Документы PDF", &["pdf"]).save_file()))
        });
        #[cfg(not(windows))]
        let path = cx.prompt_for_new_path(&std::env::var_os("HOME").map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir).join("Documents"), Some("Протокол-кольпоскопии.pdf"));
        cx.spawn_in(window, async move |view, cx| {
            let chosen = path.await;
            let _ = view.update(cx, |s, cx| {
                if s.report_output_request!=Some(request) {return;}
                s.report_output_request=None;
                s.report_busy = false;
                match chosen {
                    Ok(Ok(Some(path))) if s.report_output_identity()==Some(identity) =>
                        s.output_report(ReportOutput::Export(path), cx),
                    Ok(Ok(None)) => s.message = "Экспорт PDF отменён".into(),
                    Ok(Ok(Some(_))) => s.message = "Обследование изменилось; повторите экспорт".into(),
                    _ => s.message = "Не удалось открыть диалог сохранения PDF".into(),
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }
    pub fn can_close(&mut self, cx: &mut Context<Self>) -> bool {
        if self.patient_editor.is_some() {self.message="Сохраните или отмените изменения карточки пациента перед закрытием".into();cx.notify();return false;}
        if self.phrase_panel.read(cx).has_unsaved(cx) {self.message="Сохраните или отмените изменения шаблона перед закрытием".into();cx.notify();return false;}
        self.editor.update(cx, |e, cx| {
            if e.gesture.is_some() {
                e.finish(cx);
            }
        });
        self.flush_form(cx, true);
        let e = self.editor.read(cx);
        let authenticated = self.state["authenticated"] == true;
        let ready = e.saved >= e.version
            && e.io_pending == 0
            && self.form_ack >= self.sent_form
            && !self.report_busy
            && self.workflow_request.is_none()
            && !self.opening
            && (!authenticated || checkpoint_ready(&self.state));
        if !ready {
            self.message = "Дождитесь подтверждения локальной записи перед закрытием".into();
            cx.notify();
        }
        if ready && !self.audit_close_ready {if !self.audit_close_requested {self.audit_close_requested=true;self.send(Command::CloseAudit);}return false;}
        ready
    }
    fn refresh_patient_table(&mut self, cx: &mut Context<Self>) {
        let rows = patient_rows(
            &self.patients,
            &self.studies,
            &self.study_summaries,
            &self.patient_sexes,
            if self.preview_mode {self.query.read(cx).value().to_string()} else {String::new()}.as_str(),
        );
        let mut rows=rows;
        if !self.preview_mode {
            let values:BTreeMap<Uuid,_>=self.state["native_registry"]["patients"].as_array().into_iter().flatten()
                .filter_map(|p|Some((p["id"].as_str()?.parse().ok()?,p))).collect();
            for row in &mut rows {
                if let Some(remote)=values.get(&row.id) {
                    let date=remote["last_exam_date"].as_str().and_then(m53_gpui_editor::registry::iso_date);
                    row.last_exam_date=date.max(row.last_exam_date.take());
                }
            }
        }
        let selected = self.patient;
        self.patient_table.update(cx, |table, cx| {
            let state_changed = table.delegate_mut().set_registry_state(&self.state["native_registry"]);
            table.delegate_mut().selected = selected;
            table.delegate_mut().more=self.state["native_registry"]["patients_more"]==true;
            table.delegate_mut().loading=self.state["native_registry"]["patients_loading"]==true;
            if table.delegate_mut().replace(rows) || state_changed {
                table.refresh(cx);
                if let Some(index) =
                    selected.and_then(|id| table.delegate().rows.iter().position(|r| r.id == id))
                {
                    if table.selected_row()!=Some(index) {table.set_selected_row(index, cx);}
                } else {
                    table.clear_selection(cx);
                }
                cx.notify();
            }
            cx.notify(); // Loading/has_more can change without a row change.
        });
    }
    fn authenticate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.auth_busy || self.state["authenticated"] == true {
            return;
        }
        let login = self.login.read(cx).value().trim().to_string();
        let password = self.password.read(cx).value().to_string();
        if login.is_empty() {
            self.message = "Выберите врача и введите пароль".into();
            cx.notify();
            return;
        }
        self.auth_busy = true;
        self.message.clear();
        self.send(Command::Login {
            url: self.url.read(cx).value().to_string(),
            key: self.key.read(cx).value().to_string(),
            login,
            password,
        });
        self.password
            .update(cx, |p, cx| p.set_value("", window, cx));
        cx.notify();
    }
    fn login_view(&self, cx: &Context<Self>) -> AnyElement {
        let entity = cx.entity();
        let doctors = self.doctors.clone();
        let selected = self.login.read(cx).value();
        let doctor = self
            .doctors
            .iter()
            .find(|d| d.login == selected.as_ref())
            .map(|d| d.display_name.clone())
            .unwrap_or_else(|| "Выберите врача".into());
        v_flex().flex_1().items_center().justify_center().child(
            v_flex().w(px(400.)).gap_3()
                .child(div().text_lg().font_semibold().child("Вход в рабочее место"))
                .when(cfg!(feature = "standalone"), |panel| panel.child(
                    v_flex().gap_1().child("Логин врача").child(Input::new(&self.login).disabled(self.auth_busy))
                ))
                .when(!cfg!(feature = "standalone"), |panel| panel.child(v_flex().gap_1().child("Врач").child(
                    Button::new("doctor-picker").outline().w_full().justify_start()
                        .accessibility_label("Выбрать врача из списка")
                        .child(h_flex().w_full().gap_2().child(div().flex_1().min_w_0().text_left().child(doctor))
                            .child(div().pl_2().border_l_1().border_color(cx.theme().border).child(gpui_kit::component::Icon::new(IconName::ChevronDown))))
                        .disabled(self.auth_busy)
                        .dropdown_menu(move |mut menu,_,_| {
                            for doctor in &doctors {
                                let entity=entity.clone();
                                let login=doctor.login.clone();
                                menu=menu.item(PopupMenuItem::new(format!("{} · {}",doctor.display_name,doctor.target.as_ref().map(|p|p.login.as_str()).unwrap_or(&doctor.login))).on_click(move |_,window,cx| {
                                    entity.update(cx,|s,cx| {
                                        s.manual_login=false;
                                        s.login.update(cx,|input,cx|input.set_value(login.clone(),window,cx));
                                        s.password.update(cx,|input,cx|input.set_value("",window,cx));
                                        s.password.read(cx).focus_handle(cx).focus(window,cx);
                                        cx.notify();
                                    });
                                }));
                            }
                            let entity=entity.clone();
                            if std::env::var("M53_LEGACY_LOGIN").as_deref()==Ok("1"){menu=menu.separator().item(PopupMenuItem::new("Другой врач — первый вход по логину").on_click(move |_,window,cx| {
                                entity.update(cx,|s,cx| {
                                    s.manual_login=true;
                                    s.login.update(cx,|input,cx|input.set_value("",window,cx));
                                    s.password.update(cx,|input,cx|input.set_value("",window,cx));
                                    s.login.read(cx).focus_handle(cx).focus(window,cx);
                                    cx.notify();
                                });
                            }));} menu.max_h(px(398.)).scrollable(true)
                        })
                )))
                .when(!cfg!(feature = "standalone") && self.manual_login && std::env::var("M53_LEGACY_LOGIN").as_deref()==Ok("1"),|d|d.child(Input::new(&self.login).disabled(self.auth_busy)).child(div().text_sm().text_color(cx.theme().muted_foreground).child("После успешного входа врач появится в списке на этом компьютере.")))
                .child(v_flex().gap_1().child("Пароль").child(Input::new(&self.password).disabled(self.auth_busy)))
                .child(Button::new("login").primary().label(if self.auth_busy {"Вход…"} else {"Войти"}).disabled(self.auth_busy).on_click(cx.listener(|s,_,w,cx|s.authenticate(w,cx))))
                .when(cfg!(feature = "standalone"), |view| view.child(
                    Button::new("source-license").ghost().label("Исходный код и лицензия")
                        .on_click(|_, _, cx| cx.open_url(m53_client::public_source_url()))
                ))
                .child(Button::new("connection-settings").ghost().label("Настройки подключения").on_click(cx.listener(|s,_,_,cx|{s.navigate(Screen::Settings,cx);})))
        ).into_any_element()
    }
    fn stop_video(&mut self, cx: &mut Context<Self>) {
        if self.audit_camera_active {self.audit_camera_active=false;self.send(Command::Activity(m53_client::activity::Action::new("camera.disconnected")));}
        self.video_epoch += 1;
        let _ = self
            .video_worker
            .tx
            .send(video::Request::Stop(self.video_epoch));
        self.video_running = false;
        self.video_image = None;
        self.video_note = "Камера остановлена".into();
        self.editor.update(cx, |e, cx| {
            e.live_active = false;
            e.live_image = None;
            cx.notify();
        });
        self.pedal_latch.reset();
        cx.notify();
    }
    fn start_video(&mut self, cx: &mut Context<Self>) {
        let Some(config) = self.video_config.clone() else {
            self.video_note = "Сначала выберите плату и режим в настройках".into();
            cx.notify();
            return;
        };
        self.video_epoch += 1;
        self.video_image = None;
        self.video_generation = 0;
        self.video_note = "Подключение камеры…".into();
        self.video_running = true;
        let _ = self
            .pedal_tx
            .send(pedal::Request::SaveVideo(Some(config.clone())));
        if self
            .video_worker
            .tx
            .send(video::Request::Start(self.video_epoch, config))
            .is_err()
        {
            self.video_running = false;
            self.video_note = "Служба видеозахвата недоступна".into();
        }
        self.editor.update(cx, |e, cx| {
            e.live_active = true;
            e.live_image = None;
            cx.notify();
        });
        cx.notify();
    }
    fn poll_video(&mut self, cx: &mut Context<Self>) {
        if self.video_running
            && matches!(
                self.screen,
                Screen::Patients | Screen::Protocol | Screen::Report
            )
        {
            self.stop_video(cx);
        }
        while let Ok(event) = self.video_worker.events.try_recv() {
            match event {
                video::Event::Devices(result) => match result {
                    Ok(devices) => {
                        self.video_devices = devices;
                        self.video_note = if self.video_devices.is_empty() {
                            "Устройства видеозахвата не найдены".into()
                        } else {
                            "Выберите плату и видеорежим".into()
                        };
                    }
                    Err(error) => self.video_note = error,
                },
                video::Event::Status {
                    epoch,
                    active,
                    message,
                } => {
                    if epoch != self.video_epoch {
                        continue;
                    }
                    if !active {
                        self.video_running = false;
                        self.video_image = None;
                        self.editor.update(cx, |e, cx| {
                            e.live_active = false;
                            e.live_image = None;
                            cx.notify();
                        });
                        self.pedal_latch.reset();
                    }
                    if active != self.audit_camera_active {self.audit_camera_active=active;self.send(Command::Activity(m53_client::activity::Action::new(if active {"camera.connected"}else{"camera.disconnected"})));}
                    self.video_note = message;
                }
            }
            cx.notify();
        }
        if !self.video_running {
            return;
        }
        let frame = self.video_worker.latest.lock().ok().and_then(|f| f.clone());
        if let Some(frame) = frame {
            if frame.epoch != self.video_epoch {
                return;
            }
            if frame.received.elapsed() > Duration::from_secs(1) {
                if update_stale_video_note(&mut self.video_note) { cx.notify(); }
                return;
            }
            if frame.generation == self.video_generation {
                return;
            }
            self.video_generation = frame.generation;
            if let Some(rgba) =
                image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra.clone())
            {
                let image = std::sync::Arc::new(RenderImage::new(vec![image::Frame::new(rgba)]));
                self.video_image = Some(image.clone());
                self.editor.update(cx, |e, cx| {
                    e.live_image = Some(image);
                    cx.notify();
                });
                self.video_note = "Видеосигнал поступает".into();
                cx.notify();
            }
        }
    }
    fn capture_allowed(&self, cx: &Context<Self>) -> bool {
        self.capture_block_reason(cx).is_none()
    }
    fn capture_block_reason(&self, cx: &Context<Self>) -> Option<&'static str> {
        if self.history.is_some(){return Some("Закройте историю редакций перед съёмкой");}
        if self.screen != Screen::Study {return Some("Откройте этап обследования для съёмки педалью");}
        if !self.video_running {return Some("Включите камеру для съёмки педалью");}
        if self.video_pending {return Some("Дождитесь сохранения предыдущего снимка");}
        if self.workflow_request.is_some() || self.opening || self.report_busy {
            return Some("Дождитесь завершения текущей операции");
        }
        if !self.detail.as_ref().is_some_and(|d|d.study.status == "draft") {
            return Some("Снимки можно добавлять только в черновик обследования");
        }
        let editor = self.editor.read(cx);
        if !editor.ready || editor.io_pending != 0 || editor.saved < editor.version {
            return Some("Дождитесь сохранения снимков и разметки");
        }
        if !self.video_worker.latest.lock().ok().and_then(|f|f.clone()).is_some_and(|f|
            f.epoch == self.video_epoch && f.received.elapsed() < Duration::from_secs(1)) {
            return Some("Нет свежего кадра с камеры — проверьте видеосигнал");
        }
        None
    }
    fn capture_frame(&mut self, cx: &mut Context<Self>) {
        if !self.capture_allowed(cx) {
            return;
        }
        let Some(frame) = self.video_worker.latest.lock().ok().and_then(|f| f.clone()) else {
            return;
        };
        let study = self.detail.as_ref().unwrap().study.id;
        self.video_pending = true;
        self.editor.update(cx, |e, _| {e.io_pending += 1;e.photo_pending += 1;});
        if self
            .worker
            .send(Command::Capture {
                study,
                bytes: match video::snapshot_bytes(frame.jpeg.as_ref()) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        self.video_pending = false;
                        self.editor
                            .update(cx, |e, _| {e.io_pending = e.io_pending.saturating_sub(1);e.photo_pending=e.photo_pending.saturating_sub(1);});
                        self.message = e;
                        cx.notify();
                        return;
                    }
                },
            })
            .is_err()
        {
            self.video_pending = false;
            self.editor
                .update(cx, |e, _| {e.io_pending = e.io_pending.saturating_sub(1);e.photo_pending=e.photo_pending.saturating_sub(1);});
            self.message = "Не работает сохранение снимков".into();
        }
        cx.notify();
    }
    fn study_view(&self, cx: &Context<Self>) -> AnyElement {
        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                h_flex()
                    .h(px(40.))
                    .flex_shrink_0()
                    .px_3()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("video-start")
                            .outline()
                            .small()
                            .label(if self.video_running {
                                "Остановить камеру"
                            } else {
                                "Включить камеру"
                            })
                            .disabled(
                                self.video_pending
                                    || self
                                        .detail
                                        .as_ref()
                                        .is_none_or(|d| d.study.status != "draft")
                                    || self.approved_history(),
                            )
                            .on_click(cx.listener(|s, _, _, cx| {
                                if s.video_running {
                                    s.stop_video(cx)
                                } else {
                                    s.start_video(cx)
                                }
                            })),
                    )
                    .child(
                        Button::new("video-live")
                            .ghost()
                            .small()
                            .label("Живое видео")
                            .disabled(!self.video_running)
                            .on_click(cx.listener(|s, _, window, cx| {
                                s.editor.update(cx, |e, cx| {
                                    e.live_active = true;
                                    e.focus_handle.focus(window, cx);
                                    cx.notify();
                                });
                            })),
                    )
                    .child(
                        Button::new("video-capture")
                            .primary()
                            .small()
                            .icon(IconName::Camera)
                            .label(if self.video_pending {
                                "Сохранение…"
                            } else {
                                "Сделать снимок"
                            })
                            .disabled(!self.capture_allowed(cx))
                            .on_click(cx.listener(|s, _, _, cx| s.capture_frame(cx))),
                    )
                    .child(
                        div()
                            .text_sm()
                            .flex_1()
                            .min_w_0()
                            .text_ellipsis()
                            .child(self.video_note.clone()),
                    )
                    .child(
                        Button::new("video-settings")
                            .ghost()
                            .small()
                            .icon(IconName::Settings2)
                            .accessibility_label("Настройки платы видеозахвата")
                            .tooltip("Настройки платы видеозахвата")
                            .on_click(cx.listener(|s, _, _, cx| s.navigate(Screen::Settings, cx))),
                    ),
            )
            .child(self.editor.clone())
            .into_any_element()
    }
    fn video_settings(&self, cx: &Context<Self>) -> AnyElement {
        let entity = cx.entity();
        let devices = self.video_devices.clone();
        let current = self
            .video_config
            .as_ref()
            .and_then(|c| self.video_devices.iter().find(|d| d.id == c.device_id));
        let label = current.map(|d| d.name.clone()).unwrap_or_else(|| {
            if self.video_config.is_some() {
                "Устройство недоступно".into()
            } else {
                "Выберите устройство".into()
            }
        });
        let modes = current.map(|d| d.modes.clone()).unwrap_or_default();
        let mode_entity = cx.entity();
        let controls=v_flex().flex_1().min_w_0().gap_2()
            .child(div().text_sm().child("Устройство"))
            .child(h_flex().gap_2().items_center()
                .child(Button::new("video-device").outline().flex_1().min_w_0().label(label).icon(IconName::ChevronDown).disabled(self.video_running).dropdown_menu(move|mut menu,_,_|{for device in &devices{let entity=entity.clone();let id=device.id.clone();let preferred=device.preferred();menu=menu.item(PopupMenuItem::new(device.name.clone()).on_click(move|_,_,cx|entity.update(cx,|s,cx|{s.video_config=preferred.clone().map(|mode|video::Config{device_id:id.clone(),mode});s.video_note=if s.video_config.is_some(){"Устройство выбрано".into()}else{"Не удалось получить режимы. Остановите захват в других программах и обновите список".into()};cx.notify();})));}menu.max_h(px(300.)).scrollable(true)}))
                .child(Button::new("video-enumerate").outline().icon(IconName::RefreshCw).accessibility_label("Обновить список устройств видеозахвата").tooltip("Обновить список устройств").disabled(self.video_running).on_click(cx.listener(|s,_,_,cx|{s.video_note="Поиск устройств…".into();let _=s.video_worker.tx.send(video::Request::Enumerate);cx.notify();}))))
            .child(div().text_sm().child("Разрешение, частота и формат"))
            .child(Button::new("video-mode").outline().w_full().label(self.video_config.as_ref().map(|c|c.mode.label()).unwrap_or("Выберите видеорежим".into())).icon(IconName::ChevronDown).disabled(self.video_running||modes.is_empty()).dropdown_menu(move|mut menu,_,_|{for mode in &modes{let entity=mode_entity.clone();let mode=mode.clone();menu=menu.item(PopupMenuItem::new(mode.label()).on_click(move|_,_,cx|entity.update(cx,|s,cx|{if let Some(config)=s.video_config.as_mut(){config.mode=mode.clone();}cx.notify();})));}menu.max_h(px(300.)).scrollable(true)}))
            .child(h_flex().gap_2().pt_1()
                .child(Button::new("video-config-save").outline().label("Сохранить").disabled(self.video_config.is_none()||self.video_running).on_click(cx.listener(|s,_,_,cx|{s.send(Command::Activity(m53_client::activity::Action::new("settings.changed").field("video")));let _=s.pedal_tx.send(pedal::Request::SaveVideo(s.video_config.clone()));s.video_note="Сохранение настройки…".into();cx.notify();})))
                .child(Button::new("video-test").primary().label(if self.video_running{"Остановить"}else{"Проверить видео"}).disabled(self.video_config.is_none()).on_click(cx.listener(|s,_,_,cx|{if s.video_running{s.stop_video(cx)}else{s.start_video(cx)}}))));
        let preview = v_flex()
            .w(px(300.))
            .flex_shrink_0()
            .gap_2()
            .child(div().text_sm().child("Предпросмотр"))
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(169.))
                    .flex_shrink_0()
                    .overflow_hidden()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted)
                    .when_some(
                        self.video_image.clone().filter(|_| self.video_running),
                        |d, image| {
                            d.child(
                                img(image)
                                    .absolute()
                                    .inset_0()
                                    .size_full()
                                    .object_fit(ObjectFit::Contain),
                            )
                        },
                    )
                    .when(!self.video_running || self.video_image.is_none(), |d| {
                        d.child(
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(if self.video_running {
                                    "Ожидание кадра…"
                                } else {
                                    "Камера выключена"
                                }),
                        )
                    }),
            );
        v_flex().gap_3().pt_3().border_t_1().border_color(cx.theme().border)
            .child(div().font_semibold().child("Видеозахват · USB / PCI Express"))
            .child(h_flex().gap_4().items_start().child(controls).child(preview))
            .child(div().text_sm().child(self.video_note.clone()))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Перед запуском остановите захват в OBS и других программах. Для PCI Express нужен драйвер видеозахвата Windows."))
            .into_any_element()
    }
    fn colposcope_settings(&self, cx: &Context<Self>) -> AnyElement {
        let entity = cx.entity();
        let models: Vec<String> = pedal::COLPOSCOPES
            .iter()
            .map(|s| s.to_string())
            .chain(self.colposcope.custom_models.iter().cloned())
            .collect();
        v_flex().gap_2().pt_3().border_t_1().border_color(cx.theme().border)
            .child(div().font_semibold().child("Кольпоскоп"))
            .child(Button::new("colposcope-model").outline().label(self.colposcope.selected.clone().unwrap_or("Модель не выбрана".into())).icon(IconName::ChevronDown).dropdown_menu(move|mut menu,_,_|{for model in std::iter::once(None).chain(models.iter().cloned().map(Some)){let entity=entity.clone();menu=menu.item(PopupMenuItem::new(model.clone().unwrap_or("Не выбран".into())).on_click(move|_,_,cx|entity.update(cx,|s,cx|{let mut config=s.colposcope.clone();config.selected=model.clone();s.send(Command::Activity(m53_client::activity::Action::new("settings.changed").field("colposcope_model")));let _=s.pedal_tx.send(pedal::Request::SaveColposcope(config));cx.notify();})));}menu}))
            .child(h_flex().gap_2().child(Input::new(&self.colposcope_custom)).child(Button::new("add-colposcope").outline().label("Добавить").on_click(cx.listener(|s,_,_,cx|{let name=s.colposcope_custom.read(cx).value().trim().to_string();if name.is_empty(){return;}let mut config=s.colposcope.clone();if !pedal::COLPOSCOPES.iter().chain(config.custom_models.iter().map(|s|s.as_str()).collect::<Vec<_>>().iter()).any(|m|m.eq_ignore_ascii_case(&name)){config.custom_models.push(name.clone());}config.selected=Some(name);let _=s.pedal_tx.send(pedal::Request::SaveColposcope(config));cx.notify();}))))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Выбранная модель сохраняется для новых обследований и указывается в протоколе."))
            .into_any_element()
    }
    fn diagnosis_priority_settings(&self, cx: &Context<Self>) -> AnyElement {
        let mut panel = v_flex().gap_2().pt_3().border_t_1().border_color(cx.theme().border)
            .child(div().font_semibold().child("Основные диагнозы МКБ‑10"))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Общая группа для всех рабочих мест. Она отображается первой при выборе диагноза."));
        if let Some(draft) = &self.priority_draft {
            let mut rows = v_flex().id("priority-rows").gap_1().max_h(px(240.)).overflow_y_scroll();
            for (i, id) in draft.ids.iter().copied().enumerate() {
                let label = self.diagnoses.catalog.as_ref().and_then(|c| c.items.binary_search_by_key(&id, |e| e.guid).ok().map(|n| format!("{} {}", c.items[n].code, c.items[n].name)))
                    .unwrap_or_else(|| "Запись временно отсутствует в справочнике".into());
                rows = rows.child(h_flex().gap_1().items_center()
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .child(Button::new(("priority-up", i)).outline().label("↑").disabled(self.priority_busy || i == 0).tooltip("Переместить выше").on_click(cx.listener(move |s,_,_,cx| { if let Some(d) = &mut s.priority_draft { d.move_up(i); } cx.notify(); })))
                    .child(Button::new(("priority-down", i)).outline().label("↓").disabled(self.priority_busy || i + 1 == draft.ids.len()).tooltip("Переместить ниже").on_click(cx.listener(move |s,_,_,cx| { if let Some(d) = &mut s.priority_draft { d.move_down(i); } cx.notify(); })))
                    .child(Button::new(("priority-remove", i)).outline().label("×").disabled(self.priority_busy).tooltip("Удалить из группы").on_click(cx.listener(move |s,_,_,cx| { if let Some(d) = &mut s.priority_draft { d.remove(i); } cx.notify(); }))));
            }
            panel = panel.child(rows).child(h_flex().gap_2().child(div().flex_1().child(Input::new(&self.priority_search)))
                .child(Button::new("priority-find").outline().label("Найти").disabled(self.priority_busy).on_click(cx.listener(|s,_,_,cx| { s.priority_query = s.priority_search.read(cx).value().trim().to_owned(); cx.notify(); }))));
            if !self.priority_query.is_empty() {
                if let Some(catalog) = &self.diagnoses.catalog {
                    let mut results = v_flex().id("priority-results").gap_1().max_h(px(200.)).overflow_y_scroll();
                    for index in catalog.search(&self.priority_query, true, 50) {
                        let entry = &catalog.items[index];
                        let id = entry.guid;
                        results = results.child(Button::new(("priority-add", index)).outline()
                            .label(format!("{} {}", entry.code, entry.name)).disabled(self.priority_busy || draft.ids.contains(&id))
                            .on_click(cx.listener(move |s,_,_,cx| { if let Some(d) = &mut s.priority_draft { d.add(id); } cx.notify(); })));
                    }
                    panel = panel.child(results);
                }
            }
            panel = panel.child(h_flex().gap_2()
                .child(Button::new("priority-save").primary().label("Сохранить для всех").disabled(self.priority_busy).on_click(cx.listener(|s,_,_,cx| {
                    if let Some(d) = s.priority_draft.clone() { let request=Uuid::new_v4(); s.priority_busy = true; s.priority_request=Some(request); s.priority_message.clear(); s.send(Command::SaveDiagnosisPriority { request, base: d.base, ids: d.ids }); } cx.notify();
                })))
                .child(Button::new("priority-cancel").outline().label("Отменить и обновить").disabled(self.priority_busy).on_click(cx.listener(|s,_,_,cx| { s.priority_draft = None; s.priority_query.clear(); s.priority_message = "Запрошено обновление группы".into(); s.send(Command::RefreshDiagnosisPriority); cx.notify(); }))));
        } else {
            let count = self.diagnoses.priority.as_ref().map(|p| p.diagnosis_ids.len());
            panel = panel.child(div().text_sm().child(count.map(|n| format!("В группе: {n}")).unwrap_or_else(|| "Группа ещё не получена с сервера".into())))
                .child(Button::new("priority-edit").outline().label("Изменить группу").disabled(count.is_none() || self.state["authenticated"] != true)
                    .on_click(cx.listener(|s,_,_,cx| { if let Some(group) = &s.diagnoses.priority { s.priority_draft = Some(m53_client::diagnosis_priority::Draft::new((**group).clone())); s.priority_message.clear(); } cx.notify(); })));
        }
        if !self.diagnoses.priority_error.is_empty() { panel = panel.child(div().text_sm().child(self.diagnoses.priority_error.clone())); }
        if !self.priority_message.is_empty() { panel = panel.child(div().text_sm().child(self.priority_message.clone())); }
        panel.into_any_element()
    }
    fn settings(&self, cx: &Context<Self>) -> AnyElement {
        let entity = cx.entity();
        v_flex().flex_1().min_h_0().id("hardware-settings").overflow_y_scroll().p_4().items_center().child(v_flex().w_full().max_w(px(760.)).gap_3()
            .child(div().text_lg().font_semibold().child("Настройки"))
            .child(self.guidance_settings(cx))
            .child(self.diagnosis_priority_settings(cx))
            .child(div().text_sm().child(format!("Рабочее место: {} · Ключ: {}",self.state["native_activity"]["workstation"]["name"].as_str().unwrap_or("Ожидание сервера"),self.state["native_activity"]["workstation"]["key_name"].as_str().unwrap_or("—"))))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(format!("Журнал действий: ожидают передачи {}",self.state["native_activity"]["pending"].as_u64().unwrap_or(0))))
            .child("Адрес API").child(Input::new(&self.url)).child("Ключ рабочего места").child(Input::new(&self.key))
            .child(div().pt_3().border_t_1().border_color(cx.theme().border).font_semibold().child("Педаль · FS2016BT1_D"))
            .child(h_flex().gap_2().child(div().flex_1().child(format!("Связь: {}",self.pedal_connection.label())))
                .child(Button::new("pedal-bluetooth").outline().label("Bluetooth Windows").on_click(|_,_,cx|cx.open_url("ms-settings:bluetooth")))
                .child(Button::new("pedal-refresh").outline().label("Проверить связь").on_click(cx.listener(|s,_,_,cx|{let _=s.pedal_tx.send(pedal::Request::Refresh);s.pedal_connection=PedalConnection::Checking;s.pedal_checked=std::time::Instant::now();cx.notify();}))))
            .child(gpui_kit::component::switch::Switch::new("pedal-enabled").label("Использовать педаль").checked(self.pedal_draft.enabled).disabled(self.pedal_saving).on_change(cx.listener(|s,value,_,cx|{s.pedal_draft.enabled=*value;s.pedal_latch.reset();cx.notify();})))
            .child(gpui_kit::component::switch::Switch::new("pedal-sound").label("Звук нажатия педали").checked(self.pedal_draft.sound).disabled(self.pedal_saving).on_change(cx.listener(|s,value,_,cx|{s.pedal_draft.sound=*value;cx.notify();})))
            .child(Button::new("pedal-key").outline().label(format!("Клавиша: F{}",self.pedal_draft.key)).icon(IconName::ChevronDown).disabled(self.pedal_saving).dropdown_menu(move |mut menu,_,_|{for key in 13..=22 {let entity=entity.clone();menu=menu.item(PopupMenuItem::new(format!("F{key}")).on_click(move |_,_,cx|entity.update(cx,|s,cx|{s.pedal_draft.key=key;s.pedal_testing=false;s.pedal_latch.reset();s.pedal_test_latch.reset();cx.notify();})));}menu}))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Клавиша должна совпадать с настройкой FootSwitch. Программа не перепрограммирует педаль. Сопряжение выполняется в Bluetooth Windows."))
            .child(h_flex().gap_2()
                .child(Button::new("pedal-save").primary().label("Сохранить педаль").disabled(self.pedal_saving).on_click(cx.listener(|s,_,_,cx|{match s.pedal_tx.send(pedal::Request::Save(s.pedal_draft)){Ok(())=>s.pedal_saving=true,Err(_)=>s.pedal_note="Служба оборудования недоступна".into()};cx.notify();})))
                .child(Button::new("pedal-test").outline().label(if self.pedal_testing {"Остановить проверку"}else{"Проверить нажатие"}).on_click(cx.listener(|s,_,w,cx|{s.pedal_testing=!s.pedal_testing;s.pedal_counts=(0,0,0);s.pedal_test_latch.reset();s.pedal_focus.focus(w,cx);cx.notify();}))))
            .when(self.pedal_testing,|d|d.child(div().font_semibold().child(format!("F{} · Нажатий: {} · Отпусканий: {} · Повторов: {}",self.pedal_draft.key,self.pedal_counts.0,self.pedal_counts.1,self.pedal_counts.2))))
            .child(div().text_sm().child(self.pedal_note.clone()))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Проверка работает в активном окне. Источник клавиатурного события отдельно не различается. Снимки педалью создаются только на этапе обследования."))
            .child(self.video_settings(cx))
            .child(self.colposcope_settings(cx))
            .child(Button::new("close-settings").outline().label("Вернуться").on_click(cx.listener(|s,_,_,cx|{s.navigate(s.settings_return,cx);s.send(Command::LoginProfiles(s.url.read(cx).value().to_string(),s.key.read(cx).value().to_string()));cx.notify();}))))
            .into_any_element()
    }
    fn change_task_date(&mut self, days:i64, cx:&mut Context<Self>) {
        if let Some(date)=self.task_date.checked_add_signed(chrono::Duration::days(days)) {
            self.task_date=date;
            self.send(Command::Registry(crate::registry_browser::Request::Date(date.format("%Y-%m-%d").to_string())));
            self.state["native_tasks"]=json!([]);
            self.state["native_registry"]["tasks_loading"]=json!(true);
            cx.notify();
        }
    }
}
impl Render for Workplace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.viewport = window.viewport_size();
        if let Some(url) = self.transport_address.take() {
            self.url.update(cx, |input, cx| input.set_value(url, window, cx));
        }
        // Live frames have unique texture IDs. Retire the previously painted
        // image instead of accumulating it in the window's GPU sprite atlas.
        if self.video_painted.as_ref().map(|image| image.id)
            != self.video_image.as_ref().map(|image| image.id)
        {
            if let Some(previous) = self.video_painted.take() {
                let _ = window.drop_image(previous);
            }
            self.video_painted = self.video_image.clone();
        }
        let authenticated = self.state["authenticated"] == true;
        if !authenticated {
            self.close_history(cx);self.patient_editor=None;self.patient_editor_pending=None;self.patient_edit_request=None;self.patient_editor_subscription=None;
        }
        if let Some(context)=self.patient_editor_pending.take() {
            let panel=cx.new(|cx|crate::patient_editor::PatientEditor::new(context,window,cx));
            self.patient_editor_subscription=Some(cx.subscribe(&panel,|s,_,event:&crate::patient_editor::PatientEditEvent,cx|{
                match event.clone(){
                    crate::patient_editor::PatientEditEvent::Cancel=>{s.patient_editor=None;s.patient_editor_subscription=None;},
                    crate::patient_editor::PatientEditEvent::Save{patient,body}=>{
                        let request=Uuid::new_v4();s.patient_edit_request=Some((request,patient));s.send(Command::ManualPatient{request,patient,body:Some(body)});
                    },
                }cx.notify();
            }));
            self.patient_editor=Some(panel);
        }
        let nav = self.application_header(cx);
        let phrase_width = if self.viewport.width > px(1600.) {360.} else {300.};
        let form_width = (f32::from(self.viewport.width) - phrase_width - 24.).max(1.);
        for form in self.form.iter().chain(self.remote_form.iter()) {
            form.update(cx, |f, cx| {f.set_available_width(form_width, cx);f.set_diagnoses(self.diagnoses.clone());});
        }
        if let Some(form)=self.history.as_ref().and_then(|h|h.form.as_ref()){
            form.update(cx,|f,cx|f.set_available_width((f32::from(self.viewport.width)-306.).max(1.),cx));
        }
        let details_width = (f32::from(self.viewport.width) * 0.29).clamp(230., 340.);
        let table_width = (f32::from(self.viewport.width) - details_width - 32.).max(1.);
        let registry_locked=self.patient_registry_locked();
        self.patient_table.update(cx, |table, cx| {
            table.row_selectable=!registry_locked;
            if table.delegate_mut().set_available_width(table_width) {table.refresh(cx);}
        });
        let visible_form=visible_protocol_form(
            self.history.as_ref().and_then(|history|history.form.as_ref()),
            self.remote_form.as_ref().filter(|_|self.history.is_none() && self.viewing_remote),
            self.form.as_ref().filter(|_|self.history.is_none() && !self.viewing_remote),
        );
        if let Some(form) = visible_form {
            let selected = self
                .editor
                .read(cx)
                .selected
                .filter(|id| form.read(cx).value["colpo"]["marks"][id.to_string()].is_object());
            form.update(cx, |f, _| {
                f.compact_marker_card = self.screen == Screen::Study;
                f.focus_mark = if self.screen == Screen::Study {
                    selected
                } else {
                    None
                }
            });
            self.editor.update(cx, |e, _| {
                e.properties = if self.screen == Screen::Study {
                    Some(form.clone())
                } else {
                    None
                }
            });
        }
        let body = if self.history.is_some() && !self.approved_history() && authenticated && self.screen != Screen::Settings {
            self.history_view(cx)
        } else if self.screen == Screen::Settings {
            self.settings(cx)
        } else if !authenticated {
            self.login_view(cx)
        } else {
            match self.screen {
                Screen::Patients => self.patients(cx),
                Screen::Study => self.study_view(cx),
                Screen::Protocol => {
                    let form=if self.approved_history() {self.history.as_ref().and_then(|h|h.form.as_ref())}
                        else if self.viewing_remote {self.remote_form.as_ref()}else{self.form.as_ref()};
                    h_flex().flex_1().min_h_0().min_w_0().items_start()
                        .child(div().id(if self.approved_history() {"historical-protocol-form"} else {"current-protocol-form"}).test_support().flex_1().min_w_0().h_full().child(form.map(|f|f.clone().into_any_element()).unwrap_or_else(||div().child("Откройте обследование").into_any_element())))
                        .child(v_flex().id("visit-template-sidebar").w(px(if window.viewport_size().width>px(1600.) {360.}else{300.})).flex_shrink_0().h_full().min_h_0().overflow_y_scroll().border_l_1().border_color(cx.theme().border).child(self.visit_panel.clone()).child(div().min_h(px(300.)).child(self.phrase_panel.clone())))
                        .into_any_element()
                },
                Screen::Report => self.report(cx),
                Screen::Settings => unreachable!(),
            }
        };
        let body = if self.approved_history() && self.screen != Screen::Settings
            && self.history.as_ref().is_some_and(|history|history.revisions_expanded)
        {
            h_flex().flex_1().min_h_0().items_stretch().overflow_hidden()
                .child(self.history_revision_list(cx))
                .child(v_flex().flex_1().min_w_0().min_h_0().overflow_hidden().child(body))
                .into_any_element()
        } else { body };
        let mut root = v_flex()
            .track_focus(&self.pedal_focus)
            .capture_key_down(cx.listener(|s, event: &KeyDownEvent, _, cx| {
                let key = event.keystroke.key.as_str();
                if s.screen == Screen::Settings && s.pedal_testing && s.pedal_draft.matches(key) {
                    let config = PedalConfig {
                        enabled: true,
                        ..s.pedal_draft
                    };
                    if s.pedal_test_latch.press(config, key, event.is_held, true) {
                        s.pedal_counts.0 += 1;
                        config.feedback();
                    } else {
                        s.pedal_counts.2 += 1;
                    }
                    cx.stop_propagation();
                    cx.notify();
                } else if s.pedal_latch.press(
                    s.pedal_applied,
                    key,
                    event.is_held,
                    s.screen == Screen::Study,
                ) {
                    // A received key is actionable even while Bluetooth status is stale.
                    // Keep the physical status indicator independent from keyboard input.
                    if let Some(reason) = s.capture_block_reason(cx) {
                        s.message = reason.into();
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }
                    s.pedal_applied.feedback();
                    if let Some(d)=&s.detail {s.send(Command::Activity(m53_client::activity::Action::new("pedal.capture_pressed").study(d.study.id)));}
                    s.capture_frame(cx);
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .capture_key_up(cx.listener(|s, event: &KeyUpEvent, _, cx| {
                let key = event.keystroke.key.as_str();
                s.pedal_latch.release(s.pedal_applied, key);
                if s.screen == Screen::Settings && s.pedal_testing && s.pedal_draft.matches(key) {
                    s.pedal_test_latch.release(s.pedal_draft, key);
                    s.pedal_counts.1 += 1;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .text_size(px(14.))
            .child(nav);
        if self.history.is_none() || self.approved_history() {
            if let Some(patient) = self.patient_summary(cx) {root = root.child(patient);}
        }
        if self.history.as_ref().is_none_or(|history| history.prefer_approved) {
            if let Some(steps) = self.stage_navigation(cx) {root = root.child(steps);}
        }
        if let Some(metadata)=self.protocol_metadata_bar(cx){root=root.child(metadata);}
        if self.history.is_none() && self.screen == Screen::Protocol && self.remote_form.is_some() {
            root=root.child(h_flex().px_4().py_2().gap_2().child("Есть конфликт версий. Сравните данные, затем отредактируйте локальную форму.")
            .child(Button::new("local-version").ghost().small().label("Локальная версия").selected(!self.viewing_remote).on_click(cx.listener(|s,_,_,cx|{s.viewing_remote=false;cx.notify();})))
            .child(Button::new("remote-version").ghost().small().label("Серверная — просмотр").selected(self.viewing_remote).on_click(cx.listener(|s,_,_,cx|{s.viewing_remote=true;cx.notify();})))
            .child(Button::new("merge-version").outline().small().label("Отправить объединенную локальную версию").disabled(self.viewing_remote).on_click(cx.listener(|s,_,_,cx|{s.flush_form(cx,true);s.message.clear();s.send(Command::Resolve);}))))
        }
        let mut protocol_ready = false;
        if self.history.is_none() && self.screen == Screen::Protocol {
            let approved=self.detail.as_ref().is_some_and(|d|d.study.status=="approved");
            let mut problems=match (&self.form,&self.dictionary) {
                (Some(form),Some(dictionary))=>serde_json::from_value::<m53_domain::protocol::Protocol>(form.read(cx).value.clone())
                    .map(|protocol|m53_client::workspace::approval_problems(&protocol,dictionary))
                    .unwrap_or_else(|_|vec!["Проверьте заполнение протокола".into()]),
                _=>vec!["Дождитесь загрузки протокола и справочника".into()],
            };
            if self.form.as_ref().is_some_and(|f|!f.read(cx).unfinished.is_empty()) {
                problems.push("Завершите ввод полей протокола".into());
            }
            if !approved && self.remote_form.is_some() {
                problems.push("Сверьте конфликт версий — локальный черновик доступен для работы".into());
            }
            let (notice,ready)=protocol_notice(approved,&problems);
            protocol_ready = ready && !approved && self.remote_form.is_none();
            let tooltip=notice.clone();
            let navigation = self.form.as_ref().map(|f| f.update(cx, |f, _| f.approval_navigation())).unwrap_or_default();
            let form = self.form.clone();
            root=root.child(h_flex().h(px(32.)).flex_shrink_0().px_4().items_center()
                .border_b_1().border_color(cx.theme().border)
                .text_sm().text_color(if ready {cx.theme().success} else {cx.theme().danger})
                .child(div().id("protocol-approval-notice").flex_1().min_w_0().text_ellipsis().test_support()
                    .child(notice)
                    .tooltip(move|window,cx|gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window,cx)))
                .when(!approved && !navigation.is_empty(), |row| row.child(
                    Button::new("validation-navigation").ghost().small().label("Перейти к полю").icon(IconName::ChevronDown)
                        .disabled(self.viewing_remote)
                        .tooltip(if self.viewing_remote { "Сначала выберите «Локальная версия»" } else { "Раскрыть раздел с незаполненным обязательным полем" })
                        .dropdown_menu(move |mut menu, _, _| {
                            for (path, label) in &navigation {
                                let path = path.clone(); let form = form.clone();
                                menu = menu.item(PopupMenuItem::new(label.clone()).on_click(move |_, _, cx| {
                                    if let Some(form) = &form { form.update(cx, |f, cx| {f.navigate_problem(&path, cx);}); }
                                }));
                            }
                            menu
                        }))));
            if !approved && self.approval_pending {
                root = root.child(div().px_4().py_1().text_sm().text_color(cx.theme().muted_foreground)
                    .child("Ожидается подтверждение прежней редакции. Текущий черновик доступен для работы и сохраняется на компьютере."));
            }
        }
        if self.history.is_none(){if let Some(card) = self.guidance_card(cx) { root = root.child(card); }}
        root = root.child(v_flex().flex_1().min_h_0().child(body));
        if self.history.is_none() && self.screen == Screen::Study && authenticated && self.detail.is_some() {
            root = root.child(
                h_flex()
                    .h(px(48.))
                    .flex_shrink_0()
                    .px_4()
                    .gap_2()
                    .items_center()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(if self.workflow.examination_complete {
                                "Этап обследования завершен"
                            } else {
                                if self.video_running {"Сохраните нужные кадры. «Провести осмотр» остановит камеру для разметки."} else {"Разметьте сохранённые снимки, затем заполните протокол"}
                            }),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("finish-examination")
                            .primary()
                            .label(if self.video_running {"Провести осмотр"} else {"Заполнить протокол"})
                            .disabled(
                                self.workflow_request.is_some()
                                    || self.editor.read(cx).io_pending > 0,
                            )
                            .on_click(cx.listener(|s, _, _, cx| {
                                if s.video_running {
                                    s.stop_video(cx);
                                    s.message = "Нанесите метки на сохранённые снимки, затем заполните протокол".into();
                                    return;
                                }
                                if s.step_allowed(Screen::Protocol, cx) {
                                    s.navigate(Screen::Protocol, cx);
                                } else {
                                    s.request_workflow(WorkflowAction::FinishExamination, cx);
                                }
                            })),
                    ),
            );
        }
        if self.history.is_none() && self.screen == Screen::Protocol && authenticated {
            if !self.approval_message.is_empty() {
                root = root.child(div().flex_shrink_0().border_t_1().border_color(cx.theme().border)
                    .px_4().py_2().text_sm().child(self.approval_message.clone()));
            }
            root = root.child(
                h_flex()
                    .h(px(48.))
                    .px_4()
                    .gap_2()
                    .items_center()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("back-to-study")
                            .ghost().icon(IconName::ArrowLeft).label("Вернуться к обследованию")
                            .disabled(self.workflow_request.is_some())
                            .on_click(cx.listener(|s, _, _, cx| s.navigate(Screen::Study, cx))),
                    )
                    .child(
                        Button::new("draft")
                            .outline()
                            .label("Сохранить черновик")
                            .disabled(self.workflow_request.is_some())
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.flush_form(cx, true);
                                s.send(Command::Save);
                            })),
                    )
                    .when(self.state["manual_identity_context"]["origin"]=="manual_continuity" && self.state["manual_identity_context"]["patient"]["identity_state"]=="provisional", |row| row.child(
                        Button::new("manual-identity-confirm")
                            .outline()
                            .label(if self.state["manual_identity_context"]["manual_identity_verification"].is_object() {"Сверка подтверждена"}else{"Подтвердить сверку"})
                            .tooltip("Нажатием подтверждаю: я лично сверил(а) ФИО, дату рождения и пол пациента на экране. Утверждение протокола выполняется отдельно")
                            .disabled(self.workflow_request.is_some() || self.approval_busy || self.approval_pending || self.detail.as_ref().is_none_or(|d|d.study.status!="draft"))
                            .on_click(cx.listener(|s,_,_,cx|{s.flush_form(cx,true);s.send(Command::ConfirmManualIdentity);cx.notify();}))
                    ))
                    .child(
                        Button::new("approve")
                            .outline()
                            .when(protocol_ready, |button| button.primary())
                            .label(if self.approval_busy{"Утверждение…"}else if self.approval_pending{"Ожидает подтверждения"}else{"Утвердить"})
                            .tooltip("Утверждение выполняется после сохранения и подтверждается сервером")
                            .disabled(self.approval_busy || self.approval_pending ||
                                self.workflow_request.is_some()
                                    || self
                                        .detail
                                        .as_ref()
                                        .is_none_or(|d| d.study.status == "approved"),
                            )
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.request_approval(cx);
                            })),
                    )
                    .child(
                        Button::new("reopen")
                            .ghost()
                            .label("Новая редакция")
                            .disabled(
                                self.workflow_request.is_some()
                                    || self
                                        .detail
                                        .as_ref()
                                        .is_none_or(|d| d.study.status != "approved"),
                            )
                            .on_click(cx.listener(|s, _, _, _| s.send(Command::Reopen))),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("to-report")
                            .outline()
                            .tooltip("Предпросмотр доступен до утверждения. Печать и отправка — после утверждения")
                            .label("Заполнить отчёт")
                            .disabled(self.workflow_request.is_some() || self.viewing_remote)
                            .on_click(cx.listener(|s, _, _, cx| {
                                if s.step_allowed(Screen::Report, cx) {
                                    s.navigate(Screen::Report, cx);
                                } else {
                                    s.request_workflow(WorkflowAction::FinishProtocol, cx);
                                }
                            })),
                    ),
            );
        }
        if (self.history.is_none() || self.approved_history()) && self.screen == Screen::Report && authenticated {
            root = root.child(h_flex().h(px(48.)).flex_shrink_0().px_4().items_center()
                .border_t_1().border_color(cx.theme().border)
                .child(Button::new("back-to-protocol").ghost().icon(IconName::ArrowLeft)
                    .label("Вернуться к протоколу")
                    .disabled(self.workflow_request.is_some() || (self.report_busy && self.preview_request.is_none()))
                    .on_click(cx.listener(|s, _, _, cx| s.navigate(Screen::Protocol, cx)))));
        }
        let local = if authenticated {
            format!(
                "{} · {} · {}",
                if self.state["sync"]["durable"] == true {
                    "Сохранено на компьютере"
                } else {
                    "Локальная запись…"
                },
                if self.state["sync"]["online"] == true {
                    "API доступен"
                } else {
                    "Нет связи с API"
                },
                {
                    let status = self.state["sync"]["server_status"].as_str().unwrap_or("Подтверждение сервера ещё не получено");
                    let others = self.state["sync"]["other_blocked"].as_u64().unwrap_or(0);
                    if self.detail.is_some() && others > 0 { format!("{status} · Ошибок в других обследованиях: {others}") } else { status.to_string() }
                }
            )
        } else {
            "Рабочее место".into()
        };
        let status_message = footer_message(
            &self.message,
            self.state["message"].as_str().unwrap_or(""),
            self.state["sync"]["error"].as_str().unwrap_or(""),
            self.state["native_activity"]["error"].as_str().unwrap_or(""),
        );
        root.child(
            h_flex()
                .h(px(30.))
                .flex_shrink_0()
                .px_4()
                .gap_3()
                .items_center()
                .border_t_1()
                .border_color(cx.theme().border)
                .text_xs()
                .child(div().id("local-save-status").flex_1().min_w_0().truncate()
                    .child(local.clone())
                    .tooltip(move |w, cx| Tooltip::new(local.clone()).build(w, cx)))
                .child(div().id("workplace-status").flex_1().min_w_0().truncate()
                    .child(status_message.clone())
                    .tooltip(move |w, cx| Tooltip::new(status_message.clone()).build(w, cx))),
        )
    }
}

pub fn launch() -> anyhow::Result<()> {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(|cx| {
            gpui_kit::init(cx);
            m53_gpui_editor::calendar_locale::init();
            crate::appearance::sync_system(None, cx);
            cx.activate(true);
            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(m53_client::APP_NAME.into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1360.), px(900.)),
                    cx,
                ))),
                window_min_size: Some(size(px(1024.), px(720.))),
                ..Default::default()
            };
            let (window, view) =
                gpui_kit::open_window(options, cx, |w, cx| cx.new(|cx| Workplace::new(w, cx)))
                    .expect("Не удалось открыть GPUI");
            #[cfg(all(windows, not(feature = "standalone")))]
            m53_client::updates::launcher::ready();
            let weak = view.downgrade();
            let _ = window.update(cx, |_, w, cx| {
                w.on_window_should_close(cx, move |_, cx| {
                    weak.update(cx, |s, cx| s.can_close(cx)).unwrap_or(true)
                })
            });
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
        });
    Ok(())
}

#[cfg(not(windows))]
pub fn render_previews(directory: &str) -> anyhow::Result<()> {
    use gpui_kit::component::{Theme, ThemeMode};
    use gpui_kit::test::TestWindowExt;
    use std::sync::Arc;
    std::fs::create_dir_all(directory)?;
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::AllAssets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);
    m53_gpui_editor::calendar_locale::init();
    let (window, view) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(
                        px(std::env::var("M53_GPUI_PREVIEW_WIDTH")
                            .ok()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(1360.)),
                        px(std::env::var("M53_GPUI_PREVIEW_HEIGHT")
                            .ok()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(900.)),
                    ),
                ))),
                show: false,
                focus: false,
                ..Default::default()
            },
            cx,
            |w, cx| cx.new(|cx| Workplace::new(w, cx)),
        )
    })?;
    cx.update(|cx| {
        Theme::change(
            if std::env::var_os("M53_GPUI_PREVIEW_LIGHT").is_some() {
                ThemeMode::Light
            } else {
                ThemeMode::Dark
            },
            None,
            cx,
        );
        view.update(cx, |s, _| s.preview_mode = true);
        crate::appearance::contrast(cx);
    });
    cx.update_window(window, |_, w, cx| w.render_frame(cx))?;
    cx.capture_screenshot(window)?
        .save(std::path::Path::new(directory).join("native-login.png"))?;
    let (mut detail, dictionary, asset) = crate::verification::fixture();
    detail.protocol.as_mut().unwrap().schema_version = 2;
    detail.protocol.as_mut().unwrap().colpo = Some(Default::default());
    detail.patient.family_name = "Синтетическая".into();
    detail.patient.given_name = "Пациентка".into();
    detail.patient.external_id = Some("11111111-2222-4333-8444-555555555555".into());
    let mut photo = m53_gpui_editor::Photo::new(1000, 800);
    photo.id = asset;
    photo.add_callout(7, 0.25, 0.35);
    detail = m53_gpui_editor::integration::apply(
        &detail,
        &dictionary,
        &m53_gpui_editor::Session {
            photos: vec![photo.clone()],
            selected: 0,
        },
    )
    .map_err(anyhow::Error::msg)?;
    let pixels = image::RgbaImage::from_pixel(1000, 800, image::Rgba([225, 225, 225, 255]));
    let image = Arc::new(RenderImage::new(vec![image::Frame::new(pixels)]));
    let thumbnail=Arc::new(RenderImage::new(vec![image::Frame::new(image::RgbaImage::from_pixel(256,205,image::Rgba([225,225,225,255])))]));
    let layout = ReportLayout {
        title: "Протокол кольпоскопии".into(),
        blocks: vec![
            ReportBlock::Patient,
            ReportBlock::Assessment,
            ReportBlock::Findings,
            ReportBlock::Conclusion,
            ReportBlock::Recommendations,
            ReportBlock::Photos,
            ReportBlock::Signature,
        ],
        photo_columns: 1, letterhead: None,
    };
    cx.update(|cx|view.update(cx,|s,cx| {
        s.state=json!({"authenticated":true,"doctor":"Тестовый врач","detail":detail,"sync":{"online":false,"pending":0,"durable":true},"message":"Синтетические данные"});
        s.patients=vec![detail.patient.clone()];s.studies=vec![detail.study.clone()];s.templates=vec![Template{id:Uuid::new_v4(),version_id:Uuid::new_v4(),version:1,layout:layout.clone()}];
        s.receive(Event::Opened{detail:Box::new(detail.clone()),dictionary:dictionary.clone(),journal:json!({"selected_photos":[asset]})},cx);
        s.receive(Event::Texture{study:detail.study.id,asset,width:1000,height:800,image:image.clone()},cx);
        s.receive(Event::Thumbnail{study:detail.study.id,asset,image:thumbnail.clone()},cx);
        s.refresh_patient_table(cx);
        s.state["native_tasks"]=json!([{"id":Uuid::new_v4(),"row_version":1,"appointment_date":s.task_date.format("%Y-%m-%d").to_string(),"patient":{"last_name":"Тестовый","first_name":"Пациент"},"assigned_doctor_id":null}]);
        use m53_client::ui_guidance::{Snapshot, ContextToken, Scope};
        s.receive(Event::Guidance(Snapshot { epoch:2, context:Some(ContextToken {epoch:2,scope:Scope {
            server_namespace:"http://127.0.0.1:8787".into(),organization_id:Uuid::from_u128(1),
            user_id:Uuid::from_u128(2),workstation_id:Uuid::from_u128(3),
        }}), writable:true, ..Default::default() }), cx);
        s.state["native_registry"]=json!({"patients_status":"Показаны сохранённые данные. Сервер недоступен","tasks_status":"Показаны сохранённые задания. Сервер недоступен"});
    }));
    if std::env::var_os("M53_REGISTRY_ROUTING_PROOF").is_some() {
        let (tx,rx)=mpsc::channel();
        let mut approved=detail.clone();
        approved.study.status="approved".into();
        approved.revision_id=Some(Uuid::new_v4());
        let mut draft=detail.clone();
        draft.study.id=Uuid::new_v4();
        draft.study.status="draft".into();
        draft.revision_id=Some(Uuid::new_v4());
        let approved_id=approved.study.id;
        let draft_id=draft.study.id;
        cx.update(|cx|view.update(cx,|s,cx|{
            s.worker=tx;
            s.screen=Screen::Patients;
            s.patient=Some(detail.patient.id);
            s.detail=None;
            s.preview_study=Some(approved_id);
            s.studies=vec![approved.study.clone(),draft.study.clone()];
            cx.notify();
        }));
        cx.update_window(window,|_,w,cx|{
            w.render_frame(cx);
            w.click(format!("study-{draft_id}"),cx);
            w.render_frame(cx);
            assert_eq!(view.read(cx).preview_study,Some(draft_id));
            assert!(!rx.try_iter().any(|command|matches!(command,Command::Open(_))));
            w.click("open-selected-study",cx);
            assert!(rx.try_iter().any(|command|matches!(command,Command::Open(id) if id==draft_id)));
            view.update(cx,|s,cx|s.receive(Event::Opened{detail:Box::new(draft.clone()),dictionary:dictionary.clone(),journal:json!({})},cx));
            w.render_frame(cx);
            assert_eq!(view.read(cx).detail.as_ref().unwrap().study.id,draft_id);
            assert_eq!(view.read(cx).detail.as_ref().unwrap().revision_id,draft.revision_id);
            assert!(view.read(cx).screen==Screen::Protocol && view.read(cx).history.is_none());
            assert!(!view.read(cx).form.as_ref().unwrap().read(cx).readonly);
            rx.try_iter().for_each(drop);
            w.click("save-and-exit",cx);
            let (study,request,action)=rx.try_iter().find_map(|command|match command {
                Command::Workflow{study,request,action,close_preview:false}=>Some((study,request,action)),
                _=>None,
            }).unwrap();
            assert_eq!(study,draft_id);
            view.update(cx,|s,cx|s.receive(Event::Workflow{study,request,action,result:Ok(WorkflowStatus::default())},cx));
            w.render_frame(cx);
            assert!(view.read(cx).screen==Screen::Patients);
            assert_eq!(view.read(cx).preview_study,Some(draft_id));
            w.double_click(format!("study-{approved_id}"),cx);
            assert!(rx.try_iter().any(|command|matches!(command,Command::Open(id) if id==approved_id)));
            view.update(cx,|s,cx|s.receive(Event::Opened{detail:Box::new(approved.clone()),dictionary:dictionary.clone(),journal:json!({})},cx));
            w.render_frame(cx);
            assert_eq!(view.read(cx).detail.as_ref().unwrap().revision_id,approved.revision_id);
            assert!(view.read(cx).screen==Screen::Report);
            assert!(view.read(cx).form.as_ref().unwrap().read(cx).readonly);
            assert!(rx.try_iter().any(|command|matches!(command,Command::Preview{study,..} if study==approved_id)));
        })?;
        println!("PASS: selected draft opens by button, return keeps selection, double click opens the exact approved report");
        return Ok(());
    }
    if std::env::var_os("M53_APPROVED_ENTRY_PROOF").is_some() {
        cx.update(|cx|view.update(cx,|s,cx|{
            let mut approved=detail.clone();
            approved.study.status="approved".into();
            approved.revision_id=Some(Uuid::new_v4());
            s.studies = vec![approved.study.clone()];
            s.open_study(detail.study.id,cx);
            s.receive(Event::Opened{detail:Box::new(approved),dictionary:dictionary.clone(),journal:json!({})},cx);
            assert!(s.screen==Screen::Report,"approved study must start at Report");
            assert!(s.preview_request.is_some(),"opening an approved study must start report preview");
            assert!(s.form.as_ref().unwrap().read(cx).readonly);

            let mut draft=detail.clone();
            draft.study.status="draft".into();
            draft.revision_id=Some(Uuid::new_v4());
            s.receive(Event::Opened{detail:Box::new(draft.clone()),dictionary:dictionary.clone(),journal:json!({})},cx);
            assert!(s.screen==Screen::Protocol,"confirmed new revision must stay on Protocol");
            assert!(!s.form.as_ref().unwrap().read(cx).readonly);
            s.studies = vec![draft.study.clone()];
            s.open_study(detail.study.id,cx);
            s.receive(Event::Opened{detail:Box::new(draft.clone()),dictionary:dictionary.clone(),journal:json!({})},cx);
            assert!(s.screen==Screen::Protocol,"a selected draft must not open an approved report");
            assert!(s.history.is_none());
            assert!(!s.form.as_ref().unwrap().read(cx).readonly);
            s.studies[0].status = "approved".into();
            s.open_study(detail.study.id,cx);
            s.receive(Event::Opened{detail:Box::new(draft.clone()),dictionary:dictionary.clone(),journal:json!({})},cx);
            let study=draft.study.id;
            let request=s.history.as_ref().unwrap().request.unwrap().0;
            s.receive(Event::ProtocolHistory{request,study,revision:None,result:Ok((json!({"study_id":study,
                "revisions":[{"revision_id":draft.revision_id,"revision_number":36,"status":"draft"}],"next_before_revision":36}),None))},cx);
            let older=s.history.as_ref().unwrap().request.unwrap().0;
            let approved=Uuid::new_v4();
            s.receive(Event::ProtocolHistory{request:older,study,revision:None,result:Ok((json!({"study_id":study,
                "revisions":[{"revision_id":approved,"revision_number":33,"status":"approved"}],"next_before_revision":null}),None))},cx);
            let (request,selected)=s.history.as_ref().unwrap().request.unwrap();
            assert_eq!(selected,Some(approved));
            let mut approved_protocol = draft.protocol.clone().unwrap();
            approved_protocol.conclusion.value = m53_domain::protocol::Assessment::Assessed("Approved snapshot only".into());
            s.receive(Event::ProtocolHistory{request,study,revision:Some(approved),result:Ok((json!({
                "study_id":study,"revision_id":approved,"revision_number":33,"readonly":true,"status":"approved",
                "performer":{"display_name":"Synthetic doctor"},"patient":draft.patient,"protocol":approved_protocol,"annotations":[]
            }),Some(dictionary.clone())))},cx);
            assert!(s.screen==Screen::Report, "historical approval must open Report");
            assert!(s.history.as_ref().unwrap().report_lines.as_ref().unwrap().as_ref().unwrap()
                .iter().any(|line|line.contains("Approved snapshot only")));
            assert!(s.history.as_ref().unwrap().form.as_ref().unwrap().read(cx).readonly);
            assert_eq!(s.detail.as_ref().unwrap().revision_id,draft.revision_id);
            s.navigate(Screen::Settings,cx);
            s.navigate(Screen::Report,cx);
            assert_eq!(s.history.as_ref().unwrap().selected,Some(approved));
        }));
        cx.update_window(window,|_,w,cx|{
            w.render_frame(cx);
            assert!(w.find("study-stages").visible());
            assert!(w.find("report-default-template").visible());
            assert!(w.find("report-pages-heading").visible());
            assert!(w.find("native-report").visible());
            w.click("nav-Обследование",cx);
            view.update(cx,|s,cx|s.receive(Event::Texture{study:detail.study.id,asset,width:1000,height:800,image:image.clone()},cx));
            w.render_frame(cx);
            assert!(w.find("video-start").visible());
            w.click("nav-Протокол",cx);
            w.render_frame(cx);
            assert!(w.find("historical-protocol-form").visible());
            w.click("nav-Отчет",cx);
            w.render_frame(cx);
            assert!(w.find("report-default-template").visible());
            assert!(w.try_find("close-protocol-history").is_none(),"approved protocol must not offer draft in the main view");
            w.click("approved-revisions-toggle",cx);
            w.render_frame(cx);
            assert!(w.try_find("close-protocol-history").is_none(),"expanded approved revisions must not expose the draft");
            view.update(cx,|s,cx|{
                s.history.as_mut().unwrap().report_lines=Some(Ok(vec!["Synthetic report line".into(); 318]));
                cx.notify();
            });
            w.render_frame(cx);
            let footer=w.find("back-to-protocol").bounds().top();
            assert!(w.find("native-report").bounds().bottom() <= footer, "report must fit above the footer");
            assert!(w.find("report-page-list").bounds().bottom() <= footer, "thumbnails must fit above the footer");
            w.scroll("native-report",ScrollDelta::Pixels(point(px(0.),px(-220.))),cx);
            w.render_frame(cx);
            assert!(view.read(cx).report_scroll.offset().y < px(0.), "mouse wheel must scroll the report");
            let thumbnail_top=w.find("report-page-0").bounds().top();
            w.scroll("report-page-list",ScrollDelta::Pixels(point(px(0.),px(-180.))),cx);
            w.render_frame(cx);
            assert!(w.find("report-page-0").bounds().top() < thumbnail_top, "thumbnails must scroll independently");
            w.scroll("report-page-list",ScrollDelta::Pixels(point(px(0.),px(400.))),cx);
            w.render_frame(cx);
            w.click("report-page-0",cx);
            w.render_frame(cx);
            w.click("report-next-page",cx);
            w.render_frame(cx);
            assert_eq!(view.read(cx).report_view.page,1);
            assert!(view.read(cx).report_scroll.offset().y < px(0.), "page navigation must scroll the report");
            let (tx,rx)=mpsc::channel();
            view.update(cx,|s,_|s.worker=tx);
            w.click("save-and-exit",cx);
            let (study,request,action)=match rx.try_recv().unwrap() {
                Command::Workflow{study,request,action,close_preview:true}=>(study,request,action),
                _=>panic!("approved protocol must close without saving the draft"),
            };
            assert_eq!(action,WorkflowAction::SaveAndExit);
            view.update(cx,|s,cx|s.receive(Event::Workflow{study,request,action,result:Ok(WorkflowStatus::default())},cx));
            w.render_frame(cx);
            assert!(view.read(cx).history.is_none());
            assert!(view.read(cx).screen==Screen::Patients);
        })?;
        println!("PASS: current and historical approved entry, paging, settings return, and close without draft save");
        return Ok(());
    }
    if std::env::var_os("M53_PATIENT_MATCH_PROOF_ONLY").is_some() {
        let (tx,rx)=mpsc::channel();
        let mut existing=detail.patient.clone();existing.external_id=Some(Uuid::new_v4().to_string());
        existing.birth_date=Some("2000-01-02".into());
        cx.update_window(window,|_,w,cx|{
            view.update(cx,|s,cx|{
                s.worker=tx;s.screen=Screen::Patients;s.show_new=true;
                s.family.update(cx,|input,cx|input.set_value(existing.family_name.clone(),w,cx));
                s.given.update(cx,|input,cx|input.set_value(existing.given_name.clone(),w,cx));
                s.birth.update(cx,|input,cx|input.set_date(gpui_kit::component::calendar::Date::Single(Some("2000-01-02".parse().unwrap())),w,cx));
                cx.notify();
            });
            w.render_frame(cx);w.click("new-patient",cx);w.render_frame(cx);
        })?;
        let request=rx.try_iter().find_map(|cmd| match cmd {Command::CheckNewPatient{request,input}=>{assert_eq!(input.birth_date,existing.birth_date);Some(request)},Command::NewPatient(..)=>panic!("creation must wait for lookup"),_=>None}).unwrap();
        cx.update(|cx|view.update(cx,|s,cx|s.receive(Event::NewPatientMatches{request,result:Ok(vec![existing.clone()])},cx)));
        let action=format!("reuse-patient-{}",existing.id);
        cx.update_window(window,|_,w,cx|{w.render_frame(cx);assert!(w.find(action.clone()).visible());})?;
        cx.capture_screenshot(window)?.save(std::path::Path::new(directory).join("existing-patient-match.png"))?;
        cx.update_window(window,|_,w,cx|{w.click(action,cx);w.render_frame(cx);})?;
        cx.update(|cx|{assert_eq!(view.read(cx).patient,Some(existing.id));assert!(!view.read(cx).show_new);});
        let commands:Vec<_>=rx.try_iter().collect();
        assert!(commands.iter().any(|cmd|matches!(cmd,Command::UseMatchedPatient{patient,..} if patient.id==existing.id)));
        assert!(!commands.iter().any(|cmd|matches!(cmd,Command::NewPatient(..))));
        println!("PASS: matching MIS card selected without creating a patient");
        return Ok(());
    }
    if std::env::var_os("M53_SINGLE_REPORT_TEMPLATE_PROOF").is_some() {
        let (tx,rx)=mpsc::channel();
        let expected=cx.update(|cx|view.update(cx,|s,cx|{
            s.worker=tx;
            s.screen=Screen::Report;
            let first=s.templates.first().unwrap().clone();
            let mut alternative=first.clone();alternative.version_id=Uuid::new_v4();
            s.templates.push(alternative);
            s.preview(cx);
            first.version_id
        }));
        cx.update_window(window,|_,w,cx|{
            w.render_frame(cx);
            assert!(w.try_find("template").is_none());
            assert!(w.find("report-default-template").visible());
        })?;
        assert!(rx.try_iter().any(|cmd|matches!(cmd,Command::Preview{template,..} if template.version_id==expected)));
        cx.capture_screenshot(window)?.save(std::path::Path::new(directory).join("single-report-template.png"))?;
        println!("PASS: first template used without a selector");
        return Ok(());
    }
    if std::env::var_os("M53_PATIENT_HISTORY_PROOF_ONLY").is_some() {
        let (test_tx,test_rx)=mpsc::channel();
        let probe=Uuid::new_v4();
        let context=json!({"id":detail.patient.id,"row_version":4,"origin":"manual","manual_edit_allowed":true,"family_name":"Ручная","given_name":"Пациентка","patronymic":null,"birth_date":"2000-01-02","sex":"F"});
        cx.update(|cx|view.update(cx,|s,cx|{
            s.worker=test_tx;s.screen=Screen::Patients;s.patient=Some(detail.patient.id);
            s.state["sync"]["online"]=json!(true);
            s.patient_edit_access=Some((detail.patient.id,probe,false));
            let mut mis=context.clone();mis["origin"]=json!("1c");mis["manual_edit_allowed"]=json!(false);
            s.receive(Event::ManualPatient{request:probe,patient:detail.patient.id,saved:false,result:Ok(mis)},cx);
        }));
        cx.update_window(window,|_,w,cx|{
            w.render_frame(cx);assert!(w.try_find("edit-selected-patient").is_none());
            assert!(w.try_find("selected-study-history").is_none());
            view.update(cx,|s,cx|s.receive(Event::ManualPatient{request:probe,patient:detail.patient.id,saved:false,result:Ok(context.clone())},cx));
            w.render_frame(cx);assert!(w.find("edit-selected-patient").visible());
            w.click("edit-selected-patient",cx);w.render_frame(cx);
            assert!(!view.read(cx).patient_table.read(cx).row_selectable);
        })?;
        let load=cx.update(|cx|view.update(cx,|s,cx|{
            s.navigate(Screen::Settings,cx);
            assert!(s.screen==Screen::Patients,"ожидание карточки пациента блокирует переход");
            s.patient_edit_request.unwrap().0
        }));
        cx.update_window(window,|_,w,cx|{
            w.render_frame(cx);
            let task=view.read(cx).state["native_tasks"][0]["id"].as_str().unwrap().to_owned();
            w.click(format!("task-{task}"),cx);w.render_frame(cx);
            assert!(!test_rx.try_iter().any(|c|matches!(c,Command::ClaimTask(..))),"задание не должно открываться во время загрузки карточки");
            view.update(cx,|s,cx|{
                s.patient_sexes.insert(detail.patient.id,PatientSex::Female);
                s.receive(Event::ManualPatient{request:load,patient:detail.patient.id,saved:false,result:Ok(json!({"id":detail.patient.id,"row_version":4,"origin":"manual","manual_edit_allowed":true,"family_name":"Ручная","given_name":"Пациентка","patronymic":null,"birth_date":"2000-01-02","sex":null}))},cx);
            });
            w.render_frame(cx);assert!(w.find("save-manual-patient").visible());assert!(!view.read(cx).patient_table.read(cx).row_selectable);
            w.click(format!("task-{task}"),cx);w.render_frame(cx);
            assert!(!test_rx.try_iter().any(|c|matches!(c,Command::ClaimTask(..))),"задание не должно открываться во время изменения карточки");
            w.click("save-manual-patient",cx);w.render_frame(cx);
        })?;
        assert!(test_rx.try_iter().any(|command|matches!(command,
                Command::ManualPatient{body:Some(body),..} if body.get("sex")==Some(&Value::Null)
            )),"explicit server sex clear must survive saving the manual form");
        let (save,patient)=cx.update(|cx|view.update(cx,|s,cx|{
            let request=s.patient_edit_request.unwrap();
            s.receive(Event::ManualPatient{request:Uuid::new_v4(),patient:request.1,saved:true,result:Ok(json!({}))},cx);
            assert!(s.patient_editor.is_some(),"запоздавший ответ не закрывает форму");request
        }));
        cx.update(|cx|view.update(cx,|s,cx|s.receive(Event::ManualPatient{request:save,patient,saved:true,result:Err("Синтетический конфликт карточки — введённые данные сохранены".into())},cx)));
        cx.update_window(window,|_,w,cx|{w.render_frame(cx);assert!(w.find("save-manual-patient").visible());assert!(!view.read(cx).patient_table.read(cx).row_selectable);})?;
        cx.capture_screenshot(window)?.save(std::path::Path::new(directory).join("native-manual-patient-edit.png"))?;
        cx.update_window(window,|_,w,cx|{w.click("cancel-manual-patient",cx);w.render_frame(cx);})?;
        cx.update_window(window,|_,w,cx|{w.render_frame(cx);assert!(!view.read(cx).patient_registry_locked());assert!(view.read(cx).patient_table.read(cx).row_selectable);})?;
        let live_session=cx.update(|cx|json!(view.read(cx).editor.read(cx).session));
        let (live_value,live_generation)=cx.update(|cx|view.update(cx,|s,cx|{
            let form=s.form.as_ref().unwrap().read(cx);let saved=(form.value.clone(),form.generation);
            s.open_history(detail.study.id,cx);saved
        }));
        let revision=Uuid::new_v4();
        cx.update(|cx|view.update(cx,|s,cx|{
            let request=s.history.as_ref().unwrap().request.unwrap().0;
            s.receive(Event::ProtocolHistory{request,study:detail.study.id,revision:None,result:Ok((json!({"study_id":detail.study.id,"revisions":[{"revision_id":revision,"revision_number":1,"status":"approved","readonly":true,"created_at":"2026-10-20T00:30:00Z","approved_at":"2026-10-20T00:45:00Z"}],"next_before_revision":null}),None))},cx);
        }));
        cx.update_window(window,|_,w,cx|{w.render_frame(cx);w.click(format!("history-{revision}"),cx);w.render_frame(cx);})?;
        cx.update(|cx|view.update(cx,|s,cx|{
            let request=s.history.as_ref().unwrap().request.unwrap().0;
            s.receive(Event::ProtocolHistory{request,study:detail.study.id,revision:Some(revision),result:Ok((json!({"study_id":detail.study.id,"revision_id":revision,"revision_number":1,"readonly":true,"status":"approved","created_at":"2026-10-20T00:30:00Z","approved_at":"2026-10-20T00:45:00Z","performer":{"display_name":"Врач исследования А"},"patient":detail.patient,"protocol":detail.protocol,"annotations":detail.annotations}),Some(dictionary.clone())))},cx);
            assert!(s.history.as_ref().unwrap().form.as_ref().unwrap().read(cx).readonly);
            assert_eq!(s.history.as_ref().unwrap().form.as_ref().unwrap().read(cx).photo_order,
                s.editor.read(cx).session.photos.iter().filter(|photo|!photo.removed).map(|photo|photo.id).collect::<Vec<_>>());
            let form=s.form.as_ref().unwrap().read(cx);assert_eq!(form.value,live_value);assert_eq!(form.generation,live_generation);
            test_rx.try_iter().for_each(drop);s.request_approval(cx);
            assert!(!test_rx.try_iter().any(|c|matches!(c,Command::Approve)),"просмотр истории не утверждает текущий черновик");
        }));
        cx.update_window(window,|_,w,cx|{w.render_frame(cx);assert!(w.try_find("approve").is_none());assert!(w.try_find("draft").is_none());assert!(w.try_find("reopen").is_none());assert!(w.find("historical-protocol-form").bounds().size.height>px(200.),"историческая форма не должна схлопываться");assert!(w.try_find("save-and-exit").is_none());})?;
        cx.capture_screenshot(window)?.save(std::path::Path::new(directory).join("native-protocol-history.png"))?;
        cx.update(|cx|view.update(cx,|s,cx| {
            let mark=detail.annotations.first().expect("synthetic fixture has a mark").id;
            s.history.as_ref().unwrap().form.as_ref().unwrap().update(cx,|form,_|form.show_mark=Some(mark));
            s.form.as_ref().unwrap().update(cx,|form,_|form.show_mark=Some(Uuid::nil()));
            s.navigate_requested_mark(cx);
            assert!(s.history.as_ref().unwrap().show_photo);
            assert_eq!(s.editor.read(cx).selected,Some(mark));
            assert!(s.editor.read(cx).readonly && s.editor.read(cx).historical_readonly);
            assert_eq!(s.form.as_ref().unwrap().read(cx).show_mark,Some(Uuid::nil()),"hidden live form must not consume the click");
            s.form.as_ref().unwrap().update(cx,|form,_|form.show_mark=None);
        }));
        cx.update_window(window,|_,w,cx|{w.render_frame(cx);assert!(w.find("history-return-to-protocol").visible());w.click("history-return-to-protocol",cx);w.render_frame(cx);})?;
        cx.update_window(window,|_,w,cx|{w.click("close-protocol-history",cx);w.render_frame(cx);let s=view.read(cx);let form=s.form.as_ref().unwrap().read(cx);assert_eq!(form.value,live_value);assert_eq!(form.generation,live_generation);assert_eq!(json!(s.editor.read(cx).session),live_session,"historical image focus must restore the draft session");})?;
        cx.update(|cx|view.update(cx,|s,cx| {
            let request=Uuid::new_v4();s.report_output_request=Some(request);s.report_busy=true;
            s.receive(Event::FileDone(None),cx);assert!(s.report_busy,"PNG completion must preserve PDF busy");
            s.receive(Event::Error("Synthetic unrelated IO failure".into()),cx);assert!(s.report_busy);
            s.receive(Event::ReportOutput{request:Uuid::new_v4(),result:Ok(ReportCompletion::Printed(true))},cx);assert!(s.report_busy,"old output completion must not finish the new output");
            s.receive(Event::ReportOutput{request,result:Ok(ReportCompletion::Printed(false))},cx);
            assert!(!s.report_busy && s.report_output_request.is_none());
        }));
        println!("PASS: delayed patient load/navigation/tasks, failed save preservation, scoped readonly history, current draft unchanged");
        return Ok(());
    }
    if std::env::var_os("M53_READONLY_PROOF_ONLY").is_some() {
        let form = cx.update(|cx| view.update(cx, |s, _| {
            s.screen = Screen::Protocol;
            s.form.as_ref().unwrap().clone()
        }));
        return readonly_form_proof::verify(&mut cx, window, &form);
    }
    if std::env::var_os("M53_DIAGNOSES_PROOF_ONLY").is_some() {
        return diagnosis_proof::verify(&mut cx,window,&view,directory);
    }
    for (screen, name) in [
        (Screen::Study, "native-study.png"),
        (Screen::Patients, "native-patients.png"),
        (Screen::Protocol, "native-protocol.png"),
        (Screen::Settings, "native-settings.png"),
    ] {
        cx.update(|cx| view.update(cx, |s, _| s.screen = screen));
        cx.update_window(window, |_, w, cx| w.render_frame(cx))?;
        cx.capture_screenshot(window)?
            .save(std::path::Path::new(directory).join(name))?;
    }
    // Synthetic registry geometry: conclusion length and selection must not move actions or labels.
    let registry_saved = cx.update(|cx| view.update(cx, |s, _| {
        let saved = (s.screen, s.patient, s.preview_study, s.studies.clone(), s.study_summaries.clone());
        let mut first = detail.study.clone();
        first.status = "approved".into();
        let mut second = first.clone();
        second.id = Uuid::from_u128(0x53594e54484554494302);
        s.screen = Screen::Patients;
        s.patient = Some(detail.patient.id);
        s.studies = vec![first.clone(), second.clone()];
        let revision=Uuid::from_u128(0x53594e54484554494303);
        s.study_summaries = BTreeMap::from([
            (detail.study.id, StudySummary { row_version:Some(first.row_version),revision_id:Some(revision),
                mis_delivery:Some(json!({"job_id":Uuid::from_u128(0x53594e54484554494304),"revision_id":revision,
                    "template_version_id":Uuid::from_u128(0x53594e54484554494305),"pdf_sha256":"a".repeat(64),"state":"sent","delivered":true})),
                exam_date: Some("2026-10-25".into()),
                doctor_name: Some("Тестовый врач".into()), conclusion: Some("Краткое синтетическое заключение.".into()), ..Default::default() }),
            (second.id, StudySummary { exam_date: Some("2026-10-24".into()),
                doctor_name: Some("Тестовый врач".into()),
                conclusion: Some("Длинное синтетическое заключение для проверки переноса строк и устойчивого положения действий. ".repeat(8)), ..Default::default() }),
        ]);
        saved
    }));
    let registry_ids = [detail.study.id, Uuid::from_u128(0x53594e54484554494302)];
    let mut registry_geometry = Vec::new();
    for (index, selected) in registry_ids.iter().enumerate() {
        let geometry = cx.update_window(window, |_, w, cx| {
            // This synthetic preview transition never opens a study or calls the API.
            view.update(cx, |s, cx| { s.preview_study = Some(*selected); cx.notify(); });
            w.render_frame(cx);
            let labels = registry_ids.map(|id| w.try_find(SharedString::from(format!("study-label-{id}"))).map(|label| label.bounds()));
            let check = w.try_find(SharedString::from(format!("study-check-{selected}"))).map(|check| check.bounds());
            assert!(w.find(SharedString::from(format!("study-mis-{}",registry_ids[0]))).visible(),"current delivered revision needs a MIS badge");
            assert!(w.try_find(SharedString::from(format!("study-mis-{}",registry_ids[1]))).is_none(),"unconfirmed revision must not get a MIS badge");
            (w.find("open-selected-study").bounds(), w.find("new-study").bounds(),
                w.try_find("registry-studies-panel").map(|panel| panel.bounds()), labels, check)
        })?;
        cx.capture_screenshot(window)?.save(std::path::Path::new(directory)
            .join(format!("native-registry-selected-{}.png", index + 1)))?;
        registry_geometry.push(geometry);
    }
    let first = &registry_geometry[0];
    let second = &registry_geometry[1];
    assert_eq!(first.0.top(), second.0.top(), "opening action must stay fixed when conclusion length changes");
    assert_eq!(first.1.top(), second.1.top(), "new-study action must stay fixed when conclusion length changes");
    for (index, geometry) in registry_geometry.iter().enumerate() {
        let panel = geometry.2.expect("registry panel needs a measurable boundary");
        assert!(geometry.0.top() >= panel.top() && geometry.0.bottom() <= geometry.1.top(), "actions must remain ordered within the panel");
        let bottom_inset = panel.bottom() - geometry.1.bottom();
        assert!(bottom_inset >= px(0.) && bottom_inset <= px(24.), "registry actions must be anchored near the panel bottom: {bottom_inset:?}");
        let check = geometry.4.expect("selected study must expose its Check slot");
        assert_eq!(check.size.width, px(20.), "selected Check needs a fixed 20px slot");
        let selected_label = geometry.3[index].expect("selected study label must be measurable");
        assert!(selected_label.left() >= check.right(), "Check must occupy its own slot before the label");
        for row in 0..registry_ids.len() {
            assert_eq!(first.3[row].expect("first-frame study label").left(),
                geometry.3[row].expect("study label").left(), "selection must not shift study label text");
        }
    }
    cx.update(|cx| view.update(cx, |s, _| {
        (s.screen, s.patient, s.preview_study, s.studies, s.study_summaries) = registry_saved;
    }));
    // Open the actual birth-date and schema-driven protocol controls, not a mock calendar.
    for field in ["birth", "context.exam_date", "context.lmp"] {
        let date = cx.update_window(window, |_, w, cx| {
            view.update(cx, |s, _| {s.screen=if field=="birth" {Screen::Patients} else {Screen::Protocol};s.show_new=field=="birth";});
            w.render_frame(cx);
            let date = if field=="birth" {view.read(cx).birth.clone()} else {
                view.read(cx).form.as_ref().unwrap().read(cx).preview_date_state(field)
            };
            date.update(cx, |s,cx| s.set_date(gpui_kit::component::calendar::Date::Single(Some("2026-10-25".parse().unwrap())),w,cx));
            w.render_frame(cx);
            w.click(("date-picker",date.entity_id()),cx);
            w.render_frame(cx);
            assert_eq!(w.find("calendar-MonthToggle").label(),Some("Октябрь"));
            for (i,label) in ["Пн","Вт","Ср","Чт","Пт","Сб","Вс"].into_iter().enumerate() {
                assert_eq!(w.find(SharedString::from(format!("weekday-0-{i}"))).label(),Some(label),"{field}: week starts on Monday");
            }
            assert_eq!(w.find("calendar-2026-10-26-0-4").bounds().left(),w.find("weekday-0-0").bounds().left(),"Monday date must occupy first column");
            assert_eq!(w.find("calendar-2026-10-25-0-3").bounds().left(),w.find("weekday-0-6").bounds().left(),"Sunday date must occupy last column");
            date
        })?;
        // The kit's popup fades in using the wall clock (150ms).
        std::thread::sleep(Duration::from_millis(250));
        cx.update_window(window, |_,w,cx|w.render_frame(cx))?;
        cx.capture_screenshot(window)?.save(std::path::Path::new(directory).join(format!("native-calendar-{field}.png")))?;
        cx.update_window(window, |_, w, cx| {
            w.click("calendar-2026-10-26-0-4",cx);
        })?;
        // CalendarEvent -> DatePickerEvent subscriptions settle at the update boundary.
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            assert_eq!(w.find(("date-picker",date.entity_id())).value(),Some("26.10.2026"));
            assert_eq!(date.read(cx).date(),gpui_kit::component::calendar::Date::Single(Some("2026-10-26".parse().unwrap())));
            view.update(cx, |s, _| {s.show_new=false;});
        })?;
    }
    // The approved chrome keeps steps separate from the top bar and exposes dismissal directly.
    cx.update_window(window, |_, w, cx| {
        view.update(cx, |s, _| {s.screen=Screen::Protocol;});
        w.render_frame(cx);
        let header=w.find("application-header").bounds();
        let patient=w.find("patient-summary").bounds();
        let steps=w.find("study-stages").bounds();
        assert!(patient.top()>=header.bottom()-px(1.));
        assert!(steps.top()>=patient.bottom()-px(1.));
        assert!(w.find("current-doctor").bounds().left()>w.find("help-settings").bounds().left());
        assert!(w.find("guidance-dismiss").visible(),"persistent dismissal must be a visible button");
        view.update(cx, |s,cx| {
            let before=s.guidance.preferences.clone();
            let mut stale=s.guidance.context.clone();
            stale.as_mut().unwrap().epoch=1;
            for action in [m53_client::ui_guidance::Action::SetEnabled(false),
                m53_client::ui_guidance::Action::Complete(m53_client::ui_guidance::TipId::ProtocolSections),
                m53_client::ui_guidance::Action::Dismiss(m53_client::ui_guidance::TipId::ProtocolSections),
                m53_client::ui_guidance::Action::ResetProgress] {
                assert!(!s.guidance_action(&stale,action,cx),"a stale rendered control must not target the new doctor's scope");
            }
            assert!(!s.guidance_pending);assert_eq!(s.guidance.preferences,before);
        });
        w.click("guidance-dismiss",cx);
        assert!(view.read(cx).guidance_pending);
        assert!(view.read(cx).guidance_closed);
        view.update(cx, |s,cx| {
            let mut ack=s.guidance.clone();
            ack.preferences.dismissed_tip_ids.insert(m53_client::ui_guidance::TipId::ProtocolSections.as_str().into());
            s.receive(Event::Guidance(ack),cx);
            assert!(!s.guidance_pending);
            // Restore only this synthetic proof's initial scope for subsequent independent assertions.
            s.guidance.preferences.dismissed_tip_ids.clear();s.guidance_closed=false;
        });
    })?;
    // Real help controls do not mutate clinical state; ACKs here are synthetic and never persisted.
    cx.update_window(window, |_, w, cx| {
        view.update(cx, |s, _| {s.screen=Screen::Protocol;});
        w.render_frame(cx);
        assert!(w.find("protocol-approval-notice").visible());
        assert!(w.find("validation-navigation").visible(),"field navigation must exist on the first frame");
        let guide_height=w.find("guidance-card").bounds().size.height;
        view.update(cx, |s, _| {s.guidance.preferences.completed_tip_ids.insert(m53_client::ui_guidance::TipId::ProtocolSections.as_str().into());});
        w.render_frame(cx);
        assert_eq!(w.find("guidance-card").bounds().size.height,guide_height,"a different hint must not move the form");
        let before=view.read(cx).form.as_ref().unwrap().read(cx).value.clone();
        let (test_tx,test_rx)=mpsc::channel();
        let worker=view.update(cx,|s,_|std::mem::replace(&mut s.worker,test_tx));
        w.click("help-settings",cx); w.render_frame(cx);
        assert!(view.read(cx).screen==Screen::Protocol,"header help must toggle without leaving the current screen");
        assert!(view.read(cx).guidance_pending);
        assert!(matches!(test_rx.try_recv(),Ok(Command::Guidance(m53_client::ui_guidance::Request {action:m53_client::ui_guidance::Action::SetEnabled(false),..}))));
        w.click("help-settings",cx);
        assert!(test_rx.try_recv().is_err(),"pending save must block duplicate toggles");
        view.update(cx, |s,cx| {
            let mut ack=s.guidance.clone();ack.preferences.enabled=false;s.receive(Event::Guidance(ack),cx);
            assert!(!s.guidance.preferences.enabled);assert!(!s.guidance_pending);
            let mut stale=s.guidance.clone();stale.epoch=1;stale.context.as_mut().unwrap().epoch=1;
            stale.preferences.enabled=true;s.receive(Event::Guidance(stale),cx);
            assert!(!s.guidance.preferences.enabled,"old login cannot re-enable help");
            s.screen=Screen::Protocol;
            s.worker=worker;
        });
        w.render_frame(cx);
        let s=view.read(cx);
        assert!(w.try_find("guidance-understood").is_none(),"help OFF must hide the teaching card");
        assert!(w.find("protocol-approval-notice").visible(),"help OFF must retain real errors");
        assert_eq!(s.form.as_ref().unwrap().read(cx).value,before,"help never changes clinical inputs");
    })?;
    cx.capture_screenshot(window)?.save(std::path::Path::new(directory).join("native-help-off-errors.png"))?;
    cx.update(|cx|view.update(cx,|s,_|{
        s.guidance_closed=true;
    }));
    cx.update_window(window, |_,w,cx| {
        w.render_frame(cx);
        let (test_tx,test_rx)=mpsc::channel();
        let worker=view.update(cx,|s,_|std::mem::replace(&mut s.worker,test_tx));
        w.click("help-settings",cx);
        assert!(view.read(cx).guidance_pending);
        assert!(matches!(test_rx.try_recv(),Ok(Command::Guidance(m53_client::ui_guidance::Request {action:m53_client::ui_guidance::Action::SetEnabled(true),..}))));
        assert!(!view.read(cx).guidance_closed,"enabling help must reopen the contextual card");
        assert!(view.read(cx).screen==Screen::Protocol);
        view.update(cx,|s,cx|{let mut ack=s.guidance.clone();ack.preferences.enabled=true;s.receive(Event::Guidance(ack),cx);s.worker=worker;});
        w.render_frame(cx);
        assert!(w.find("guidance-card").visible());
    })?;
    // Appending a server page must not scroll back to the selected first row.
    cx.update(|cx|view.update(cx,|s,cx|{
        s.screen=Screen::Patients;
        s.patients=(0..50).map(|i|{
            let mut p=detail.patient.clone();p.family_name=format!("Пациент{i:03}");
            if i>0 {p.id=Uuid::from_u128(20000+i);}
            p
        }).collect();
        s.refresh_patient_table(cx);
        s.patient_table.update(cx,|t,cx|t.scroll_to_row(35,cx));
    }));
    cx.update_window(window,|_,w,cx|w.render_frame(cx))?;
    let before=cx.update(|cx|view.read(cx).patient_table.read(cx).vertical_scroll_handle.0.borrow().base_handle.offset().y);
    cx.update(|cx|view.update(cx,|s,cx|{
        s.patients.extend((50..100).map(|i|{
            let mut p=detail.patient.clone();p.family_name=format!("Пациент{i:03}");p.id=Uuid::from_u128(20000+i);p
        }));
        s.refresh_patient_table(cx);
    }));
    cx.update_window(window,|_,w,cx|w.render_frame(cx))?;
    let after=cx.update(|cx|view.read(cx).patient_table.read(cx).vertical_scroll_handle.0.borrow().base_handle.offset().y);
    assert_eq!(before,after,"page append must preserve table scroll");
    cx.update(|cx|view.update(cx,|s,cx|{s.patients=vec![detail.patient.clone()];s.refresh_patient_table(cx);}));
    // Same production button: video -> saved photos, without completing a step.
    cx.update(|cx|view.update(cx,|s,cx|{
        s.screen=Screen::Study;s.video_running=true;
        s.editor.update(cx,|e,_|{e.live_active=true;e.live_image=Some(image.clone());});
    }));
    cx.update_window(window,|_,w,cx|w.render_frame(cx))?;
    cx.capture_screenshot(window)?.save(std::path::Path::new(directory).join("native-live-exam.png"))?;
    cx.update_window(window,|_,w,cx|{
        w.click("finish-examination",cx);
        let s=view.read(cx);
        assert!(!s.video_running);assert!(s.screen==Screen::Study);
        assert!(!s.editor.read(cx).live_active);
        assert!(s.workflow_request.is_none());
    })?;
    cx.update(|cx| {
        view.update(cx, |s, _| {
            s.screen = Screen::Report;
            s.report_lines = m53_client::report::native_lines(
                &detail,
                &layout,
                "Тестовый врач",
                &dictionary.terms,
            )
            .unwrap();
            s.report_photos = vec![(asset, image.clone())];
        })
    });
    cx.update_window(window, |_, w, cx| w.render_frame(cx))?;
    cx.capture_screenshot(window)?
        .save(std::path::Path::new(directory).join("native-report.png"))?;
    cx.update_window(window, |_, w, cx| {
        w.simulate_next_frame(cx);w.render_frame(cx);
        let request=view.read(cx).preview_request;
        let lines=view.read(cx).report_lines.clone();
        let left=w.find("report-navigation-sidebar").bounds();
        let paper=w.find("native-report").bounds();
        let delivery=w.find("report-delivery-panel").bounds();
        assert!(left.right()<=paper.left()+px(1.));
        assert!(paper.right()<=delivery.left()+px(1.));
        assert!(w.find("send-result-1c").bounds().left()>=delivery.left());
        assert!(w.find("report-zoom-1").bounds().right()<=left.right());
        w.click("report-zoom-1",cx);w.render_frame(cx);
        assert_eq!(view.read(cx).report_view.zoom,crate::report_view::Zoom::FitPage);
        w.click("report-next-page",cx);w.render_frame(cx);
        assert_eq!(view.read(cx).report_view.page,1);
        w.click("report-previous-page",cx);w.render_frame(cx);
        assert_eq!(view.read(cx).report_view.page,0);
        // In short windows thumbnails scroll independently; click a visible target.
        let target=w.find("report-page-1").bounds();
        let list=w.find("report-page-list").bounds();
        if target.center().y>=list.bottom() || target.center().y<list.top() {
            w.scroll("report-page-list",ScrollDelta::Pixels(point(px(0.),px(-200.))),cx);w.render_frame(cx);
        }
        w.click("report-page-1",cx);w.render_frame(cx);
        assert_eq!(view.read(cx).report_view.page,1,"left page list must navigate the same PDF");
        w.scroll("report-page-list",ScrollDelta::Pixels(point(px(0.),px(600.))),cx);w.render_frame(cx);
        w.click("report-page-0",cx);w.render_frame(cx);
        assert_eq!(view.read(cx).preview_request,request,"zoom must not regenerate the PDF");
        assert_eq!(view.read(cx).report_lines,lines);
        // Approval from the report uses an isolated command channel, never a real API.
        let (test_tx,test_rx)=std::sync::mpsc::channel();
        let saved=view.update(cx,|s,_| (std::mem::replace(&mut s.worker,test_tx),s.message.clone()));
        w.click("report-draft-to-protocol",cx);
        assert!(view.read(cx).screen==Screen::Report,"approval must keep the report tab");
        assert!(view.read(cx).approval_busy,"report button must request approval");
        view.update(cx,|s,cx|s.request_approval(cx));
        assert_eq!(test_rx.try_iter().filter(|c|matches!(c,Command::Approve)).count(),1,"busy repeat cannot enqueue approval twice");
        view.update(cx,|s,cx|{
            s.approval_busy=false;s.approval_message="Проверка обязательных полей".into();cx.notify();
        });
        w.render_frame(cx);
        assert!(view.read(cx).screen==Screen::Report);
        assert!(w.find("report-approval-message").visible(),"approval errors must be visible on the report");
        view.update(cx,|s,_|{s.worker=saved.0;s.message=saved.1;s.approval_message.clear();});
    })?;
    cx.capture_screenshot(window)?.save(std::path::Path::new(directory).join("native-report-fit-page.png"))?;
    // Delivery UI distinguishes a previous receipt from the current immutable PDF.
    let delivery_before=cx.update(|cx| {
        let s=view.read(cx);
        (s.detail.clone(),s.report_identity.clone(),s.state["native_result_jobs"].clone())
    });
    let proof_revision=Uuid::from_u128(70001);
    let proof_template=cx.update(|cx|view.read(cx).templates[0].version_id);
    let proof_hash="a".repeat(64);
    cx.update(|cx|view.update(cx,|s,_| {
        s.detail.as_mut().unwrap().revision_id=Some(proof_revision);
        s.detail.as_mut().unwrap().study.status="draft".into();
        s.report_identity=Some((proof_revision,proof_template,proof_hash.clone()));
        s.state["native_result_jobs"]=json!([{"job_id":Uuid::from_u128(71001),"revision_id":Uuid::from_u128(70000),
            "template_version_id":proof_template,"pdf_sha256":"b".repeat(64),"state":"sent","delivered":true,"sequence":1,
            "receipt":{"action":"created","reception_number":"DEMO-000124"},"delivered_at":"2026-10-24T00:00:00Z"}]);
    }));
    cx.update_window(window,|_,w,cx| {
        w.render_frame(cx);
        {
            let s=view.read(cx);
            let shown=crate::report_delivery::present(&s.state["native_result_jobs"],s.report_delivery_context());
            assert_eq!(shown.state,crate::report_delivery::State::PreviousDelivered);
            assert_eq!(shown.tone(),crate::report_delivery::Tone::Muted);
            assert!(care_path::require_print(s.detail.as_ref().unwrap()).is_err());
            assert!(!s.report_history_expanded);
        }
        w.click("print-report",cx);
        w.scroll("report-delivery-panel",ScrollDelta::Pixels(point(px(0.),px(-300.))),cx);w.render_frame(cx);
        w.click("send-result-1c",cx);
        assert!(!view.read(cx).report_busy,"draft controls must not dispatch print or send");
        w.scroll("report-delivery-panel",ScrollDelta::Pixels(point(px(0.),px(600.))),cx);w.render_frame(cx);
    })?;
    cx.capture_screenshot(window)?.save(std::path::Path::new(directory).join("native-report-previous-delivery.png"))?;
    cx.update_window(window,|_,w,cx| {
        w.scroll("report-delivery-panel",ScrollDelta::Pixels(point(px(0.),px(-600.))),cx);w.render_frame(cx);
        w.click("report-delivery-history",cx);w.render_frame(cx);
        assert!(view.read(cx).report_history_expanded);
    })?;
    cx.update(|cx|view.update(cx,|s,_| {
        s.detail.as_mut().unwrap().study.status="approved".into();
        s.state["native_result_jobs"][0]["revision_id"]=json!(proof_revision);
        s.state["native_result_jobs"][0]["pdf_sha256"]=json!(proof_hash);
        s.report_history_expanded=false;
    }));
    cx.update_window(window,|_,w,cx| {
        w.render_frame(cx);
        w.scroll("report-delivery-panel",ScrollDelta::Pixels(point(px(0.),px(600.))),cx);w.render_frame(cx);
        {
            let s=view.read(cx);
            let shown=crate::report_delivery::present(&s.state["native_result_jobs"],s.report_delivery_context());
            assert_eq!(shown.state,crate::report_delivery::State::CurrentDelivered);
            assert_eq!(shown.tone(),crate::report_delivery::Tone::Success);
            assert!(care_path::require_print(s.detail.as_ref().unwrap()).is_ok());
        }
        w.click("send-result-1c",cx);
        assert!(!view.read(cx).report_busy,"delivered PDF must not be sent twice");
    })?;
    cx.capture_screenshot(window)?.save(std::path::Path::new(directory).join("native-report-current-delivery.png"))?;
    cx.update(|cx|view.update(cx,|s,_| {
        s.state["native_result_jobs"][0]["state"]=json!("pending");
        s.state["native_result_jobs"][0]["delivered"]=json!(false);
    }));
    cx.update_window(window,|_,w,cx| {
        w.render_frame(cx);
        {
            let s=view.read(cx);
            let shown=crate::report_delivery::present(&s.state["native_result_jobs"],s.report_delivery_context());
            assert_eq!(shown.state,crate::report_delivery::State::CurrentPending);
            assert_ne!(shown.tone(),crate::report_delivery::Tone::Success);
        }
        w.click("send-result-1c",cx);
        assert!(!view.read(cx).report_busy,"pending PDF must not create a duplicate request");
    })?;
    cx.capture_screenshot(window)?.save(std::path::Path::new(directory).join("native-report-pending.png"))?;
    cx.update(|cx|view.update(cx,|s,_| {
        s.detail=delivery_before.0;s.report_identity=delivery_before.1;
        s.state["native_result_jobs"]=delivery_before.2;s.report_history_expanded=false;
    }));
    // Manual verification is an explicit separate action, never implicit approval.
    cx.update(|cx|view.update(cx,|s,_|{s.screen=Screen::Protocol;s.approval_busy=false;s.approval_pending=false;}));
    cx.update_window(window,|_,w,cx| {
        let (tx,rx)=std::sync::mpsc::channel();
        let saved=view.update(cx,|s,_| {
            let saved=(std::mem::replace(&mut s.worker,tx),s.state["manual_identity_context"].clone());
            s.state["manual_identity_context"]=json!({"origin":"manual_continuity","patient":{"identity_state":"provisional"},"manual_identity_verification":null});
            saved
        });
        w.render_frame(cx);
        assert!(w.find("manual-identity-confirm").visible());
        w.click("manual-identity-confirm",cx);
        let commands:Vec<_>=rx.try_iter().collect();
        assert_eq!(commands.iter().filter(|c|matches!(c,Command::ConfirmManualIdentity)).count(),1);
        assert!(!commands.iter().any(|c|matches!(c,Command::Approve)),"verification must not approve the clinical content");
        view.update(cx,|s,cx|{s.worker=saved.0;s.state["manual_identity_context"]=saved.1;cx.notify();});
    })?;
    // Real Kit controls mutate the clinical form, not a second test model.
    cx.update(|cx| view.update(cx, |s, _| s.screen = Screen::Protocol));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("colpo-group-0-2", cx);
        let form = view.read(cx).form.as_ref().unwrap().read(cx);
        assert!(form.value["colpo"].is_object());
        assert_eq!(form.value["colpo"]["marks"].as_object().unwrap().len(), 1);
        assert_eq!(form.colpo_group_for_test(0), Some(2));
        assert!(
            serde_json::from_value::<m53_domain::protocol::Protocol>(form.value.clone()).is_ok()
        );
    })?;
    for group in [0,1,2,3] {
        cx.update_window(window,|_,w,cx|{
            w.render_frame(cx);
            let header=format!("colpo-group-0-{group}");
            let header_bounds=w.find(header.clone()).bounds();

            let form=view.read(cx).form.as_ref().unwrap().clone();
            let scroll=form.read(cx).scroll_for_test();
            let viewport=scroll.bounds();
            let offset=scroll.offset();
            scroll.set_offset(point(offset.x,(offset.y+viewport.top()-header_bounds.top()).min(px(0.))));
            w.render_frame(cx);
            w.click(header,cx);
            w.render_frame(cx);w.simulate_next_frame(cx);w.render_frame(cx);
            let (target,focus)=form.read(cx).navigation_for_test().unwrap();
            assert_eq!(target,format!("group:0:{group}"));
            assert!(focus.contains_focused(w,cx),"section heading must keep focus");
            let heading=w.find(format!("colpo-group-0-{group}")).bounds();
            assert!(w.find(format!("colpo-group-0-{group}")).visible());
            let stable=scroll.offset();
            w.simulate_next_frame(cx);w.render_frame(cx);
            assert_eq!(scroll.offset(),stable,"section must not jump on the following frame");
            assert!(heading.top()>=scroll.bounds().top()-px(1.));
        })?;
        cx.capture_screenshot(window)?.save(std::path::Path::new(directory).join(format!("native-required-group-{group}.png")))?;
    }
    for inadequate in [false,true] {
        cx.update_window(window,|_,w,cx|{
            let form=view.read(cx).form.as_ref().unwrap().clone();
            form.update(cx,|f,_|{
                f.value["sections"][0]["adequacy"]["value"]=if inadequate {json!({"state":"assessed","value":{"state":"inadequate","reason":""}})} else {json!({"state":"not_assessed"})};
                f.generation+=1;f.invalidate_validation();
            });
            w.render_frame(cx);
            let header=w.find("colpo-group-0-2").bounds();
            let scroll=form.read(cx).scroll_for_test();let offset=scroll.offset();
            scroll.set_offset(point(offset.x,(offset.y+scroll.bounds().top()-header.top()).min(px(0.))));
            w.render_frame(cx);
            if form.read(cx).colpo_group_for_test(0)==Some(2) {w.click("colpo-group-0-2",cx);w.render_frame(cx);}
            w.click("colpo-group-0-2",cx);w.render_frame(cx);w.simulate_next_frame(cx);w.render_frame(cx);
            assert_eq!(form.read(cx).colpo_group_for_test(0),Some(2),"test must inspect an expanded section");
            let (target,focus)=form.read(cx).navigation_for_test().unwrap();
            assert_eq!(target,"group:0:2");
            assert!(focus.contains_focused(w,cx));
        })?;
    }
    let mut ready_dictionary=dictionary.clone();ready_dictionary.clinically_approved=true;
    let mut ready=m53_client::model::blank_protocol(ready_dictionary.version_id);
    ready.schema_version=2;ready.colpo=Some(Default::default());
    ready.colpo.as_mut().unwrap().fields.insert("context.no_cervix".into(),m53_domain::colpo::Answer::Checked(true));
    ready.conclusion=m53_client::model::field(m53_domain::protocol::Assessment::Assessed("Синтетическое заключение".into()));
    assert!(m53_client::workspace::approval_problems(&ready,&ready_dictionary).is_empty());
    cx.update(|cx|view.update(cx,|s,cx|{
        s.dictionary=Some(ready_dictionary.clone());
        s.detail.as_mut().unwrap().protocol=Some(ready.clone());
        s.form=Some(cx.new(|_|NativeForm::new(json!(ready),ready_dictionary.clone(),BTreeMap::new(),false)));
    }));
    cx.update_window(window,|_,w,cx|w.render_frame(cx))?;
    cx.capture_screenshot(window)?.save(std::path::Path::new(directory).join("native-protocol-ready.png"))?;
    cx.update(|cx|view.update(cx,|s,cx|{
        s.detail.as_mut().unwrap().study.status="approved".into();
        s.form.as_ref().unwrap().update(cx,|f,_|f.readonly=true);
    }));
    cx.update_window(window,|_,w,cx|w.render_frame(cx))?;
    cx.capture_screenshot(window)?.save(std::path::Path::new(directory).join("native-protocol-approved.png"))?;
    // Exercise the real compact panel and cancellation without a server or patient data.
    cx.update(|cx|view.update(cx,|s,cx| {
        s.screen=Screen::Protocol;
        s.detail.as_mut().unwrap().study.status="draft".into();
        s.form.as_ref().unwrap().update(cx,|f,_|f.readonly=false);
        s.visit_panel.update(cx,|p,cx| {
            p.set_templates(vec![m53_client::offline::VisitTemplateSummary {id:Uuid::from_u128(10001),title:"Синтетический шаблон".into(),row_version:1,updated_at:"2030-01-01".into()}],cx);
            p.set_state(false,false,cx);
        });
        cx.notify();
    }));
    let visit_before=cx.update(|cx|view.read(cx).form.as_ref().unwrap().read(cx).value.clone());
    cx.update_window(window,|_,w,cx| {
        w.render_frame(cx);
        assert!(w.find("visit-template-save-open").visible());
        w.click("visit-template-save-open",cx);w.render_frame(cx);
        assert!(w.try_find("visit-template-save").is_some());
    })?;
    cx.capture_screenshot(window)?.save(std::path::Path::new(directory).join("native-visit-template-save.png"))?;
    // A failed save keeps the editor; a confirmed save closes it.
    let (study, generation, saved)=cx.update(|cx| {
        let s=view.read(cx);
        let study=s.detail.as_ref().unwrap().study.id;
        let form=s.form.as_ref().unwrap().read(cx);
        let protocol=serde_json::from_value(form.value.clone()).unwrap();
        let saved=m53_client::offline::VisitTemplateDetail {
            metadata:m53_client::offline::VisitTemplateSummary {id:Uuid::from_u128(10001),title:"Синтетический шаблон".into(),row_version:1,updated_at:"2030-01-01".into()},
            protocol:m53_domain::visit_template::VisitTemplate::capture(&protocol).unwrap(),
        };
        (study,form.generation,saved)
    });
    let failed=Uuid::new_v4();
    cx.update(|cx|view.update(cx,|s,cx| {s.visit_study=Some(study);s.visit_request=Some((failed,study,generation,None));s.receive(Event::VisitTemplateSaved {request:failed,result:Err("Синтетическая ошибка".into())},cx);}));
    cx.update_window(window,|_,w,cx| {w.render_frame(cx);assert!(w.try_find("visit-template-save").is_some(),"ошибка не должна закрывать форму");})?;
    let completed=Uuid::new_v4();
    cx.update(|cx|view.update(cx,|s,cx| {s.visit_request=Some((completed,study,generation,None));s.receive(Event::VisitTemplateSaved {request:Uuid::new_v4(),result:Ok(saved.clone())},cx);}));
    cx.update_window(window,|_,w,cx| {w.render_frame(cx);assert!(w.try_find("visit-template-save").is_some(),"запоздавший ответ не должен закрывать форму");})?;
    cx.update(|cx|view.update(cx,|s,cx| {s.visit_request=Some((completed,study,generation,None));s.receive(Event::VisitTemplateSaved {request:completed,result:Ok(saved)},cx);}));
    cx.update_window(window,|_,w,cx| {w.render_frame(cx);assert!(w.try_find("visit-template-save").is_none(),"подтверждённое сохранение должно закрыть форму");})?;
    cx.update_window(window,|_,w,cx| {
        w.click("visit-template-clear",cx);w.render_frame(cx);
        assert!(w.find("visit-template-confirm").visible());
        w.click("visit-template-cancel",cx);w.render_frame(cx);
        assert_eq!(view.read(cx).form.as_ref().unwrap().read(cx).value,visit_before);
    })?;
    diagnosis_proof::verify(&mut cx, window, &view, directory)?;
    // The diagnosis priority proof finishes in settings; the next proof needs the protocol.
    cx.update(|cx| view.update(cx, |s, cx| {s.screen=Screen::Protocol;cx.notify();}));
    let readonly_form = cx.update(|cx| view.read(cx).form.as_ref().unwrap().clone());
    readonly_form_proof::verify(&mut cx, window, &readonly_form)?;
    cx.capture_screenshot(window)?.save(std::path::Path::new(directory).join("native-readonly-protocol.png"))?;
    cx.update(|cx| view.update(cx, |s,cx| {
        assert!(!s.templates.is_empty());
        s.preview_request=None;s.report_busy=true;
        s.detail.as_mut().unwrap().revision_id=Some(proof_revision);
        s.report_identity=Some((proof_revision,s.templates[0].version_id,"a".repeat(64)));
        assert!(s.report_identity.is_some(), "proof must begin with an identified preview");
        let event=Event::State { snapshot:s.state.clone(), patients:s.patients.clone(), studies:s.studies.clone(), templates:vec![], diagnoses:s.diagnoses.clone() };
        s.receive(event,cx);
        assert!(s.report_busy, "template disappearance must not finish an active output");
        assert!(s.report_identity.is_none(), "removed template cannot retain a preview identity");
    }));
    println!("Native screens, guidance controls/stale ACK/OFF preserves validation, report zoom/navigation without rerender, page append, camera transition, section anchors, ready/approved and diagnosis picker/order/late event checks passed");
    Ok(())
}
