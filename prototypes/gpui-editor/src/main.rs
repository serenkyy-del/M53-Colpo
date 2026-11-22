//! Native Rust/GPUI workplace. The isolated editor remains available with --pilot.
#![cfg_attr(windows, windows_subsystem = "windows")]
mod appearance;
mod ui_chrome;
#[cfg(not(windows))]
mod editor_regression_proof;
mod clinic;
#[cfg(all(target_os = "linux", feature = "standalone"))]
mod linux_output;
mod native_form;
mod diagnosis_picker;
mod patient_table;
mod patient_editor;
mod protocol_history;
mod registry_browser;
mod registry_view;
mod report_view;
mod report_delivery;
mod marker_card;
mod phrase_panel;
mod visit_template_panel;
mod photo_loader;
mod photo_export;
mod startup;
mod storage;
mod vector;
#[cfg(not(windows))]
mod verification;
mod workplace;
use gpui_kit::assets::{AllAssets, IconName};
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{
    ActiveTheme, Disableable, ElementExt, Selectable, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem},
    scroll::ScrollableElement,
    h_flex, v_flex,
};
use gpui_kit::{prelude::*, *};
use m53_client::defects::{Handle, PRESETS};
use m53_contracts::workflow::AnnotationLayer;
use m53_gpui_editor::{Photo, Rect, Session};
use std::{
    collections::HashMap,
    sync::{Arc, mpsc},
    time::Duration,
};
use storage::{Reply, Request};
use uuid::Uuid;

fn update_crop(photo: &mut Photo, start: (f32, f32), end: (f32, f32), before: &Photo) -> Option<Uuid> {
    let end = (end.0.clamp(0., 1.), end.1.clamp(0., 1.));
    let (left, top) = (start.0.min(end.0), start.1.min(end.1));
    let (width, height) = ((start.0 - end.0).abs(), (start.1 - end.1).abs());
    if width < 0.01 || height < 0.01 { *photo = before.clone(); return None; }
    photo.annotations.retain(|a| !matches!(a.layer, AnnotationLayer::OutputRegion { .. }));
    let annotation = photo.annotation(left, top, AnnotationLayer::OutputRegion { width, height });
    let id = annotation.id;
    photo.annotations.push(annotation);
    Some(id)
}
fn keyboard_delta(key: &str, fine: bool) -> Option<(f32, f32)> {
    let step = if fine { 0.001 } else { 0.01 };
    match key { "left" => Some((-step,0.)), "right" => Some((step,0.)), "up" => Some((0.,-step)), "down" => Some((0.,step)), _ => None }
}
fn keyboard_crop_allowed(ready: bool, readonly: bool, focused: bool) -> bool { ready && !readonly && focused }

#[cfg(test)]
mod crop_regression_tests {
    use super::{update_crop, keyboard_delta, keyboard_crop_allowed, Photo, AnnotationLayer};
    #[test]
    fn keyboard_crop_requires_ready_editable_focused_canvas() {
        assert!(keyboard_crop_allowed(true,false,true));
        assert!(!keyboard_crop_allowed(false,false,true));
        assert!(!keyboard_crop_allowed(true,true,true));
        assert!(!keyboard_crop_allowed(true,false,false));
    }
    #[test]
    fn keyboard_arrows_offer_precise_and_normal_steps() {
        assert_eq!(keyboard_delta("left", false), Some((-0.01, 0.)));
        assert_eq!(keyboard_delta("up", true), Some((0., -0.001)));
        assert_eq!(keyboard_delta("enter", false), None);
    }
    #[test]
    fn returning_crop_to_origin_restores_previous_region() {
        let mut photo = Photo::new(1000, 800);
        let old = photo.annotation(0.2, 0.2, AnnotationLayer::OutputRegion { width: 0.3, height: 0.3 });
        let old_id = old.id;
        photo.annotations.push(old);
        let before = photo.clone();
        update_crop(&mut photo, (0.1, 0.1), (0.9, 0.9), &before);
        update_crop(&mut photo, (0.1, 0.1), (0.101, 0.101), &before);
        assert_eq!(photo.annotations.len(), 1);
        assert_eq!(photo.annotations[0].id, old_id);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Tool {
    Select,
    Hand,
    Crop,
}
#[derive(Clone, Copy)]
enum Gesture {
    Callout(Uuid, Handle, (f32, f32)),
    Pan((f32, f32), (f32, f32)),
    Crop((f32, f32)),
    Target(Uuid, Handle, (f32, f32)),
    Region(Uuid, u8, (f32, f32, f32, f32), (f32, f32)),
}
struct Editor {
    clinic: Option<(Uuid, mpsc::Sender<clinic::Command>)>,
    export_patient: Option<(String, String)>,
    readonly: bool,
    historical_readonly: bool,
    photo_selecting: bool,
    photo_checked: Vec<Uuid>,
    filmstrip_scroll: ScrollHandle,
    filmstrip_width: f32,
    photo_demand: Option<(Uuid, photo_loader::Selection)>,
    live_active: bool,
    live_image: Option<Arc<RenderImage>>,
    properties: Option<Entity<native_form::NativeForm>>,
    focus_handle: FocusHandle,
    palette_vulva: bool,
    palette_open: bool,
    hover_cursor: CursorStyle,
    palette_search: Entity<InputState>,
    _palette_search: Subscription,
    session: Session,
    images: HashMap<Uuid, Arc<RenderImage>>,
    thumbnails: HashMap<Uuid, Arc<RenderImage>>,
    tool: Tool,
    preset: Option<usize>,
    pending_tip: Option<(f32, f32)>,
    pointer: Option<(f32, f32)>,
    selected: Option<Uuid>,
    dismissed_properties: Option<Uuid>,
    bulk_mode: bool,
    checked: Vec<Uuid>,
    retag: Option<Uuid>,
    gesture: Option<Gesture>,
    bounds: Rect,
    zoom: f32,
    pan: (f32, f32),
    undo: Vec<Session>,
    before_gesture: Option<Session>,
    worker: Option<mpsc::Sender<Request>>,
    replies: Option<mpsc::Receiver<Reply>>,
    version: u64,
    saved: u64,
    ready: bool,
    message: String,
    failure: bool,
    settings: bool,
    io_pending: usize,
    photo_pending: usize,
    _appearance: Option<Subscription>,
}
impl Editor {
    fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        worker: Option<(mpsc::Sender<Request>, mpsc::Receiver<Reply>)>,
    ) -> Self {
        let (tx, rx) = worker
            .map(|(tx, rx)| (Some(tx), Some(rx)))
            .unwrap_or_default();
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            crate::appearance::sync_system(Some(window), cx);
            cx.notify();
        });
        let palette_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Найти обозначение…"));
        let palette_observer = cx.observe(&palette_search, |_, _, cx| cx.notify());
        let mut editor = Self {
            clinic: None,
            export_patient: None,
            readonly: false,
            historical_readonly: false,
            photo_selecting:false,
            photo_checked:vec![],
            filmstrip_scroll:ScrollHandle::default(),
            filmstrip_width: 1024.,
            photo_demand: None,
            live_active: false,
            live_image: None,
            properties: None,
            focus_handle: cx.focus_handle(),
            palette_vulva: false,
            palette_open: false,
            hover_cursor: CursorStyle::Arrow,
            palette_search,
            _palette_search: palette_observer,
            session: Session::default(),
            images: HashMap::new(),
            thumbnails: HashMap::new(),
            tool: Tool::Select,
            preset: None,
            pending_tip: None,
            pointer: None,
            selected: None,
            dismissed_properties: None,
            bulk_mode: false,
            checked: vec![],
            retag: None,
            gesture: None,
            bounds: Rect {
                x: 0.,
                y: 0.,
                w: 1.,
                h: 1.,
            },
            zoom: 1.,
            pan: (0., 0.),
            undo: vec![],
            before_gesture: None,
            ready: tx.is_none(),
            worker: tx,
            replies: rx,
            version: 0,
            saved: 0,
            message: "Открытие локального проекта…".into(),
            failure: false,
            settings: false,
            io_pending: 0,
            photo_pending: 0,
            _appearance: Some(appearance),
        };
        if let Some(tx) = &editor.worker {
            let _ = tx.send(Request::Restore);
        } else {
            editor.message = "Визуальная проверка • синтетический снимок".into();
        }
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                if view.update(cx, |view, cx| view.poll(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        editor
    }
    fn photo(&self) -> Option<&Photo> {
        self.session.photos.get(self.session.selected).filter(|p|!p.removed)
    }
    fn photo_mut(&mut self) -> Option<&mut Photo> {
        self.session.photos.get_mut(self.session.selected).filter(|p|!p.removed)
    }
    fn image_rect(&self) -> Option<Rect> {
        self.photo()
            .map(|p| Rect::fit(self.bounds, p.width, p.height, self.zoom, self.pan))
    }
    fn poll(&mut self, cx: &mut Context<Self>) {
        if let Some((study, worker)) = self.clinic.clone() {
            let selection = self.photo_selection();
            if self.photo_demand.as_ref() != Some(&(study, selection.clone())) {
                self.retain_photo_cache(&selection, cx);
                let _ = worker.send(clinic::Command::PhotoDemand { study, selection: selection.clone() });
                self.photo_demand = Some((study, selection));
                cx.notify();
            }
        }
        loop {
            let reply = self.replies.as_ref().and_then(|rx| rx.try_recv().ok());
            let Some(reply) = reply else { break };
            match reply {
                Reply::Restored(session, images) => {
                    self.session = session;
                    self.images = images.iter().map(|(id, full, _)| (*id, full.clone())).collect();
                    self.thumbnails = images.into_iter().map(|(id, _, small)| (id, small)).collect();
                    self.ready = true;
                    self.message = "Локальный проект открыт".into();
                }
                Reply::Imported(photo, image, thumbnail) => {
                    self.images.insert(photo.id, image);
                    self.thumbnails.insert(photo.id, thumbnail);
                    self.session.photos.push(photo);
                    self.session.selected = self.session.photos.len() - 1;
                    self.fit();
                    self.persist();
                }
                Reply::Saved(version) => {
                    self.saved = self.saved.max(version);
                    if self.saved == self.version {
                        self.message = "Сохранено на компьютере".into();
                        self.failure = false;
                    }
                }
                Reply::ImportFinished => {
                    self.io_pending = self.io_pending.saturating_sub(1);
                }
                Reply::Exported => {
                    self.io_pending = self.io_pending.saturating_sub(1);
                    self.message = "PNG сохранен • оригинал не изменен".into();
                    self.failure = false;
                }
                Reply::Error(error, io_complete) => {
                    if io_complete {
                        self.io_pending = self.io_pending.saturating_sub(1);
                    }
                    self.message = error;
                    self.failure = true;
                }
            }
            cx.notify();
        }
    }
    fn photo_selection(&self) -> photo_loader::Selection {
        photo_loader::Selection::for_session(&self.session, self.filmstrip_scroll.offset().x.as_f32(), self.filmstrip_width)
    }
    fn retain_photo_cache(&mut self, selection: &photo_loader::Selection, cx: &mut Context<Self>) {
        // Called by the event poll outside a Window update: release sprite-atlas entries too.
        self.images.retain(|id,image| {
            let keep=selection.full.contains(id);
            if !keep {cx.drop_image(image.clone(),None);}
            keep
        });
        self.thumbnails.retain(|id,image| {
            let keep=selection.thumbnails.contains(id);
            if !keep {cx.drop_image(image.clone(),None);}
            keep
        });
    }
    fn clear_photo_cache(&mut self, cx: &mut Context<Self>) {
        for (_,image) in self.images.drain().chain(self.thumbnails.drain()) {cx.drop_image(image,None);}
        self.photo_demand=None;
    }
    fn persist(&mut self) {
        if !self.ready || self.historical_readonly {
            return;
        }
        self.version += 1;
        if let Some((study, worker)) = &self.clinic {
            if worker
                .send(clinic::Command::Editor {
                    study: *study,
                    version: self.version,
                    session: self.session.clone(),
                })
                .is_err()
            {
                self.failure = true;
            }
            return;
        }
        self.message = "Сохранение на компьютере…".into();
        if let Some(tx) = &self.worker
            && tx
                .send(Request::Save(self.version, self.session.clone()))
                .is_err()
        {
            self.failure = true;
            self.message = "Не работает локальное сохранение".into();
        }
    }
    fn checkpoint(&mut self) {
        if self.undo.len() >= 50 {
            self.undo.remove(0);
        }
        self.undo.push(self.session.clone());
    }
    fn fit(&mut self) {
        if self.zoom != 1. {self.audit_action("image.zoom_changed");}
        if self.pan != (0.,0.) {self.audit_action("image.pan_changed");}
        self.bulk_mode=false;self.checked.clear();
        self.zoom = 1.;
        self.pan = (0., 0.);
        self.selected = None;
        self.retag = None;
        self.gesture = None;
    }
    fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ready || self.readonly || self.io_pending > 0 {
            return;
        }
        #[cfg(windows)]
        let paths = cx.background_executor().spawn(async {
            Ok::<_, String>(Ok::<_, String>(
                rfd::FileDialog::new()
                    .set_title("Открыть снимок")
                    .add_filter("Снимки", &["jpg", "jpeg", "png", "tif", "tiff", "bmp"])
                    .pick_files(),
            ))
        });
        #[cfg(not(windows))]
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Открыть снимок".into()),
        });
        cx.spawn_in(window, async move |view, cx| match paths.await {
            Ok(Ok(Some(paths))) => {
                let _ = view.update(cx, |view, cx| {
                    if !view.ready || view.readonly || view.io_pending > 0 {return;}
                    view.message = "Импорт снимков…".into();
                    view.failure = false;
                    view.io_pending += 1;
                    if let Some((study, tx)) = &view.clinic {
                        view.photo_pending += 1;
                        if tx.send(clinic::Command::Import {
                            study: *study,
                            paths,
                        }).is_err() {
                            view.photo_pending=view.photo_pending.saturating_sub(1);
                            view.io_pending=view.io_pending.saturating_sub(1);
                            view.failure=true;view.message="Не работает сохранение снимков".into();
                        }
                    } else if let Some(tx) = &view.worker {
                        let _ = tx.send(Request::Import(paths));
                    }
                    cx.notify();
                });
            }
            Ok(Err(_)) | Err(_) => {
                let _ = view.update(cx, |view, cx| {
                    view.message = "Не удалось открыть выбор файлов".into();
                    view.failure = true;
                    cx.notify();
                });
            }
            _ => (),
        })
        .detach();
    }
    fn export(&mut self, asset: Option<Uuid>, marked: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ready || self.io_pending > 0 { return; }
        let study = self.clinic.as_ref().map(|(study, _)| *study);
        let patient = self.export_patient.clone();
        let visible = self.session.photos.iter().filter(|photo| !photo.removed).enumerate();
        let files: Vec<_> = visible.filter(|(_, photo)| asset.is_none_or(|id| photo.id == id))
            .map(|(index, photo)| {
                let name = photo_export::file_name(patient.as_ref().map(|(name, date)| (name.as_str(), date.as_str())), index + 1, marked);
                (photo.id, name)
            }).collect();
        if files.is_empty() { return; }
        let all = asset.is_none();
        let default_name = files[0].1.clone();
        #[cfg(windows)]
        let documents = m53_client::preview::documents_directory();
        #[cfg(not(windows))]
        let documents = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir).join("Documents");
        #[cfg(windows)]
        let path = cx.background_executor().spawn(async move {
            let dialog = rfd::FileDialog::new().set_directory(documents);
            let choice = if all { dialog.set_title("Выберите папку для снимков").pick_folder() }
            else { dialog.set_title("Сохранить снимок").set_file_name(default_name).add_filter("Снимок PNG", &["png"]).save_file() };
            Ok::<_, String>(Ok::<_, String>(choice))
        });
        #[cfg(not(windows))]
        let folder = all.then(|| cx.prompt_for_paths(PathPromptOptions {
            files: false, directories: true, multiple: false, prompt: Some("Выберите папку для снимков".into()),
        }));
        #[cfg(not(windows))]
        let path = (!all).then(|| cx.prompt_for_new_path(&documents, Some(&default_name)));
        self.io_pending += 1;
        cx.spawn_in(window, async move |view, cx| {
            #[cfg(windows)]
            let selected = path.await.ok().and_then(Result::ok);
            #[cfg(not(windows))]
            let selected = match (folder, path) {
                (Some(folder), _) => folder.await.ok().and_then(Result::ok).map(|paths| paths.and_then(|paths| paths.into_iter().next())),
                (_, Some(path)) => path.await.ok().and_then(Result::ok),
                _ => None,
            };
            let _ = view.update(cx, |view, cx| {
                view.io_pending = view.io_pending.saturating_sub(1).max(view.photo_pending);
                let result = (|| -> Result<usize, &str> {
                    let Some(path) = selected.ok_or("Не удалось открыть диалог сохранения")? else { return Ok(0); };
                    if !view.ready || study != view.clinic.as_ref().map(|(study, _)| *study) || patient != view.export_patient {
                        return Err("Обследование изменилось. Повторите сохранение снимков");
                    }
                    let photos: Vec<_> = files.into_iter().map(|(id, name)| {
                        let mut photo = view.session.photos.iter().find(|photo| photo.id == id && !photo.removed)
                            .cloned().ok_or("Состав снимков изменился. Повторите сохранение")?;
                        if !marked { photo.annotations.clear(); }
                        let destination = if all { path.join(name) } else { path.clone() };
                        Ok((photo, destination))
                    }).collect::<Result<_, &str>>()?;
                    if let Some((study, tx)) = &view.clinic {
                        tx.send(clinic::Command::ExportPhotos { study: *study, photos }).map_err(|_| "Не работает сохранение снимков")?;
                        Ok(1)
                    } else if let Some(tx) = &view.worker {
                        let mut pending = 0;
                        for (photo, path) in photos {
                            if tx.send(Request::Export(photo, path)).is_err() { break; }
                            pending += 1;
                        }
                        if pending == 0 { return Err("Не работает сохранение снимков"); }
                        Ok(pending)
                    } else {
                        Err("Не работает сохранение снимков")
                    }
                })();
                match result {
                    Ok(0) => {},
                    Ok(pending) => { view.message = "Подготовка PNG…".into(); view.failure = false; view.io_pending += pending; },
                    Err(error) => { view.message = error.into(); view.failure = true; },
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn export_menu(&self, button: Button, asset: Option<Uuid>, cx: &Context<Self>) -> impl IntoElement {
        let editor = cx.entity();
        button.disabled(!self.ready || self.io_pending > 0 || self.session.visible_count() == 0)
            .dropdown_menu(move |mut menu, _, _| {
                for (marked, label) in [(true, "С разметкой"), (false, "Без разметки")] {
                    let editor = editor.clone();
                    menu = menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        editor.update(cx, |editor, cx| editor.export(asset, marked, window, cx));
                    }));
                }
                menu
            })
    }
    fn allows_marker(&self, preset: usize) -> bool {
        self.photo().is_some_and(|p| m53_client::defects::allows_marker(p.annotations.iter(), preset))
    }
    fn stamp(&mut self, preset: usize, position: Point<Pixels>) {
        if !self.allows_marker(preset) { self.message="На одном снимке используются обозначения только шейки или только вульвы".into(); return; }
        if self.readonly
            || self
                .photo()
                .is_none_or(|p| !self.images.contains_key(&p.id))
        {
            return;
        }
        let Some(rect) = self.image_rect() else {
            return;
        };
        let (x, y) = rect.normalized(position.x.as_f32(), position.y.as_f32());
        if !rect.contains(position.x.as_f32(), position.y.as_f32()) {
            return;
        }
        let Some((tip_x, tip_y)) = self.pending_tip.take() else {
            self.pending_tip = Some((x, y));
            self.pointer = Some((x, y));
            self.message = "Острие установлено — выберите центр кружка. Esc — отмена".into();
            return;
        };
        self.checkpoint();
        self.selected = self
            .photo_mut()
            .map(|p| p.place_callout(preset, (tip_x, tip_y), (x, y)));
        self.preset = None;
        self.pointer = None;
        self.message.clear();
        self.tool = Tool::Select;
        self.persist();
    }
    fn down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        if !self.ready || self.photo().is_none_or(|p|!self.images.contains_key(&p.id)) {
            return;
        }
        let Some(rect) = self.image_rect() else {
            return;
        };
        let pos = (event.position.x.as_f32(), event.position.y.as_f32());
        if self.tool == Tool::Hand {
            self.gesture = Some(Gesture::Pan(pos, self.pan));
            cx.notify();
            return;
        }
        if !rect.contains(pos.0, pos.1) {
            self.selected = None;
            cx.notify();
            return;
        }
        let (x, y) = rect.normalized(pos.0, pos.1);
        self.dismissed_properties = None;
        if self.readonly {
            // Approved images support inspection without starting a clinical mutation/drag.
            self.selected = self.photo().and_then(|p| p.hit(rect, pos.0, pos.1)).map(|(id, _)| id);
            cx.notify();
            return;
        }
        if let Some(preset) = self.preset {
            self.stamp(preset, event.position);
            cx.notify();
            return;
        }
        if event.click_count == 2
            && let Some((id, _)) = self.photo().and_then(|p| p.hit(rect, pos.0, pos.1))
        {
            self.selected = Some(id);
            self.retag = Some(id);
            self.palette_open = true;
            self.preset = None;
            cx.notify();
            return;
        }
        if self.tool == Tool::Crop {
            self.pointer = None;
            self.before_gesture = Some(self.session.clone());
            self.gesture = Some(Gesture::Crop((x, y)));
            cx.notify();
            return;
        }
        self.before_gesture = None;
        self.gesture = None;
        if let Some((id, gesture)) = self.annotation_hit(rect, pos) {
            self.before_gesture = Some(self.session.clone());
            self.gesture = Some(gesture);
            self.selected = Some(id);
        } else {
            self.selected = None;
        }
        cx.notify();
    }
    /// Hover and pointer-down share the same stacking and handle tolerances.
    fn annotation_hit(&self, rect: Rect, pos: (f32, f32)) -> Option<(Uuid, Gesture)> {
        if !rect.contains(pos.0, pos.1) { return None; }
        let (x, y) = rect.normalized(pos.0, pos.1);
        let photo = self.photo()?;
        // Selected target handles take priority over overlapping callouts/crop.
        if let Some(a) = photo.annotations.iter().find(|a| {
                Some(a.id) == self.selected
                    && !photo.hidden.contains(&a.id)
                    && matches!(a.layer, AnnotationLayer::Target { .. })
        }) {
            let d = ((x - a.x) * rect.w).hypot((y - a.y) * rect.h);
            let radius = a.radius * rect.w.min(rect.h);
            if (d - radius).abs() <= 12. || d <= 14. {
                return Some((a.id, Gesture::Target(
                    a.id,
                    if d <= 14. {
                        Handle::Body
                    } else {
                        Handle::Radius
                    },
                    (x - a.x, y - a.y),
                )));
            }
        }
        if let Some((id, handle)) = photo.hit(rect, pos.0, pos.1) {
            let a = photo.annotations
                .iter()
                .find(|a| a.id == id)
                ?;
            return Some((id, Gesture::Callout(
                id,
                handle,
                if handle == Handle::Body {
                    (x - a.x, y - a.y)
                } else {
                    (0., 0.)
                },
            )));
        }
        // Target/crop are independently selectable geometric objects.
                for a in photo
                    .annotations
                    .iter()
                    .rev()
                    .filter(|a| !photo.hidden.contains(&a.id))
                {
                    match a.layer {
                        AnnotationLayer::Target { .. } => {
                            let d = ((x - a.x) * rect.w).hypot((y - a.y) * rect.h);
                            let r = a.radius * rect.w.min(rect.h);
                            if (d - r).abs() < 9. || d < 14. {
                                return Some((a.id, Gesture::Target(
                                    a.id,
                                    if d < 14. {
                                        Handle::Body
                                    } else {
                                        Handle::Radius
                                    },
                                    (x - a.x, y - a.y),
                                )));
                            }
                        }
                        AnnotationLayer::OutputRegion { width, height } => {
                            let corners = [
                                (a.x, a.y),
                                (a.x + width, a.y),
                                (a.x + width, a.y + height),
                                (a.x, a.y + height),
                            ];
                            let corner = corners.iter().position(|(cx, cy)| {
                                ((x - cx) * rect.w).hypot((y - cy) * rect.h) <= 12.
                            });
                            let border = x >= a.x
                                && x <= a.x + width
                                && y >= a.y
                                && y <= a.y + height
                                && (((x - a.x) * rect.w).min((a.x + width - x) * rect.w) <= 10.
                                    || ((y - a.y) * rect.h).min((a.y + height - y) * rect.h)
                                        <= 10.);
                            if corner.is_some()
                                || border
                                || (self.selected == Some(a.id)
                                    && x >= a.x
                                    && x <= a.x + width
                                    && y >= a.y
                                    && y <= a.y + height)
                            {
                                return Some((a.id, Gesture::Region(
                                    a.id,
                                    corner.map_or(4, |i| i as u8),
                                    (a.x, a.y, width, height),
                                    (x, y),
                                )));
                            }
                        }
                        _ => (),
                    }
                }
        None
    }
    fn pointer_cursor(&self, pos: (f32,f32)) -> CursorStyle {
        if self.tool==Tool::Hand {
            return if matches!(self.gesture, Some(Gesture::Pan(..))) {CursorStyle::ClosedHand} else {CursorStyle::OpenHand};
        }
        if self.readonly { return CursorStyle::Arrow; }
        if self.preset.is_some() || self.tool == Tool::Crop { return CursorStyle::Crosshair; }
        let hit = self.gesture.or_else(|| self.image_rect().and_then(|r| self.annotation_hit(r, pos)).map(|(_, g)| g));
        if let Some(g)=hit { return match g {
            Gesture::Callout(_,Handle::Radius,_) | Gesture::Target(_,Handle::Radius,_) => CursorStyle::ResizeLeftRight,
            Gesture::Region(_,c,..) if c<4 => if c==0 || c==2 {CursorStyle::ResizeUpLeftDownRight} else {CursorStyle::ResizeUpRightDownLeft},
            Gesture::Crop(..)=>CursorStyle::Crosshair,
            _=>if self.gesture.is_some() {CursorStyle::ClosedHand} else {CursorStyle::OpenHand},
        }; }
        CursorStyle::Arrow
    }
    fn motion(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.readonly && self.gesture.is_some_and(|g| !matches!(g, Gesture::Pan(..))) {
            self.cancel_gesture();
            cx.notify();
            return;
        }
        if self.preset.is_some() {
            self.pointer = self
                .image_rect()
                .filter(|r| r.contains(event.position.x.as_f32(), event.position.y.as_f32()))
                .map(|r| r.normalized(event.position.x.as_f32(), event.position.y.as_f32()));
            cx.notify();
            return;
        }
        if self.tool == Tool::Crop && self.pointer.is_some() && self.gesture.is_none() {
            if let Some(rect)=self.image_rect().filter(|r|r.contains(event.position.x.as_f32(),event.position.y.as_f32())) {
                let next=rect.normalized(event.position.x.as_f32(),event.position.y.as_f32());
                if self.pointer!=Some(next) {self.pointer=Some(next);cx.notify();}
            }
        }
        let cursor = self.pointer_cursor((event.position.x.as_f32(),event.position.y.as_f32()));
        if self.hover_cursor != cursor { self.hover_cursor = cursor; cx.notify(); }
        let Some(gesture) = self.gesture else { return };
        if event.pressed_button != Some(MouseButton::Left) {
            self.finish(cx);
            return;
        }
        let Some(rect) = self.image_rect() else {
            return;
        };
        let (x, y) = rect.normalized(event.position.x.as_f32(), event.position.y.as_f32());
        match gesture {
            Gesture::Pan(start, pan) => {
                self.pan = (
                    pan.0 + event.position.x.as_f32() - start.0,
                    pan.1 + event.position.y.as_f32() - start.1,
                )
            }
            Gesture::Callout(id, handle, offset) => {
                if let Some(p) = self.photo_mut() {
                    p.edit(id, handle, x - offset.0, y - offset.1);
                }
            }
            Gesture::Target(id, handle, offset) => {
                if let Some(p) = self.photo_mut() {
                    let aspect = p.width as f32 / p.height as f32;
                    if let Some(a) = p.annotations.iter_mut().find(|a| a.id == id) {
                        if handle == Handle::Body {
                            a.x = (x - offset.0).clamp(0., 1.);
                            a.y = (y - offset.1).clamp(0., 1.);
                        } else {
                            a.radius = ((x - a.x) * aspect.max(1.))
                                .hypot((y - a.y) * (1. / aspect).max(1.))
                                .clamp(0.05, 1.);
                        }
                    }
                }
            }
            Gesture::Region(id, corner, old, start) => {
                if let Some(p) = self.photo_mut()
                    && let Some(a) = p.annotations.iter_mut().find(|a| a.id == id)
                {
                    let (mut left, mut top, w, h) = old;
                    let (mut right, mut bottom) = (left + w, top + h);
                    if corner == 4 {
                        left = (left + x - start.0).clamp(0., 1. - w);
                        top = (top + y - start.1).clamp(0., 1. - h);
                        right = left + w;
                        bottom = top + h;
                    } else {
                        if corner == 0 || corner == 3 {
                            left = x.clamp(0., right - 0.01);
                        } else {
                            right = x.clamp(left + 0.01, 1.);
                        }
                        if corner == 0 || corner == 1 {
                            top = y.clamp(0., bottom - 0.01);
                        } else {
                            bottom = y.clamp(top + 0.01, 1.);
                        }
                    }
                    a.x = left;
                    a.y = top;
                    a.layer = AnnotationLayer::OutputRegion {
                        width: right - left,
                        height: bottom - top,
                    };
                }
            }
            Gesture::Crop(start) => {
                let before = self.before_gesture.as_ref().and_then(|s|s.photos.get(s.selected)).cloned();
                if let Some(p) = self.photo_mut() {
                    if let Some(before) = before.as_ref() { self.selected = update_crop(p, start, (x, y), before); }
                }
            }
        }
        cx.notify();
    }
    fn audit_action(&self, code:&str) {
        if let Some((study,tx))=&self.clinic {let mut action=m53_client::activity::Action::new(code).study(*study);action.entity_id=self.photo().map(|p|p.id);let _=tx.send(clinic::Command::Activity(action));}
    }
    fn cancel_gesture(&mut self) {
        match self.gesture.take() {
            Some(Gesture::Pan(_, pan)) => self.pan = pan,
            Some(_) => {
                if let Some(before) = self.before_gesture.take() { self.session = before; }
            }
            None => (),
        }
        self.before_gesture = None;
        self.hover_cursor = CursorStyle::Arrow;
        if self.selected.is_some_and(|id| self.photo().is_none_or(|p| !p.annotations.iter().any(|a| a.id == id))) {
            self.selected = None;
        }
    }
    fn undo_action(&mut self, cx: &mut Context<Self>) {
        // Guard the action itself as well as the button; approval can arrive
        // between render and click, and temporary locks must preserve history.
        if self.readonly || self.live_active || self.gesture.is_some() { return; }
        if let Some(before) = self.undo.pop() {
            self.session = before;
            self.selected = None;
            self.cancel_marker_pick();
            self.persist();
            cx.notify();
        }
    }
    fn whole_photo_action(&mut self, cx: &mut Context<Self>) {
        if self.readonly || !self.ready || self.gesture.is_some() { return; }
        if self.photo().is_none_or(|p| !p.annotations.iter().any(|a| matches!(a.layer, AnnotationLayer::OutputRegion { .. }))) { return; }
        self.checkpoint();
        if let Some(photo) = self.photo_mut() {
            photo.annotations.retain(|a| !matches!(a.layer, AnnotationLayer::OutputRegion { .. }));
        }
        self.selected = None;
        self.cancel_marker_pick();
        self.persist();
        cx.notify();
    }
    fn finish(&mut self, cx: &mut Context<Self>) {
        if self.readonly && self.gesture.is_some_and(|g| !matches!(g, Gesture::Pan(..))) {
            self.cancel_gesture();
            cx.notify();
            return;
        }
        if matches!(self.gesture,Some(Gesture::Pan(_,pan)) if self.pan != pan) {self.audit_action("image.pan_changed");}
        let moved = self
            .gesture
            .take()
            .is_some_and(|g| !matches!(g, Gesture::Pan(..)));
        if moved {
            if let Some(before) = self.before_gesture.take() {
                // Compare only at release, never on the pointer-move path.
                let changed = match (serde_json::to_value(&before), serde_json::to_value(&self.session)) {
                    (Ok(before), Ok(current)) => before != current,
                    _ => true,
                };
                if changed {
                    if self.undo.len() >= 50 { self.undo.remove(0); }
                    self.undo.push(before);
                    self.persist();
                }
            }
            self.tool = Tool::Select;
        } else {
            self.before_gesture = None;
        }
        cx.notify();
    }
    fn target(&mut self) {
        if self.readonly {
            return;
        }
        self.checkpoint();
        if let Some(p) = self.photo_mut() {
            let id = if let Some(a) = p
                .annotations
                .iter()
                .find(|a| matches!(a.layer, AnnotationLayer::Target { .. }))
            {
                a.id
            } else {
                let mut a = p.annotation(
                    0.5,
                    0.5,
                    AnnotationLayer::Target {
                        rotation_degrees: 0.,
                        include_in_report: true,
                    },
                );
                a.radius = 0.42;
                let id = a.id;
                p.annotations.push(a);
                id
            };
            p.hidden.retain(|v| *v != id);
            self.selected = Some(id);
        }
        self.tool = Tool::Select;
        self.preset = None;
        self.persist();
    }
    fn remove_ids(&mut self, ids: Vec<Uuid>) {
        if self.readonly || ids.is_empty() {return;}
        self.checkpoint();
        if let Some(p)=self.photo_mut() {p.remove_annotations(&ids);}
        self.selected=None; self.checked.clear(); self.bulk_mode=false;
        self.cancel_marker_pick();
        self.persist();
    }
    fn remove(&mut self) {
        if let Some(id)=self.selected {self.remove_ids(vec![id]);}
    }
    fn palette_block_reason(&self) -> Option<&'static str> {
        let ix=PRESETS.iter().position(|p| (p.region != m53_domain::protocol::Region::Cervix)==self.palette_vulva)?;
        if self.allows_marker(ix) {return None;}
        let cervix=self.photo().is_some_and(|p| m53_client::defects::allows_marker(p.annotations.iter(),0));
        let vulva_ix=PRESETS.iter().position(|p| p.region==m53_domain::protocol::Region::Vulva).unwrap();
        let vulva=self.allows_marker(vulva_ix);
        Some(if !cervix && !vulva {
            "Недоступно: на снимке уже смешаны метки шейки матки и вульвы. Удалите метки одной области или выберите другой снимок."
        } else if self.palette_vulva {
            "Недоступно: на этом снимке идет исследование шейки матки. Для исследования вульвы выберите отдельный снимок."
        } else {
            "Недоступно: на этом снимке идет исследование вульвы. Для исследования шейки матки выберите отдельный снимок."
        })
    }
    fn canvas(&self, cx: &Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let menu_editor = entity.clone();
        let mut photo = self.photo().cloned();
        if let (Some(photo), Some(preset), Some((x, y))) = (&mut photo, self.preset, self.pointer) {
            let (cx, cy, tx, ty) = match self.pending_tip {
                Some((tx, ty)) => (x, y, tx, ty),
                None => ((x + 0.12).min(0.97), (y + 0.10).min(0.97), x, y),
            };
            let mut a = photo.annotation(
                cx,
                cy,
                AnnotationLayer::Callout {
                    end_x: tx,
                    end_y: ty,
                    full_name: PRESETS[preset].name.into(),
                    term_code: PRESETS[preset].code.map(str::to_owned),
                },
            );
            a.label = PRESETS[preset].short.into();
            photo.annotations.push(a);
        }
        let texture = photo.as_ref().and_then(|p| self.images.get(&p.id)).cloned();
        let zoom = self.zoom;
        let pan = self.pan;
        let selected = self.selected;
        let crop_pointer = (self.tool == Tool::Crop).then_some(self.pointer).flatten();
        div()
            .id("image-canvas")
            .aria_label("Снимок. Выберите обозначение или область отчёта, перемещайте указатель стрелками, Shift для точного шага, Enter устанавливает точки, Escape отменяет. Выбранный элемент можно двигать стрелками.")
            .track_focus(&self.focus_handle)
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .cursor(if self.readonly && self.tool != Tool::Hand {CursorStyle::Arrow} else if self.preset.is_some() || self.tool==Tool::Crop {CursorStyle::Crosshair} else {self.hover_cursor})
            .on_mouse_down(MouseButton::Left, cx.listener(Self::down))
            .on_key_down(cx.listener(|s, event: &KeyDownEvent, window, cx| {
                if keyboard_crop_allowed(s.ready, s.readonly, s.focus_handle.is_focused(window)) && !s.live_active
                    && (s.gesture.is_none() || matches!(s.gesture,Some(Gesture::Crop(_))))
                    && !event.keystroke.modifiers.control && !event.keystroke.modifiers.alt && !event.keystroke.modifiers.platform {
                    let key = event.keystroke.key.as_str();
                    if let Some((dx,dy)) = keyboard_delta(key, event.keystroke.modifiers.shift) {
                        if s.tool == Tool::Crop {
                            let (x,y)=s.pointer.unwrap_or((0.5,0.5));
                            let next=((x+dx).clamp(0.,1.),(y+dy).clamp(0.,1.));
                            s.pointer=Some(next);
                            if let Some(Gesture::Crop(start))=s.gesture {
                                let before=s.before_gesture.as_ref().and_then(|v|v.photos.get(v.selected)).cloned();
                                if let Some(photo)=s.photo_mut() && let Some(before)=before.as_ref() {s.selected=update_crop(photo,start,next,before);}
                            }
                            cx.notify();cx.stop_propagation();return;
                        }
                        if s.preset.is_some() {
                            let (x,y) = s.pointer.unwrap_or((0.5,0.5));
                            s.pointer = Some(((x+dx).clamp(0.,1.),(y+dy).clamp(0.,1.)));
                            s.message = "Стрелки перемещают указатель, Shift — точнее, Enter — установить точку, Esc — отмена".into();
                            cx.notify(); cx.stop_propagation(); return;
                        }
                        if let Some(id)=s.selected && s.photo().is_some_and(|p|p.annotations.iter().any(|a|a.id==id && !p.hidden.contains(&id))) {
                            let changed = s.photo().and_then(|p|p.annotations.iter().find(|a|a.id==id)).is_some_and(|a| {
                                let (max_x,max_y)=match &a.layer {AnnotationLayer::OutputRegion {width,height}=>(1.-*width,1.-*height),_=>(1.,1.)};
                                (a.x+dx).clamp(0.,max_x.max(0.)) != a.x || (a.y+dy).clamp(0.,max_y.max(0.)) != a.y
                            });
                            if !changed { cx.stop_propagation(); return; }
                            s.checkpoint();
                            if let Some(photo)=s.photo_mut() && let Some(a)=photo.annotations.iter_mut().find(|a|a.id==id) {
                                let (max_x,max_y)=match &a.layer {AnnotationLayer::OutputRegion {width,height}=>(1.-*width,1.-*height),_=>(1.,1.)};
                                a.x=(a.x+dx).clamp(0.,max_x.max(0.));a.y=(a.y+dy).clamp(0.,max_y.max(0.));
                            }
                            s.persist();cx.notify();cx.stop_propagation();return;
                        }
                    }
                    if key == "enter" && s.tool==Tool::Crop && s.photo().is_some_and(|p|s.images.contains_key(&p.id)) {
                        if matches!(s.gesture,Some(Gesture::Crop(_))) {s.finish(cx);s.pointer=None;}
                        else {s.before_gesture=Some(s.session.clone());s.gesture=Some(Gesture::Crop(s.pointer.unwrap_or((0.5,0.5))));}
                        cx.notify();cx.stop_propagation();return;
                    }
                    if key == "enter" && let Some(preset)=s.preset && let Some(rect)=s.image_rect() {
                        let (x,y)=s.pointer.unwrap_or((0.5,0.5));
                        s.stamp(preset,point(px(rect.x+x*rect.w),px(rect.y+y*rect.h)));
                        cx.notify();cx.stop_propagation();return;
                    }
                }
                if event.keystroke.key == "delete" && s.focus_handle.is_focused(window) && s.gesture.is_none() && s.preset.is_none() {s.remove(); cx.notify();}
                if event.keystroke.key == "escape" {
                    s.cancel_gesture();
                    s.dismissed_properties = s.selected;
                    s.palette_open=false;
                    s.retag=None;
                    s.preset = None;
                    s.pending_tip = None;
                    s.pointer = None;
                    s.message.clear();
                    cx.notify();
                }
            }))
            .on_mouse_move(cx.listener(Self::motion))
            .on_hover(cx.listener(|s, hovered: &bool, _, cx| {
                if !*hovered && s.preset.is_some() {
                    s.pointer = None;
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|s, _, _, cx| s.finish(cx)))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|s, _, _, cx| s.finish(cx)))
            .on_prepaint(move |bounds, _, cx| {
                entity.update(cx, |s, cx| {
                    let next = Rect {
                        x: bounds.origin.x.as_f32() + 24.,
                        y: bounds.origin.y.as_f32() + 24.,
                        w: (bounds.size.width.as_f32() - 48.).max(1.),
                        h: (bounds.size.height.as_f32() - 48.).max(1.),
                    };
                    if s.bounds != next { s.bounds = next; if s.selected.is_some() { cx.notify(); } }
                });
            })
            .child(
                canvas(
                    move |bounds, _, _| bounds,
                    move |_, bounds, window, cx| {
                        if let (Some(photo), Some(texture)) = (photo, texture) {
                            let view = Rect {
                                x: bounds.origin.x.as_f32() + 24.,
                                y: bounds.origin.y.as_f32() + 24.,
                                w: (bounds.size.width.as_f32() - 48.).max(1.),
                                h: (bounds.size.height.as_f32() - 48.).max(1.),
                            };
                            let r = Rect::fit(view, photo.width, photo.height, zoom, pan);
                            let image_bounds =
                                Bounds::new(point(px(r.x), px(r.y)), size(px(r.w), px(r.h)));
                            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                                let _ = window.paint_image(
                                    bounds,
                                    image_bounds,
                                    Corners::default(),
                                    texture,
                                    0,
                                    false,
                                );
                                vector::paint(&photo, r, selected, window, cx);
                                if let Some((x,y))=crop_pointer {
                                    let center=point(px(r.x+x*r.w),px(r.y+y*r.h));
                                    window.paint_quad(fill(Bounds::new(point(center.x-px(8.),center.y-px(1.)),size(px(16.),px(2.))),rgb(0x1686d9)));
                                    window.paint_quad(fill(Bounds::new(point(center.x-px(1.),center.y-px(8.)),size(px(2.),px(16.))),rgb(0x1686d9)));
                                }
                            });
                        }
                    },
                )
                .size_full(),
            )
            .children(self.marker_properties(cx))
            .when(self.photo().is_some_and(|p|!self.images.contains_key(&p.id)),|d|d.child(div().absolute().inset_0().flex().items_center().justify_center().text_color(cx.theme().muted_foreground).child("Загрузка снимка…")))
            .when(self.photo().is_none(), |d| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .v_flex()
                        .items_center()
                        .justify_center()
                        .gap_3()
                        .text_color(cx.theme().muted_foreground)
                        .child("Добавьте снимки для разметки")
                        .child(
                            Button::new("empty-import")
                                .label("Выбрать снимки")
                                .icon(IconName::FolderOpen)
                                .disabled(!self.ready)
                                .on_click(cx.listener(|s, _, w, cx| s.import(w, cx))),
                        ),
                )
            })
            .test_support()
            .context_menu(move |menu, window, cx| {
                let (area, can_use, mixed) = menu_editor.update(cx, |s, cx| {
                    let area = s.marker_area();
                    let can_use = s.ready && !s.readonly && s.photo().is_some_and(|p| s.images.contains_key(&p.id));
                    let mixed = !s.allows_marker(0) && !s.allows_marker(Self::vulva_preset());
                    s.cancel_marker_pick();
                    cx.notify();
                    (area, can_use, mixed)
                });
                if !can_use { return menu.label("Разметка доступна в рабочем черновике со снимком"); }
                if mixed { return menu.label("Удалите метки одной области: шейка и вульва смешаны"); }
                if let Some(vulva) = area { return Self::marker_menu(menu, menu_editor.clone(), vulva, window, cx); }
                let cervix = menu_editor.clone(); let vulva = menu_editor.clone();
                menu.submenu("Шейка", window, cx, move |menu, window, cx| Self::marker_menu(menu, cervix.clone(), false, window, cx))
                    .submenu("Вульва", window, cx, move |menu, window, cx| Self::marker_menu(menu, vulva.clone(), true, window, cx))
            })
    }
    fn marker_properties(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if self.gesture.is_some() || self.bulk_mode || self.palette_open || self.preset.is_some() { return None; }
        let selected = self.selected.filter(|id| self.dismissed_properties != Some(*id))?;
        let photo = self.photo()?;
        let marker = photo.annotations.iter().find(|a| a.id == selected && !photo.hidden.contains(&a.id))?;
        let AnnotationLayer::Callout { end_x, end_y, full_name, term_code } = &marker.layer else { return None; };
        let form = self.properties.as_ref()?;
        // The shared form and its mark selection remain authoritative for all property edits.
        if form.read(cx).focus_mark != Some(selected) { return None; }
        let image = self.image_rect()?;
        let origin = (self.bounds.x - 24., self.bounds.y - 24.);
        let radius = m53_client::defects::CALLOUT_RADIUS * image.w.min(image.h) + 6.;
        let center = (image.x + marker.x * image.w - origin.0, image.y + marker.y * image.h - origin.1);
        let tip = (image.x + *end_x * image.w - origin.0, image.y + *end_y * image.h - origin.1);
        let badge = marker_card::BoxRect::new(center.0 - radius, center.1 - radius, radius * 2., radius * 2.);
        let catalog_id = form.read(cx).value["colpo"]["marks"][selected.to_string()]["catalog_id"].as_str()?.to_string();
        let desired_height = marker_card::content_height(m53_domain::colpo::FIELDS.iter().filter(|field| field.parent == catalog_id).map(|field| {
            use m53_domain::colpo::Control;
            match field.control { Control::Text => 104., Control::Choice => 60., Control::Check => 28., Control::Date => 52., Control::Unknown => 48. }
        })) + if self.readonly { 24. } else { 0. };
        let placement = marker_card::place_arrow(
            marker_card::BoxRect::new(0., 0., self.bounds.w + 48., self.bounds.h + 48.),
            marker_card::BoxRect::new(image.x - origin.0, image.y - origin.1, image.w, image.h),
            badge, tip, 310., desired_height,
        )?;
        let color = crate::appearance::marker_text(m53_domain::colpo::marker_color(&marker.label, full_name, term_code.as_deref()), cx);
        Some(v_flex().id("selected-marker-properties").absolute().left(px(placement.x)).top(px(placement.y))
            .w(px(placement.w)).h(px(placement.h)).min_h_0().rounded_md().border_1().border_color(cx.theme().border)
            .bg(cx.theme().background).shadow_md().cursor_default().occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_move(|_, _, cx| cx.stop_propagation())
            .capture_key_down(cx.listener(|s, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    s.dismissed_properties = s.selected;
                    window.focus(&s.focus_handle, cx);
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(h_flex().flex_shrink_0().gap_2().items_start().px_2().py_1().border_b_1().border_color(cx.theme().border)
                .child(div().font_semibold().text_color(color).child(marker.label.clone()))
                .child(div().flex_1().min_w_0().text_sm().child(full_name.clone()))
                .child(Button::new("close-marker-properties").ghost().small().icon(IconName::X)
                    .accessibility_label("Закрыть свойства метки").tooltip("Закрыть свойства метки · Esc")
                    .on_click(cx.listener(|s, _, window, cx| { s.dismissed_properties = s.selected; window.focus(&s.focus_handle, cx); cx.notify(); }))))
            .when(self.readonly, |card| card.child(div().flex_shrink_0().px_2().pt_1().text_xs().text_color(cx.theme().muted_foreground).child("Только просмотр")))
            .child(div().id(SharedString::from(format!("marker-property-scroll-{selected}"))).flex_1().min_h_0().overflow_y_scroll().child(div().p_2().pb_4().child(form.clone())))
            .test_support().into_any_element())
    }
    fn tools(&self, cx: &Context<Self>) -> impl IntoElement {
        let mut tools = v_flex()
            .w(px(56.))
            .flex_shrink_0()
            .items_center()
            .py_3()
            .gap_2()
            .border_r_1()
            .border_color(cx.theme().border);
        for (id, tool, icon, label) in [
            (
                "select",
                Tool::Select,
                IconName::MousePointer2,
                "Выбрать и переместить",
            ),
            ("hand", Tool::Hand, IconName::Hand, "Перемещать снимок"),
            ("crop", Tool::Crop, IconName::Crop, "Выбрать область отчета"),
        ] {
            tools = tools.child(
                Button::new(id)
                    .ghost()
                    .icon(icon)
                    .accessibility_label(label)
                    .tooltip(label)
                    .disabled(self.readonly && tool == Tool::Crop)
                    .selected(self.tool == tool && self.preset.is_none())
                    .on_click(cx.listener(move |s, _, window, cx| {
                        s.tool = tool;
                        s.preset = None;
                        s.pending_tip = None;
                        s.pointer = (tool==Tool::Crop).then_some((0.5,0.5));
                        if tool==Tool::Crop {s.message="Стрелки перемещают указатель, Shift — точнее, Enter — углы области, Esc — отмена".into();window.focus(&s.focus_handle,cx);}
                        cx.notify();
                    })),
            );
        }
        tools=tools            .child(div().w(px(40.)).border_t_1().border_color(cx.theme().border).my_1())
            .child(
                Button::new("target")
                    .ghost()
                    .icon(IconName::Crosshair)
                    .accessibility_label("Мишень")
                    .tooltip("Мишень")
                    .disabled(self.readonly||self.live_active||self.photo().is_none())
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.target();
                        cx.notify();
                    })),
            );
        tools=tools.child(div().w(px(40.)).border_t_1().border_color(cx.theme().border).my_1());
        for (vulva,label) in [(false,"Шейка"),(true,"Вульва")] {
            tools=tools.child(Button::new(if vulva {"tool-vulva"} else {"tool-cervix"}).ghost().small().label(label)
                .tooltip(format!("Обозначения: {label}"))
                .selected(self.palette_open && self.palette_vulva==vulva)
                .disabled(self.readonly || self.photo().is_none())
                .on_click(cx.listener(move |s,_,_,cx| {
                    s.palette_open=!s.palette_open || s.palette_vulva!=vulva;
                    s.palette_vulva=vulva;s.retag=None;s.preset=None;s.pending_tip=None;s.pointer=None;cx.notify();
                })));
        }
        tools
            .child(div().w(px(40.)).border_t_1().border_color(cx.theme().border).my_1())
            .child(
                Button::new("fit")
                    .ghost()
                    .icon(IconName::Maximize)
                    .accessibility_label("Вписать снимок")
                    .tooltip("Вписать снимок")
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.fit();
                        cx.notify();
                    })),
            )
            .child(
                Button::new("zoom-in")
                    .ghost()
                    .icon(IconName::Plus)
                    .accessibility_label("Увеличить снимок")
                    .tooltip("Увеличить")
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.audit_action("image.zoom_changed");
                        s.zoom = (s.zoom * 1.25).min(8.);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("zoom-out")
                    .ghost()
                    .icon(IconName::Minus)
                    .accessibility_label("Уменьшить снимок")
                    .tooltip("Уменьшить")
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.audit_action("image.zoom_changed");
                        s.zoom = (s.zoom / 1.25).max(0.25);
                        cx.notify();
                    })),
            )

    }
    fn layers(&self, cx: &Context<Self>) -> impl IntoElement {
        let mut layers = v_flex().gap_1();
        if let Some(photo) = self.photo() {
            for a in photo.annotations.iter().rev().filter(|a| self.bulk_mode || self.selected.is_none_or(|id| id == a.id)) {
                let id = a.id;
                let visible = !photo.hidden.contains(&id);
                let label = match &a.layer {
                    AnnotationLayer::Callout { full_name, .. } => {
                        format!("{} · {}", a.label, full_name)
                    }
                    AnnotationLayer::Target { .. } => "Мишень".into(),
                    AnnotationLayer::OutputRegion { .. } => "Область отчета".into(),
                    _ => format!("Старая метка · {}", a.label),
                };
                layers = layers.child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .when(self.bulk_mode, |row| row.child(Checkbox::new(SharedString::from(format!("check-{id}")))
                            .checked(self.checked.contains(&id)).disabled(self.readonly)
                            .on_click(cx.listener(move |s,value:&bool,_,cx| {
                                if *value {if !s.checked.contains(&id) {s.checked.push(id);}} else {s.checked.retain(|v| *v!=id);}
                                cx.notify();
                            }))))
                        .child(
                            Button::new(SharedString::from(format!("visibility-{id}")))
                                .ghost()
                                .small()
                                .accessibility_label(if visible { "Скрыть элемент на экране" } else { "Показать элемент на экране" })
                                .icon(if visible {
                                    IconName::Eye
                                } else {
                                    IconName::EyeOff
                                })
                                .tooltip(if visible {
                                    "Скрыть на экране"
                                } else {
                                    "Показать на экране"
                                })
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    s.checkpoint();
                                    if let Some(p) = s.photo_mut() {
                                        if p.hidden.contains(&id) {
                                            p.hidden.retain(|v| *v != id);
                                        } else {
                                            p.hidden.push(id);
                                        }
                                    }
                                    s.persist();
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new(SharedString::from(format!("layer-{id}")))
                                .ghost()
                                .small()
                                .flex_1()
                                .min_w_0()
                                .justify_start()
                                .icon(if self.selected == Some(id) {IconName::ChevronDown} else {IconName::ChevronRight})
                                .label(label.clone())
                                .tooltip(label)
                                .selected(self.selected == Some(id))
                                .on_click(cx.listener(move |s, _, window, cx| {
                                    window.focus(&s.focus_handle,cx);
                                    if s.bulk_mode {
                                        if s.checked.contains(&id) {s.checked.retain(|v| *v!=id);} else {s.checked.push(id);}
                                        cx.notify();return;
                                    }
                                    s.selected = if s.selected==Some(id) {None} else {Some(id)};
                                    s.tool = Tool::Select;
                                    s.preset = None;
                                    s.pending_tip = None;
                                    s.pointer = None;
                                    cx.notify();
                                })),
                        )
                        .when(self.selected == Some(id) && !self.bulk_mode, |row| row.child(
                            Button::new(SharedString::from(format!("delete-{id}"))).ghost().small()
                                .accessibility_label("Удалить этот элемент")
                                .icon(IconName::Trash).tooltip("Удалить этот элемент").disabled(self.readonly)
                                .on_click(cx.listener(move |s,_,_,cx| {s.remove_ids(vec![id]);cx.notify();})))),
                );
            }
        }
        v_flex()
            .w(px(280.))
            .flex_shrink_0()
            .border_l_1()
            .border_color(cx.theme().border)
            .p_3()
            .gap_2()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(crate::ui_chrome::panel_heading("Данные обследования", cx).flex_1())
                    .child(h_flex().gap_0p5().flex_shrink_0()
                        .child(Button::new("bulk-toggle").ghost().small()
                            .accessibility_label(if self.bulk_mode {"Отменить выбор элементов"} else {"Выбрать несколько элементов"})
                            .icon(if self.bulk_mode {IconName::X} else {IconName::ListChecks})
                            .tooltip(if self.bulk_mode {"Отменить выбор"} else {"Выбрать несколько элементов"})
                            .selected(self.bulk_mode)
                            .disabled(self.readonly || self.photo().is_none_or(|p| p.annotations.is_empty()))
                            .on_click(cx.listener(|s,_,_,cx| {s.bulk_mode=!s.bulk_mode;s.selected=None;s.checked.clear();cx.notify();})))
                        .when(self.bulk_mode, |row| row.child(Button::new("bulk-delete").ghost().small()
                            .accessibility_label(format!("Удалить выбранные элементы: {}", self.checked.len()))
                            .icon(IconName::Trash).tooltip(format!("Удалить выбранные элементы ({})",self.checked.len()))
                            .disabled(self.checked.is_empty() || self.readonly)
                            .on_click(cx.listener(|s,_,_,cx| {s.remove_ids(s.checked.clone());cx.notify();}))))
                        .child(Button::new("clear-annotations").ghost().small().icon(IconName::Eraser)
                            .accessibility_label("Очистить всю разметку текущего снимка")
                            .tooltip("Очистить всю разметку текущего снимка. Можно отменить.")
                            .disabled(self.readonly || self.photo().is_none_or(|p| p.annotations.is_empty()))
                            .on_click(cx.listener(|s,_,_,cx| {
                                let ids=s.photo().map(|p| p.annotations.iter().map(|a| a.id).collect()).unwrap_or_default();
                                s.remove_ids(ids);cx.notify();
                            })))),
            )
            .child(
                div()
                    .id("layer-scroll")
                    .flex_1().min_h_0()
                    .overflow_y_scroll()
                    .child(layers),
            )

    }
    fn palette(&self, cx: &Context<Self>) -> impl IntoElement {
        let width=(self.bounds.w+32.).min(920.).max(480.);
        let count=if width>=760. {3} else {2};
        let mut columns:Vec<_>=(0..count).map(|_| v_flex().flex_1().min_w_0().gap_2()).collect();
        let mut heights=vec![0usize;count];
        let search=self.palette_search.read(cx).value().to_lowercase();
        let mut groups:Vec<_>=m53_domain::colpo::MARKER_GROUPS.iter().enumerate().collect();
        groups.sort_by_key(|(g,_)| std::cmp::Reverse(PRESETS.iter().filter(|p| (p.region != m53_domain::protocol::Region::Cervix)==self.palette_vulva && p.marker_group()==*g).count()));
        for (group,(title,explanation,color)) in groups {
            let presets:Vec<_>=PRESETS.iter().enumerate().filter(|(_,p)|
                (p.region != m53_domain::protocol::Region::Cervix)==self.palette_vulva && p.marker_group()==group &&
                format!("{} {}",p.short,p.name).to_lowercase().contains(&search)).collect();
            if presets.is_empty() {continue;}
            let column=heights.iter().enumerate().min_by_key(|(_,height)| **height).unwrap().0;
            heights[column]+=presets.len()+2;
            let block=v_flex().gap_0p5()
                .child(div().border_b_1().border_color(cx.theme().border).pb_1().text_xs().font_semibold()
                    .text_color(crate::appearance::marker_text(*color,cx)).child(*title))
                .children(presets.iter().map(|(ix,_)| self.preset_button(*ix,cx)));
            let _=explanation;
            let old=std::mem::replace(&mut columns[column],v_flex());
            columns[column]=old.child(block);
        }
        v_flex().w(px(width)).max_h(px(self.bounds.h+48.)).p_1().gap_1()
            .bg(cx.theme().background).border_1().border_color(cx.theme().border)
            .child(h_flex().items_center().justify_between()
                .child(div().font_semibold().child(if self.palette_vulva {"Обозначения вульвы"} else {"Обозначения шейки матки"}))
                .child(Button::new("close-palette").ghost().small().icon(IconName::X).accessibility_label("Закрыть каталог обозначений").tooltip("Закрыть")
                    .on_click(cx.listener(|s,_,w,cx| {s.palette_open=false;s.retag=None;w.focus(&s.focus_handle,cx);cx.notify();}))))
            .when_some(self.palette_block_reason(), |d,reason| d.child(div().p_2().bg(cx.theme().secondary)
                .border_1().border_color(cx.theme().border).font_semibold().child(reason)))
            .child(Input::new(&self.palette_search).small().disabled(self.palette_block_reason().is_some()))
            .child(h_flex().items_start().gap_2().when(self.palette_block_reason().is_some(),|d| d.opacity(0.35)).children(columns))

    }
    fn vulva_preset() -> usize {
        PRESETS.iter().position(|p| p.region != m53_domain::protocol::Region::Cervix).unwrap()
    }
    fn marker_area(&self) -> Option<bool> {
        match (self.allows_marker(0), self.allows_marker(Self::vulva_preset())) {
            (true, false) => Some(false),
            (false, true) => Some(true),
            (true, true) => self.preset.map(|ix| PRESETS[ix].region != m53_domain::protocol::Region::Cervix)
                .or_else(|| self.palette_open.then_some(self.palette_vulva)),
            _ => None,
        }
    }
    fn cancel_marker_pick(&mut self) {
        self.preset = None; self.pending_tip = None; self.pointer = None;
        self.retag = None; self.palette_open = false;
    }
    fn choose_preset(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ready || self.readonly || !self.allows_marker(ix) { return; }
        self.palette_open = false;
        self.palette_vulva = PRESETS[ix].region != m53_domain::protocol::Region::Cervix;
        if let Some(id) = self.retag.take() {
            self.checkpoint();
            if let Some(a) = self.photo_mut().and_then(|p| p.annotations.iter_mut().find(|a| a.id == id))
                && let AnnotationLayer::Callout { full_name, term_code, .. } = &mut a.layer {
                a.label = PRESETS[ix].short.into(); *full_name = PRESETS[ix].name.into();
                *term_code = PRESETS[ix].code.map(str::to_owned);
            }
            if matches!(ix, 6 | 11) && let Some(p) = self.photo_mut() {
                p.annotations.retain(|a| a.id == id || !matches!(&a.layer, AnnotationLayer::Callout {full_name,..} if full_name == PRESETS[6].name || full_name == PRESETS[11].name));
            }
            self.persist();
        } else {
            self.preset = Some(ix); self.pending_tip = None; self.pointer = Some((0.5, 0.5));
            self.message = "Стрелки перемещают указатель, Shift — точнее, Enter — установить точку, Esc — отмена".into();
            self.selected = None; self.tool = Tool::Select;
        }
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }
    fn marker_menu(mut menu: PopupMenu, editor: Entity<Self>, vulva: bool, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        menu = menu.label(if vulva { "Обозначения вульвы" } else { "Обозначения шейки матки" });
        for (group, (title, _, color)) in m53_domain::colpo::MARKER_GROUPS.iter().enumerate() {
            let presets: Vec<_> = PRESETS.iter().enumerate().filter(|(_, p)|
                (p.region != m53_domain::protocol::Region::Cervix) == vulva && p.marker_group() == group).map(|(ix, _)| ix).collect();
            if presets.is_empty() { continue; }
            let group_editor = editor.clone();
            let icon = gpui_kit::component::Icon::new(IconName::Circle).data(br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><circle cx="12" cy="12" r="8" fill="currentColor"/></svg>"#).text_color(crate::appearance::marker_text(*color, cx));
            menu = menu.submenu_with_icon(Some(icon), format!("{title} ({})", presets.len()), window, cx, move |mut group_menu, _, cx| {
                for &ix in &presets {
                    let preset = &PRESETS[ix];
                    let entity = group_editor.clone();
                    group_menu = group_menu.item(PopupMenuItem::element(move |_, cx| {
                        let preset = &PRESETS[ix];
                        h_flex().id(("marker-menu-item", ix)).w_full().min_w_0().gap_2().items_center()
                            .aria_label(format!("{} — {}", preset.short, preset.name))
                            .child(div().w(px(64.)).flex_shrink_0().whitespace_nowrap().font_semibold().text_color(crate::appearance::marker_text(preset.marker_color(), cx)).child(preset.short))
                            .child(div().flex_1().min_w_0().truncate().text_sm().text_color(cx.theme().foreground).child(preset.name))
                            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(format!("{} — {}", preset.short, preset.name)).build(window, cx)).test_support()
                    })
                        .icon(gpui_kit::component::Icon::new(IconName::Circle).data(br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><circle cx="12" cy="12" r="8" fill="currentColor"/></svg>"#).text_color(crate::appearance::marker_text(preset.marker_color(), cx)))
                        .disabled(!group_editor.read(cx).allows_marker(ix))
                        .on_click(move |_, window, cx| entity.update(cx, |s, cx| s.choose_preset(ix, window, cx))));
                }
                group_menu.min_w(px(300.)).max_w(px(420.)).max_h(px(360.)).scrollable(true)
            });
        }
        menu.min_w(px(250.)).max_w(px(340.)).max_h(px(360.)).scrollable(true)
    }
    fn preset_button(&self, ix: usize, cx: &Context<Self>) -> impl IntoElement {
        let p = &PRESETS[ix];
        Button::new(("preset", ix)).outline().small().w_full().justify_start()
            .text_color(crate::appearance::marker_text(p.marker_color(), cx))
            .h(px(22.)).py_0()
            .child(h_flex().w_full().items_center().gap_1()
                .child(div().w(px(48.)).flex_shrink_0().font_semibold().text_color(crate::appearance::marker_text(p.marker_color(), cx)).child(p.short))
                .child(div().flex_1().min_w_0().truncate().text_xs().text_color(cx.theme().foreground).child(p.name)))
            .tooltip(if self.allows_marker(ix) {format!("{} — {}. Первый клик: острие; второй: центр кружка.", p.short, p.name)} else {format!("{} — {}. На снимке уже есть обозначения другой области; используйте отдельный снимок.",p.short,p.name)})
            .selected(self.preset == Some(ix))
            .disabled(!self.ready || self.readonly || !self.allows_marker(ix))
            .on_click(cx.listener(move |s, _, window, cx| s.choose_preset(ix, window, cx)))
    }
    fn remove_photos(&mut self,ids:Vec<Uuid>,cx:&mut Context<Self>){
        if self.readonly||!self.ready||self.io_pending>0||!self.session.photos.iter().any(|p|!p.removed&&ids.contains(&p.id)){return;}
        self.checkpoint();self.session.remove_photos(&ids);self.photo_checked.clear();self.photo_selecting=false;self.live_active=false;self.selected=None;self.cancel_marker_pick();self.fit();self.persist();cx.notify();
    }
    fn filmstrip(&self,cx:&Context<Self>)->impl IntoElement {
        let can_remove=self.ready&&!self.readonly&&self.io_pending==0;
        let mut thumbs=h_flex().gap_3().items_start();let mut display=0;
        for (i,p) in self.session.photos.iter().enumerate().filter(|(_,p)|!p.removed){
            display+=1;let id=p.id;
            let tile=div().id(("thumbnail",i)).relative().w(px(128.)).h(px(80.)).flex_shrink_0().rounded_md().overflow_hidden().border_2()
                .border_color(if self.session.selected==i{cx.theme().primary}else{cx.theme().border}).cursor_pointer()
                .when_some(self.thumbnails.get(&id).cloned(),|d,image|d.child(img(image).absolute().inset_0().size_full().object_fit(ObjectFit::Contain)))
                .on_click(cx.listener(move|s,_,_,cx|{if s.photo_selecting {if s.photo_checked.contains(&id){s.photo_checked.retain(|p|*p!=id);}else{s.photo_checked.push(id);}cx.notify();return;}s.cancel_marker_pick();s.session.selected=i;s.live_active=false;s.fit();s.persist();cx.notify();}))
                .when(!self.photo_selecting,|d|d.child(h_flex().absolute().right_0().top_0().bg(cx.theme().background)
                    .child(self.export_menu(Button::new(("download-photo",i)).ghost().small().icon(IconName::Download)
                        .accessibility_label(format!("Сохранить снимок {display}")).tooltip(format!("Сохранить снимок {display}")), Some(id), cx))
                    .child(Button::new(("remove-photo",i)).ghost().small().icon(IconName::Trash).text_color(cx.theme().danger).accessibility_label("Удалить снимок из обследования").tooltip("Удалить снимок из обследования").disabled(!can_remove).on_click(cx.listener(move|s,_,_,cx|{cx.stop_propagation();s.remove_photos(vec![id],cx);})))) )
                .when(self.photo_selecting,|d|d.child(div().absolute().left_0().top_0().p_1().bg(cx.theme().background).child(Checkbox::new(("select-photo",i)).checked(self.photo_checked.contains(&id)).on_click(cx.listener(move|s,checked,_,cx|{cx.stop_propagation();if *checked{if !s.photo_checked.contains(&id){s.photo_checked.push(id);}}else{s.photo_checked.retain(|p|*p!=id);}cx.notify();})))));
            thumbs=thumbs.child(v_flex().w(px(128.)).flex_shrink_0().gap_1().child(tile).child(Checkbox::new(("in-report",i)).small().label(format!("Снимок {display}")).checked(p.in_report).disabled(self.readonly || self.io_pending>0).on_click(cx.listener(move|s,checked,_,cx|{s.checkpoint();if let Some(photo)=s.session.photos.get_mut(i){photo.in_report=*checked;}s.persist();cx.notify();}))));
        }
        v_flex().h(px(164.)).min_w_0().flex_shrink_0().border_t_1().border_color(cx.theme().border).px_4().py_2().gap_2()
            .child(h_flex().h(px(28.)).items_center().gap_2().text_sm()
                .child(div().text_color(cx.theme().muted_foreground).child(format!("Снимки · {}",self.session.visible_count())))
                .when(self.photo_selecting,|d|d.child(div().child(format!("Выбрано: {}",self.photo_checked.len()))))
                .child(div().flex_1())
                .when(!self.photo_selecting, |d| d.child(self.export_menu(Button::new("download-all-photos").ghost().small()
                    .icon(IconName::Download).label("Сохранить все"), None, cx)))
                .when(self.photo_selecting,|d|d
                    .child(Button::new("photos-select-all").ghost().small().label("Выбрать все").disabled(!can_remove).on_click(cx.listener(|s,_,_,cx|{s.photo_checked=s.session.photos.iter().filter(|p|!p.removed).map(|p|p.id).collect();cx.notify();})))
                    .child(Button::new("photos-remove-selected").ghost().small().icon(IconName::Trash).text_color(cx.theme().danger).accessibility_label(format!("Удалить выбранные снимки: {}",self.photo_checked.len())).tooltip(format!("Удалить выбранные снимки: {}",self.photo_checked.len())).disabled(!can_remove||self.photo_checked.is_empty()).on_click(cx.listener(|s,_,_,cx|s.remove_photos(s.photo_checked.clone(),cx)))))
                .child(Button::new("photos-select-mode").ghost().small().icon(if self.photo_selecting{IconName::X}else{IconName::ListChecks}).accessibility_label(if self.photo_selecting{"Отменить выделение снимков"}else{"Выделить несколько снимков"}).tooltip(if self.photo_selecting{"Отменить выделение"}else{"Выделить несколько снимков"}).disabled(!can_remove||self.session.visible_count()==0).on_click(cx.listener(|s,_,_,cx|{s.photo_selecting=!s.photo_selecting;s.photo_checked.clear();cx.notify();})))
                .child(Button::new("photos-scroll-left").ghost().small().icon(IconName::ChevronLeft).accessibility_label("Прокрутить снимки влево").tooltip("Прокрутить снимки влево").disabled(self.session.visible_count()<2).on_click(cx.listener(|s,_,_,cx|{let p=s.filmstrip_scroll.offset();s.filmstrip_scroll.set_offset(point((p.x+px(300.)).min(px(0.)),p.y));cx.notify();})))
                .child(Button::new("photos-scroll-right").ghost().small().icon(IconName::ChevronRight).accessibility_label("Прокрутить снимки вправо").tooltip("Прокрутить снимки вправо").disabled(self.session.visible_count()<2).on_click(cx.listener(|s,_,_,cx|{let p=s.filmstrip_scroll.offset();s.filmstrip_scroll.set_offset(point(p.x-px(300.),p.y));cx.notify();}))))
            .child(div().id("filmstrip-scroll").relative().flex_1().min_h_0().min_w_0().w_full().pb_3().overflow_x_scroll().track_scroll(&self.filmstrip_scroll).horizontal_scrollbar(&self.filmstrip_scroll).on_prepaint({let entity=cx.entity();move |bounds,_,cx|{entity.update(cx,|e,_|e.filmstrip_width=bounds.size.width.as_f32());}}).child(thumbs))
    }

}
impl Render for Editor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.dismissed_properties.is_some() && self.dismissed_properties != self.selected { self.dismissed_properties = None; }
        if let Some(form) = &self.properties {
            let mark = self.selected.filter(|id| form.read(cx).value["colpo"]["marks"][id.to_string()].is_object());
            if !form.read(cx).compact_marker_card || form.read(cx).focus_mark != mark {
                form.update(cx, |form, cx| { form.compact_marker_card = true; form.focus_mark = mark; cx.notify(); });
            }
        }
        v_flex().size_full().min_h_0().bg(cx.theme().background).text_color(cx.theme().foreground).text_size(px(14.))
            .when(self.clinic.is_none(),|d|d.child(h_flex().h(px(56.)).flex_shrink_0().px_4().gap_3().items_center().border_b_1().border_color(cx.theme().border)
                .child(div().font_semibold().child(m53_client::APP_NAME))
                .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Редактор снимков · пилот GPUI"))
                .child(div().flex_1())
                .child(Button::new("settings").ghost().icon(IconName::Settings2).accessibility_label(if self.settings {"Закрыть настройки"} else {"Открыть настройки"}).tooltip("Настройки").on_click(cx.listener(|s,_,_,cx|{s.settings = !s.settings;cx.notify();})))))
            .when(self.settings,|d|d.child(h_flex().px_4().py_3().gap_3().border_b_1().border_color(cx.theme().border)
                .child("Тема и масштаб — системные. Проект хранится отдельно от основного клиента.").child(Button::new("close-settings").ghost().label("Закрыть").on_click(cx.listener(|s,_,_,cx|{s.settings=false;cx.notify();})))))
            .child(h_flex().h(px(44.)).flex_shrink_0().px_4().gap_2().items_center().border_b_1().border_color(cx.theme().border)
                .child(Button::new("import").outline().small().icon(IconName::FolderOpen).label("Открыть снимок").disabled(!self.ready||self.readonly).on_click(cx.listener(|s,_,w,cx|s.import(w,cx))))
                .child(Button::new("undo").ghost().small().icon(IconName::Undo2).accessibility_label("Отменить последнее действие").tooltip("Отменить последнее действие").disabled(self.readonly||self.live_active||self.gesture.is_some()||self.undo.is_empty()).on_click(cx.listener(|s,_,_,cx|s.undo_action(cx))))
                .child(div().text_sm().text_color(cx.theme().muted_foreground).child(if self.zoom==1.{"Вписан целиком".into()}else{format!("Масштаб ×{:.2}",self.zoom)}))
                .child(div().flex_1())
                .child(Button::new("whole-photo").ghost().small().label("Весь снимок в отчет").disabled(self.readonly||self.photo().is_none()||self.gesture.is_some()).on_click(cx.listener(|s,_,_,cx|s.whole_photo_action(cx))))
                .when_some(self.photo().map(|photo| photo.id), |d, id| d.child(self.export_menu(
                    Button::new("export").primary().small().icon(IconName::Download).label("Экспорт PNG"), Some(id), cx))))
            .child(h_flex().items_stretch().flex_1().min_h_0().when(!self.live_active,|d|d.child(self.tools(cx))).child(div().relative().flex_1().min_w_0().h_full().overflow_hidden().flex().when(!self.live_active,|d|d.child(self.canvas(cx))).when(self.live_active,|d|d.child(div().size_full().flex().items_center().justify_center().track_focus(&self.focus_handle).on_mouse_down(MouseButton::Left,cx.listener(|s,_,window,cx|s.focus_handle.focus(window,cx))).when_some(self.live_image.clone(),|d,image|d.child(img(image).absolute().inset_0().size_full().object_fit(ObjectFit::Contain))).when(self.live_image.is_none(),|d|d.child("Ожидание видеосигнала…")))).when(self.palette_open, |d| d.child(div().absolute().left_0().top_0().bottom_0().child(self.palette(cx))))).when(!self.live_active,|d|d.child(self.layers(cx))))
            .child(self.filmstrip(cx))
            .when(self.clinic.is_none(),|d|d.child(h_flex().h(px(30.)).flex_shrink_0().px_4().gap_2().items_center().border_t_1().border_color(cx.theme().border).text_xs()
                .text_color(if self.failure{cx.theme().danger}else{cx.theme().muted_foreground})
                .child(self.message.clone()).child(div().flex_1()).child("Локальный пилот · API не подключен")))
    }
}
fn main() {
    startup::install();
    match std::panic::catch_unwind(run) {
        Ok(Ok(())) => (),
        Ok(Err(error)) => {
            eprintln!("{error}");
            startup::failure();
            std::process::exit(1);
        }
        Err(_) => {
            startup::failure();
            std::process::exit(1);
        }
    }
}
fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    #[cfg(not(windows))]
    if args.get(1).is_some_and(|a| a == "--render-native") {
        return workplace::render_previews(args.get(2).map(String::as_str).unwrap_or("/tmp"));
    }
    #[cfg(not(windows))]
    if args.get(1).is_some_and(|a| a == "--smoke-api") {
        return clinic::smoke_api(
            args.get(2)
                .map(String::as_str)
                .ok_or_else(|| anyhow::anyhow!("Credentials file required"))?,
        );
    }
    if args.get(1).is_some_and(|a| a == "--render" || a == "--render-marker-card") {
        return render_preview(
            args.get(2).map(String::as_str).unwrap_or("gpui-editor.png"),
            args.get(3).is_some_and(|a| a == "light"),
            args.get(1).is_some_and(|a| a == "--render-marker-card"),
        );
    }
    if !args.iter().any(|a| a == "--pilot") {
        let result=workplace::launch();
        #[cfg(windows)] m53_client::report::chromium::shutdown();
        return result;
    }
    let store = storage::open_store().map_err(anyhow::Error::msg)?;
    let worker = storage::start(store);
    gpui_kit::application()
        .with_assets(AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            crate::appearance::sync_system(None, cx);
            cx.activate(true);
            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(format!("{} — редактор снимков", m53_client::APP_NAME).into()),
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
            let (handle, view) = gpui_kit::open_window(options, cx, move |window, cx| {
                cx.new(|cx| Editor::new(window, cx, Some(worker)))
            })
            .expect("Не удалось открыть GPUI");
            let weak = view.downgrade();
            let _ = handle.update(cx, |_, window, cx| {
                window.on_window_should_close(cx, move |_, cx| {
                    weak.update(cx, |s, cx| {
                        if s.gesture.is_some() {
                            s.finish(cx);
                        }
                        if s.saved < s.version || s.io_pending > 0 {
                            s.message = "Дождитесь сохранения на компьютере перед закрытием".into();
                            cx.notify();
                            false
                        } else {
                            true
                        }
                    })
                    .unwrap_or(true)
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
fn render_preview(path: &str, light: bool, marker_card_only: bool) -> anyhow::Result<()> {
    use gpui_kit::component::{Theme, ThemeMode};
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(AllAssets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(|cx| {
        gpui_kit::init(cx);
        Theme::change(
            if light {
                ThemeMode::Light
            } else {
                ThemeMode::Dark
            },
            None,
            cx,
        );
    });
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
            |w, cx| cx.new(|cx| Editor::new(w, cx, None)),
        )
    })?;
    cx.update(|cx| {
        Theme::change(
            if light {
                ThemeMode::Light
            } else {
                ThemeMode::Dark
            },
            None,
            cx,
        );
        crate::appearance::contrast(cx);
    });
    if std::env::var_os("M53_EDITOR_REGRESSION_ONLY").is_some() {
        return editor_regression_proof::verify(&mut cx, window, &view);
    }
    cx.update(|cx| {
        view.update(cx, |s, _| {
            let mut p = Photo::new(1000, 800);
            let mut pixels = image::RgbaImage::new(1000, 800);
            // Neutral synthetic calibration specimen: no patient photograph is bundled.
            for (x, y, pixel) in pixels.enumerate_pixels_mut() {
                let dx = x as f32 - 500.;
                let dy = y as f32 - 400.;
                let d = (dx * dx + dy * dy).sqrt();
                let v = if d < 280. { 215u8 } else { 242 };
                *pixel = image::Rgba([v, v, v, 255]);
            }
            for pixel in pixels.pixels_mut() {
                pixel.0.swap(0, 2);
            }
            s.images.insert(
                p.id,
                Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])),
            );
            p.add_callout(7, 0.22, 0.35);
            p.add_callout(1, 0.78, 0.62);
            s.session.photos.push(p);
            s.target();
            s.saved = s.version;
            s.message = "Синтетический снимок · настоящий рендер GPUI".into();
        })
    });
    cx.update_window(window, |_, w, cx| {
        use gpui_kit::test::TestWindowExt;
        w.render_frame(cx);
    })?;
    cx.capture_screenshot(window)?.save(path)?;
    if marker_card_only { return verify_marker_card_workflow(&mut cx, window, view, path); }
    verify_pointer_workflow(&mut cx, window, view)?;
    cx.capture_screenshot(window)?
        .save(format!("{path}.edited.png"))?;
    println!("GPUI render and pointer checks passed: {path}");
    Ok(())
}

#[cfg(windows)]
fn render_preview(_: &str, _: bool, _: bool) -> anyhow::Result<()> {
    anyhow::bail!("Offscreen preview is supported on the Linux build only")
}

#[cfg(not(windows))]
fn verify_marker_card_workflow(
    cx: &mut HeadlessAppContext,
    handle: AnyWindowHandle,
    view: Entity<Editor>,
    path: &str,
) -> anyhow::Result<()> {
    use gpui_kit::test::TestWindowExt;
    use m53_domain::colpo::{Colpo, Mark};
    use std::collections::BTreeMap;
    editor_regression_proof::verify(cx, handle, &view)?;
    let (_, dictionary, asset) = crate::verification::fixture();
    let preset = PRESETS.iter().position(|p| p.id == "vulva.other.trauma").unwrap();
    let alternate = PRESETS.iter().position(|p| p.id == "vulva.other.hematoma").unwrap();
    let mut photo = Photo::new(1000, 800);
    photo.id = asset;
    let mark = photo.place_callout(preset, (0.28, 0.55), (0.40, 0.35));
    let mut protocol = m53_client::model::blank_protocol(dictionary.version_id);
    protocol.schema_version = 2;
    protocol.colpo = Some(Colpo { marks: BTreeMap::from([(mark, Mark {
        asset_id: asset, catalog_id: PRESETS[preset].id.to_string(), properties: BTreeMap::new(),
    })]), ..Default::default() });
    let form = cx.update(|cx| cx.new(|_| native_form::NativeForm::new(serde_json::json!(protocol), dictionary, BTreeMap::new(), false)));
    cx.update(|cx| view.update(cx, |s, cx| {
        s.session = Session { photos: vec![photo], selected: 0 };
        s.images.clear();
        s.images.insert(asset, Arc::new(RenderImage::new(vec![image::Frame::new(image::RgbaImage::from_pixel(1000, 800, image::Rgba([228, 228, 228, 255])))])));
        s.properties = Some(form.clone());
        s.selected = None;
        s.dismissed_properties = None;
        s.cancel_marker_pick();
        s.ready = true;
        s.readonly = false;
        s.fit();
        cx.notify();
    }));
    let before_typing = cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        let r = view.read(cx).image_rect().unwrap();
        let origin = w.find("image-canvas").bounds().origin;
        w.click_at("image-canvas", point(px(r.x + 0.40 * r.w), px(r.y + 0.35 * r.h)) - origin, cx);
        w.render_frame(cx);
        assert_eq!(view.read(cx).selected, Some(mark));
        let card = w.find("selected-marker-properties");
        assert!(card.visible(), "pointer selection must open the adjacent marker card");
        let textbox = gpui_kit::base::test_support::snapshots(w).into_iter().find(|node| {
            node.visible() && node.role() == Some(Role::MultilineTextInput) && node.path().contains(&ElementId::from("selected-marker-properties"))
        }).expect("shared property Textarea must be visible inside the card");
        let before = serde_json::json!(view.read(cx).session);
        w.click_at("selected-marker-properties", textbox.bounds().center() - card.bounds().origin, cx);
        w.input("abcd", cx);
        w.press("home", cx);
        w.press("delete", cx);
        let edited = gpui_kit::base::test_support::snapshots(w).into_iter().find(|node| {
            node.role() == Some(Role::MultilineTextInput) && node.path().contains(&ElementId::from("selected-marker-properties"))
        }).expect("property Textarea must remain mounted while typing");
        assert_eq!(edited.focused(), Some(true), "pointer click must focus the native Textarea: {edited:?}");
        assert_eq!(edited.value(), Some("bcd"), "native Textarea must process input/Home/Delete before its subscription is delivered: {edited:?}");
        before
    })?;
    // InputEvent subscriptions run as App effects after the window update returns.
    cx.update(|cx| {
        assert_eq!(form.read(cx).value["colpo"]["marks"][mark.to_string()]["properties"]["comment"]["value"], "bcd", "the shared form must receive the native Textarea edit");
        assert_eq!(serde_json::json!(view.read(cx).session), before_typing, "property input/Delete must preserve marker and all annotation coordinates");
    });
    cx.capture_screenshot(handle)?.save(format!("{path}.card.png"))?;
    cx.update_window(handle, |_, w, cx| {
        w.press("escape", cx);
        assert!(w.try_find("selected-marker-properties").is_none(), "Esc from the property textbox must close the card");
        assert_eq!(view.read(cx).selected, Some(mark), "closing properties must preserve marker selection");
        // Read-only pointer inspection shows the same card without enabling its controls.
        view.update(cx, |s, _| s.readonly = true);
        form.update(cx, |f, cx| { f.readonly = true; cx.notify(); });
        w.render_frame(cx);
        let r = view.read(cx).image_rect().unwrap();
        let origin = w.find("image-canvas").bounds().origin;
        w.click_at("image-canvas", point(px(r.x + 0.40 * r.w), px(r.y + 0.35 * r.h)) - origin, cx);
        assert!(w.find("selected-marker-properties").visible());
        let before = form.read(cx).value.clone();
        w.input("must-not-change-readonly", cx);
        assert_eq!(form.read(cx).value, before);
        w.click("close-marker-properties", cx);
        view.update(cx, |s, _| s.readonly = false);
        form.update(cx, |f, cx| { f.readonly = false; cx.notify(); });
        w.render_frame(cx);
        w.right_click("image-canvas", cx);
    })?;
    // ContextMenu attaches its popup through App effects; inspect it after that update completes.
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        let title = m53_domain::colpo::MARKER_GROUPS[PRESETS[alternate].marker_group()].0;
        let group = gpui_kit::base::test_support::snapshots(w).into_iter().find(|node| {
            node.visible() && node.label().is_some_and(|label| label.starts_with(title))
        }).expect("the context menu must expose the preset's existing catalog group");
        w.click(group.path().last().unwrap().clone(), cx);
    })?;
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.find(("marker-menu-item", alternate)).visible(), "group submenu must expose the same preset as the palette");
    })?;
    cx.capture_screenshot(handle)?.save(format!("{path}.menu.png"))?;
    let count_before_placement = cx.update_window(handle, |_, w, cx| {
        w.click(("marker-menu-item", alternate), cx);
        assert_eq!(view.read(cx).preset, Some(alternate), "context-menu choice must use the common palette handler");
        view.read(cx).photo().unwrap().annotations.len()
    })?;
    // The menu's DismissEvent removes its overlay after the choice update returns.
    // A click before that delivery can be consumed by the still-mounted menu layer.
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find(("marker-menu-item", alternate)).is_none(), "the chosen context menu must dismiss before image placement");
        let r = view.read(cx).image_rect().unwrap();
        let origin = w.find("image-canvas").bounds().origin;
        w.click_at("image-canvas", point(px(r.x + 0.65 * r.w), px(r.y + 0.65 * r.h)) - origin, cx);
        assert!(view.read(cx).pending_tip.is_some(), "the first image click must reach the common stamp handler and set its tip");
        assert_eq!(view.read(cx).photo().unwrap().annotations.len(), count_before_placement, "first click sets only the arrow tip");
    })?;
    cx.update_window(handle, |_, w, cx| {
        w.render_frame(cx);
        let r = view.read(cx).image_rect().unwrap();
        let origin = w.find("image-canvas").bounds().origin;
        w.click_at("image-canvas", point(px(r.x + 0.75 * r.w), px(r.y + 0.45 * r.h)) - origin, cx);
        assert_eq!(view.read(cx).photo().unwrap().annotations.len(), count_before_placement + 1, "second click commits the badge and marker");
        assert_ne!(view.read(cx).selected, Some(mark));
        assert!(w.try_find("selected-marker-properties").is_none(), "a new selection must not retain the old marker's card");
    })?;
    // Regression: the third checkbox used to be clipped in read-only property cards.
    cx.update_window(handle, |_, w, cx| {
        let spot = PRESETS.iter().position(|p| p.id == "vulva.type.spot").unwrap();
        view.update(cx, |s, _| {
            s.cancel_marker_pick(); s.selected = Some(mark); s.dismissed_properties = None; s.readonly = true;
            let a = s.photo_mut().unwrap().annotations.iter_mut().find(|a| a.id == mark).unwrap();
            a.label = PRESETS[spot].short.into();
            if let AnnotationLayer::Callout { full_name, term_code, .. } = &mut a.layer {
                *full_name = PRESETS[spot].name.into(); *term_code = None;
            }
        });
        form.update(cx, |f, cx| {
            f.readonly = true; f.focus_mark = Some(mark);
            f.value["colpo"]["marks"][mark.to_string()]["catalog_id"] = serde_json::json!("vulva.type.spot");
            cx.notify();
        });
        w.render_frame(cx);
        let card = w.find("selected-marker-properties").bounds();
        let field = m53_domain::colpo::FIELDS.iter().filter(|f| f.parent == "vulva.type.spot").last().unwrap();
        let last = w.find(SharedString::from(format!("field-/colpo/marks/{mark}/properties/{}", field.id))).bounds();
        assert!(last.origin.y >= card.origin.y && last.bottom() <= card.bottom(), "last read-only checkbox must fit inside the card: {last:?} / {card:?}");
    })?;
    cx.capture_screenshot(handle)?.save(format!("{path}.readonly-checks.png"))?;
    println!("Headless marker card pointer/keyboard/readonly/context-menu checks passed: {path}");
    Ok(())
}

#[cfg(not(windows))]
fn verify_pointer_workflow(
    cx: &mut HeadlessAppContext,
    handle: AnyWindowHandle,
    view: Entity<Editor>,
) -> anyhow::Result<()> {
    use gpui_kit::test::TestWindowExt;
    cx.update_window(handle,|_,w,cx| {
        w.click("hand",cx);
        assert!(view.read(cx).tool == Tool::Hand);
        w.click("select",cx);
        assert!(view.read(cx).tool == Tool::Select);
        w.click("zoom-in",cx);
        assert_eq!(view.read(cx).zoom,1.25);
        w.click("zoom-out",cx);
        assert_eq!(view.read(cx).zoom,1.0);
        w.click("zoom-in",cx);
        w.click("fit",cx);
        assert_eq!(view.read(cx).zoom,1.0);
        let count=view.read(cx).photo().unwrap().annotations.len();
        w.click(("preset",0usize),cx);
        assert_eq!(view.read(cx).preset,Some(0),"Kit palette button must remain clickable");
        let r=view.read(cx).image_rect().unwrap();
        let pos=|x:f32,y:f32|point(px(r.x+x*r.w),px(r.y+y*r.h));
        let origin=w.find("image-canvas").bounds().origin;
        w.click_at("image-canvas",pos(0.35,0.6)-origin,cx);
        assert_eq!(view.read(cx).photo().unwrap().annotations.len(),count,"first click must not commit a marker");
        w.click_at("image-canvas",pos(0.55,0.3)-origin,cx);
        assert_eq!(view.read(cx).photo().unwrap().annotations.len(),count+1);
        let id=view.read(cx).selected.unwrap();
        let layer=view.read(cx).photo().unwrap().annotations.last().unwrap().layer.clone();
        w.drag(pos(0.55,0.3),pos(0.45,0.3),cx);
        let a=view.read(cx).photo().unwrap().annotations.iter().find(|a|a.id==id).unwrap();
        assert!((a.x-0.45).abs()<0.01);assert_eq!(a.layer,layer,"moving label must preserve arrow tip");
        let AnnotationLayer::Callout{end_x,end_y,..}=a.layer else{panic!("callout expected")};
        w.drag(pos(end_x,end_y),pos(0.63,0.6),cx);
        let a=view.read(cx).photo().unwrap().annotations.iter().find(|a|a.id==id).unwrap();
        assert!(matches!(a.layer,AnnotationLayer::Callout{end_x,end_y,..} if (end_x-0.63).abs()<0.01&&(end_y-0.6).abs()<0.01));
        let radius=a.radius*r.w.min(r.h)/r.w;
        w.drag(pos(a.x+radius,a.y),pos(a.x+radius*1.4,a.y),cx);
        let a=view.read(cx).photo().unwrap().annotations.iter().find(|a|a.id==id).unwrap();assert!(a.radius>0.05);
        w.click(("preset",1usize),cx);
        w.click_at("image-canvas",pos(0.8,0.7)-origin,cx);
        w.click_at("image-canvas",pos(0.70,0.42)-origin,cx);
        assert_eq!(view.read(cx).photo().unwrap().annotations.len(),count+2,"two palette clicks must add a defect");
        let marker_id=view.read(cx).selected.unwrap();
        w.drag(pos(0.70,0.42),pos(0.50,0.50),cx);
        w.double_click("image-canvas",cx);
        assert_eq!(view.read(cx).retag,Some(marker_id));
        w.click(("preset",0usize),cx);
        let changed=view.read(cx).photo().unwrap().annotations.iter().find(|a|a.id==marker_id).unwrap();
        assert_eq!(changed.label,"Р");assert!((changed.x-0.5).abs()<0.01);
        w.click("crop",cx);w.drag(pos(0.10,0.10),pos(0.90,0.85),cx);
        let crop=view.read(cx).photo().unwrap().annotations.iter().find(|a|matches!(a.layer,AnnotationLayer::OutputRegion{..})).unwrap();
        assert!((crop.x-0.1).abs()<0.01);assert!(matches!(crop.layer,AnnotationLayer::OutputRegion{width,height} if (width-0.8).abs()<0.01&&(height-0.75).abs()<0.01));
        w.drag(pos(0.90,0.85),pos(0.80,0.80),cx);
        let crop=view.read(cx).photo().unwrap().annotations.iter().find(|a|matches!(a.layer,AnnotationLayer::OutputRegion{..})).unwrap();
        assert!(matches!(crop.layer,AnnotationLayer::OutputRegion{width,height} if (width-0.7).abs()<0.01&&(height-0.7).abs()<0.01));
        w.click("undo",cx);
        let crop=view.read(cx).photo().unwrap().annotations.iter().find(|a|matches!(a.layer,AnnotationLayer::OutputRegion{..})).unwrap();
        assert!(matches!(crop.layer,AnnotationLayer::OutputRegion{width,..} if (width-0.8).abs()<0.01));
        w.click("target",cx);
        let target_id=view.read(cx).selected.unwrap();
        w.click("target",cx);
        assert_eq!(view.read(cx).selected,Some(target_id));
        let photo=view.read(cx).photo().unwrap();
        assert_eq!(photo.annotations.iter().filter(|a|matches!(a.layer,AnnotationLayer::Target{..})).count(),1);
        assert!(!photo.hidden.contains(&target_id));
        let a=photo.annotations.iter().find(|a|a.id==target_id).unwrap().clone();
        w.drag(pos(a.x,a.y),pos(a.x+0.03,a.y+0.02),cx);
        let moved=view.read(cx).photo().unwrap().annotations.iter().find(|a|a.id==target_id).unwrap().clone();
        assert!((moved.x-a.x-0.03).abs()<0.01 && (moved.y-a.y-0.02).abs()<0.01);
        let dx=moved.radius*r.w.min(r.h)/r.w;
        w.drag(pos(moved.x+dx,moved.y),pos(moved.x+dx*0.7,moved.y),cx);
        let resized=view.read(cx).photo().unwrap().annotations.iter().find(|a|a.id==target_id).unwrap();
        assert!((resized.radius-moved.radius*0.7).abs()<0.01,"target edge must resize without moving center");
        assert_eq!((resized.x,resized.y),(moved.x,moved.y));
        w.drag(pos(0.90,0.10),pos(0.85,0.15),cx);
        let crop=view.read(cx).photo().unwrap().annotations.iter().find(|a|matches!(a.layer,AnnotationLayer::OutputRegion{..})).unwrap();
        assert!((crop.x-0.1).abs()<0.01 && (crop.y-0.15).abs()<0.01,"top-right crop corner must resize");
        w.drag(pos(0.35,0.85),pos(0.40,0.90),cx);
        let crop=view.read(cx).photo().unwrap().annotations.iter().find(|a|matches!(a.layer,AnnotationLayer::OutputRegion{..})).unwrap();
        assert!((crop.x-0.15).abs()<0.01 && (crop.y-0.20).abs()<0.01,"crop border must move the whole region");
    })?;
    Ok(())
}

