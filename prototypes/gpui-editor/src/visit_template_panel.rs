//! Presentation and intents only; the owner validates and performs clinical/server operations.
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu, PopupMenuItem},
    switch::Switch,
    v_flex, ActiveTheme, Disableable, StyledExt,
};
use gpui_kit::{prelude::*, *};
use m53_client::offline::VisitTemplateSummary;
use uuid::Uuid;

fn title_key(title: &str) -> String {
    title.trim().to_lowercase()
}

fn save_choice(
    title: &str,
    overwrite: bool,
    selected: Option<&VisitTemplateSummary>,
    rows: &[VisitTemplateSummary],
) -> Result<(String, Option<VisitTemplateSummary>), &'static str> {
    if overwrite {
        let selected = selected.ok_or("Выберите шаблон для перезаписи")?;
        let current = rows
            .iter()
            .find(|row| row.id == selected.id && row.row_version == selected.row_version)
            .ok_or("Шаблон изменился. Обновите список")?;
        return Ok((current.title.clone(), Some(current.clone())));
    }
    let title = title.trim();
    if title.is_empty() {
        return Err("Введите название шаблона");
    }
    if rows
        .iter()
        .any(|row| title_key(&row.title) == title_key(title))
    {
        return Err("Шаблон с таким названием уже есть. Выберите его и включите перезапись");
    }
    Ok((title.to_owned(), None))
}

fn template_label(row: &VisitTemplateSummary, rows: &[VisitTemplateSummary]) -> String {
    if rows
        .iter()
        .filter(|other| title_key(&other.title) == title_key(&row.title))
        .count()
        > 1
    {
        let number = rows.iter().filter(|other| title_key(&other.title) == title_key(&row.title)).position(|other| other.id == row.id).map_or(0, |index| index + 1);
        format!("{} · вариант {} · {}", row.title, number, row.updated_at.get(..10).unwrap_or(&row.updated_at))
    } else {
        row.title.clone()
    }
}

#[cfg(test)]
mod save_choice_tests {
    use super::{save_choice, template_label, Uuid, VisitTemplateSummary};
    fn entry(id: u128, title: &str) -> VisitTemplateSummary {
        VisitTemplateSummary {
            id: Uuid::from_u128(id),
            title: title.into(),
            row_version: 1,
            updated_at: "2030-01-02T09:00:00Z".into(),
        }
    }
    #[test]
    fn duplicate_name_requires_explicit_overwrite_even_with_case_and_spaces() {
        let rows = vec![entry(1, "Щ1")];
        assert!(save_choice("  щ1  ", false, None, &rows).is_err());
        assert!(save_choice("Новый", false, None, &rows).is_ok());
    }
    #[test]
    fn overwriting_selected_template_keeps_its_identity_and_title() {
        let rows = vec![entry(1, "Щ1"), entry(2, "Другой")];
        let (title, chosen) =
            save_choice("Осталось от прежнего ввода", true, Some(&rows[1]), &rows).unwrap();
        assert_eq!(title, "Другой");
        assert_eq!(chosen.unwrap().id, rows[1].id);
        assert!(save_choice("", true, None, &rows).is_err());
    }
    #[test]
    fn duplicate_names_remain_distinguishable_and_unique_names_stay_clean() {
        let rows = vec![entry(1, "Щ1"), entry(2, "щ1"), entry(3, "Другой")];
        let a = template_label(&rows[0], &rows);
        let b = template_label(&rows[1], &rows);
        assert_ne!(a, b);
        assert!(a.contains("вариант 1"));
        assert_eq!(template_label(&rows[2], &rows), "Другой");
    }
}

#[derive(Debug, Clone)]
pub enum VisitTemplatePanelEvent {
    Refresh,
    Confirming,
    Apply(VisitTemplateSummary),
    Clear,
    Save {
        title: String,
        overwrite: Option<VisitTemplateSummary>,
    },
}

pub struct VisitTemplatePanel {
    templates: Vec<VisitTemplateSummary>,
    selected: Option<Uuid>,
    overwrite: bool,
    editing: bool,
    busy: bool,
    readonly: bool,
    note: String,
    note_error: bool,
    pending: Option<VisitTemplatePanelEvent>,
    title: Entity<InputState>,
    _subscription: Subscription,
}

impl EventEmitter<VisitTemplatePanelEvent> for VisitTemplatePanel {}

impl VisitTemplatePanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Название шаблона приёма"));
        let subscription = cx.subscribe(&title, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        Self {
            templates: Vec::new(),
            selected: None,
            overwrite: false,
            editing: false,
            busy: false,
            readonly: true,
            note: String::new(),
            note_error: false,
            pending: None,
            title,
            _subscription: subscription,
        }
    }

    /// Replace server metadata; no protocol data is retained by this panel.
    pub fn set_templates(&mut self, templates: Vec<VisitTemplateSummary>, cx: &mut Context<Self>) {
        self.templates = templates;
        if self.selected().is_none() {
            self.selected = None;
            self.overwrite = false;
        }
        cx.notify();
    }
    pub fn clear_pending(&mut self, cx: &mut Context<Self>) {
        self.pending = None;
        self.selected = None;
        self.overwrite = false;
        self.editing = false;
        cx.notify();
    }

    /// The owner must update these gates when the study or operation changes.
    pub fn set_state(&mut self, busy: bool, readonly: bool, cx: &mut Context<Self>) {
        if self.busy == busy && self.readonly == readonly {
            return;
        }
        self.busy = busy;
        self.readonly = readonly;
        if readonly {
            self.pending = None;
        }
        cx.notify();
    }

    pub fn set_message(&mut self, message: impl Into<String>, error: bool, cx: &mut Context<Self>) {
        self.note = message.into();
        self.note_error = error;
        cx.notify();
    }

    /// Only a matching, confirmed server save closes the editor.
    pub fn save_confirmed(&mut self, cx: &mut Context<Self>) {
        self.editing = false;
        self.overwrite = false;
        cx.notify();
    }

    fn selected(&self) -> Option<&VisitTemplateSummary> {
        self.templates.iter().find(|t| Some(t.id) == self.selected)
    }

    fn emit_action(&mut self, event: VisitTemplatePanelEvent, cx: &mut Context<Self>) {
        if self.busy || self.readonly {
            return;
        }
        self.pending = Some(event);
        cx.emit(VisitTemplatePanelEvent::Confirming);
        self.note.clear();
        self.note_error = false;
        cx.notify();
    }
}

impl Render for VisitTemplatePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let disabled = self.busy || self.readonly || self.pending.is_some();
        let selected = self.selected().cloned();
        let caption = selected
            .as_ref()
            .map(|t| template_label(t, &self.templates))
            .unwrap_or_else(|| "Выберите шаблон приёма".into());
        let templates = self.templates.clone();
        let entity = cx.entity();
        let title = self.title.read(cx).value().trim().to_owned();
        v_flex().w_full().gap_2().p_3().border_1().border_color(cx.theme().border).rounded_md()
            .child(h_flex().items_center().justify_between().child(div().font_semibold().child("Заполнить приём по шаблону"))
                .child(Button::new("visit-template-refresh").ghost().icon(IconName::RefreshCw).tooltip("Обновить список").disabled(self.busy || self.pending.is_some())
                    .on_click(cx.listener(|s, _, _, cx| { if !s.busy {s.busy=true;cx.emit(VisitTemplatePanelEvent::Refresh);cx.notify();} }))))
            .child(Button::new("visit-template-select").outline().w_full().justify_start()
                .label(caption.clone()).icon(IconName::ChevronDown)
                .disabled(disabled || templates.is_empty())
                .dropdown_menu(move |mut menu, _, _| {
                    for metadata in &templates {
                        let entity = entity.clone(); let metadata = metadata.clone();
                        let label=template_label(&metadata,&templates);
                        menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                            entity.update(cx, |s, cx| {
                                if s.busy || s.readonly { return; }
                                s.selected = Some(metadata.id);
                                cx.notify();
                            });
                        }));
                    }
                    menu.max_h(px(280.)).scrollable(true)
                }))
            .child(h_flex().gap_2()
                .child(Button::new("visit-template-apply").primary().label("Заполнить")
                    .disabled(disabled || selected.is_none())
                    .on_click(cx.listener(|s, _, _, cx| {
                        if let Some(template) = s.selected().cloned() { s.emit_action(VisitTemplatePanelEvent::Apply(template), cx); }
                    })))
                .child(Button::new("visit-template-clear").outline().label("Очистить")
                    .disabled(disabled).on_click(cx.listener(|s, _, _, cx| s.emit_action(VisitTemplatePanelEvent::Clear, cx)))))
            .when(self.pending.is_some(), |d| d.child(v_flex().gap_1().p_2().border_1().border_color(cx.theme().danger)
                .child(match self.pending.as_ref().unwrap() {
                    VisitTemplatePanelEvent::Apply(t)=>format!("Заменить поля приёма шаблоном «{}»?",template_label(t,&self.templates)),
                    VisitTemplatePanelEvent::Clear=>"Очистить заполненные поля приёма?".into(),
                    VisitTemplatePanelEvent::Save {overwrite:Some(t),..}=>format!("Перезаписать общий шаблон «{}»?",template_label(t,&self.templates)),
                    VisitTemplatePanelEvent::Save {title,overwrite:None}=>format!("Сохранить общий шаблон «{}» для всех врачей?",title),
                    _=>"Подтвердить действие?".into(),
                })
                .child(div().text_sm().child("Даты и снимки сохраняются. Шаблоны доступны всем врачам организации."))
                .child(h_flex().gap_2()
                    .child(Button::new("visit-template-confirm").primary().label("Подтвердить").on_click(cx.listener(|s, _, _, cx| {
                        if s.busy || s.readonly { return; }
                        if let Some(event)=s.pending.take() { s.busy=true; cx.emit(event); cx.notify(); }
                    })))
                    .child(Button::new("visit-template-cancel").outline().label("Отмена").on_click(cx.listener(|s, _, _, cx| {s.pending=None;cx.notify();}))))))
            .child(Button::new("visit-template-save-open").outline().w_full().label(if self.editing {"Отменить сохранение"} else {"Сохранить как шаблон"})
                .disabled(disabled).on_click(cx.listener(|s,_,_,cx|{s.editing=!s.editing;cx.notify();})))
            .when(self.editing, |d| d.child(v_flex().gap_2()
              .when(!self.overwrite, |d| d.child(Input::new(&self.title).aria_label("Название шаблона приёма").disabled(disabled)))
              .when(self.overwrite, |d| d.child(div().text_sm().child(format!("Перезаписывается: {}", caption))))
              .child(Switch::new("visit-template-overwrite").label("Перезаписать выбранный шаблон")
                .checked(self.overwrite).disabled(disabled || selected.is_none())
                .on_change(cx.listener(|s, value, _, cx| {
                    if !s.busy && !s.readonly { s.overwrite = *value && s.selected().is_some(); cx.notify(); }
                })))
              .child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child(if self.overwrite { "Содержимое выбранного общего шаблона будет заменено для всех врачей организации." }
                    else { "Будет создан новый общий шаблон, доступный всем врачам организации." }))
              .child(Button::new("visit-template-save").outline().label("Сохранить")
                .disabled(disabled || (title.is_empty() && (!self.overwrite || selected.is_none())) || (self.overwrite && selected.is_none()))
                .on_click(cx.listener(|s, _, _, cx| {
                    let title=s.title.read(cx).value().to_string();
                    match save_choice(&title,s.overwrite,s.selected(),&s.templates) {
                        Ok((title,overwrite))=>s.emit_action(VisitTemplatePanelEvent::Save { title, overwrite }, cx),
                        Err(error)=>{s.note=error.into();s.note_error=true;cx.notify();},
                    }
                })))))
            .when(self.busy, |d| d.child(div().text_sm().text_color(cx.theme().muted_foreground).child("Выполняется операция с шаблоном…")))
            .when(self.readonly, |d| d.child(div().text_sm().text_color(cx.theme().muted_foreground).child("Изменение приёма недоступно в режиме просмотра")))
            .when(!self.note.is_empty(), |d| d.child(div().text_sm()
                .when(self.note_error, |d| d.text_color(cx.theme().danger)).child(self.note.clone())))
    }
}
