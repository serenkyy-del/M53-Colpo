use super::*;

impl Workplace {
    pub(super) fn patients(&self, cx: &Context<Self>) -> AnyElement {
        let patient_form = cx.entity();
        let checking=self.new_patient_check.is_some();
        let checking_or_matches=checking||!self.new_patient_matches.is_empty();
        let mut patient_studies: Vec<_> = self
            .studies
            .iter()
            .filter(|study| Some(study.patient_id) == self.patient)
            .collect();
        patient_studies.sort_by(|a, b| {
            let date = |id: &Uuid| {
                self.study_summaries
                    .get(id)
                    .and_then(|summary| summary.exam_date.as_ref())
            };
            date(&b.id).cmp(&date(&a.id)).then_with(|| a.id.cmp(&b.id))
        });
        let selected = self
            .preview_study
            .filter(|id| patient_studies.iter().any(|s| s.id == *id))
            .or_else(|| {
                self.detail
                    .as_ref()
                    .filter(|d| Some(d.study.patient_id) == self.patient)
                    .map(|d| d.study.id)
            })
            .or_else(|| patient_studies.first().map(|study| study.id));
        let mut studies = v_flex()
            .gap_2()
            .child(crate::ui_chrome::panel_heading("Обследования пациента", cx)
                .id("registry-study-heading").test_support());
        if let Some(panel)=&self.patient_editor {studies=studies.child(panel.clone());}
        if self.patient.is_none() {
            studies = studies.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Выберите пациента в таблице"),
            );
        } else {
            let history = &self.state["native_registry"];
            let scoped_patient = history["studies_patient_id"].as_str().and_then(|id| id.parse::<Uuid>().ok());
            let status = if scoped_patient == self.patient {
                history["studies_status"].as_str().unwrap_or("Загружаем историю обследований…")
            } else if history["online"] == true || self.state["sync"]["online"] == true {
                "Загружаем историю обследований…"
            } else {
                "История обследований не сохранена на этом компьютере. Сервер недоступен"
            };
            if !status.is_empty() {
            studies = studies.child(
                div()
                    .id("registry-study-status").test_support()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(status.to_owned()),
            );
            }
        }
        for (index, study) in patient_studies.iter().filter(|_|self.patient_editor.is_none()).enumerate() {
            let id = study.id;
            let summary = self.study_summaries.get(&id);
            let date = display_date(summary.and_then(|s| s.exam_date.as_deref()));
            let status = if study.status == "approved" {
                "утверждено"
            } else {
                "черновик"
            };
            let delivered = summary.is_some_and(|summary| summary.mis_delivered(study));
            studies = studies.child(
                Button::new(SharedString::from(format!("study-{id}")))
                    .when(selected == Some(id), |button| button.primary())
                    .when(selected != Some(id), |button| button.outline())
                    .w_full()
                    .border_1()
                    .justify_start()
                    .child(h_flex().w_full().min_w_0().gap_2().items_center()
                        .child(div().id(SharedString::from(format!("study-check-{id}"))).test_support().w(px(20.)).flex_shrink_0()
                            .when(selected == Some(id),|d|d.child(gpui_kit::component::Icon::new(IconName::Check).small())))
                        .child(div().id(SharedString::from(format!("study-label-{id}"))).test_support().flex_1().min_w_0().truncate().text_left()
                            .child(format!("№{} · {date} · {status}", index + 1)))
                        .child(div().w(px(24.)).flex_shrink_0().when(delivered,|d|d.child(
                            div().id(SharedString::from(format!("study-mis-{id}"))).test_support().px_1().rounded_sm().text_xs()
                                .bg(rgb(0xfff1a8)).text_color(rgb(0xc92b27)).font_bold().child("1С")))))
                    .selected(selected == Some(id))
                    .disabled(self.opening)
                    .tooltip(if delivered {"Утверждённая редакция доставлена в МИС. Двойной клик — открыть обследование"}
                        else {"Один клик — сведения, двойной клик — открыть обследование. Доставка текущей редакции в МИС не подтверждена"})
                    .on_click(cx.listener(move |s, event: &ClickEvent, _, cx| {
                        s.preview_study = Some(id);
                        if event.click_count() == 2 {
                            s.open_study(id, cx);
                        }
                        cx.notify();
                    })),
            );
        }
        if let Some(id) = selected.filter(|_|self.patient_editor.is_none()) {
            let summary = self.study_summaries.get(&id).cloned().unwrap_or_default();
            studies = studies.child(
                v_flex()
                    .mt_2()
                    .pt_3()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .gap_3()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Дата обследования"),
                            )
                            .child(display_date(summary.exam_date.as_deref())),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Врач"),
                            )
                            .child(summary.doctor_name.unwrap_or_else(|| "Не указан".into())),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Заключение врача"),
                            )
                            .child(div().w_full().child(
                                summary.conclusion.unwrap_or_else(|| "Не заполнено".into()),
                            )),
                    ),
            );
        }
        let actions=v_flex().gap_2().pt_3().flex_shrink_0().border_t_1().border_color(cx.theme().border)
            .child(Button::new("open-selected-study").outline().w_full().label("Открыть обследование")
                .disabled(selected.is_none() || self.opening)
                .on_click(cx.listener(move |s,_,_,cx| {if let Some(id)=selected {s.open_study(id,cx);} })))
            .child(
            Button::new("new-study")
                .primary()
                .w_full()
                .label("Новое обследование")
                .disabled(self.patient.is_none() || self.opening || self.patient_editor.is_some() || self.patient_edit_request.is_some())
                .on_click(cx.listener(|s, _, _, cx| {
                    if let Some(id) = s.patient {
                        s.opening = true;
                        s.send(Command::NewStudy(id));
                    }
                    cx.notify();
                })),
        );
        let mut task_list = v_flex().gap_1();
        let mut task_count = 0usize;
        if let Some(tasks) = self.state["native_tasks"].as_array() {
            for task in tasks {
                let Some(id) = task["id"].as_str().and_then(|id| Uuid::parse_str(id).ok()) else {
                    continue;
                };
                let version = task["row_version"].as_i64().unwrap_or(0);
                task_count += 1;
                let p = &task["patient"];
                let name = ["last_name", "first_name", "middle_name"].iter()
                    .filter_map(|key| p[*key].as_str()).map(str::trim)
                    .filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" ");
                let name = if name.is_empty() { "ФИО не указано".into() } else { name };
                let card = p["card_number"].as_str().map(str::trim)
                    .filter(|s| !s.is_empty()).unwrap_or("не указана");
                let identity = format!("ДР: {} · Карта: {card} · Задание: {}",
                    display_date(p["birth_date"].as_str()),
                    display_date(task["appointment_date"].as_str()));
                let action = if task["assigned_doctor_id"].is_null() { "Взять" } else { "Открыть" };
                task_list = task_list.child(
                    h_flex().w_full().gap_2().p_2().items_center().border_b_1().border_color(cx.theme().border)
                        .child(v_flex().flex_1().min_w_0().gap_1()
                            .child(div().font_semibold().whitespace_normal().child(name.clone()))
                            .child(div().text_xs().text_color(cx.theme().muted_foreground).whitespace_normal().child(identity.clone())))
                        .child(Button::new(SharedString::from(format!("task-{id}")))
                            .outline().small().flex_shrink_0().label(action)
                            .accessibility_label(format!("{action} задание: {name}. {identity}"))
                            .disabled(self.opening || self.patient_editor.is_some() || self.patient_edit_request.is_some() || self.patient_editor_pending.is_some())
                            .on_click(cx.listener(move |s, _, _, cx| {
                                s.opening = true;
                                s.send(Command::ClaimTask(id, version));
                                cx.notify();
                            }))),
                );
            }
        }
        let tasks_loading=self.state["native_registry"]["tasks_loading"]==true;
        let tasks_status = self.state["native_registry"]["tasks_status"].as_str().unwrap_or("");
        let patients_status = self.state["native_registry"]["patients_status"].as_str().unwrap_or("");
        let tasks_panel=v_flex().gap_2().flex_shrink_0().border_t_1().border_color(cx.theme().border).pt_2()
            .child(h_flex().items_center().gap_2().flex_wrap()
                .child(crate::ui_chrome::panel_heading("", cx).flex_1()
                    .child(div().px_1().rounded_sm().bg(rgb(0xfff1a8)).text_color(rgb(0xc92b27)).font_bold().child("1С"))
                    .child(format!("Задания из МИС · {task_count}{}", if self.state["native_registry"]["tasks_more"] == true { "+" } else { "" })))
                .child(h_flex().h(px(32.)).rounded_md().bg(cx.theme().secondary).gap_2()
                    .child(Button::new("tasks-prev-date").ghost().small().icon(IconName::ChevronLeft).accessibility_label("Предыдущий день заданий").tooltip("Предыдущий день").on_click(cx.listener(|s,_,_,cx|s.change_task_date(-1,cx))))
                    .child(div().text_center().w(px(100.)).child(self.task_date.format("%d.%m.%Y").to_string()))
                    .child(Button::new("tasks-next-date").ghost().small().icon(IconName::ChevronRight).accessibility_label("Следующий день заданий").tooltip("Следующий день").on_click(cx.listener(|s,_,_,cx|s.change_task_date(1,cx)))))
                .child(Button::new("tasks-today").outline().small().h(px(32.)).bg(cx.theme().secondary).label("Сегодня").on_click(cx.listener(|s,_,_,cx|{s.task_date=chrono::Local::now().date_naive();s.change_task_date(0,cx);}))))
            .child(div().id("task-list-scroll").max_h(px(320.)).overflow_y_scroll().child(task_list))
            .when(!tasks_status.is_empty(),|d|d.child(div().text_sm().text_color(cx.theme().muted_foreground).child(tasks_status.to_owned())))
            .when(self.state["native_registry"]["tasks_more"]==true,|d|d.child(Button::new("tasks-more").ghost().label("Ещё задания").disabled(tasks_loading).on_click(cx.listener(|s,_,_,_|s.send(Command::Registry(crate::registry_browser::Request::MoreTasks))))));
        h_flex()
            .items_stretch()
            .flex_1()
            .min_h_0()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .p_4()
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Input::new(&self.query).flex_1().disabled(self.patient_registry_locked()))
                            .when(self.can_open_patient_editor(),|row|row.child(Button::new("edit-selected-patient").outline().label("Карточка пациента")
                                .on_click(cx.listener(|s,_,_,cx|{if s.can_open_patient_editor(){if let Some(patient)=s.patient{let request=Uuid::new_v4();s.patient_edit_request=Some((request,patient));s.send(Command::ManualPatient{request,patient,body:None});cx.notify();}}}))))
                            .child(
                                Button::new("new-patient-toggle")
                                    .outline()
                                    .icon(IconName::UserRoundPlus)
                                    .accessibility_label("Новый пациент")
                                    .tooltip("Новый пациент")
                                    .disabled(self.patient_registry_locked())
                                    .on_click(cx.listener(|s, _, _, cx| {
                                        if !s.show_new {
                                            s.sex = None;
                                        }
                                        s.show_new = !s.show_new;
                                        s.new_patient_check=None;s.new_patient_matches.clear();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("refresh")
                                    .ghost()
                                    .icon(IconName::RefreshCw)
                                    .accessibility_label("Обновить картотеку")
                                    .tooltip("Обновить")
                                    .disabled(self.patient_registry_locked())
                                    .on_click(cx.listener(|s, _, _, cx| {s.patient_edit_access=None;s.send(Command::Refresh);cx.notify();})),
                            ),
                    )
                    .when(self.show_new, |d| {
                        d.child(
                            v_flex()
                                .p_3()
                                .gap_2()
                                .border_1()
                                .border_color(cx.theme().border)
                                .rounded_md()
                                .child(div().font_semibold().child("Новый пациент"))
                                .child(Input::new(&self.family).disabled(checking_or_matches))
                                .child(Input::new(&self.given).disabled(checking_or_matches))
                                .child(Input::new(&self.patronymic).disabled(checking_or_matches))
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .flex_wrap()
                                        .child("Дата рождения")
                                        .child(div().w(px(190.)).flex_shrink_0().child(
                                            DatePicker::new(&self.birth).disabled(checking_or_matches).placeholder("Не указана"),
                                        ))
                                        .child(div().ml_4().child("Пол"))
                                        .child(
                                            Button::new("patient-sex")
                                                .outline().disabled(checking_or_matches)
                                                .w(px(190.))
                                                .justify_start()
                                                .accessibility_label(
                                                    "Пол пациента: выбрать из списка",
                                                )
                                                .child(
                                                    h_flex()
                                                        .w_full()
                                                        .gap_2()
                                                        .child(
                                                            div()
                                                                .flex_1()
                                                                .text_left()
                                                                .child(display_sex(self.sex)),
                                                        )
                                                        .child(
                                                            div()
                                                                .pl_2()
                                                                .border_l_1()
                                                                .border_color(cx.theme().border)
                                                                .child(
                                                                    gpui_kit::component::Icon::new(
                                                                        IconName::ChevronDown,
                                                                    ),
                                                                ),
                                                        ),
                                                )
                                                .dropdown_menu(move |mut menu, _, _| {
                                                    for sex in [
                                                        None,
                                                        Some(PatientSex::Female),
                                                        Some(PatientSex::Male),
                                                    ] {
                                                        let entity = patient_form.clone();
                                                        menu = menu.item(
                                                            PopupMenuItem::new(display_sex(sex))
                                                                .on_click(move |_, _, cx| {
                                                                    entity.update(cx, |s, cx| {
                                                                        if s.new_patient_check.is_some() || !s.new_patient_matches.is_empty(){return;}
                                                                        s.sex = sex;
                                                                        cx.notify();
                                                                    });
                                                                }),
                                                        );
                                                    }
                                                    menu
                                                }),
                                        ),
                                )
                                .child(
                                    Button::new("new-patient")
                                        .outline()
                                        .label(if checking {"Проверяем совпадения…"}else{"Создать пациента"})
                                        .disabled(checking_or_matches)
                                        .on_click(cx.listener(|s, _, _, cx| {
                                            if s.family.read(cx).value().trim().is_empty()
                                                || s.given.read(cx).value().trim().is_empty()
                                            {
                                                s.message = "Укажите фамилию и имя пациента".into();
                                                cx.notify();
                                                return;
                                            }
                                            let patronymic =
                                                s.patronymic.read(cx).value().trim().to_string();
                                            if s.new_patient_check.is_some() || !s.new_patient_matches.is_empty(){return;}
                                            let input=NewPatient {
                                                    family_name: s
                                                        .family
                                                        .read(cx)
                                                        .value()
                                                        .trim()
                                                        .to_string(),
                                                    given_name: s
                                                        .given
                                                        .read(cx)
                                                        .value()
                                                        .trim()
                                                        .to_string(),
                                                    patronymic: (!patronymic.is_empty())
                                                        .then_some(patronymic),
                                                    birth_date: s
                                                        .birth
                                                        .read(cx)
                                                        .date_time()
                                                        .start()
                                                        .map(|d| d.format("%Y-%m-%d").to_string()),
                                            };
                                            let request=Uuid::new_v4();
                                            s.new_patient_check=Some((request,input.clone(),s.sex));
                                            s.send(Command::CheckNewPatient{request,input});
                                            cx.notify();
                                        })),
                                )
                                .when(!self.new_patient_matches.is_empty(),|panel|{
                                    let mut matches=v_flex().gap_2().child(div().font_semibold().child("Найдены совпадающие карточки. Выберите существующую:"));
                                    for patient in &self.new_patient_matches {
                                        let id=patient.id;
                                        let patient=patient.clone();
                                        let date=patient.birth_date.clone().unwrap_or_default();
                                        let query=patient.external_id.clone().filter(|s|!s.trim().is_empty()).unwrap_or_else(||date.clone());
                                        let label=format!("{} · {}{}{}",patient_name(&patient),display_date(Some(&date)),if patient.external_id.is_some(){" · МИС"}else{""},patient.card_number.as_ref().map(|n|format!(" · карта № {n}")).unwrap_or_default());
                                        matches=matches.child(h_flex().gap_2().items_center()
                                            .child(div().flex_1().min_w_0().whitespace_normal().child(label))
                                            .child(Button::new(SharedString::from(format!("reuse-patient-{id}"))).outline().label("Использовать карточку")
                                                .on_click(cx.listener(move|s,_,window,cx|{
                                                    if !s.show_new || !s.new_patient_matches.iter().any(|p|p.id==id){return;}
                                                    s.show_new=false;s.new_patient_matches.clear();s.new_patient_check=None;
                                                    s.patient=Some(id);s.preview_study=None;s.patient_edit_access=None;
                                                    s.query.update(cx,|input,cx|input.set_value(query.clone(),window,cx));
                                                    s.query_due=None;
                                                    s.send(Command::UseMatchedPatient{patient:patient.clone(),query:query.clone()});
                                                    s.refresh_patient_table(cx);
                                                    s.message="Выбрана существующая карточка пациента".into();cx.notify();
                                                }))));
                                    }
                                    panel.child(div().id("new-patient-matches").max_h(px(160.)).overflow_y_scroll().child(matches))
                                        .child(Button::new("edit-new-patient-search").ghost().label("Изменить данные поиска").on_click(cx.listener(|s,_,_,cx|{s.new_patient_matches.clear();cx.notify();})))
                                })
                                .child(
                                    Button::new("cancel-new-patient")
                                        .ghost()
                                        .label("Отмена")
                                        .on_click(cx.listener(|s, _, _, cx| {
                                            s.show_new = false;
                                            s.new_patient_check=None;s.new_patient_matches.clear();
                                            cx.notify();
                                        })),
                                ),
                        )
                    })
                    .when(!patients_status.is_empty(), |d| d.child(div().text_sm()
                        .text_color(cx.theme().muted_foreground).child(patients_status.to_owned())))
                    .child(
                        div().id("patient-list").relative().flex_1().min_h_0().child(
                            DataTable::new(&self.patient_table)
                                .stripe(true)
                                .bordered(true),
                        ).when(self.patient_registry_locked(),|d|d.child(div().absolute().inset_0().occlude())),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "Загружено пациентов: {} · Сортировка — нажатием на заголовок",
                                self.patient_table.read(cx).delegate().rows.len()
                            )),
                    )
                    .when(!cfg!(feature = "standalone"), |panel| panel.child(tasks_panel)),
            )
            .child(
                v_flex()
                    .id("registry-studies-panel")
                    .test_support()
                    .w(px((f32::from(self.viewport.width) * 0.29).clamp(230., 340.)))
                    .flex_shrink_0()
                    .min_h_0()
                    .p_4()
                    .gap_3()
                    .border_l_1()
                    .border_color(cx.theme().border)
                    .child(div().id("study-list").flex_1().min_h_0().overflow_y_scroll().child(studies))
                    .when(self.patient_editor.is_none(),|d|d.child(actions)),
            )
            .into_any_element()
    }
}
