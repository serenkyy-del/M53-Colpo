//! Optional instruction UI. Clinical states and actions never depend on this module.
use super::*;
use m53_client::ui_guidance::{Action, ContextToken, Tip, TipId, TIPS};

impl Workplace {
    pub(super) fn guidance_action(&mut self, expected: &Option<ContextToken>, action: Action, cx: &mut Context<Self>) -> bool {
        if self.guidance_pending || &self.guidance.context != expected { return false; }
        if let Some(request) = self.guidance.request(action) {
            self.guidance_pending = true;
            if self.worker.send(Command::Guidance(request)).is_err() {
                self.guidance_pending = false;
                self.guidance.error = "Не удалось сохранить настройку помощи: локальный обработчик недоступен".into();
            }
            cx.notify();
            return self.guidance_pending;
        }
        false
    }

    fn contextual_tip(&self, cx: &Context<Self>) -> Option<&'static Tip> {
        if self.guidance_closed || self.state["authenticated"] != true { return None; }
        if self.detail.as_ref().is_some_and(|d| d.study.status == "approved")
            && matches!(self.screen, Screen::Study | Screen::Protocol) { return None; }
        let ids: &[TipId] = match self.screen {
            Screen::Patients => &[TipId::PatientsTask],
            Screen::Study if self.video_running || self.editor.read(cx).session.photos.iter().all(|p| p.removed) => &[TipId::StudyCapture],
            Screen::Study if self.editor.read(cx).selected.is_some() => &[TipId::StudyMarkup, TipId::StudyToProtocol, TipId::StudyPhotoSelection],
            Screen::Study => &[TipId::StudyPhotoSelection, TipId::StudyMarkup, TipId::StudyToProtocol],
            Screen::Protocol if self.form.as_ref().is_some_and(|f| f.read(cx).active_phrase_target().is_some()) => &[TipId::ProtocolPhrases, TipId::ProtocolSections, TipId::ProtocolRequired],
            Screen::Protocol => &[TipId::ProtocolSections, TipId::ProtocolRequired, TipId::ProtocolPhrases],
            Screen::Report if self.detail.as_ref().is_some_and(|d| d.study.status == "approved") => &[TipId::ReportDelivery],
            // The permanent draft notice already explains this guard and offers the next action.
            Screen::Report => &[],
            Screen::Settings => &[],
        };
        ids.iter().find_map(|id| self.guidance.should_show(*id).then(|| TIPS.iter().find(|tip| tip.id == *id)).flatten())
    }

    pub(super) fn guidance_card(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let tip = self.contextual_tip(cx)?;
        let id = tip.id;
        let entity = cx.entity();
        let context = self.guidance.context.clone();
        let complete_context = context.clone();
        let close_context = context.clone();
        let colors = crate::appearance::guidance_colors(cx);
        let button_style = gpui_kit::component::button::ButtonCustomVariant::new(cx)
            .color(colors.background)
            .foreground(colors.foreground)
            .hover(colors.hover)
            .active(colors.active);
        Some(h_flex().id("guidance-card").test_support().items_center().gap_3().px_4().py_2().flex_shrink_0()
            .border_b_1().border_color(colors.border).bg(colors.background).text_color(colors.foreground)
            .child(gpui_kit::component::Icon::new(IconName::Lightbulb).size(px(22.)).text_color(colors.foreground))
            .child(v_flex().flex_1().min_w_0().gap_1()
                .child(div().text_sm().font_semibold().child(tip.title))
                // Reserve two lines: changing the contextual explanation must not move the field.
                .child(div().id("guidance-body").text_sm().h(px(40.)).line_clamp(2).text_color(colors.secondary_foreground).child(tip.body)
                    .tooltip(move |w,cx| gpui_kit::component::tooltip::Tooltip::new(tip.body).build(w,cx))))
            .child(Button::new("guidance-understood").custom(button_style).small().label("Понятно")
                .border_1().border_color(colors.border)
                .text_color(if self.guidance_pending {colors.secondary_foreground} else {colors.foreground})
                .disabled(self.guidance_pending)
                .on_click(cx.listener(move |s, _, _, cx| {
                    if s.guidance_action(&complete_context, Action::Complete(id), cx) {s.guidance_closed = true;}
                })))
            .child(Button::new("guidance-dismiss").custom(button_style).small().label("Больше не показывать")
                .border_color(cx.theme().transparent)
                .text_color(if self.guidance_pending || !self.guidance.writable {colors.secondary_foreground} else {colors.foreground})
                .accessibility_label("Больше не показывать эту подсказку")
                .tooltip("Скрыть эту подсказку для вас на этом рабочем месте. Все подсказки отключаются через лампочку слева")
                .disabled(self.guidance_pending || !self.guidance.writable)
                .on_click(move |_,_,cx| entity.update(cx, |s,cx| {
                    if s.guidance_action(&context, Action::Dismiss(id),cx) {s.guidance_closed=true;}
                })))
            .child(Button::new("guidance-close").custom(button_style).small().icon(IconName::X)
                .border_color(cx.theme().transparent)
                .accessibility_label("Скрыть подсказку до следующего этапа")
                .tooltip("Скрыть до следующего этапа. Все подсказки можно отключить в настройках")
                .on_click(cx.listener(move |s, _, _, cx| {if s.guidance.context==close_context {s.guidance_closed = true;cx.notify();}})))
            .into_any_element())
    }

    pub(super) fn guidance_settings(&self, cx: &Context<Self>) -> AnyElement {
        let enabled_context=self.guidance.context.clone();
        let reset_context=enabled_context.clone();
        v_flex().gap_2().p_3().rounded_md().border_1().border_color(cx.theme().border)
            .child(div().font_semibold().child("Помощь в работе"))
            .child(gpui_kit::component::switch::Switch::new("guidance-enabled")
                .label("Показывать подсказки по этапам")
                .checked(self.guidance.preferences.enabled)
                .disabled(!self.guidance.writable || self.guidance_pending)
                .on_change(cx.listener(move |s, enabled, _, cx| {
                    if s.guidance_action(&enabled_context, Action::SetEnabled(*enabled), cx) {s.guidance_closed = false;}
                })))
            .child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child("Настройка сохраняется для вас на этом рабочем месте и доступна офлайн. Проверки полей и сообщения об ошибках остаются видимыми."))
            .child(Button::new("guidance-reset").outline().small().label("Показать подсказки снова")
                .disabled(!self.guidance.writable || self.guidance_pending)
                .on_click(cx.listener(move |s, _, _, cx| {if s.guidance_action(&reset_context, Action::ResetProgress, cx) {s.guidance_closed=false;}})))
            .when(!self.guidance.error.is_empty(), |d| d.child(div().text_sm().text_color(cx.theme().danger).child(self.guidance.error.clone())))
            .when(self.guidance.context.is_none(), |d| d.child(div().text_sm().text_color(cx.theme().muted_foreground).child("Войдите под врачом, чтобы настроить подсказки.")))
            .into_any_element()
    }
}
