//! Native, fixed right-hand phrase library. Text insertion stays in NativeForm.
use crate::native_form::{PhraseTarget, ergonomics};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    switch::Switch,
    v_flex,
};
use gpui_kit::{prelude::*, *};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Clone)]
pub enum PhrasePanelEvent {
    Apply {
        target: PhraseTarget,
        template: Uuid,
        version: u64,
        confirmed: bool,
        text: String,
        replace: bool,
    },
    EditorOpened {
        field_code: String,
        template: Option<Uuid>,
    },
    Save {
        id: Option<Uuid>,
        field_code: String,
        scope: String,
        title: String,
        body: String,
    },
    Preferences {
        replace: bool,
        include_shared: bool,
    },
    Resolve {
        operation: Uuid,
        keep_local: bool,
    },
}
struct Editing {
    id: Option<Uuid>,
    field_code: String,
    label: String,
    scope: String,
    initial_title: String,
    initial_body: String,
}
pub struct PhrasePanel {
    pub target: Option<PhraseTarget>,
    pub snapshot: Value,
    allowed: bool,
    title: Entity<InputState>,
    body: Entity<TextareaState>,
    search: Entity<InputState>,
    search_reset: bool,
    _subscriptions: Vec<Subscription>,
    editing: Option<Editing>,
    saving: bool,
    note: String,
    note_error: bool,
    submit_attempted: bool,
}
impl EventEmitter<PhrasePanelEvent> for PhrasePanel {}
impl PhrasePanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Наименование шаблона"));
        let body = cx.new(|cx| TextareaState::new(window, cx).placeholder("Текст готовой фразы").auto_grow(4, 12));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Найти фразу"));
        let subscriptions = vec![
            cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) { cx.notify(); }
            }),
            cx.subscribe(&title, |s, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) && s.submit_attempted { cx.notify(); }
            }),
            cx.subscribe(&body, |s, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) && s.submit_attempted { cx.notify(); }
            }),
        ];
        Self {
            target: None,
            snapshot: json!({}),
            allowed: false,
            title,
            body,
            search,
            search_reset: false,
            _subscriptions: subscriptions,
            editing: None,
            saving: false,
            note: String::new(),
            note_error: false,
            submit_attempted: false,
        }
    }
    pub fn set_target(
        &mut self,
        target: Option<PhraseTarget>,
        allowed: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let changed = self.target != target;
        if changed || self.allowed != allowed {
            if changed { self.search_reset = true; }
            self.target = target;
            self.allowed = allowed;
            cx.notify();
        }
        changed
    }
    pub fn set_snapshot(&mut self, snapshot: Value, cx: &mut Context<Self>) {
        if self.snapshot != snapshot {
            self.snapshot = snapshot;
            cx.notify();
        }
    }
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.target = None;
        self.snapshot = json!({});
        self.allowed = false;
        self.editing = None;
        self.saving = false;
        self.note.clear();
        self.note_error = false;
        self.submit_attempted = false;
        self.search_reset = true;
        cx.notify();
    }
    pub fn has_unsaved(&self, cx: &App) -> bool {
        self.editing.as_ref().is_some_and(|e| {
            e.initial_title != self.title.read(cx).value().as_ref()
                || e.initial_body != self.body.read(cx).value().as_ref()
        })
    }
    pub fn saved(&mut self, result: Result<(), String>, cx: &mut Context<Self>) {
        self.saving = false;
        match result {
            Ok(()) => {
                self.editing = None;
                self.note = "Шаблон сохранён на компьютере".into();
                self.note_error = false;
            }
            Err(error) => { self.note = error; self.note_error = true; },
        };
        cx.notify();
    }
    fn begin(&mut self, item: Option<Value>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = &self.target else {
            return;
        };
        let value = item.unwrap_or(json!({}));
        let title = value["title"].as_str().unwrap_or("").to_owned();
        let body = value["body"].as_str().unwrap_or("").to_owned();
        self.title
            .update(cx, |state, cx| state.set_value(title.clone(), window, cx));
        self.body
            .update(cx, |state, cx| state.set_value(body.clone(), window, cx));
        self.editing = Some(Editing {
            id: value["id"].as_str().and_then(|id| id.parse().ok()),
            field_code: target.field_code.clone(),
            label: target.label.clone(),
            scope: value["scope"].as_str().unwrap_or("personal").into(),
            initial_title: title,
            initial_body: body,
        });
        self.note.clear();
        self.note_error = false;
        self.submit_attempted = false;
        cx.emit(PhrasePanelEvent::EditorOpened {
            field_code: target.field_code.clone(),
            template: self.editing.as_ref().and_then(|e| e.id),
        });
        cx.notify();
    }
    fn preferences(&mut self, replace: bool, include_shared: bool, cx: &mut Context<Self>) {
        self.snapshot["preferences"]["apply_mode"] =
            json!(if replace { "replace" } else { "append" });
        self.snapshot["preferences"]["include_shared"] = json!(include_shared);
        cx.emit(PhrasePanelEvent::Preferences {
            replace,
            include_shared,
        });
        cx.notify();
    }
}
impl Render for PhrasePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.search_reset {
            self.search_reset = false;
            self.search.update(cx, |state, cx| state.set_value("", window, cx));
        }
        let replace = self.snapshot["preferences"]["apply_mode"] == "replace";
        let common = self.snapshot["preferences"]["include_shared"]
            .as_bool()
            .unwrap_or(true);
        let pending = self.snapshot["pending"].as_u64().unwrap_or(0);
        let prefs_busy = false;
        let mut panel = v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .gap_3()
            .p_3()
            .child(crate::ui_chrome::panel_heading("Шаблоны заполнения", cx));
        let error = self.snapshot["error"].as_str().unwrap_or("");
        if !error.is_empty() {
            panel = panel.child(div().text_sm().text_color(cx.theme().danger).child(error.to_string()));
        }
        if !self.note.is_empty() && self.editing.is_none() {
            panel = panel.child(div().text_sm()
                .when(self.note_error, |d| d.text_color(cx.theme().danger))
                .child(self.note.clone()));
        }
        if let Some(editing) = &self.editing {
            let editing_id = editing.id;
            let shared = editing.scope == "shared";
            let (title_error, body_error) = if self.submit_attempted {
                ergonomics::editor_errors(self.title.read(cx).value().as_ref(), self.body.read(cx).value().as_ref())
            } else { (None, None) };
            panel = panel
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("phrase-back")
                                .ghost()
                                .small()
                                .icon(IconName::ChevronLeft)
                                .tooltip("К списку шаблонов")
                                .disabled(self.saving)
                                .on_click(cx.listener(|s, _, _, cx| {
                                    s.editing = None;
                                    s.note.clear();
                                    cx.notify();
                                })),
                        )
                        .child(div().font_semibold().child(if editing_id.is_some() {
                            "Редактирование"
                        } else {
                            "Новый шаблон"
                        })),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("Поле: {}", editing.label.as_str())),
                )
                .child(div().text_sm().child("Наименование"))
                .child(v_flex().gap_1()
                    .when(title_error.is_some(), |d| d.p_1().border_1().border_color(cx.theme().danger).rounded_md())
                    .child(Input::new(&self.title).aria_label("Наименование шаблона").disabled(self.saving))
                    .when(title_error.is_some(), |d| d.child(div().text_sm().text_color(cx.theme().danger).child(title_error.unwrap()))))
                .child(div().text_sm().child("Текст шаблона"))
                .child(v_flex().gap_1()
                    .when(body_error.is_some(), |d| d.p_1().border_1().border_color(cx.theme().danger).rounded_md())
                    .child(Textarea::new(&self.body).aria_label("Текст шаблона").disabled(self.saving))
                    .when(body_error.is_some(), |d| d.child(div().text_sm().text_color(cx.theme().danger).child(body_error.unwrap()))))
                .child(
                    Switch::new("phrase-scope")
                        .label("Общий шаблон")
                        .checked(shared)
                        .disabled(editing_id.is_some() || self.saving)
                        .on_change(cx.listener(|s, v, _, cx| {
                            if let Some(e) = &mut s.editing {
                                e.scope = if *v { "shared" } else { "personal" }.into();
                            }
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(if shared {
                            "Общий для врачей организации. Любой врач может редактировать."
                        } else {
                            "Личный: доступен только вам."
                        }),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("phrase-cancel")
                                .outline()
                                .label("Отмена")
                                .disabled(self.saving)
                                .on_click(cx.listener(|s, _, _, cx| {
                                    s.editing = None;
                                    s.note.clear();
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("phrase-save")
                                .primary()
                                .label(if self.saving {
                                    "Сохранение…"
                                } else {
                                    "Сохранить"
                                })
                                .disabled(self.saving)
                                .on_click(cx.listener(|s, _, _, cx| {
                                    let Some(e) = &s.editing else {
                                        return;
                                    };
                                    let title = s.title.read(cx).value().to_string();
                                    let body = s.body.read(cx).value().to_string();
                                    s.submit_attempted = true;
                                    let errors = ergonomics::editor_errors(&title, &body);
                                    if errors.0.is_some() || errors.1.is_some() {
                                        s.note.clear();
                                        cx.notify();
                                        return;
                                    }
                                    s.note.clear();
                                    s.note_error = false;
                                    s.saving = true;
                                    cx.emit(PhrasePanelEvent::Save {
                                        id: e.id,
                                        field_code: e.field_code.clone(),
                                        scope: e.scope.clone(),
                                        title,
                                        body,
                                    });
                                    cx.notify();
                                })),
                        ),
                );
            if !self.note.is_empty() {
                panel = panel.child(div().text_sm()
                    .when(self.note_error, |d| d.text_color(cx.theme().danger)).child(self.note.clone()));
            }
        } else if let Some(target) = &self.target {
            panel = panel
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().primary)
                        .child(format!("Для поля «{}»", target.label)),
                )
                .child(
                    Switch::new("phrase-common")
                        .label("Показывать общие")
                        .checked(common)
                        .disabled(prefs_busy)
                        .on_change(cx.listener(move |s, v, _, cx| s.preferences(replace, *v, cx))),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(if common {
                            "Личные и общие шаблоны"
                        } else {
                            "Только личные шаблоны"
                        }),
                )
                .child(
                    div()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .pt_3()
                        .child(
                            Switch::new("phrase-replace")
                                .label("Заменять текст поля")
                                .checked(replace)
                                .disabled(prefs_busy)
                                .on_change(
                                    cx.listener(move |s, v, _, cx| s.preferences(*v, common, cx)),
                                ),
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .child(if replace {
                            "Замена всего текста поля"
                        } else {
                            "Добавление с новой строки"
                        }),
                );
            let items = self.snapshot["templates"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            let query = self.search.read(cx).value().to_string();
            panel = panel.child(Input::new(&self.search).aria_label("Поиск шаблона по названию и тексту"));
            let matching: Vec<_> = items
                .iter()
                .filter(|t| {
                    t["field_code"] == target.field_code && (common || t["scope"] == "personal")
                        && ergonomics::phrase_matches(&query, t["title"].as_str().unwrap_or(""), t["body"].as_str().unwrap_or(""))
                })
                .cloned()
                .collect();
            if matching.is_empty() {
                panel = panel.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .py_3()
                        .child(if query.trim().is_empty() { "Для этого поля шаблонов пока нет" } else { "Подходящих фраз не найдено" }),
                );
            }
            for item in matching {
                let Some(id) = item["id"].as_str().and_then(|s| s.parse::<Uuid>().ok()) else {
                    continue;
                };
                let title = item["title"].as_str().unwrap_or("Шаблон").to_string();
                let text = item["body"].as_str().unwrap_or("").to_string();
                let preview = ergonomics::phrase_preview(&text);
                let version = item["row_version"].as_u64().unwrap_or(0);
                let confirmed =
                    version > 0 && item["sync_state"].as_str().is_none_or(|s| s == "synced");
                let target = target.clone();
                let can_apply = self.allowed;
                let state = item["sync_state"].as_str().unwrap_or("synced");
                let scope = if item["scope"] == "shared" {
                    "Общий"
                } else {
                    "Личный"
                };
                let label = match state {
                    "pending" => format!("{scope} · Ожидает передачи"),
                    "conflict" => format!("{scope} · Конфликт"),
                    _ => scope.into(),
                };
                panel = panel.child(
                    v_flex()
                        .gap_1()
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded_md()
                        .p_2()
                        .child(
                            h_flex()
                                .gap_1()
                                .items_start()
                                .child(
                                    Button::new(SharedString::from(format!("phrase-use-{id}")))
                                        .ghost()
                                        .flex_1()
                                        .min_w_0()
                                        .justify_start()
                                        .label(title.clone())
                                        .tooltip(text.clone())
                                        .disabled(!can_apply)
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            cx.emit(PhrasePanelEvent::Apply {
                                                target: target.clone(),
                                                template: id,
                                                version,
                                                confirmed,
                                                text: text.clone(),
                                                replace,
                                            });
                                        })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("phrase-edit-{id}")))
                                        .ghost()
                                        .small()
                                        .icon(IconName::Pencil)
                                        .tooltip("Редактировать шаблон")
                                        .disabled(!item["can_edit"].as_bool().unwrap_or(false))
                                        .on_click(cx.listener(move |s, _, window, cx| {
                                            s.begin(Some(item.clone()), window, cx)
                                        })),
                                ),
                        )
                        .child(div().text_sm().text_color(cx.theme().muted_foreground)
                            .w_full().min_w_0().line_clamp(2).text_ellipsis().child(preview))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(label),
                        ),
                );
            }
            panel = panel
                .child(
                    Button::new("phrase-new")
                        .outline()
                        .w_full()
                        .icon(IconName::Plus)
                        .label("Новый шаблон")
                        .on_click(cx.listener(|s, _, window, cx| s.begin(None, window, cx))),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Режим вставки и список запоминаются для врача."),
                );
            if !self.allowed {
                panel = panel.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("В утверждённом протоколе вставка недоступна"),
                );
            }
        } else {
            panel = panel.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Выберите текстовое поле протокола — здесь появятся его шаблоны."),
            );
        }
        if let Some(conflicts) = self.snapshot["conflicts"].as_array() {
            for (index, conflict) in conflicts.iter().enumerate() {
                let Some(operation) = conflict["operation_id"]
                    .as_str()
                    .and_then(|s| s.parse::<Uuid>().ok())
                else {
                    continue;
                };
                let local = &conflict["local"];
                let remote = &conflict["remote"];
                let preferences = local["apply_mode"].is_string();
                let version_conflict = conflict["code"]
                    .as_str()
                    .is_some_and(|code| code.ends_with("version_conflict"));
                let has_remote = version_conflict
                    && remote["row_version"].as_u64().unwrap_or(0) > 0
                    && (remote["body"].is_string() || remote["apply_mode"].is_string());
                let mut card = v_flex()
                    .gap_2()
                    .border_1()
                    .border_color(cx.theme().danger)
                    .rounded_md()
                    .p_2()
                    .child(div().font_semibold().child(if preferences {
                        "Конфликт настроек шаблонов"
                    } else if version_conflict {
                        "Есть другая версия шаблона"
                    } else {
                        "Изменение не передано"
                    }))
                    .child(
                        div().text_sm().child(
                            conflict["message"]
                                .as_str()
                                .unwrap_or("Локальные изменения сохранены")
                                .to_owned(),
                        ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .child(format!("Ваш вариант: {}", comparison_text(local))),
                    );
                if has_remote {
                    card = card
                        .child(
                            div()
                                .text_sm()
                                .child(format!("На сервере: {}", comparison_text(remote))),
                        )
                        .child(
                            Button::new(("phrase-server", index))
                                .outline()
                                .small()
                                .label("Принять серверную версию")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(PhrasePanelEvent::Resolve {
                                        operation,
                                        keep_local: false,
                                    })
                                })),
                        )
                        .child(
                            Button::new(("phrase-local", index))
                                .outline()
                                .small()
                                .label("Отправить мой вариант")
                                .disabled(!conflict["can_keep_local"].as_bool().unwrap_or(false))
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(PhrasePanelEvent::Resolve {
                                        operation,
                                        keep_local: true,
                                    })
                                })),
                        );
                } else {
                    card=card.child(div().text_sm().text_color(cx.theme().muted_foreground).child(if version_conflict{"Актуальная версия ещё загружается. Локальные изменения остаются на компьютере."}else{"Локальный вариант сохранён. Требуется разбор ошибки синхронизации."}));
                }
                panel = panel.child(card);
            }
        }
        if pending > 0 {
            panel = panel.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("Ожидают синхронизации: {pending}")),
            );
        }
        div()
            .flex_1()
            .min_h_0()
            .id("phrase-panel-scroll")
            .overflow_y_scroll()
            .child(panel)
    }
}

fn comparison_text(value: &Value) -> String {
    if let Some(body) = value["body"].as_str() {
        return body.to_owned();
    }
    if value["apply_mode"].is_string() {
        return format!(
            "{}; {}",
            if value["apply_mode"] == "replace" {
                "Заменять текст"
            } else {
                "Добавлять текст"
            },
            if value["include_shared"] == true {
                "Личные и общие"
            } else {
                "Только личные"
            }
        );
    }
    "Изменение сохранено локально".into()
}
