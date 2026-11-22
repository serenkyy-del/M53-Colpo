use super::*;
use crate::diagnosis_picker;
use gpui_kit::{
    base::TestSupportExt,
    component::{
        Icon,
        combobox::{Combobox, ComboboxEvent},
        tooltip::Tooltip,
    },
};
use m53_client::{
    diagnosis_cache::Snapshot,
    diagnosis_rows::{self, Edit},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use uuid::Uuid;

struct Row {
    picker: Option<Entity<diagnosis_picker::PickerState>>,
    version: Option<Uuid>,
    priority: Option<Arc<m53_client::diagnosis_priority::Group>>,
    open: Arc<AtomicBool>,
    picker_subscription: Option<Subscription>,
}
#[derive(Default)]
pub(super) struct Ui {
    snapshot: Snapshot,
    notice: String,
    blank: Vec<Uuid>,
    rows: BTreeMap<Uuid, Row>,
}

impl NativeForm {
    #[cfg(not(windows))]
    pub(crate) fn diagnosis_picker_for_test(
        &self,
        index: usize,
        _cx: &App,
    ) -> Entity<diagnosis_picker::PickerState> {
        let rows = diagnosis_rows::rows(&self.value);
        let id = rows
            .iter()
            .map(|r| r.id)
            .chain(self.diagnoses.blank.iter().copied())
            .nth(index)
            .unwrap();
        self.diagnoses.rows[&id].picker.as_ref().unwrap().clone()
    }
    #[cfg(not(windows))]
    pub(crate) fn diagnosis_note_for_test(&self) -> Entity<TextareaState> {
        self.areas["/diagnosis_note/value"].clone()
    }
    pub fn set_diagnoses(&mut self, snapshot: Snapshot) {
        self.diagnoses.snapshot = snapshot;
    }
    fn diagnosis_edit(&mut self, edit: Edit, cx: &mut Context<Self>) {
        if diagnosis_rows::apply(&mut self.value, edit, self.readonly) {
            self.audit_fields.insert("/diagnoses".into());
            self.generation += 1;
            self.changed = Instant::now();
            cx.notify();
        }
    }
    fn diagnosis_select(&mut self, id: Uuid, guid: Uuid, version: Uuid, cx: &mut Context<Self>) {
        if self.readonly {
            return;
        }
        if self
            .diagnoses
            .snapshot
            .catalog
            .as_ref()
            .map(|c| c.metadata.version_id)
            != Some(version)
        {
            self.diagnoses.notice = "Справочник обновился. Повторите выбор диагноза.".into();
            if let Some(row) = self.diagnoses.rows.get(&id) {
                row.open.store(false, Ordering::Relaxed);
            }
            cx.notify();
            return;
        }
        let Some(entry) = self.diagnoses.snapshot.catalog.as_ref().and_then(|c| {
            c.items
                .binary_search_by_key(&guid, |e| e.guid)
                .ok()
                .map(|i| c.items[i].clone())
        }) else {
            return;
        };
        let new = self.diagnoses.blank.contains(&id);
        self.diagnoses.notice.clear();
        self.diagnosis_edit(Edit::Select { id, entry, new }, cx);
        if diagnosis_rows::rows(&self.value)
            .iter()
            .any(|row| row.id == id)
        {
            self.diagnoses.blank.retain(|row| *row != id);
        }
        cx.notify();
    }
    fn diagnosis_row_state(
        &mut self,
        id: Uuid,
        saved: Option<&m53_domain::protocol::ProtocolDiagnosis>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let version = self
            .diagnoses
            .snapshot
            .catalog
            .as_ref()
            .map(|c| c.metadata.version_id);
        if self.diagnoses.rows.get(&id).is_some_and(|r| {
            (r.version == version && r.priority == self.diagnoses.snapshot.priority)
                || r.open.load(Ordering::Relaxed)
        }) {
            return;
        }
        let mut picker_subscription = None;
        let picker = self.diagnoses.snapshot.catalog.clone().map(|catalog| {
            let source_version = catalog.metadata.version_id;
            let priorities = Arc::new(
                self.diagnoses
                    .snapshot
                    .priority
                    .as_ref()
                    .map(|p| p.diagnosis_ids.clone())
                    .unwrap_or_default(),
            );
            let state = diagnosis_picker::new(
                catalog,
                saved.map(|r| r.diagnosis_id),
                priorities,
                window,
                cx,
            );
            picker_subscription = Some(cx.subscribe_in(
                &state,
                window,
                move |s, _, event: &ComboboxEvent<diagnosis_picker::Delegate>, _, cx| {
                    if let ComboboxEvent::Change(values) = event {
                        if let Some(guid) = values.first() {
                            s.diagnosis_select(id, *guid, source_version, cx);
                        }
                    } else if let ComboboxEvent::Confirm(_) = event {
                        if let Some(row) = s
                            .diagnoses
                            .rows
                            .get(&id)
                            .filter(|row| row.version == Some(source_version))
                        {
                            row.open.store(false, Ordering::Relaxed);
                            cx.notify();
                        }
                    }
                },
            ));
            state
        });
        // Catalogue changes replace the chooser only. The common note uses the
        // same persistent multiline state as conclusion, independently of rows.
        if let Some(row) = self.diagnoses.rows.get_mut(&id) {
            row.picker = picker;
            row.version = version;
            row.priority = self.diagnoses.snapshot.priority.clone();
            row.picker_subscription = picker_subscription;
            return;
        }
        self.diagnoses.rows.insert(
            id,
            Row {
                picker,
                version,
                priority: self.diagnoses.snapshot.priority.clone(),
                open: Arc::new(AtomicBool::new(false)),
                picker_subscription,
            },
        );
    }
    pub(super) fn diagnoses_view(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let saved = diagnosis_rows::rows(&self.value);
        if saved.is_empty() && self.diagnoses.blank.is_empty() && !self.readonly {
            self.diagnoses.blank.push(Uuid::new_v4());
        }
        let ids: Vec<_> = saved
            .iter()
            .map(|r| r.id)
            .chain(self.diagnoses.blank.iter().copied())
            .collect();
        self.diagnoses.rows.retain(|id, _| ids.contains(id));
        let mut out = v_flex()
            .id("diagnoses-section")
            .test_support()
            .gap_2()
            .py_2()
            .w_full()
            .min_w_0()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                crate::ui_chrome::section_heading("", cx)
                    .child(div().min_w_0().child("Диагнозы по МКБ‑10"))
                    .child(
                        div()
                            .text_xs()
                            .font_normal()
                            .text_color(cx.theme().muted_foreground)
                            .child("Необязательно"),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("diagnoses-add")
                            .outline()
                            .small()
                            .flex_shrink_0()
                            .icon(IconName::Plus)
                            .tooltip("Добавить диагноз")
                            .accessibility_label("Добавить диагноз")
                            .disabled(
                                self.readonly
                                    || ids.len() >= m53_domain::protocol::DIAGNOSES_MAX_ROWS
                                    || self.diagnoses.snapshot.catalog.is_none(),
                            )
                            .on_click(cx.listener(|s, _, _, cx| {
                                if !s.readonly
                                    && s.diagnoses.blank.len()
                                        + diagnosis_rows::rows(&s.value).len()
                                        < m53_domain::protocol::DIAGNOSES_MAX_ROWS
                                {
                                    s.diagnoses.blank.push(Uuid::new_v4());
                                    cx.notify();
                                }
                            })),
                    ),
            );
        if !self.diagnoses.notice.is_empty() {
            out = out.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.diagnoses.notice.clone()),
            );
        }
        if self.diagnoses.snapshot.catalog.is_none() {
            out = out.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.diagnoses.snapshot.loading {
                        "Загрузка справочника диагнозов…".into()
                    } else if self.diagnoses.snapshot.error.is_empty() {
                        "Справочник ещё не загружен. Для первого обновления подключитесь к серверу."
                            .into()
                    } else {
                        self.diagnoses.snapshot.error.clone()
                    }),
            );
        } else if !self.diagnoses.snapshot.error.is_empty() {
            out = out.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Обновление справочника недоступно. Используется сохранённая версия."),
            );
        }
        if ids.is_empty() {
            out = out.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Диагнозы не указаны"),
            );
        }
        for (position, id) in ids.iter().copied().enumerate() {
            let selected = saved.iter().find(|r| r.id == id);
            self.diagnosis_row_state(id, selected, window, cx);
            let state = &self.diagnoses.rows[&id];
            let label = selected
                .map(|r| format!("{} {}", r.code, r.name))
                .unwrap_or_else(|| "Код МКБ‑10 или название".into());
            let chooser = if self.readonly {
                state.open.store(false, Ordering::Relaxed);
                let tooltip = label.clone();
                readonly_value(label, cx)
                    .id(SharedString::from(format!("diagnosis-label-{id}")))
                    .truncate()
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .into_any_element()
            } else if let Some(picker) = &state.picker {
                let open = state.open.clone();
                let tooltip = label.clone();
                Combobox::new(picker)
                    .small()
                    .w_full()
                    .menu_width(px(520.))
                    .menu_max_h(px(300.))
                    .search_placeholder("Поиск по коду МКБ‑10 или названию")
                    .cleanable(false)
                    .disabled(self.readonly)
                    .render_trigger(move |context, _, _| {
                        open.store(context.is_open(), Ordering::Relaxed);
                        h_flex()
                            .id(SharedString::from(format!("diagnosis-label-{id}")))
                            .w_full()
                            .min_w_0()
                            .gap_1()
                            .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                            .child(Icon::new(IconName::ChevronDown).small())
                            .tooltip({
                                let tooltip = tooltip.clone();
                                move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx)
                            })
                    })
                    .into_any_element()
            } else {
                div()
                    .w_full()
                    .min_w_0()
                    .truncate()
                    .child(label)
                    .into_any_element()
            };
            let index = saved.iter().position(|r| r.id == id);
            let row = h_flex()
                .id(SharedString::from(format!("diagnosis-row-{id}")))
                .test_support()
                .w_full()
                .min_w_0()
                .gap_1()
                .child(div().flex_1().min_w_0().child(chooser))
                .child(
                    Button::new(("diagnosis-up", position))
                        .ghost()
                        .small()
                        .icon(IconName::ChevronUp)
                        .tooltip("Переместить диагноз выше")
                        .accessibility_label("Переместить диагноз выше")
                        .disabled(self.readonly || index.is_none_or(|i| i == 0))
                        .on_click(cx.listener(move |s, _, _, cx| {
                            s.diagnosis_edit(Edit::Move { id, up: true }, cx)
                        })),
                )
                .child(
                    Button::new(("diagnosis-down", position))
                        .ghost()
                        .small()
                        .icon(IconName::ChevronDown)
                        .tooltip("Переместить диагноз ниже")
                        .accessibility_label("Переместить диагноз ниже")
                        .disabled(
                            self.readonly
                                || index.is_none_or(|i| {
                                    i + 1 >= diagnosis_rows::rows(&self.value).len()
                                }),
                        )
                        .on_click(cx.listener(move |s, _, _, cx| {
                            s.diagnosis_edit(Edit::Move { id, up: false }, cx)
                        })),
                )
                .child(
                    Button::new(("diagnosis-remove", position))
                        .ghost()
                        .small()
                        .icon(IconName::Trash)
                        .tooltip("Удалить диагноз")
                        .accessibility_label("Удалить диагноз")
                        .disabled(self.readonly)
                        .on_click(cx.listener(move |s, _, _, cx| {
                            if s.readonly {
                                return;
                            }
                            s.diagnosis_edit(Edit::Remove(id), cx);
                            s.diagnoses.blank.retain(|row| *row != id);
                            s.diagnoses.rows.remove(&id);
                            s.unfinished
                                .retain(|k, _| !k.starts_with(&format!("/diagnoses/{id}/")));
                            cx.notify();
                        })),
                );
            out = out.child(row);
        }
        let note = self.text(
            "/diagnosis_note/value",
            &self.value["diagnosis_note"]["value"].clone(),
            false,
            false,
            true,
            true,
            window,
            cx,
        );
        out = out.child(
            v_flex()
                .id("diagnosis-note")
                .test_support()
                .w_full()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .child("Дополнение к диагнозам · необязательно"),
                )
                .child(note),
        );
        if let Some(nav) = self.navigation.as_ref().filter(|n| n.path == "diagnoses") {
            out = out
                .anchor_scroll(Some(nav.anchor.clone()))
                .track_focus(&nav.focus);
        }
        out.into_any_element()
    }
}
