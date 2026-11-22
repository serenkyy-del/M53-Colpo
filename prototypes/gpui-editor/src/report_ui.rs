use super::*;
use std::sync::Arc;
use crate::report_view::{PAGE_WIDTH, PageSize, Zoom};
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::{ElementExt, scroll::ScrollableElement};

const PAGE_GAP: f32 = 20.;
const PAGE_PADDING: f32 = 20.;

fn page_dimensions(zoom: Zoom, width: f32, height: f32) -> PageSize {
    // Leave room for the scrollbars as well as the paper's outer padding.
    zoom.dimensions(
        (width - PAGE_PADDING * 2. - 16.).max(1.),
        (height - PAGE_PADDING * 2.).max(1.),
    )
}

impl Workplace {
    fn visible_report_pages(&self) -> &[Arc<RenderImage>] {
        if self.approved_history() {
            #[cfg(windows)]
            if let Some(Ok(pdf)) = self.history.as_ref().and_then(|history|history.pdf_pages.as_ref()) {
                return &pdf.pages;
            }
            return &[];
        }
        &self.report_pages
    }

    fn visible_report_lines(&self) -> &[String] {
        if self.approved_history() {
            #[cfg(windows)]
            { return &[]; }
            #[cfg(not(windows))]
            return self.history.as_ref().and_then(|history| history.report_lines.as_ref())
                .and_then(|result| result.as_ref().ok()).map(Vec::as_slice).unwrap_or_default();
        }
        &self.report_lines
    }

    fn report_page_count(&self) -> usize {
        if self.visible_report_pages().is_empty() {
            self.visible_report_lines().len().div_ceil(53)
                + if self.approved_history() {0} else {self.report_photos.len()}
        } else {
            self.visible_report_pages().len()
        }
    }

    fn report_page_dimensions(&self) -> PageSize {
        let measured = self.report_scroll.bounds().size;
        let width = if measured.width > px(0.) {
            measured.width.as_f32()
        } else {
            let panels = crate::report_view::panel_widths(self.viewport.width.as_f32());
            self.viewport.width.as_f32() - panels.navigation - panels.delivery
        };
        let height = if measured.height > px(0.) {
            measured.height.as_f32()
        } else {
            (self.viewport.height.as_f32() - 260.).max(1.)
        };
        page_dimensions(self.report_view.zoom, width, height)
    }

    fn scroll_report_page(&mut self, cx: &mut Context<Self>) {
        let count = self.report_page_count();
        self.report_view.set_page(self.report_view.page, count);
        let size = self.report_page_dimensions();
        self.report_scroll.set_offset(point(
            px(0.),
            px(-self.report_view.page_offset(size, PAGE_GAP)),
        ));
        cx.notify();
    }

    pub(super) fn report_delivery_context(&self) -> crate::report_delivery::Context<'_> {
        crate::report_delivery::Context {
            approved: self
                .detail
                .as_ref()
                .is_some_and(|d| d.study.status == "approved"),
            revision_id: self.detail.as_ref().and_then(|d| d.revision_id),
            template_version_id: self.templates.first().map(|t| t.version_id),
            preview_identity: self.report_identity.as_ref(),
        }
    }

    fn report_navigation(
        &self,
        view: crate::report_view::ReportView,
        count: usize,
        scale: f32,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let thumb = crate::report_view::thumbnail_dimensions((width - 48.).min(96.), 136.);
        let mut list = v_flex().gap_1().p_2();
        for index in 0..count {
            let preview = if let Some(page) = self.visible_report_pages().get(index) {
                img(page.clone())
                    .w(px(thumb.width))
                    .h(px(thumb.height))
                    .object_fit(ObjectFit::Contain)
                    .into_any_element()
            } else {
                div()
                    .w(px(thumb.width))
                    .h(px(thumb.height))
                    .bg(cx.theme().secondary)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(gpui_kit::component::Icon::new(IconName::FileText).small())
                    .into_any_element()
            };
            list = list.child(
                Button::new(SharedString::from(format!("report-page-{index}")))
                    .ghost()
                    .w_full()
                    .h(px(174.))
                    .px_2()
                    .justify_start()
                    .accessibility_label(format!("Перейти к странице {} отчёта", index + 1))
                    .selected(view.page == index)
                    .child(
                        v_flex()
                            .w_full()
                            .items_center()
                            .gap_2()
                            .child(preview)
                            .child(div().w_full().text_center().text_sm().child(format!("Страница {}", index + 1))),
                    )
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.report_view.set_page(index, s.report_page_count());
                        s.scroll_report_page(cx);
                    })),
            );
        }
        v_flex()
            .id("report-navigation-sidebar")
            .w(px(width))
            .flex_shrink_0()
            .h_full()
            .min_h_0()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                v_flex()
                    .p_2()
                    .gap_1()
                    .child(crate::ui_chrome::panel_heading("Страницы", cx).id("report-pages-heading").test_support())
                    .child(
                        h_flex()
                            .w_full().h(px(32.)).rounded_md().bg(cx.theme().secondary)
                            .items_center()
                            .justify_between()
                            .child(
                                Button::new("report-previous-page")
                                    .ghost()
                                    .small()
                                    .icon(IconName::ChevronLeft)
                                    .accessibility_label("Предыдущая страница отчёта")
                                    .tooltip(format!("Предыдущая страница · {}",view.page_label(count)))
                                    .disabled(count == 0 || view.page == 0)
                                    .on_click(cx.listener(|s, _, _, cx| {
                                        s.report_view.previous();
                                        s.scroll_report_page(cx);
                                    })),
                            )
                            .child(div().flex_1().text_center().text_xs().child(if count == 0 {
                                "Нет страниц".to_string()
                            } else {
                                format!("{} из {count}", view.page + 1)
                            }))
                            .child(
                                Button::new("report-next-page")
                                    .ghost()
                                    .small()
                                    .icon(IconName::ChevronRight)
                                    .accessibility_label("Следующая страница отчёта")
                                    .tooltip("Следующая страница")
                                    .disabled(count == 0 || view.page + 1 >= count)
                                    .on_click(cx.listener(|s, _, _, cx| {
                                        s.report_view.next(s.report_page_count());
                                        s.scroll_report_page(cx);
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .id("report-page-list")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(list),
            )
            .child(
                v_flex()
                    .p_2()
                    .gap_1()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .mb_1()
                            .child(div().text_sm().font_semibold().child("Масштаб"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{}%", (scale * 100.).round() as u32)),
                            ),
                    )
                    .children(
                        [Zoom::FitWidth, Zoom::FitPage, Zoom::ActualSize]
                            .into_iter()
                            .enumerate()
                            .map(|(i, zoom)| {
                                Button::new(SharedString::from(format!("report-zoom-{i}")))
                                    .ghost()
                                    .small()
                                    .w_full()
                                    .justify_start()
                                    .label(zoom.label())
                                    .selected(self.report_view.zoom == zoom)
                                    .on_click(cx.listener(move |s, _, _, cx| {
                                        s.report_view.zoom = zoom;
                                        s.scroll_report_page(cx);
                                    }))
                                    .into_any_element()
                            }),
                    ),
            )
            .test_support()
            .into_any_element()
    }

    fn report_delivery_panel(
        &self,
        shown: &crate::report_delivery::Presentation<'_>,
        locked: bool,
        label: &str,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        use crate::report_delivery::{State, Tone};
        let delivered = shown.state == State::CurrentDelivered;
        let color = match shown.tone() {
            Tone::Success => cx.theme().success,
            Tone::Danger => cx.theme().danger,
            _ => cx.theme().muted_foreground,
        };
        let mut content = v_flex()
            .p_3()
            .gap_3()
            .min_w_0()
            .child(crate::ui_chrome::panel_heading("Передача в МИС", cx).id("report-delivery-heading").test_support())
            .child(
                v_flex()
                    .p_2().rounded_md().bg(cx.theme().secondary)
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_start()
                            .text_color(color)
                            .child(
                                gpui_kit::component::Icon::new(if delivered {
                                    IconName::Check
                                } else {
                                    IconName::Info
                                })
                                .small(),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .font_semibold()
                                    .child(shown.title),
                            ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(shown.explanation),
                    ),
            );
        if let Some(job) = shown.current_job.filter(|_| delivered) {
            let action = match job["receipt"]["action"].as_str() {
                Some("created") => "Приём создан",
                Some("attached") => "Результат добавлен к приёму",
                Some("replaced") => "Результат приёма обновлён",
                _ => "Результат сохранён в МИС",
            };
            let number = job["receipt"]["reception_number"].as_str().unwrap_or("—");
            let time = job["delivered_at"]
                .as_str()
                .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
                .map(|v| {
                    v.with_timezone(&chrono::Local)
                        .format("%d.%m.%Y %H:%M")
                        .to_string()
                })
                .unwrap_or_default();
            content = content.child(
                v_flex()
                    .gap_1()
                    .text_sm()
                    .child(action)
                    .child(format!("Приём № {number}"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("Подтверждено 1С · {time}")),
                    ),
            );
        }
        content=content.child(Button::new("send-result-1c").outline().small().px_2().w_full()
            .when(delivered,|b|b.success().bg(cx.theme().button_success).text_color(cx.theme().button_success_foreground))
            .accessibility_label("Отправить в МИС 1С")
            .child(h_flex().w_full().min_w_0().gap_1().items_center()
                .child(gpui_kit::component::Icon::new(IconName::ArrowRight).small())
                .child(div().px_1().rounded_sm().bg(rgb(0xfff1a8)).text_color(rgb(0xc92b27)).font_bold().child("1С"))
                .child(div().flex_1().min_w_0().truncate().text_sm().child(match shown.state {
                    State::CurrentDelivered=>"Доставлено в МИС",
                    State::CurrentPending if shown.current_job.is_some_and(|job|job["state"]=="retry")=>"Повтор отправки",
                    State::CurrentPending=>"В очереди",
                    _=>"Отправить в МИС"})))
            .tooltip(if self.detail.as_ref().is_none_or(|d|!d.mis_task_bound) {"Отправка доступна только для обследования по заданию из МИС. Связь с заданием не подтверждена.".to_string()}
                else if locked {format!("{label}. Повторная отправка этой версии не требуется")} else {"Отправить готовый PDF в МИС 1С после утверждения протокола".to_string()})
            .disabled(self.report_busy||self.preview_request.is_some()||locked||self.detail.as_ref().is_none_or(|d|d.study.status!="approved"||!d.mis_task_bound))
            .on_click(cx.listener(|s,_,_,cx|{
                if let Some(detail)=&s.detail {
                    if let Err(error)=m53_client::delivery::require_mis_task(detail) {s.message=error;cx.notify();return;}
                    let study=detail.study.id;
                    let selected=s.editor.read(cx).session.photos.iter().filter(|p|!p.removed&&p.in_report).map(|p|p.id).collect();
                    let Some(template)=s.templates.first().cloned() else {s.message="Шаблон протокола недоступен".into();cx.notify();return;};
                    let Some(expected)=s.report_identity.clone().filter(|identity| Some(identity.0)==detail.revision_id && identity.1==template.version_id) else {s.message="Обновите предпросмотр перед отправкой".into();cx.notify();return;};
                    if s.report_busy || s.preview_request.is_some() {return;}
                    let request=Uuid::new_v4();
                    s.send_report_output(request,Command::SendResult{request,study,selected,template,expected});cx.notify();
                }
            })));
        if self.detail.as_ref().is_some_and(|d|!d.mis_task_bound) {
            content=content.child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child("Отправка в МИС доступна только по заданию из МИС. Связь с заданием не подтверждена."));
        }
        if let Some(previous)=shown.previous_delivery {
            if let Some(number)=previous["receipt"]["reception_number"].as_str().filter(|s|!s.is_empty()) {
                content=content.child(div().text_xs().text_color(cx.theme().muted_foreground)
                    .child(format!("Прежняя квитанция · приём № {number}")));
            }
        }
        if self.state["native_result_jobs"]
            .as_array()
            .is_some_and(|jobs| !jobs.is_empty())
        {
            content = content.child(
                Button::new("report-delivery-history")
                    .outline()
                    .small()
                    .w_full()
                    .h(px(32.)).bg(cx.theme().secondary)
                    .label("История передач")
                    .icon(if self.report_history_expanded {
                        IconName::ChevronUp
                    } else {
                        IconName::ChevronDown
                    })
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.report_history_expanded = !s.report_history_expanded;
                        cx.notify();
                    })),
            );
            if self.report_history_expanded {
                let history = crate::report_delivery::history(
                    &self.state["native_result_jobs"],
                    self.report_delivery_context(),
                );
                if history.is_empty() {
                    content = content.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Других передач нет"),
                    );
                }
                for job in history {
                    let time = job["delivered_at"]
                        .as_str()
                        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
                        .map(|v| {
                            v.with_timezone(&chrono::Local)
                                .format("%d.%m.%Y %H:%M")
                                .to_string()
                        })
                        .unwrap_or_default();
                    let number = job["receipt"]["reception_number"].as_str().unwrap_or("—");
                    content = content.child(
                        v_flex()
                            .gap_1()
                            .pt_2()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("Передача · {}", m53_client::delivery::status(job)))
                            .child(format!("Приём № {number} · {time}")),
                    );
                }
            }
        }
        div()
            .id("report-delivery-panel")
            .w(px(width))
            .flex_shrink_0()
            .h_full()
            .min_h_0()
            .border_l_1()
            .border_color(cx.theme().border)
            .overflow_y_scroll()
            .child(content)
            .test_support()
            .into_any_element()
    }

    pub(super) fn report(&self, cx: &Context<Self>) -> AnyElement {
        // Preserve the existing exact-byte resend guard and clinical handlers.
        let current_job = self.report_identity.as_ref().and_then(|(r, t, h)| {
            if self.detail.as_ref().and_then(|d| d.revision_id) != Some(*r)
                || self.templates.first().map(|t| t.version_id) != Some(*t)
            {
                return None;
            }
            m53_client::delivery::matching(&self.state["native_result_jobs"], *r, *t, h)
        });
        let delivery_locked = current_job.is_some_and(m53_client::delivery::prevents_resend);
        let delivery_label = current_job
            .filter(|job| m53_client::delivery::prevents_resend(job))
            .map(m53_client::delivery::status)
            .unwrap_or("Отправить в МИС");
        let shown = crate::report_delivery::present(
            &self.state["native_result_jobs"],
            self.report_delivery_context(),
        );
        let page_count = self.report_page_count();
        let page_size = self.report_page_dimensions();
        let scale = page_size.width / PAGE_WIDTH;
        let mut view = self.report_view;
        view.set_page(view.page, page_count);
        let measured = self.report_scroll.bounds().size;
        let scroll = self.report_scroll.clone();
        let scroll_entity = cx.entity();
        let panels = crate::report_view::panel_widths(self.viewport.width.as_f32());
        let template_hint = self.templates.first()
            .map(|t| format!("{} · версия {}",t.layout.title,t.version))
            .unwrap_or_else(|| "Шаблон протокола недоступен".into());
        let draft = !self.approved_history() && self
            .detail
            .as_ref()
            .is_some_and(|d| d.study.status != "approved");
        let mut pages = v_flex()
            .gap(px(PAGE_GAP))
            .items_center()
            .p(px(PAGE_PADDING))
            .w(px(page_size.width + PAGE_PADDING * 2.))
            .min_w_full();
        for page in self.visible_report_pages() {
            pages = pages.child(
                img(page.clone())
                    .w(px(page_size.width))
                    .h(px(page_size.height))
                    .flex_shrink_0()
                    .object_fit(ObjectFit::Contain),
            );
        }
        for lines in self
            .visible_report_lines()
            .chunks(53)
            .filter(|_| self.visible_report_pages().is_empty())
        {
            pages = pages.child(
                v_flex()
                    .w(px(page_size.width))
                    .h(px(page_size.height))
                    .flex_shrink_0()
                    .bg(white())
                    .text_color(black())
                    .p(px(53. * scale))
                    .gap_0()
                    .children(lines.iter().map(|line| {
                        div()
                            .h(px(17.6 * scale))
                            .text_size(px(12.4 * scale))
                            .when(m53_client::report::is_body_heading(line), |d| {
                                d.font_semibold()
                                    .bg(rgb(0xe5e7eb))
                                    .border_b_1()
                                    .border_color(rgb(0x6b7280))
                            })
                            .child(line.clone())
                    })),
            );
        }
        for (i, (id, image)) in self
            .report_photos
            .iter()
            .enumerate()
            .filter(|_| !self.approved_history() && self.visible_report_pages().is_empty())
        {
            pages = pages.child(
                v_flex()
                    .w(px(page_size.width))
                    .h(px(page_size.height))
                    .flex_shrink_0()
                    .bg(white())
                    .text_color(black())
                    .p(px(48. * scale))
                    .gap(px(12. * scale))
                    .text_size(px(14. * scale))
                    .child(format!(
                        "Снимок {}",
                        self.detail
                            .as_ref()
                            .and_then(|d| d
                                .assets
                                .iter()
                                .filter(|a| a.kind == "original")
                                .position(|a| a.id == *id))
                            .map(|n| n + 1)
                            .unwrap_or(i + 1)
                    ))
                    .child(
                        img(image.clone())
                            .size_full()
                            .object_fit(ObjectFit::Contain),
                    ),
            );
        }

        v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                h_flex()
                    .h(px(44.))
                    .flex_shrink_0()
                    .px_3()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex().id("report-default-template").test_support().gap_1().items_center().text_sm()
                            .child(gpui_kit::component::Icon::new(IconName::FileText).small())
                            .child("Шаблон протокола")
                            .tooltip(move |w,cx| Tooltip::new(template_hint.clone()).build(w,cx)),
                    )
                    .child(
                        Button::new("refresh-preview")
                            .ghost()
                            .small()
                            .icon(IconName::RefreshCw)
                            .accessibility_label("Обновить предпросмотр")
                            .tooltip("Обновить предпросмотр")
                            .disabled(self.report_busy || self.history.as_ref().is_some_and(|history|history.busy()))
                            .on_click(cx.listener(|s, _, _, cx| s.preview(cx))),
                    )
                    .child(div().w(px(1.)).h(px(20.)).mx_1().bg(cx.theme().border))
                    .child(
                        Button::new("print-report")
                            .outline()
                            .small()
                            .icon(IconName::Printer)
                            .accessibility_label("Печать отчёта")
                            .tooltip(
                                if self.report_output_available()
                                {
                                    "Печать отчёта"
                                } else {
                                    "Ответ не утверждён"
                                },
                            )
                            .disabled(
                                self.report_busy
                                    || self.templates.is_empty()
                                    || !self.report_output_available(),
                            )
                            .on_click(
                                cx.listener(|s, _, _, cx| s.output_report(ReportOutput::Print, cx)),
                            ),
                    )
                    .child(
                        Button::new("export-pdf")
                            .outline()
                            .small()
                            .icon(IconName::FileDown)
                            .accessibility_label("Экспорт в PDF")
                            .tooltip(
                                if self.report_output_available()
                                {
                                    "Экспорт в PDF"
                                } else {
                                    "Ответ не утверждён"
                                },
                            )
                            .disabled(
                                self.report_busy
                                    || self.templates.is_empty()
                                    || !self.report_output_available(),
                            )
                            .on_click(cx.listener(|s, _, w, cx| s.export_pdf(w, cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(if self.report_busy {
                                "Подготовка документа…"
                            } else {
                                "Предпросмотр отчёта"
                            }),
                    ),
            )
            .when(self.approved_history(), |d| {
                if let Some(history)=&self.history {
                    if !history.note.is_empty() {
                        return d.child(div().px_3().py_2().text_color(cx.theme().danger).child(history.note.clone()));
                    }
                    if history.request.is_some() && history.metadata.is_null() {
                        return d.child(div().px_3().py_2().child("Загружаем утверждённую редакцию…"));
                    }
                }
                #[cfg(windows)]
                if let Some(history)=&self.history {
                    if let Some(Err(error))=&history.pdf_pages {
                        return d.child(div().px_3().py_2().text_color(cx.theme().danger).child(error.clone()));
                    }
                    if history.pdf_request.is_some() {
                        return d.child(div().px_3().py_2().child("Формируем PDF…"));
                    }
                }
                d
            })
            .when(draft, |d| {
                d.child(
                    h_flex()
                        .min_h(px(64.))
                        .flex_shrink_0()
                        .px_3()
                        .gap_2()
                        .items_center()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(gpui_kit::component::Icon::new(IconName::Info).small().text_color(cx.theme().danger))
                        .child(v_flex().flex_1().min_w_0().gap_1()
                            .child(div().font_semibold().text_color(cx.theme().danger).child("Отчёт не утверждён"))
                            .child(div().text_sm().child(if cfg!(feature = "standalone") {
                                "Сохранение PDF и печать недоступны до утверждения."
                            } else {
                                "Передача в МИС, сохранение PDF и печать недоступны до утверждения."
                            })))
                        .child(
                            Button::new("report-draft-to-protocol")
                                .success()
                                .when(!self.approval_busy && !self.approval_pending && self.workflow_request.is_none()
                                    && (!self.report_busy || self.preview_request.is_some()),
                                    |button| button.bg(rgb(0x15803d)).text_color(rgb(0xffffff)))
                                .small()
                                .label(if self.approval_busy {"Утверждение…"} else if self.approval_pending {"Ожидает подтверждения"} else {"Утвердить"})
                                .disabled(
                                    self.approval_busy || self.approval_pending || self.workflow_request.is_some()
                                        || (self.report_busy && self.preview_request.is_none()),
                                )
                                .on_click(
                                    cx.listener(|s, _, _, cx| s.request_approval(cx)),
                                ),
                        ),
                )
            })
            .when(!self.approval_message.is_empty(), |d| d.child(
                div().id("report-approval-message").test_support().flex_shrink_0().px_3().py_1().text_sm()
                    .text_color(if self.approval_busy || self.approval_pending {cx.theme().muted_foreground} else if draft {cx.theme().danger} else {cx.theme().success})
                    .child(self.approval_message.clone())))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .items_stretch()
                    .child(self.report_navigation(view, page_count, scale, panels.navigation, cx))
                    .child(
                        div()
                            .id("native-report")
                            .test_support()
                            .bg(cx.theme().muted)
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .overflow_x_scroll()
                            .overflow_y_scroll()
                            .track_scroll(&self.report_scroll)
                            .horizontal_scrollbar(&self.report_scroll)
                            .vertical_scrollbar(&self.report_scroll)
                            .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()))
                            .on_prepaint(move |bounds, _, cx| {
                                let resized = (bounds.size.width - measured.width).abs() > px(0.5)
                                    || (bounds.size.height - measured.height).abs() > px(0.5);
                                scroll_entity.update(cx, |s, cx| {
                                    if resized {
                                        // Fitting follows the actual preview viewport, including wrapped toolbars/notices.
                                        let size = page_dimensions(
                                            s.report_view.zoom,
                                            bounds.size.width.as_f32(),
                                            bounds.size.height.as_f32(),
                                        );
                                        scroll.set_offset(point(
                                            px(0.),
                                            px(-s.report_view.page_offset(size, PAGE_GAP)),
                                        ));
                                        cx.notify();
                                    } else if page_count > 0 {
                                        let top = (-scroll.offset().y.as_f32()).max(0.);
                                        let current = crate::report_view::page_at_offset(
                                            top,
                                            bounds.size.height.as_f32(),
                                            page_size,
                                            PAGE_GAP,
                                            PAGE_PADDING,
                                            page_count,
                                            s.report_view.page,
                                        );
                                        let previous = s.report_view.page;
                                        s.report_view.set_page(current, page_count);
                                        if previous != s.report_view.page {
                                            cx.notify();
                                        }
                                    }
                                });
                            })
                            .child(pages),
                    )
                    .when(!cfg!(feature = "standalone") && self.approved_history(), |row| row.child(v_flex().w(px(panels.delivery)).flex_shrink_0().p_3().gap_2()
                        .child(crate::ui_chrome::panel_heading("Передача в МИС", cx))
                        .child(div().text_sm().child("Для исторической редакции доступен просмотр, печать и экспорт."))))
                    .when(!cfg!(feature = "standalone") && !self.approved_history(), |row| row.child(self.report_delivery_panel(
                        &shown,
                        delivery_locked,
                        delivery_label,
                        panels.delivery,
                        cx,
                    ))),
            )
            .into_any_element()
    }
}
