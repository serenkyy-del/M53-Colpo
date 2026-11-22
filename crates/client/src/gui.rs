use crate::{
    connection::Connection,
    report::{self, Photo},
};
use eframe::egui;
use m53_contracts::{ApproveProtocolRequest, SavedProtocol, workflow::*};
use m53_domain::{ExaminationState, FeatureState, protocol::*, validation::Dictionary};
use std::collections::BTreeMap;
use uuid::Uuid;

pub fn launch() -> eframe::Result {
    eframe::run_native(
        "М53 — кольпоскопия",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([1380.0, 900.0]),
            ..Default::default()
        },
        Box::new(|cc| {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "m53".into(),
                std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
                    "../assets/DejaVuSans.ttf"
                ))),
            );
            fonts
                .families
                .get_mut(&egui::FontFamily::Proportional)
                .unwrap()
                .insert(0, "m53".into());
            cc.egui_ctx.set_fonts(fonts);
            crate::editor_ui::configure(&cc.egui_ctx);
            let mut app = Desktop::default();
            if let Ok((url, key)) = crate::credentials::load() {
                app.url = url;
                app.key = key;
            }
            Ok(Box::new(app))
        }),
    )
}
use crate::model::{blank_protocol, field};
#[path = "native_ui.rs"]
mod native_ui;

struct Desktop {
    host_window: isize,
    web_panel: Option<crate::web_panel::Panel>,
    settings_open: bool,
    settings_tab: usize,
    patients_view: bool,
    patient_query: String,
    patronymic: String,
    photo_tool: usize,
    photo_zoom: f32,
    crop_start: Option<egui::Pos2>,
    moving_annotation: Option<Uuid>,
    selected_annotation: Option<Uuid>,
    layer_view: crate::editor_ui::LayerView,
    annotation_handle: crate::defects::Handle,
    annotation_drag_offset: egui::Vec2,
    defect_choice: usize,
    defect_picker: Option<Uuid>,
    thumbnails: BTreeMap<Uuid, egui::TextureHandle>,
    thumbnail_busy: std::collections::BTreeSet<Uuid>,
    thumbnail_retry: BTreeMap<Uuid, std::time::Instant>,
    url: String,
    key: String,
    login: String,
    password: String,
    connection: Option<Connection>,
    doctor: String,
    message: String,
    patients: Vec<Patient>,
    studies: Vec<Study>,
    orders: Vec<Order>,
    order: Option<Uuid>,
    patient: Option<Uuid>,
    detail: Option<StudyDetail>,
    dictionary: Option<Dictionary>,
    dictionary_versions: Vec<TerminologyVersion>,
    templates: Vec<Template>,
    template: usize,
    family: String,
    given: String,
    birth: String,
    dirty: bool,
    tab: usize,
    selected_asset: Option<Uuid>,
    texture: Option<egui::TextureHandle>,
    photo_bytes: BTreeMap<Uuid, Vec<u8>>,
    selected_photos: std::collections::BTreeSet<Uuid>,
    radius: f32,
    label: String,
    lesion_link: Option<Uuid>,
    advanced: String,
    auth_job: Option<std::sync::mpsc::Receiver<Result<Connection, String>>>,
    events_tx: std::sync::mpsc::Sender<UiEvent>,
    events_rx: std::sync::mpsc::Receiver<UiEvent>,
    photo_busy: bool,
    pending_open: Option<Uuid>,
    last_poll: std::time::Instant,
    last_edit: std::time::Instant,
    edit_fingerprint: String,
    last_autosave: String,
    closing: bool,
    review: bool,
    advanced_edited: bool,
    background_jobs: usize,
    pending_imports: Vec<std::path::PathBuf>,
    pending_import_study: Option<Uuid>,
    seen_conflict: Option<Uuid>,
    failed_operation_json: String,
}
enum UiEvent {
    Thumbnail(Uuid, Result<egui::TextureHandle, String>),
    Status(Result<(), String>),
    Archive(Result<(), String>),
    Imported(Uuid, Result<(), String>),
    Photo(Uuid, Result<egui::TextureHandle, String>),
}
impl Default for Desktop {
    fn default() -> Self {
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        Self {
            web_panel: None,
            host_window: 0,
            settings_open: false,
            settings_tab: 0,
            patients_view: true,
            patient_query: String::new(),
            patronymic: String::new(),
            photo_tool: 5,
            photo_zoom: 1.0,
            crop_start: None,
            moving_annotation: None,
            selected_annotation: None,
            layer_view: Default::default(),
            annotation_handle: crate::defects::Handle::Body,
            annotation_drag_offset: egui::Vec2::ZERO,
            defect_choice: 0,
            defect_picker: None,
            thumbnails: BTreeMap::new(),
            thumbnail_busy: Default::default(),
            thumbnail_retry: BTreeMap::new(),
            url: "http://127.0.0.1:8787".into(),
            key: String::new(),
            login: "demo".into(),
            password: String::new(),
            connection: None,
            doctor: String::new(),
            message: "Подключитесь к серверу".into(),
            patients: vec![],
            studies: vec![],
            orders: vec![],
            order: None,
            patient: None,
            detail: None,
            dictionary: None,
            dictionary_versions: vec![],
            templates: vec![],
            template: 0,
            family: String::new(),
            given: String::new(),
            birth: String::new(),
            dirty: false,
            tab: 0,
            selected_asset: None,
            texture: None,
            photo_bytes: BTreeMap::new(),
            selected_photos: Default::default(),
            radius: 0.04,
            label: String::new(),
            lesion_link: None,
            advanced: String::new(),
            auth_job: None,
            events_tx,
            events_rx,
            photo_busy: false,
            pending_open: None,
            last_poll: std::time::Instant::now(),
            last_edit: std::time::Instant::now(),
            edit_fingerprint: String::new(),
            last_autosave: String::new(),
            closing: false,
            review: false,
            advanced_edited: false,
            background_jobs: 0,
            pending_imports: vec![],
            pending_import_study: None,
            seen_conflict: None,
            failed_operation_json: String::new(),
        }
    }
}
impl Desktop {
    fn refresh(&mut self) -> Result<(), String> {
        let c = self.connection.as_ref().ok_or("Нет подключения")?;
        self.patients = c.get("/api/v1/patients")?;
        self.studies = c.get("/api/v1/studies")?;
        self.orders = c.get("/api/v1/orders")?;
        self.dictionary_versions = c.get("/api/v1/terminology/versions")?;
        if self.dictionary.is_none() {
            self.dictionary = Some(c.get("/api/v1/terminology")?);
        }
        self.templates = c.get("/api/v1/templates")?;
        Ok(())
    }
    fn open(&mut self, id: Uuid) -> Result<(), String> {
        if self.dirty {
            return Err("Сохраните изменения или отмените их перед переходом".into());
        }
        let mut detail: StudyDetail = self
            .connection
            .as_ref()
            .unwrap()
            .get(&format!("/api/v1/studies/{id}"))?;
        if detail.protocol.is_none() {
            detail.protocol = Some(blank_protocol(
                self.dictionary
                    .as_ref()
                    .ok_or("Словарь не загружен")?
                    .version_id,
            ));
        }
        let version = detail.protocol.as_ref().unwrap().terminology_version_id;
        self.dictionary = Some(
            self.connection
                .as_ref()
                .unwrap()
                .get(&format!("/api/v1/terminology?version_id={version}"))?,
        );
        self.advanced = serde_json::to_string_pretty(detail.protocol.as_ref().unwrap()).unwrap();
        self.advanced_edited = false;
        if let Some(editor) = self
            .connection
            .as_ref()
            .and_then(|c| c.offline.as_ref())
            .and_then(|o| o.editor(id))
        {
            self.advanced_edited = editor
                .get("advanced_edited")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if self.advanced_edited {
                self.advanced = editor
                    .get("advanced")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .into();
            }
        }
        self.selected_photos = detail
            .assets
            .iter()
            .filter(|a| a.kind == "original")
            .map(|a| a.id)
            .collect();
        self.detail = Some(detail);
        self.selected_asset = None;
        self.texture = None;
        self.photo_bytes.clear();
        Ok(())
    }
    fn save(&mut self) -> Result<(), String> {
        if let Some(offline) = self.connection.as_ref().and_then(|c| c.offline.as_ref()) {
            crate::workspace::check_unfinished_input(&offline.journal())?;
        }
        let detail = self.detail.as_ref().ok_or("Нет исследования")?;
        if self.tab == 3
            && serde_json::from_str::<serde_json::Value>(&self.advanced).ok()
                != detail
                    .protocol
                    .as_ref()
                    .and_then(|p| serde_json::to_value(p).ok())
        {
            return Err(
                "Полный JSON сохранен локально; примените форму перед отправкой протокола".into(),
            );
        }
        let id = detail.study.id;
        let errors = m53_domain::validation::validate(
            detail.protocol.as_ref().ok_or("Нет протокола")?,
            self.dictionary.as_ref().ok_or("Нет словаря")?,
            m53_domain::validation::ValidationMode::Draft,
        );
        if let Some(error) = errors.first() {
            if error.code == "unexamined_section_has_observations" {
                return Err("В разделе «Не исследовано» есть находки. Откройте протокол: выберите «Исследовано» или удалите ошибочно добавленные находки. Ввод сохранен на компьютере.".into());
            }
            return Err(format!("{}: {}", error.path, error.code));
        }
        let input = SaveDraft {
            expected_version: detail.study.row_version,
            protocol: detail.protocol.clone().ok_or("Нет протокола")?,
            annotations: detail.annotations.clone(),
        };
        let _: SavedProtocol = self
            .connection
            .as_ref()
            .unwrap()
            .put(&format!("/api/v1/studies/{id}/protocol"), &input)?;
        self.dirty = false;
        if let Ok(detail) = self
            .connection
            .as_ref()
            .unwrap()
            .get::<StudyDetail>(&format!("/api/v1/studies/{id}"))
        {
            self.detail = Some(detail);
        }
        Ok(())
    }
    fn load_photo(&mut self, id: Uuid, ctx: &egui::Context) -> Result<(), String> {
        if self.photo_busy {
            return Ok(());
        }
        self.photo_busy = true;
        self.selected_asset = Some(id);
        self.texture = None;
        self.selected_annotation = None;
        self.photo_zoom = 1.0;
        self.crop_start = None;
        let connection = self.connection.as_ref().unwrap().clone();
        let context = ctx.clone();
        let sender = self.events_tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let bytes = connection.download(id)?;
                let image = crate::imaging::decode(&bytes)?.to_rgba8();
                let size = [image.width() as usize, image.height() as usize];
                Ok(context.load_texture(
                    id.to_string(),
                    egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw()),
                    egui::TextureOptions::LINEAR,
                ))
            })();
            let _ = sender.send(UiEvent::Photo(id, result));
            context.request_repaint();
        });
        Ok(())
    }
    fn generate(&mut self) -> Result<(), String> {
        if self.background_jobs > 0 {
            return Err("Дождитесь завершения текущего локального импорта/рендера".into());
        }
        if self.dirty {
            self.save()?;
        }
        let detail = self.detail.clone().ok_or("Нет исследования")?;
        let template = self
            .templates
            .get(self.template)
            .ok_or("Нет шаблона")?
            .clone();
        let dictionary = self.dictionary.clone().ok_or("Нет словаря")?;
        let doctor = self.doctor.clone();
        let selected_photos = self.selected_photos.clone();
        let connection = self.connection.as_ref().unwrap().clone();
        let sender = self.events_tx.clone();
        self.background_jobs += 1;
        std::thread::spawn(move || {
            let connection = &connection;
            let result = (|| {
                let trusted: Vec<Template> = connection.get("/api/v1/templates")?;
                let saved_layout = trusted
                    .iter()
                    .find(|t| t.version_id == template.version_id)
                    .ok_or("Шаблон не найден")?;
                if serde_json::to_value(&saved_layout.layout).unwrap()
                    != serde_json::to_value(&template.layout).unwrap()
                {
                    return Err("Сначала сохраните изменения шаблона как новый шаблон".into());
                }

                let mut photos = vec![];
                let mut derived = vec![];
                for asset in &detail.assets {
                    if asset.kind == "original" {
                        let bytes = connection.download(asset.id)?;
                        let decoded = crate::imaging::decode(&bytes)?;
                        derived.push(connection.upload_derived(
                            detail.study.id,
                            asset.id,
                            crate::imaging::raw_rgb_pixels(&decoded),
                        )?);
                        if selected_photos.contains(&asset.id) {
                            photos.push(Photo {
                                id: asset.id,
                                bytes,
                                caption: format!("Снимок {}", photos.len() + 1),
                            });
                        }
                    }
                }
                let rendered = report::render_with_terms(
                    &detail,
                    &template.layout,
                    &photos,
                    &doctor,
                    &dictionary.terms,
                )?;
                let html = connection.upload(detail.study.id, "html", rendered.html)?;
                let pdf = connection.upload(detail.study.id, "pdf", rendered.pdf.clone())?;
                let input = SaveReport {
                    snapshot: detail.clone(),
                    revision_id: detail.revision_id.ok_or("Сначала сохраните протокол")?,
                    template_version_id: template.version_id,
                    html_asset_id: html.id,
                    pdf_asset_id: pdf.id,
                    client_version: env!("CARGO_PKG_VERSION").into(),
                    renderer_version: "m53-printpdf-0.7-v2-layers".into(),
                    manifest: {
                        derived.extend([html, pdf]);
                        derived
                    },
                };
                let _: serde_json::Value = connection.post(
                    &format!("/api/v1/studies/{}/reports", detail.study.id),
                    &input,
                )?;
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("PDF", &["pdf"])
                    .set_file_name("report.pdf")
                    .save_file()
                {
                    std::fs::write(&path, rendered.pdf).map_err(|_| "Не удалось сохранить PDF")?;
                    crate::preview::open_pdf(&path)?;
                }
                Ok(())
            })();
            let _ = sender.send(UiEvent::Status(result));
        });
        Ok(())
    }
    fn start_import(&mut self, id: Uuid, files: Vec<std::path::PathBuf>) {
        if self.background_jobs > 0 {
            return;
        }
        self.pending_imports = files.clone();
        self.pending_import_study = Some(id);
        self.background_jobs += 1;
        self.checkpoint();
        let connection = self.connection.as_ref().unwrap().clone();
        let sender = self.events_tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                for path in files {
                    let bytes = std::fs::read(path).map_err(
                        |_| "Не удалось прочитать исходный снимок; путь импорта сохранен",
                    )?;
                    crate::imaging::decode(&bytes)?;
                    connection.upload(id, "original", bytes)?;
                }
                Ok(())
            })();
            let _ = sender.send(UiEvent::Imported(id, result));
        });
        self.message =
            "Импорт выполняется в фоне; источник и назначение сохранены в локальном журнале".into();
    }
    fn authenticate(&mut self, resume: bool) {
        if self.auth_job.is_some() {
            return;
        }
        let url = self.url.clone();
        let key = self.key.clone();
        let login = self.login.clone();
        let password = std::mem::take(&mut self.password);
        let existing = self.connection.as_ref().and_then(|c| c.offline.clone());
        let (sender, receiver) = std::sync::mpsc::channel();
        self.auth_job = Some(receiver);
        self.message = "Вход выполняется в фоне; локальная работа доступна".into();
        std::thread::spawn(move || {
            let result = if resume {
                crate::offline_profile::resume(&login, &password)
            } else {
                Connection::new(&url, &key).and_then(|mut raw| {
                    raw.login(login.clone(), password.clone())?;
                    crate::offline_profile::connect(raw, &login, &password, existing)
                })
            };
            let _ = sender.send(result);
        });
    }
    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(panel) = self.web_panel.take() {
            match panel.result.try_recv() {
                Ok(Ok(journal)) => {
                    self.restore_journal(journal);
                    self.tab = 1;
                    if !self.advanced_edited {
                        self.advanced = self
                            .detail
                            .as_ref()
                            .and_then(|d| d.protocol.as_ref())
                            .map(|p| serde_json::to_string_pretty(p).unwrap())
                            .unwrap_or_default();
                    }
                    self.message = journal_panel_message(&self.connection);
                }
                Ok(Err(error)) => {
                    if let Some(journal) = self
                        .connection
                        .as_ref()
                        .and_then(|c| c.offline.as_ref())
                        .map(|o| o.journal())
                    {
                        self.restore_journal(journal);
                    }
                    self.message = error;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.web_panel = Some(panel);
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    return;
                }
                Err(_) => self.message = "Окно формы завершилось; локальный журнал сохранен".into(),
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        if let Some(receiver) = self.auth_job.take() {
            match receiver.try_recv() {
                Ok(Ok(connection)) => {
                    self.doctor = connection.doctor.clone();
                    let journal = connection.offline.as_ref().unwrap().journal();
                    self.connection = Some(connection);
                    self.restore_journal(journal);
                    self.message =
                        "Рабочее место открыто; сеть и синхронизация выполняются в фоне".into();
                }
                Ok(Err(error)) => self.message = error,
                Err(std::sync::mpsc::TryRecvError::Empty) => self.auth_job = Some(receiver),
                Err(_) => self.message = "Фоновый вход завершился с ошибкой; кеш сохранен".into(),
            }
        }
        while let Ok(event) = self.events_rx.try_recv() {
            match event {
                UiEvent::Thumbnail(id, result) => {
                    self.thumbnail_busy.remove(&id);
                    if let Ok(texture) = result {
                        self.thumbnails.insert(id, texture);
                    }
                }
                UiEvent::Status(result) => {
                    self.background_jobs = self.background_jobs.saturating_sub(1);
                    self.status(
                    result,
                    "Отчет сохранен локально и поставлен в очередь; backend подтверждает отдельно",
                );
                }
                UiEvent::Archive(result) => self.status(result, "Файл сохранен из локального кеша"),
                UiEvent::Imported(id, result) => {
                    self.background_jobs = self.background_jobs.saturating_sub(1);
                    if result.is_ok() {
                        self.pending_imports.clear();
                        self.pending_import_study = None;
                    }
                    if let Some(c) = &self.connection
                        && let Ok(detail) = c.get::<StudyDetail>(&format!("/api/v1/studies/{id}"))
                        && let Some(current) = self.detail.as_mut().filter(|d| d.study.id == id)
                    {
                        current.assets = detail.assets;
                    }
                    self.status(
                        result,
                        "Снимки приняты в локальную очередь и сохраняются на диск",
                    );
                }
                UiEvent::Photo(id, result) => {
                    self.photo_busy = false;
                    match result {
                        Ok(texture) if self.selected_asset == Some(id) => {
                            self.texture = Some(texture)
                        }
                        Ok(_) => {}
                        Err(error) => self.message = error,
                    }
                }
            }
        }
        if let Some(conflict) = self
            .connection
            .as_ref()
            .and_then(|c| c.offline.as_ref())
            .and_then(|o| o.conflict())
        {
            let id = conflict
                .pointer("/operation/id")
                .and_then(|v| v.as_str())
                .and_then(|v| Uuid::parse_str(v).ok());
            if id != self.seen_conflict {
                self.seen_conflict = id;
                self.review = true;
                self.failed_operation_json =
                    serde_json::to_string_pretty(&conflict["operation"]["body"]).unwrap();
            }
        }
        if self.last_poll.elapsed() > std::time::Duration::from_millis(700) {
            self.last_poll = std::time::Instant::now();
            if let Some(c) = &self.connection {
                if let Ok(v) = c.get("/api/v1/patients") {
                    self.patients = v;
                }
                if let Ok(v) = c.get("/api/v1/studies") {
                    self.studies = v;
                }
                if let Ok(v) = c.get("/api/v1/orders") {
                    self.orders = v;
                }
                if let Ok(v) = c.get("/api/v1/terminology/versions") {
                    self.dictionary_versions = v;
                }
                if self.dictionary.is_none() {
                    self.dictionary = c.get("/api/v1/terminology").ok();
                }
                if self.templates.is_empty()
                    && let Ok(v) = c.get("/api/v1/templates")
                {
                    self.templates = v;
                }
                if !self.dirty
                    && let Some(current) = &mut self.detail
                    && let Ok(cached) =
                        c.get::<StudyDetail>(&format!("/api/v1/studies/{}", current.study.id))
                {
                    current.patient = cached.patient;
                    current.study = cached.study;
                    current.revision_id = cached.revision_id;
                    current.assets = cached.assets;
                }
            }
            if let Some(id) = self.pending_open
                && self.open(id).is_ok()
            {
                self.pending_open = None;
            }
            if self.texture.is_none()
                && let Some(id) = self.selected_asset
            {
                let _ = self.load_photo(id, ctx);
            }
        }
    }
    fn checkpoint(&mut self) {
        if self.web_panel.is_some() {
            return;
        }
        let Some(offline) = self.connection.as_ref().and_then(|c| c.offline.clone()) else {
            return;
        };
        let fingerprint=serde_json::to_string(&serde_json::json!({"protocol":self.detail.as_ref().and_then(|d|d.protocol.as_ref()),"annotations":self.detail.as_ref().map(|d|&d.annotations),"advanced":self.advanced})).unwrap();
        if fingerprint != self.edit_fingerprint {
            self.last_edit = std::time::Instant::now();
            self.edit_fingerprint = fingerprint.clone();
        }
        let current_journal = serde_json::json!({"detail":self.detail,"dictionary":self.dictionary,"templates":self.templates,"template":self.template,"patient":self.patient,"order":self.order,"tab":self.tab,"family":self.family,"given":self.given,"birth":self.birth,"dirty":self.dirty,"advanced":self.advanced,"selected_photos":self.selected_photos,"label":self.label,"radius":self.radius,"lesion_link":self.lesion_link,"advanced_edited":self.advanced_edited,"pending_imports":self.pending_imports,"pending_import_study":self.pending_import_study,"patronymic":self.patronymic,"layer_view":self.layer_view});
        let mut journal = offline.journal();
        if !journal.is_object() {
            journal = serde_json::json!({});
        }
        journal
            .as_object_mut()
            .unwrap()
            .extend(current_journal.as_object().unwrap().clone());
        offline.checkpoint(journal);
        if self.dirty
            && self.tab != 3
            && self.last_edit.elapsed() > std::time::Duration::from_millis(800)
            && self.last_autosave != fingerprint
        {
            self.last_autosave = fingerprint;
            let result = self.save();
            self.status(
                result,
                "Черновик сохранен локально; ожидает подтверждения backend",
            );
        }
    }
    fn restore_journal(&mut self, journal: serde_json::Value) {
        if journal.is_null() {
            return;
        }
        self.layer_view =
            serde_json::from_value(journal.get("layer_view").cloned().unwrap_or_default())
                .unwrap_or_default();
        self.detail = serde_json::from_value(journal.get("detail").cloned().unwrap_or_default())
            .unwrap_or(None);
        self.dictionary =
            serde_json::from_value(journal.get("dictionary").cloned().unwrap_or_default())
                .unwrap_or(None);
        self.templates =
            serde_json::from_value(journal.get("templates").cloned().unwrap_or_default())
                .unwrap_or_default();
        self.template = journal
            .get("template")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        self.patient = serde_json::from_value(journal.get("patient").cloned().unwrap_or_default())
            .unwrap_or(None);
        self.order = serde_json::from_value(journal.get("order").cloned().unwrap_or_default())
            .unwrap_or(None);
        self.tab = journal.get("tab").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        self.advanced_edited = journal
            .get("advanced_edited")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        self.pending_imports = serde_json::from_value(
            journal
                .get("pending_imports")
                .cloned()
                .unwrap_or(serde_json::json!([])),
        )
        .unwrap_or_default();
        self.pending_import_study = serde_json::from_value(
            journal
                .get("pending_import_study")
                .cloned()
                .unwrap_or_default(),
        )
        .unwrap_or(None);
        self.dirty = journal
            .get("dirty")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        for (name, target) in [
            ("family", &mut self.family),
            ("given", &mut self.given),
            ("patronymic", &mut self.patronymic),
            ("birth", &mut self.birth),
            ("advanced", &mut self.advanced),
            ("label", &mut self.label),
        ] {
            *target = journal
                .get(name)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .into();
        }
        self.selected_photos =
            serde_json::from_value(journal.get("selected_photos").cloned().unwrap_or_default())
                .unwrap_or_default();
        self.radius = journal
            .get("radius")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.04) as f32;
        self.lesion_link =
            serde_json::from_value(journal.get("lesion_link").cloned().unwrap_or_default())
                .unwrap_or(None);
    }
    fn status(&mut self, result: Result<(), String>, success: &str) {
        self.message = match result {
            Ok(()) => success.into(),
            Err(e) => e,
        };
    }
}
impl Desktop {
    fn legacy_update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        if ctx.input(|i| i.viewport().close_requested()) {
            self.checkpoint();
            if self
                .connection
                .as_ref()
                .and_then(|c| c.offline.as_ref())
                .is_some_and(|o| !o.information().2)
                || self.background_jobs > 0
            {
                self.closing = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
        }
        if self.closing
            && self
                .connection
                .as_ref()
                .and_then(|c| c.offline.as_ref())
                .is_some_and(|o| o.information().2)
            && self.background_jobs == 0
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("М53 · Кольпоскопия");
            });
            ui.label(&self.message);
            if !self.pending_imports.is_empty() {
                ui.label(format!(
                    "Незавершенный локальный импорт: {} файлов",
                    self.pending_imports.len()
                ));
                if ui
                    .add_enabled(
                        self.background_jobs == 0,
                        egui::Button::new(
                            "Продолжить незавершенный импорт в исходное исследование",
                        ),
                    )
                    .clicked()
                    && let Some(id) = self.pending_import_study
                {
                    self.start_import(id, self.pending_imports.clone());
                }
            }
            if let Some(offline) = self.connection.as_ref().and_then(|c| c.offline.as_ref()) {
                let (online, pending, durable, error) = offline.information();
                ui.colored_label(
                    if online {
                        egui::Color32::GREEN
                    } else {
                        egui::Color32::YELLOW
                    },
                    format!(
                        "{} · неподтвержденных операций: {} · {}",
                        if online {
                            "API доступен"
                        } else {
                            "Автономная работа / переподключение"
                        },
                        pending,
                        if durable {
                            "локальная запись подтверждена диском"
                        } else {
                            "запись на локальный диск…"
                        }
                    ),
                );
                if !error.is_empty() {
                    ui.label(error);
                }
                if self.closing {
                    ui.label(
                        "Закрытие после завершения локальной записи. Backend ждать не требуется.",
                    );
                }
            }
        });
        if self.connection.is_none() {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.heading("Вход врача");
                ui.label("Адрес HTTPS сервиса");
                ui.text_edit_singleline(&mut self.url);
                ui.label("Ключ рабочего места");
                ui.add(egui::TextEdit::singleline(&mut self.key).password(true));
                if ui
                    .button("Запомнить адрес и ключ (DPAPI текущего пользователя)")
                    .clicked()
                {
                    let r = crate::credentials::save(&self.url, &self.key);
                    self.status(r, "Ключ защищен DPAPI и сохранен в профиле Windows");
                }
                ui.label("Пользователь");
                ui.text_edit_singleline(&mut self.login);
                ui.label("Пароль");
                ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
                if ui
                    .add_enabled(
                        self.auth_job.is_none(),
                        egui::Button::new("Войти через API"),
                    )
                    .clicked()
                {
                    self.authenticate(false);
                }
                if ui
                    .add_enabled(
                        self.auth_job.is_none(),
                        egui::Button::new("Открыть сохраненную работу автономно"),
                    )
                    .clicked()
                {
                    self.authenticate(true);
                }
            });
            return;
        }
        egui::SidePanel::left("patients")
            .default_width(260.0)
            .show(ctx, |ui| {
                let mut selected = self.dictionary.as_ref().map(|d| d.version_id);
                egui::ComboBox::from_id_salt("dictionary-version")
                    .selected_text(
                        self.dictionary_versions
                            .iter()
                            .find(|v| Some(v.id) == selected)
                            .map(|v| v.code.as_str())
                            .unwrap_or("Словарь"),
                    )
                    .show_ui(ui, |ui| {
                        for v in &self.dictionary_versions {
                            ui.add_enabled_ui(self.detail.is_none() && !self.dirty, |ui| {
                                ui.selectable_value(
                                    &mut selected,
                                    Some(v.id),
                                    format!(
                                        "{}{}",
                                        v.code,
                                        if v.clinically_approved {
                                            ""
                                        } else {
                                            " · черновик"
                                        }
                                    ),
                                );
                            });
                        }
                    });
                if selected != self.dictionary.as_ref().map(|d| d.version_id)
                    && let Some(id) = selected
                {
                    let r = self
                        .connection
                        .as_ref()
                        .unwrap()
                        .get(&format!("/api/v1/terminology?version_id={id}"))
                        .map(|d| {
                            self.dictionary = Some(d);
                        });
                    self.status(r, "Словарь выбран для новых исследований");
                }
                ui.heading("Пациенты");
                if ui.button("Обновить").clicked() {
                    let r = self.refresh();
                    self.status(r, "Список обновлен");
                }
                egui::ScrollArea::vertical()
                    .max_height(250.0)
                    .show(ui, |ui| {
                        for p in &self.patients {
                            if ui
                                .selectable_label(
                                    self.patient == Some(p.id),
                                    format!("{} {}", p.family_name, p.given_name),
                                )
                                .clicked()
                            {
                                if self.dirty {
                                    self.message =
                                        "Сохраните изменения перед сменой пациента".into();
                                } else {
                                    self.patient = Some(p.id);
                                    self.order = None;
                                    if self
                                        .detail
                                        .as_ref()
                                        .is_some_and(|d| d.study.patient_id != p.id)
                                    {
                                        self.detail = None;
                                        self.selected_asset = None;
                                        self.texture = None;
                                        self.photo_bytes.clear();
                                    }
                                }
                            }
                        }
                    });
                ui.collapsing("Новый пациент", |ui| {
                    ui.label("Фамилия");
                    ui.text_edit_singleline(&mut self.family);
                    ui.label("Имя");
                    ui.text_edit_singleline(&mut self.given);
                    ui.label("Дата рождения ГГГГ-ММ-ДД");
                    ui.text_edit_singleline(&mut self.birth);
                    if ui
                        .add_enabled(!self.dirty, egui::Button::new("Добавить"))
                        .clicked()
                    {
                        let input = NewPatient {
                            family_name: self.family.clone(),
                            given_name: self.given.clone(),
                            patronymic: None,
                            birth_date: if self.birth.trim().is_empty() {
                                None
                            } else {
                                Some(self.birth.clone())
                            },
                        };
                        let r = self
                            .connection
                            .as_ref()
                            .unwrap()
                            .post::<_, Patient>("/api/v1/patients", &input)
                            .and_then(|p| {
                                if self.dirty {
                                    self.message =
                                        "Сохраните изменения перед сменой пациента".into();
                                } else {
                                    self.patient = Some(p.id);
                                    self.order = None;
                                    if self
                                        .detail
                                        .as_ref()
                                        .is_some_and(|d| d.study.patient_id != p.id)
                                    {
                                        self.detail = None;
                                        self.selected_asset = None;
                                        self.texture = None;
                                        self.photo_bytes.clear();
                                    }
                                }
                                self.refresh()
                            });
                        self.status(r, "Пациент добавлен");
                    }
                });
                if ui
                    .add_enabled(
                        self.patient.is_some() && !self.dirty,
                        egui::Button::new("Новое исследование"),
                    )
                    .clicked()
                {
                    let r = self
                        .connection
                        .as_ref()
                        .unwrap()
                        .post::<_, Study>(
                            "/api/v1/studies",
                            &NewStudy {
                                order_id: self.order,
                                patient_id: self.patient.unwrap(),
                            },
                        )
                        .and_then(|s| {
                            self.refresh()?;
                            self.open(s.id)
                        });
                    self.status(r, "Исследование создано");
                }
                ui.separator();
                ui.heading("Исследования");
                let mut open = None;
                for s in &self.studies {
                    if Some(s.patient_id) == self.patient
                        && ui
                            .button(format!(
                                "{} · v{} · {}",
                                s.status,
                                s.row_version,
                                &s.id.to_string()[..8]
                            ))
                            .clicked()
                    {
                        open = Some(s.id);
                    }
                }
                if let Some(id) = open {
                    self.pending_open = Some(id);
                    let r = self.open(id);
                    if r.is_ok() {
                        self.pending_open = None;
                    }
                    self.status(r, "Исследование открыто из локального кеша");
                }
                ui.separator();
                if ui
                    .add_enabled(!self.dirty, egui::Button::new("Закрыть исследование"))
                    .clicked()
                {
                    self.detail = None;
                    self.selected_asset = None;
                    self.texture = None;
                    self.photo_bytes.clear();
                }
                ui.label(format!("Врач: {}", self.doctor));
                ui.collapsing(
                    "Повторный вход без закрытия черновика",
                    |ui| {
                        ui.text_edit_singleline(&mut self.login);
                        ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
                        if ui
                            .add_enabled(
                                self.auth_job.is_none(),
                                egui::Button::new("Обновить сессию"),
                            )
                            .clicked()
                        {
                            self.authenticate(false);
                        }
                    },
                );
                if ui
                    .add_enabled(
                        !self.dirty
                            && self.background_jobs == 0
                            && self
                                .connection
                                .as_ref()
                                .and_then(|c| c.offline.as_ref())
                                .is_some_and(|o| o.information().2),
                        egui::Button::new("Выйти"),
                    )
                    .clicked()
                {
                    self.checkpoint();
                    let offline = self
                        .connection
                        .as_ref()
                        .unwrap()
                        .offline
                        .as_ref()
                        .unwrap()
                        .clone();
                    match offline.shutdown() {
                        Ok(()) => {
                            let _ = self
                                .connection
                                .as_ref()
                                .unwrap()
                                .post::<_, serde_json::Value>(
                                    "/api/v1/logout",
                                    &serde_json::json!({}),
                                );
                            self.connection = None;
                            self.detail = None;
                            self.photo_bytes.clear();
                            self.texture = None;
                        }
                        Err(error) => self.message = error,
                    }
                }
            });
        egui::CentralPanel::default().show(ctx,|ui|{
 if self.detail.is_none(){ui.heading("Выберите пациента и откройте исследование");return;}
 let d=self.detail.as_ref().unwrap();ui.heading(format!("{} {} · {}",d.patient.family_name,d.patient.given_name,d.study.status));ui.label(format!("Исследование {} · версия {}",d.study.id,d.study.row_version));
 ui.horizontal(|ui|{for (i,title) in ["Протокол","Снимки и разметка","Шаблон и отчет","Полная форма JSON"].iter().enumerate(){if ui.selectable_label(self.tab==i,*title).clicked(){self.tab=i;if i==3 && !self.advanced_edited{self.advanced=serde_json::to_string_pretty(self.detail.as_ref().unwrap().protocol.as_ref().unwrap()).unwrap();}}}});ui.separator();
 let editable=self.detail.as_ref().unwrap().study.status!="approved";
 ui.horizontal(|ui|{
 if ui.add_enabled(editable,egui::Button::new(if self.dirty{"Сохранить изменения *"}else{"Сохранить протокол"})).clicked(){let r=self.save();self.status(r,"Сохранено локально; backend подтверждает отдельно");}
 if ui.add_enabled(self.dirty,egui::Button::new("Сравнить с backend (без удаления локальной работы)")).clicked(){self.review=true;self.message="Локальная работа сохранена. Сравните версии перед объединением".into();}
 if ui.add_enabled(editable && !self.dirty && self.detail.as_ref().unwrap().revision_id.is_some(),egui::Button::new("Утвердить")).clicked(){let d=self.detail.as_ref().unwrap();let id=d.study.id;let r=self.connection.as_ref().unwrap().post::<_,SavedProtocol>(&format!("/api/v1/studies/{id}/approve"),&ApproveProtocolRequest{study_id:id,revision_id:d.revision_id.unwrap(),expected_version:d.study.row_version}).and_then(|_|self.open(id));self.status(r,"Протокол утвержден");}
 if ui.add_enabled(!editable,egui::Button::new("Новая редакция")).clicked(){let d=self.detail.as_ref().unwrap();let id=d.study.id;let r=self.connection.as_ref().unwrap().post::<_,serde_json::Value>(&format!("/api/v1/studies/{id}/reopen"),&ExpectedVersion{expected_version:d.study.row_version}).and_then(|_|self.open(id));self.status(r,"Открыт новый черновик; утвержденная ревизия сохранена");}
 });ui.separator();
 egui::ScrollArea::vertical().show(ui,|ui|{match self.tab {
 0=>{ui.add_enabled_ui(editable,|ui|{let p=self.detail.as_mut().unwrap().protocol.as_mut().unwrap();self.dirty|=text_field(ui,"Показания",&mut p.indications);self.dirty|=text_field(ui,"Анамнез",&mut p.history);
 self.dirty|=crate::forms::clinical_context(ui,&mut p.clinical_context);
 for section in &mut p.sections {ui.push_id(format!("{:?}",section.region),|ui|{ui.collapsing(format!("{:?}",section.region),|ui|{let before=serde_json::to_string(section).unwrap();egui::ComboBox::from_id_salt("state").selected_text(format!("{:?}",section.state.value)).show_ui(ui,|ui|{for state in [ExaminationState::Examined,ExaminationState::NotExamined,ExaminationState::NotApplicable]{ui.selectable_value(&mut section.state.value,state,format!("{state:?}"));}});if section.state.value==ExaminationState::Examined{
 let mut adequate=match &section.adequacy.value{Assessment::Assessed(Adequacy::Adequate)=>0,Assessment::Assessed(Adequacy::Inadequate{..})=>1,_=>2};let old=adequate;egui::ComboBox::from_id_salt("adequacy").selected_text(["Адекватно","Неадекватно","Не оценено"][adequate]).show_ui(ui,|ui|{for (i,label) in ["Адекватно","Неадекватно","Не оценено"].iter().enumerate(){ui.selectable_value(&mut adequate,i,*label);}});if old!=adequate{section.adequacy=field(match adequate{0=>Assessment::Assessed(Adequacy::Adequate),1=>Assessment::Assessed(Adequacy::Inadequate{reason:String::new()}),_=>Assessment::NotAssessed});}
if let Assessment::Assessed(Adequacy::Inadequate{reason})=&mut section.adequacy.value{ui.label("Причина неадекватности");ui.text_edit_singleline(reason);}
 for finding in &mut section.findings{ui.horizontal(|ui|{ui.label(self.dictionary.as_ref().and_then(|d|d.terms.iter().find(|t|t.code==finding.term_code)).map(|t|t.label.as_str()).unwrap_or(&finding.term_code));egui::ComboBox::from_id_salt(finding.id).selected_text(format!("{:?}",finding.observation.value)).show_ui(ui,|ui|{for state in [FeatureState::Present,FeatureState::Absent,FeatureState::NotAssessed,FeatureState::NotApplicable]{ui.selectable_value(&mut finding.observation.value,state,format!("{state:?}"));}});});}
 if let Some(dictionary)=&self.dictionary{for term in dictionary.terms.iter().filter(|t|t.region==section.region && t.kind==m53_domain::validation::TermKind::Finding){if !section.findings.iter().any(|f|f.term_code==term.code) && ui.button(format!("Добавить {}",term.label)).clicked(){section.findings.push(Finding{id:Uuid::new_v4(),term_code:term.code.clone(),observation:field(FeatureState::NotAssessed),physician_confirmed:true});}}}
 crate::forms::lesions(ui,section,self.dictionary.as_ref());
 }self.dirty|=before!=serde_json::to_string(section).unwrap();});});}
 let before=serde_json::to_string(&p.cervical_assessment).unwrap();crate::forms::cervical(ui,&mut p.cervical_assessment);self.dirty|=before!=serde_json::to_string(&p.cervical_assessment).unwrap();
 let before=serde_json::to_string(&p.tests).unwrap();crate::forms::probes(ui,&mut p.tests,self.dictionary.as_ref(),&p.sections);self.dirty|=before!=serde_json::to_string(&p.tests).unwrap();
 let before=serde_json::to_string(&p.biopsies).unwrap();crate::forms::biopsies(ui,&mut p.biopsies,&p.sections);self.dirty|=before!=serde_json::to_string(&p.biopsies).unwrap();
 let before=serde_json::to_string(&p.excision).unwrap();crate::forms::excision(ui,&mut p.excision);self.dirty|=before!=serde_json::to_string(&p.excision).unwrap();
 self.dirty|=text_field(ui,"Заключение",&mut p.conclusion);self.dirty|=text_field(ui,"Рекомендации",&mut p.recommendations);
 });},
 1=>{
 if ui.add_enabled(editable && self.background_jobs==0,egui::Button::new("Импортировать снимки JPEG / PNG / TIFF / BMP")).clicked()&& let Some(files)=rfd::FileDialog::new().add_filter("Снимки",&["jpg","jpeg","png","tif","tiff","bmp"]).pick_files(){let id=self.detail.as_ref().unwrap().study.id;self.start_import(id,files);}
 let mut select=None;ui.horizontal_wrapped(|ui|{for (i,a) in self.detail.as_ref().unwrap().assets.iter().filter(|a|a.kind=="original").enumerate(){if ui.selectable_label(self.selected_asset==Some(a.id),format!("Снимок {}",i+1)).clicked(){select=Some(a.id);}}});if let Some(id)=select{let r=self.load_photo(id,ctx);self.status(r,"Снимок открыт");}
 egui::ComboBox::from_id_salt("lesion-link").selected_text(self.lesion_link.map(|id|format!("Очаг {}",&id.to_string()[..8])).unwrap_or_else(||"Без связи с очагом".into())).show_ui(ui,|ui|{ui.selectable_value(&mut self.lesion_link,None,"Без связи с очагом");for section in &self.detail.as_ref().unwrap().protocol.as_ref().unwrap().sections{for(i,lesion)in section.lesions.iter().enumerate(){ui.selectable_value(&mut self.lesion_link,Some(lesion.id),format!("{:?}: очаг {}",section.region,i+1));}}});
 ui.add(egui::Slider::new(&mut self.radius,0.005..=0.25).text("Радиус метки"));ui.text_edit_singleline(&mut self.label);ui.label("Щелчок на снимке добавляет метку. Измерения в мм без калибровки недоступны.");
 if let Some(texture)=&self.texture{let available=ui.available_width().min(950.0);let size=texture.size_vec2()*(available/texture.size_vec2().x).min(1.0);let response=ui.add(egui::Image::new((texture.id(),size)).sense(egui::Sense::click()));let d=self.detail.as_mut().unwrap();for a in &d.annotations{if Some(a.asset_id)==self.selected_asset{let center=egui::pos2(response.rect.left()+a.x*response.rect.width(),response.rect.top()+a.y*response.rect.height());ui.painter().circle_stroke(center,a.radius*response.rect.width().min(response.rect.height()),egui::Stroke::new(2.0,egui::Color32::RED));ui.painter().text(center,egui::Align2::CENTER_CENTER,&a.label,egui::FontId::proportional(12.0),egui::Color32::RED);}}
if editable && response.clicked()&& let Some(pos)=response.interact_pointer_pos(){d.annotations.push(Annotation{layer:Default::default(),coordinate_system:Default::default(),id:Uuid::new_v4(),asset_id:self.selected_asset.unwrap(),lesion_id:self.lesion_link,finding_id:None,x:(pos.x-response.rect.left())/response.rect.width(),y:(pos.y-response.rect.top())/response.rect.height(),radius:self.radius,label:self.label.clone()});self.dirty=true;}}
 let mut remove=None;for (i,a) in self.detail.as_ref().unwrap().annotations.iter().enumerate(){ui.horizontal(|ui|{ui.label(format!("Метка {}: {}",i+1,a.label));if ui.add_enabled(editable,egui::Button::new("Удалить")).clicked(){remove=Some(i);}});}
if let Some(i)=remove{self.detail.as_mut().unwrap().annotations.remove(i);self.dirty=true;}
 },
 2=>{if self.templates.is_empty(){ui.label("Нет опубликованных шаблонов");}else{self.template=self.template.min(self.templates.len()-1);egui::ComboBox::from_id_salt("template").selected_text(&self.templates[self.template].layout.title).show_ui(ui,|ui|{for (i,t) in self.templates.iter().enumerate(){ui.selectable_value(&mut self.template,i,&t.layout.title);}});
 let layout=&mut self.templates[self.template].layout;ui.text_edit_singleline(&mut layout.title);ui.add(egui::Slider::new(&mut layout.photo_columns,1..=3).text("Колонки снимков"));let mut movement=None;let mut remove=None;for (i,block) in layout.blocks.iter().enumerate(){ui.horizontal(|ui|{ui.label(format!("{block:?}"));if i>0 && ui.button("↑").clicked(){movement=Some((i,i-1));}
if i+1<layout.blocks.len() && ui.button("↓").clicked(){movement=Some((i,i+1));}
if ui.button("Убрать").clicked(){remove=Some(i);}});}
if let Some((a,b))=movement{layout.blocks.swap(a,b);}
if let Some(i)=remove{layout.blocks.remove(i);}
if ui.button("Вернуть все блоки").clicked(){layout.blocks=vec![ReportBlock::Patient,ReportBlock::Indications,ReportBlock::History,ReportBlock::Assessment,ReportBlock::Findings,ReportBlock::Photos,ReportBlock::Conclusion,ReportBlock::Recommendations,ReportBlock::Signature];}
 if ui.button("Сохранить как новый шаблон").clicked(){let layout=self.templates[self.template].layout.clone();let r=self.connection.as_ref().unwrap().post::<_,Template>("/api/v1/templates",&layout).map(|t|{self.templates.insert(0,t);self.template=0;});self.status(r,"Новый шаблон сохранен");}
 ui.collapsing("Фотографии в отчете",|ui|{for(i,a)in self.detail.as_ref().unwrap().assets.iter().filter(|a|a.kind=="original").enumerate(){let mut selected=self.selected_photos.contains(&a.id);if ui.checkbox(&mut selected,format!("Снимок {}",i+1)).changed(){if selected{self.selected_photos.insert(a.id);}else{self.selected_photos.remove(&a.id);}}}});
 if ui.add_enabled(!self.dirty,egui::Button::new("Сформировать HTML/PDF, сохранить и открыть PDF")).clicked(){let r=self.generate();self.status(r,"Формирование отчета выполняется в фоне");}
 ui.collapsing("Сохраненные HTML/PDF",|ui|{for asset in &self.detail.as_ref().unwrap().assets{if matches!(asset.kind.as_str(),"pdf"|"html") && ui.button(format!("Сохранить {} · {}",asset.kind,&asset.id.to_string()[..8])).clicked()&& let Some(path)=rfd::FileDialog::new().set_file_name(format!("report.{}",asset.kind)).save_file(){let connection=self.connection.as_ref().unwrap().clone();let id=asset.id;let sender=self.events_tx.clone();std::thread::spawn(move||{let result=connection.download(id).and_then(|bytes|std::fs::write(path,bytes).map_err(|_|"Не удалось сохранить файл".into()));let _=sender.send(UiEvent::Archive(result));});}}});
 }},
 _=>{ui.label("Полная типизированная форма JSON: дополнительные поля, очаги, пробы и эксцизия. Неизвестные поля отклоняются.");if ui.add_enabled(editable,egui::TextEdit::multiline(&mut self.advanced).code_editor().desired_rows(30).desired_width(f32::INFINITY)).changed(){self.dirty=true;self.advanced_edited=true;}
 if ui.add_enabled(editable,egui::Button::new("Применить форму")).clicked(){match serde_json::from_str::<Protocol>(&self.advanced){Ok(p)=>{self.detail.as_mut().unwrap().protocol=Some(p);self.dirty=true;self.advanced_edited=false;self.message="Форма применена; сохраните на сервере".into();},Err(_)=>self.message="Неверная структура JSON протокола".into()}}}
 }});
 });
        if self.review
            && let Some(offline) = self.connection.as_ref().and_then(|c| c.offline.clone())
            && let Some(conflict) = offline.conflict()
            && !conflict["operation"]["path"]
                .as_str()
                .unwrap_or("")
                .ends_with("/protocol")
        {
            egui::Window::new("Операция требует исправления — исходная сохранена").show(ctx, |ui| {
                ui.label(format!("{}", conflict["operation"]["path"]));
                ui.add(egui::TextEdit::multiline(&mut self.failed_operation_json).code_editor().desired_rows(10));
                if ui.button("Отправить исправленную операцию").clicked() {
                    let result = serde_json::from_str(&self.failed_operation_json)
                        .map_err(|_| "Неверный JSON".into())
                        .and_then(|body| offline.revise_failed(body));
                    if result.is_ok() { self.review = false; }
                    self.status(result, "Исправленная операция поставлена в очередь; исходная сохранена в архиве");
                }
                if ui.button("Продолжить локальную работу").clicked() { self.review = false; }
            });
        }
        if let (Some(detail), Some(offline)) = (
            self.detail.clone(),
            self.connection.as_ref().and_then(|c| c.offline.clone()),
        ) && self.review
        {
            egui::Window::new("Сравнение версий — локальная работа не удаляется").default_width(750.0).show(ctx,|ui|{ui.label("Не объединяйте заключения автоматически. Исправьте локальную форму после сравнения с сервером.");if let Some(conflict)=offline.conflict(){ui.label(format!("Отклоненная операция: {}",conflict["operation"]["path"]));if !conflict["operation"]["path"].as_str().unwrap_or("").ends_with("/protocol"){ui.add(egui::TextEdit::multiline(&mut self.failed_operation_json).code_editor().desired_rows(8));if ui.button("Отправить исправленную операцию (исходная сохранится в архиве)").clicked(){let result=serde_json::from_str(&self.failed_operation_json).map_err(|_|"Неверный JSON".into()).and_then(|body|offline.revise_failed(body));self.status(result,"Исправленная операция поставлена в локальную очередь");}}}
 if let Some(remote)=offline.remote(detail.study.id){let mut text=serde_json::to_string_pretty(&remote).unwrap();ui.label("Серверная копия (только чтение)");ui.add(egui::TextEdit::multiline(&mut text).code_editor().interactive(false).desired_rows(15).desired_width(f32::INFINITY));if ui.button("Я сравнил версии: отправить текущую локальную форму как новую редакцию").clicked(){let result=offline.resolve_protocol(&detail);if result.is_ok(){self.dirty=false;self.review=false;if let Ok(cached)=self.connection.as_ref().unwrap().get(&format!("/api/v1/studies/{}",detail.study.id)){self.detail=Some(cached);}}self.status(result,"Объединенный черновик сохранен локально; ожидает квитанции backend");}}else{ui.label("Серверная копия запрашивается в фоне. Продолжайте локальное редактирование.");}
 if ui.button("Закрыть сравнение и продолжить локально").clicked(){self.review=false;}});
        }
        self.checkpoint();
    }
}
pub(crate) fn text_field(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut Field<Assessment<String>>,
) -> bool {
    let before = serde_json::to_value(&*value).unwrap();
    ui.push_id(label, |ui| {
        ui.label(label);
        let mut mode = match value.value {
            Assessment::Assessed(_) => 0,
            Assessment::NotAssessed => 1,
            Assessment::NotApplicable => 2,
        };
        let old = mode;
        egui::ComboBox::from_id_salt("assessment-state")
            .selected_text(["Заполнено", "Не оценено", "Не применимо"][mode])
            .show_ui(ui, |ui| {
                for (i, label) in ["Заполнено", "Не оценено", "Не применимо"]
                    .iter()
                    .enumerate()
                {
                    ui.selectable_value(&mut mode, i, *label);
                }
            });
        if mode != old {
            *value = field(match mode {
                0 => Assessment::Assessed(String::new()),
                1 => Assessment::NotAssessed,
                _ => Assessment::NotApplicable,
            });
        }
        if let Assessment::Assessed(text) = &mut value.value
            && ui
                .add(
                    egui::TextEdit::multiline(text)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY),
                )
                .changed()
        {
            value.origin = Origin::Physician;
        }
    });
    before != serde_json::to_value(value).unwrap()
}

fn journal_panel_message(connection: &Option<Connection>) -> String {
    connection
        .as_ref()
        .and_then(|c| c.offline.as_ref())
        .and_then(|o| {
            o.journal()
                .get("panel_error")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        })
        .unwrap_or("Форма закрыта; работа сохранена локально".into())
}
