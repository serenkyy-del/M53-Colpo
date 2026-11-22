//! Shared application chrome; workflow availability remains in Workplace.
use super::*;

impl Workplace {
    pub(super) fn application_header(&self, cx: &Context<Self>) -> AnyElement {
        let authenticated = self.state["authenticated"] == true;
        let doctor = self.state["doctor"].as_str().unwrap_or("").to_owned();
        let guidance_context = self.guidance.context.clone();
        let guidance_enabled = self.guidance.preferences.enabled;
        let mut guidance_tooltip = if guidance_enabled {"Подсказки включены · выключить"} else {"Подсказки выключены · включить"}.to_owned();
        if !self.guidance.error.is_empty() {guidance_tooltip.push_str(&format!(" · {}", self.guidance.error));}
        let mut bar = h_flex().id("application-header").test_support().min_h(px(56.))
            .flex_shrink_0().flex_wrap().px_4().py_2().gap_2().items_center()
            .border_b_1().border_color(cx.theme().border)
            .child(h_flex().gap_2().items_center()
                .child(gpui_kit::component::Icon::new(IconName::Camera).text_color(rgb(0x219ce8)))
                .child(div().font_semibold().child(if !cfg!(feature = "standalone") && self.viewport.width < px(1100.) { "М53" } else { m53_client::APP_NAME })))
            .child(Button::new("help-settings").ghost().small()
                .icon(gpui_kit::component::Icon::new(if guidance_enabled {IconName::Lightbulb} else {IconName::LightbulbOff})
                    .text_color(if guidance_enabled {
                        if cx.theme().is_dark() {cx.theme().warning} else {rgb(0xa16207).into()}
                    } else {cx.theme().muted_foreground}))
                .label("Подсказки").accessibility_label(if guidance_enabled {"Выключить подсказки"} else {"Включить подсказки"})
                .selected(guidance_enabled)
                .disabled(self.guidance_pending || !self.guidance.writable)
                .tooltip(guidance_tooltip)
                .on_click(cx.listener(move |s, _, _, cx| {
                    if s.guidance_action(&guidance_context, m53_client::ui_guidance::Action::SetEnabled(!guidance_enabled), cx) {
                        s.guidance_closed = false;
                    }
                })));
        if authenticated && self.detail.is_none() {
            bar = bar.child(Button::new("nav-patients").ghost().small().label("Пациенты")
                .selected(self.screen == Screen::Patients)
                .on_click(cx.listener(|s, _, _, cx| s.navigate(Screen::Patients, cx))));
        }
        bar = bar.child(div().flex_1())
            .when(authenticated, |bar| bar.child(div().id("current-doctor").min_w_0()
                .max_w(px(if self.viewport.width < px(1280.) {220.} else {320.}))
                .truncate().text_sm().test_support().child(doctor.clone())
                .tooltip(move |w,cx| gpui_kit::component::tooltip::Tooltip::new(doctor.clone()).build(w,cx))))
            .child(Button::new("pedal-status").ghost().small().icon(IconName::Footprints)
                .accessibility_label(format!("Педаль: {}", self.pedal_connection.label()))
                .text_color(if self.pedal_connection == PedalConnection::Connected {cx.theme().success} else {cx.theme().muted_foreground})
                .tooltip(format!("Педаль: {} · открыть настройки", self.pedal_connection.label()))
                .on_click(cx.listener(|s, _, _, cx| s.navigate(Screen::Settings, cx))));
        if authenticated && self.detail.is_some() && (self.history.is_none() || self.approved_history()) {
            let close_only = self.screen == Screen::Report && (self.approved_history() || !self.study_changed(cx));
            bar = bar.child(Button::new("save-and-exit").outline().small()
                .label(close_study_label(close_only, self.workflow_request.is_some()))
                .tooltip(if close_only {"Закрыть отчёт и вернуться к списку пациентов"}
                    else {"Сохранить на компьютере и вернуться к списку пациентов"})
                .disabled(self.workflow_request.is_some() || self.report_busy || self.editor.read(cx).io_pending > 0)
                .on_click(cx.listener(|s,_,_,cx| s.request_workflow(WorkflowAction::SaveAndExit,cx))));
        } else if authenticated && self.history.is_none() {
            bar = bar.child(Button::new("logout").ghost().small().label(if self.logging_out {"Выход…"} else {"Выйти"})
                .disabled(self.logging_out || self.report_busy || self.editor.read(cx).io_pending > 0)
                .on_click(cx.listener(|s,_,_,cx| {
                    if s.patient_editor.is_some()||s.patient_edit_request.is_some()||s.patient_editor_pending.is_some(){s.message="Сохраните или отмените изменение карточки пациента перед выходом".into();cx.notify();return;}
                    if s.phrase_panel.read(cx).has_unsaved(cx) {s.message="Сохраните или отмените изменения шаблона перед выходом".into();cx.notify();return;}
                    s.flush_form(cx,true);s.logging_out=true;
                    s.editor.update(cx,|e,_| e.readonly=true);
                    if let Some(form)=&s.form {form.update(cx,|f,_| f.readonly=true);}
                    s.send(Command::Logout);cx.notify();
                })));
        }
        bar = bar.child(Button::new("settings").ghost().small().icon(IconName::Settings2)
            .accessibility_label("Настройки").tooltip("Настройки")
            .on_click(cx.listener(|s,_,_,cx| s.navigate(Screen::Settings,cx))))
            .child(h_flex().id("theme-control").test_support().gap_2().items_center()
                .tooltip(|w,cx| gpui_kit::component::tooltip::Tooltip::new("Светлая / тёмная тема").build(w,cx))
                .child(gpui_kit::component::Icon::new(IconName::Sun).text_color(cx.theme().muted_foreground))
                .child(gpui_kit::component::switch::Switch::new("theme-switch").small()
                    .accessibility_label("Тёмная тема").checked(cx.theme().is_dark())
                    .on_change(cx.listener(|s,dark,window,cx| {
                        s.send(Command::Activity(m53_client::activity::Action::new("settings.changed").field("theme")));
                        crate::appearance::choose(*dark,window,cx);cx.notify();
                    })))
                .child(gpui_kit::component::Icon::new(IconName::Moon).text_color(cx.theme().muted_foreground)));
        bar.into_any_element()
    }

    pub(super) fn patient_summary(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if self.state["authenticated"] != true || matches!(self.screen, Screen::Patients | Screen::Settings) {return None;}
        let detail = self.detail.as_ref()?;
        let approved = detail.study.status == "approved" || self.approved_history();
        let patient=if self.approved_history() {
            self.history.as_ref().and_then(|history|m53_client::report_identity::patient(&history.metadata).ok())
        } else if approved {m53_client::report_identity::bind(detail,&self.state["protocol_metadata"]).ok().map(|(bound,_)|bound.patient)}else{Some(detail.patient.clone())};
        let (name,identifiers,description)=if let Some(patient)=patient.as_ref(){
            (patient_name(patient),format!("Внешний ID: {}",patient.external_id.as_deref().unwrap_or("не передан")),
                format!("ДР: {} · Карта № {} · {}",display_date(patient.birth_date.as_deref()),patient.card_number.as_deref().filter(|s|!s.trim().is_empty()).unwrap_or("не указана"),
                    m53_client::patient_demographics::display_wire_sex(patient.sex,if approved{None}else{self.patient_sexes.get(&patient.id).copied()})))
        }else{("Данные утверждённой редакции загружаются".into(),"Сведения пациента этой редакции ещё не загружены".into(),String::new())};
        Some(h_flex().id("patient-summary").test_support().items_center().gap_3().px_4().py_1().flex_shrink_0()
            .border_b_1().border_color(cx.theme().border)
            .child(h_flex().flex_1().min_w_0().gap_3().items_center()
                .child(div().id("open-patient-name").max_w(px(self.viewport.width.as_f32()*0.38)).truncate().text_sm().font_semibold().child(name.clone())
                    .tooltip(move |w,cx| gpui_kit::component::tooltip::Tooltip::new(name.clone()).build(w,cx)))
                .child(div().id("patient-description").flex_1().min_w_0().text_xs().text_color(cx.theme().muted_foreground).truncate().child(description.clone())
                    .tooltip(move |w,cx| gpui_kit::component::tooltip::Tooltip::new(description.clone()).build(w,cx))))
            .child(Button::new("patient-identifiers").ghost().small().icon(IconName::Info)
                .accessibility_label(identifiers.clone()).tooltip(identifiers))
            .child(div().text_xs().px_2().py_1().rounded_sm().bg(cx.theme().muted)
                .text_color(if approved {cx.theme().success} else {cx.theme().muted_foreground})
                .child(if approved {"Утверждён"} else {"Черновик"})).into_any_element())
    }

    pub(super) fn stage_navigation(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if self.state["authenticated"] != true || self.detail.is_none() || self.screen == Screen::Settings {return None;}
        let viewing_approved_history = self.history.as_ref().is_some_and(|history| history.prefer_approved);
        let mut row = h_flex().id("study-stages").test_support().gap_2().px_4().py_1().items_center().flex_shrink_0()
            .border_b_1().border_color(cx.theme().border);
        if viewing_approved_history {
            row=row.child(Button::new("approved-revisions-toggle").outline().small().label("Редакции")
                .on_click(cx.listener(|s,_,_,cx|{
                    if let Some(history)=s.history.as_mut(){history.revisions_expanded = !history.revisions_expanded;}
                    cx.notify();
                })));
        }
        if !viewing_approved_history && let Some(study)=self.detail.as_ref().map(|d|d.study.id){row=row.child(Button::new("current-study-history").outline().small().label("Редакции").disabled(self.workflow_request.is_some()||self.approval_busy).on_click(cx.listener(move|s,_,_,cx|s.open_history(study,cx))));}
        for (index,(screen,label)) in [(Screen::Study,"Обследование"),(Screen::Protocol,"Протокол"),(Screen::Report,"Отчет")].into_iter().enumerate() {
            if index > 0 {row=row.child(gpui_kit::component::Icon::new(IconName::ChevronRight).small().text_color(cx.theme().muted_foreground));}
            let available=if viewing_approved_history {true} else {self.step_allowed(screen,cx)};
            row=row.child(Button::new(SharedString::from(format!("nav-{label}"))).ghost().small().flex_1()
                .selected(self.screen==screen).disabled(!available || self.workflow_request.is_some())
                .accessibility_label(format!("{}. {label}",index+1))
                .child(h_flex().gap_2().items_center()
                    .child(div().size(px(22.)).rounded_full().bg(cx.theme().muted).flex().items_center().justify_center().child((index+1).to_string()))
                    .child(if screen==Screen::Report {"Отчёт"} else {label}))
                .tooltip(if available {"Открыть этап"} else if screen==Screen::Protocol {"Сначала завершите обследование"} else {"Сначала завершите протокол"})
                .on_click(cx.listener(move |s,_,_,cx| {
                    if viewing_approved_history {
                        s.screen=screen; cx.notify();
                    } else { s.navigate(screen,cx); }
                })));
        }
        Some(row.into_any_element())
    }
}

fn close_study_label(close_only: bool, closing: bool) -> &'static str {
    match (close_only, closing) {
        (true, false) => "Закрыть отчёт",
        (true, true) => "Закрытие…",
        (false, false) => "Сохранить и выйти",
        (false, true) => "Сохранение…",
    }
}

#[cfg(test)]
mod close_label_tests {
    #[test]
    fn clean_report_closes_and_changes_keep_the_save_action() {
        use super::close_study_label;
        assert_eq!(close_study_label(true, false), "Закрыть отчёт");
        assert_eq!(close_study_label(false, false), "Сохранить и выйти");
        assert_eq!(close_study_label(true, true), "Закрытие…");
    }
}
