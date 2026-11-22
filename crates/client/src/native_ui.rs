//! Native Rust patient list, settings and image workstation.
use super::*;

fn name(patient: &Patient) -> String {
    [
        &*patient.family_name,
        &*patient.given_name,
        patient.patronymic.as_deref().unwrap_or(""),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" ")
}
fn status(value: &str) -> &str {
    match value {
        "approved" => "Утверждено",
        "draft" => "Черновик",
        "review" => "На проверке",
        "cancelled" => "Отменено",
        _ => value,
    }
}
impl eframe::App for Desktop {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if std::env::var("M53_LEGACY_UI").as_deref() == Ok("1") {
            self.legacy_update(ctx, frame);
            return;
        }
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(handle) = frame.window_handle()
            && let RawWindowHandle::Win32(handle) = handle.as_raw()
        {
            self.host_window = handle.hwnd.get();
        }
        self.poll(ctx);
        if ctx.input(|i| i.viewport().close_requested()) {
            self.checkpoint();
            if let Some(panel) = &self.web_panel {
                panel.request_close();
                self.closing = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            } else if self.background_jobs > 0
                || self
                    .connection
                    .as_ref()
                    .and_then(|c| c.offline.as_ref())
                    .is_some_and(|o| !o.information().2)
            {
                self.closing = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
        }
        if self.closing
            && self.web_panel.is_none()
            && self.background_jobs == 0
            && self
                .connection
                .as_ref()
                .and_then(|c| c.offline.as_ref())
                .is_none_or(|o| o.information().2)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        egui::TopBottomPanel::top("workplace-navigation").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.strong("М53 · Кольпоскопия");
                ui.label(egui::RichText::new("Рабочее место").small().weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⚙  Настройки").clicked() {
                        self.settings_open = true;
                        if let Some(panel) = &self.web_panel {
                            panel.request_close();
                        }
                    }
                });
            });
            ui.add_space(7.0);
            ui.scope(|ui| {
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(self.patients_view, "Пациенты")
                        .clicked()
                    {
                        self.patients_view = true;
                        if let Some(panel) = &self.web_panel {
                            panel.request_close();
                        }
                    }
                    if ui
                        .add_enabled(
                            self.detail.is_some(),
                            egui::Button::new("Обследование")
                                .selected(!self.patients_view && self.web_panel.is_none()),
                        )
                        .clicked()
                    {
                        self.patients_view = false;
                        if let Some(panel) = &self.web_panel {
                            panel.request_close();
                        }
                    }
                    if ui
                        .add_enabled(
                            self.detail.is_some() && self.background_jobs == 0,
                            egui::Button::new("Протокол"),
                        )
                        .clicked()
                    {
                        let r = self.open_html(ctx, false);
                        self.status(r, "Протокол открыт");
                    }
                    if ui
                        .add_enabled(
                            self.detail.is_some() && self.background_jobs == 0,
                            egui::Button::new("Отчет"),
                        )
                        .clicked()
                    {
                        let r = self.open_html(ctx, true);
                        self.status(r, "Отчет открыт");
                    }
                });
            });
            ui.add_space(5.0);
        });
        egui::TopBottomPanel::bottom("workplace-status").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                let mut durable = true;
                if let Some(offline) = self.connection.as_ref().and_then(|c| c.offline.as_ref()) {
                    let (online, pending, saved, _) = offline.information();
                    durable = saved;
                    ui.label(if saved {
                        "✓  Сохранено на компьютере"
                    } else {
                        "Запись на локальный диск…"
                    });
                    ui.separator();
                    let text = if online {
                        if pending == 0 {
                            "Синхронизировано".into()
                        } else {
                            format!("Передача: {pending} операций")
                        }
                    } else {
                        format!("Нет связи · в очереди {pending}")
                    };
                    if ui.button(text).clicked() {
                        self.settings_open = true;
                        self.settings_tab = 3;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(&self.doctor);
                    if self.closing {
                        ui.label(if durable {
                            "Завершение локальной операции…"
                        } else {
                            "Ожидаем запись на диск…"
                        });
                    }
                });
            });
        });
        if let Some(panel) = &self.web_panel {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.spinner();
                        ui.label("Открываем локальную форму…");
                    });
                    let rect = ui.max_rect();
                    let scale = ctx.pixels_per_point();
                    panel.set_bounds([
                        (rect.left() * scale).round() as i32,
                        (rect.top() * scale).round() as i32,
                        (rect.width() * scale).round().max(1.0) as i32,
                        (rect.height() * scale).round().max(1.0) as i32,
                    ]);
                });
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        if self.connection.is_none() {
            self.login_view(ctx);
            self.settings_view(ctx);
            return;
        }
        if !self.message.is_empty() {
            egui::TopBottomPanel::top("workplace-message").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [ui.available_width() - 30.0, 20.0],
                        egui::Label::new(&self.message).truncate(),
                    )
                    .on_hover_text(&self.message);
                    if ui.small_button("×").clicked() {
                        self.message.clear();
                    }
                });
            });
        }
        if self.patients_view {
            self.patient_view(ctx);
        } else {
            self.study_view(ctx);
        }
        self.settings_view(ctx);
        self.checkpoint();
    }
}
impl Desktop {
    fn open_html(&mut self, ctx: &egui::Context, report: bool) -> Result<(), String> {
        if let Some(panel) = &self.web_panel {
            panel.navigate(report);
            return Ok(());
        }
        if self.background_jobs > 0 {
            return Err("Дождитесь завершения импорта".into());
        }
        // Opening a local form must not validate/send an unfinished draft first.
        self.checkpoint();
        let mut workspace = crate::workspace::Workspace::default();
        workspace.attach(self.connection.as_ref().ok_or("Сначала войдите")?.clone());
        workspace.detail = self.detail.clone();
        workspace.dictionary = self.dictionary.clone();
        workspace.dirty = self.dirty;
        self.web_panel = Some(crate::web_panel::open(
            workspace,
            report,
            ctx.clone(),
            self.host_window,
        ));
        Ok(())
    }
    fn login_view(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(60.0);
            ui.vertical_centered(|ui| {
                ui.heading("Вход в рабочее место");
                ui.add_space(16.0);
                ui.allocate_ui(egui::vec2(340.0, 260.0), |ui| {
                    ui.label("Логин врача");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.login).desired_width(f32::INFINITY),
                    );
                    ui.add_space(8.0);
                    ui.label("Пароль");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.password)
                            .password(true)
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(14.0);
                    if ui
                        .add_enabled(
                            self.auth_job.is_none(),
                            egui::Button::new("Войти").min_size(egui::vec2(320.0, 34.0)),
                        )
                        .clicked()
                    {
                        self.authenticate(false);
                    }
                    if ui
                        .add_enabled(
                            self.auth_job.is_none(),
                            egui::Button::new("Открыть сохраненную работу без сети"),
                        )
                        .clicked()
                    {
                        self.authenticate(true);
                    }
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new("Адрес сервера и ключ рабочего места — в настройках.")
                            .small()
                            .weak(),
                    );
                    ui.label(&self.message);
                });
            });
        });
    }
    fn settings_view(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut opened = true;
        egui::Window::new("Настройки рабочего места").open(&mut opened).default_width(680.0).resizable(true).collapsible(false).show(ctx,|ui|{
            ui.horizontal(|ui|{for(i,label)in["Подключение","Учетная запись","Форма и словари","Локальные данные"].iter().enumerate(){if ui.selectable_label(self.settings_tab==i,*label).clicked(){self.settings_tab=i;}}});ui.separator();ui.add_space(7.0);
            match self.settings_tab {
                0=>{ui.label("Адрес API");ui.add(egui::TextEdit::singleline(&mut self.url).desired_width(f32::INFINITY));ui.label("Ключ рабочего места");ui.add(egui::TextEdit::singleline(&mut self.key).password(true).desired_width(f32::INFINITY));ui.add_space(10.0);if ui.button("Сохранить подключение (DPAPI)").clicked(){let r=crate::credentials::save(&self.url,&self.key);self.status(r,"Подключение сохранено в профиле Windows");}ui.label(egui::RichText::new("Цветовая схема следует настройкам компьютера. Снимки обрабатываются нативным Rust-клиентом.").small().weak());}
                1=>{ui.label(format!("Врач: {}",self.doctor));ui.label("Логин");ui.text_edit_singleline(&mut self.login);ui.label("Пароль для обновления сеанса");ui.add(egui::TextEdit::singleline(&mut self.password).password(true));if ui.add_enabled(self.auth_job.is_none(),egui::Button::new("Обновить сеанс без закрытия черновика")).clicked(){self.authenticate(false);}
 if ui.add_enabled(self.connection.is_some()&&!self.dirty&&self.background_jobs==0,egui::Button::new("Выйти из учетной записи")).clicked(){self.checkpoint();let result=self.connection.as_ref().and_then(|c|c.offline.as_ref()).ok_or("Нет профиля".into()).and_then(|o|o.shutdown());match result{Ok(())=>{let _:Result<serde_json::Value,_>=self.connection.as_ref().unwrap().post("/api/v1/logout",&serde_json::json!({}));self.connection=None;self.detail=None;self.patients.clear();self.studies.clear();self.texture=None;self.settings_open=false;},Err(e)=>self.message=e}}}
                2=>{ui.label("Словарь для новых обследований");let mut selected=self.dictionary.as_ref().map(|d|d.version_id);egui::ComboBox::from_id_salt("native-dictionary").selected_text(self.dictionary_versions.iter().find(|v|Some(v.id)==selected).map(|v|v.code.as_str()).unwrap_or("Загрузка…")).show_ui(ui,|ui|{for version in &self.dictionary_versions{ui.add_enabled_ui(self.detail.is_none()&&!self.dirty,|ui|{ui.selectable_value(&mut selected,Some(version.id),&version.code);});}});if selected!=self.dictionary.as_ref().map(|d|d.version_id)&&let(Some(id),Some(c))=(selected,&self.connection){let result=c.get(&format!("/api/v1/terminology?version_id={id}"));match result{Ok(d)=>self.dictionary=Some(d),Err(e)=>self.message=e}}ui.label(egui::RichText::new("Словарь открытого исследования закреплен за его ревизией. Для выбора другого словаря закройте исследование.").small().weak());}
                _=>{if let Some(o)=self.connection.as_ref().and_then(|c|c.offline.as_ref()){let(online,pending,durable,error)=o.information();ui.label(if online{"API доступен"}else{"Нет связи; работа продолжается локально"});ui.label(format!("Неподтвержденных операций: {pending}"));ui.label(if durable{"Локальная запись подтверждена диском"}else{"Запись на диск…"});if !error.is_empty(){ui.label(error);}ui.label("Неподтвержденные файлы не очищаются. Автоочистка подтвержденных копий выключена.");if o.conflict().is_some()&&ui.button("Открыть сравнение версий в форме протокола").clicked(){let r=self.open_html(ctx,false);self.status(r,"Откройте раздел сравнения в форме протокола");}}
 if !self.pending_imports.is_empty(){ui.label(format!("Незавершенный импорт: {} файлов",self.pending_imports.len()));if ui.add_enabled(self.background_jobs==0,egui::Button::new("Продолжить импорт")).clicked()&&let Some(id)=self.pending_import_study{self.start_import(id,self.pending_imports.clone());}}}
            }
            ui.add_space(10.0);ui.separator();ui.label(&self.message);
        });
        self.settings_open &= opened;
    }
    fn patient_view(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("patient-study-history")
            .default_width(340.0)
            .min_width(260.0)
            .show(ctx, |ui| {
                ui.add_space(10.0);
                ui.heading("Обследования");
                ui.add_space(10.0);
                if let Some(patient) = self.patients.iter().find(|p| Some(p.id) == self.patient) {
                    ui.label(name(patient));
                } else {
                    ui.label("Выберите пациента");
                }
                ui.separator();
                let mut open = None;
                let studies: Vec<_> = self
                    .studies
                    .iter()
                    .filter(|s| Some(s.patient_id) == self.patient)
                    .cloned()
                    .collect();
                for (i, study) in studies.iter().enumerate() {
                    ui.group(|ui| {
                        if ui
                            .button(format!(
                                "Обследование {} · {}",
                                studies.len() - i,
                                status(&study.status)
                            ))
                            .clicked()
                        {
                            open = Some(study.id);
                        }
                        ui.label(
                            egui::RichText::new(format!("Версия {}", study.row_version))
                                .small()
                                .weak(),
                        );
                    });
                    ui.add_space(6.0);
                }
                if let Some(id) = open {
                    self.pending_open = Some(id);
                    let r = self.open(id);
                    if r.is_ok() {
                        self.pending_open = None;
                        self.patients_view = false;
                    }
                    self.status(r, "Обследование открыто");
                }
                ui.add_space(10.0);
                let mut order = self.order;
                egui::ComboBox::from_id_salt("native-order")
                    .selected_text(
                        order
                            .and_then(|id| {
                                self.orders
                                    .iter()
                                    .find(|o| o.id == id)
                                    .map(|o| o.external_order_id.as_str())
                            })
                            .unwrap_or("Без направления"),
                    )
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut order, None, "Без направления");
                        for o in self
                            .orders
                            .iter()
                            .filter(|o| Some(o.patient_id) == self.patient)
                        {
                            ui.selectable_value(&mut order, Some(o.id), &o.external_order_id);
                        }
                    });
                self.order = order;
                if ui
                    .add_enabled(
                        self.patient.is_some() && !self.dirty,
                        egui::Button::new("＋ Новое обследование")
                            .min_size(egui::vec2(ui.available_width(), 34.0)),
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
                                patient_id: self.patient.unwrap(),
                                order_id: self.order,
                            },
                        )
                        .and_then(|s| {
                            self.refresh()?;
                            self.open(s.id)
                        });
                    if r.is_ok() {
                        self.patients_view = false;
                    }
                    self.status(r, "Обследование создано");
                }
                if self.detail.is_some()
                    && ui
                        .add_enabled(
                            !self.dirty,
                            egui::Button::new("Закрыть текущее обследование"),
                        )
                        .clicked()
                {
                    self.detail = None;
                    self.texture = None;
                    self.selected_asset = None;
                }
            });
        egui::CentralPanel::default().show(ctx,|ui|{
            ui.add_space(8.0);ui.horizontal(|ui|{ui.add(egui::TextEdit::singleline(&mut self.patient_query).hint_text("Поиск по ФИО или внешнему ID").desired_width((ui.available_width()-100.0).max(140.0)));if ui.button("Обновить").clicked(){let r=self.refresh();self.status(r,"Список обновлен");}});ui.add_space(10.0);
            let query=self.patient_query.to_lowercase();let mut pick=None;
            egui::ScrollArea::vertical().show(ui,|ui|{egui::Grid::new("patient-table").striped(true).num_columns(2).spacing([22.0,13.0]).min_col_width(150.0).show(ui,|ui|{ui.label(egui::RichText::new("ФИО пациента").weak());ui.label(egui::RichText::new("Внешний ID").weak());ui.end_row();for p in &self.patients{if !format!("{} {}",name(p),p.external_id.as_deref().unwrap_or("")).to_lowercase().contains(&query){continue;}
 if ui.selectable_label(Some(p.id)==self.patient,name(p)).clicked(){pick=Some(p.id);}ui.add(egui::Label::new(egui::RichText::new(p.external_id.as_deref().unwrap_or("Не передан")).monospace().small()).wrap().selectable(true));ui.end_row();}});
            ui.add_space(16.0);ui.collapsing("Создать локальную карточку вручную",|ui|{ui.label("Фамилия");ui.text_edit_singleline(&mut self.family);ui.label("Имя");ui.text_edit_singleline(&mut self.given);ui.label("Отчество");ui.text_edit_singleline(&mut self.patronymic);ui.label(egui::RichText::new("Внешний GUID присваивается внешней системой; приложение его не выдумывает.").small().weak());if ui.add_enabled(!self.dirty,egui::Button::new("Создать карточку")).clicked(){let r=self.connection.as_ref().unwrap().post::<_,Patient>("/api/v1/patients",&NewPatient{family_name:self.family.clone(),given_name:self.given.clone(),patronymic:(!self.patronymic.trim().is_empty()).then(||self.patronymic.clone()),birth_date:None}).map(|p|{pick=Some(p.id);});self.status(r,"Карточка сохранена локально");}});
            });
            if let Some(id)=pick{if self.dirty{self.message="Сохраните текущую работу перед сменой пациента".into();}else{self.patient=Some(id);self.order=None;if self.detail.as_ref().is_some_and(|d|d.patient.id!=id){self.detail=None;self.texture=None;self.selected_asset=None;}}}
        });
    }
    fn study_view(&mut self, ctx: &egui::Context) {
        let Some(detail) = self.detail.clone() else {
            self.patients_view = true;
            return;
        };
        egui::TopBottomPanel::top("study-identity").show(ctx, |ui| {
            ui.add_space(3.0);
            ui.horizontal_wrapped(|ui| {
                ui.strong(name(&detail.patient));
                ui.label(
                    egui::RichText::new(status(&detail.study.status))
                        .small()
                        .weak(),
                );
                ui.separator();
                ui.label(egui::RichText::new("Внешний ID:").small().weak());
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(
                            detail
                                .patient
                                .external_id
                                .as_deref()
                                .unwrap_or("Не передан"),
                        )
                        .monospace()
                        .small(),
                    )
                    .selectable(true),
                );
            });
            ui.add_space(3.0);
        });
        let editable = detail.study.status != "approved";
        if editable
            && !ctx.wants_keyboard_input()
            && ctx.input(|i| i.key_pressed(egui::Key::Delete))
            && let Some(id) = self.selected_annotation.take()
        {
            crate::defects::remove(self.detail.as_mut().unwrap(), id);
            self.dirty = true;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.selected_annotation = None;
            self.photo_tool = 5;
        }

        let originals: Vec<_> = detail
            .assets
            .iter()
            .filter(|a| a.kind == "original")
            .cloned()
            .collect();
        self.warm_thumbnails(&originals, ctx);
        egui::TopBottomPanel::bottom("native-filmstrip")
            .exact_height(132.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.strong(format!("Снимки · {}", originals.len()));
                    if ui.button("Все в отчет").clicked() {
                        self.selected_photos = originals.iter().map(|a| a.id).collect();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                self.background_jobs == 0,
                                egui::Button::new("Заполнить протокол →"),
                            )
                            .clicked()
                        {
                            let r = self.open_html(ctx, false);
                            self.status(r, "Протокол открыт");
                        }
                    });
                });
                egui::ScrollArea::horizontal().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for (i, asset) in originals.iter().enumerate() {
                            let mut included = self.selected_photos.contains(&asset.id);
                            let response = crate::editor_ui::thumbnail(
                                ui,
                                self.thumbnails.get(&asset.id),
                                i + 1,
                                Some(asset.id) == self.selected_asset,
                                &mut included,
                            );
                            if included {
                                self.selected_photos.insert(asset.id);
                            } else {
                                self.selected_photos.remove(&asset.id);
                            }
                            if response.clicked() {
                                self.selected_annotation = None;
                                let r = self.load_photo(asset.id, ctx);
                                self.status(r, "");
                            }
                            ui.add_space(4.0);
                        }
                    });
                });
            });
        let target = self
            .detail
            .as_ref()
            .unwrap()
            .annotations
            .iter()
            .find(|a| {
                Some(a.asset_id) == self.selected_asset
                    && matches!(a.layer, AnnotationLayer::Target { .. })
            })
            .cloned();
        egui::SidePanel::left("native-image-tools")
            .exact_width(52.0)
            .resizable(false)
            .show(ctx, |ui| {
                ui.add_space(8.0);
                let target_visible = target.as_ref().is_some_and(|a| self.layer_view.visible(a));
                if let Some(action) = crate::editor_ui::tools(ui, self.photo_tool, target_visible) {
                    use crate::editor_ui::ToolAction;
                    match action {
                        ToolAction::Select => self.photo_tool = 5,
                        ToolAction::Pan => self.photo_tool = 0,
                        ToolAction::Fit => self.photo_zoom = 1.0,
                        ToolAction::ZoomIn => self.photo_zoom = (self.photo_zoom * 1.25).min(4.0),
                        ToolAction::ZoomOut => self.photo_zoom = (self.photo_zoom / 1.25).max(0.25),
                        ToolAction::Crop => self.photo_tool = 3,
                        ToolAction::Whole if editable => self.replace_image_layer(None, false),
                        ToolAction::Target => {
                            if let Some(a) = &target {
                                self.layer_view.toggle(a.id);
                            } else if editable && self.selected_asset.is_some() {
                                self.replace_image_layer(
                                    Some(Annotation {
                                        layer: AnnotationLayer::Target {
                                            rotation_degrees: 0.0,
                                            include_in_report: true,
                                        },
                                        coordinate_system: Default::default(),
                                        id: Uuid::new_v4(),
                                        asset_id: self.selected_asset.unwrap(),
                                        lesion_id: None,
                                        finding_id: None,
                                        x: 0.5,
                                        y: 0.5,
                                        radius: 0.44,
                                        label: "Мишень".into(),
                                    }),
                                    true,
                                );
                            }
                        }
                        _ => (),
                    }
                }
            });
        egui::SidePanel::right("native-image-layers")
            .default_width(238.0)
            .min_width(224.0)
            .max_width(300.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("image-inspector")
                    .show(ui, |ui| {
                        ui.add_space(8.0);
                        ui.strong("Слои снимка");
                        ui.add_space(4.0);
                        let annotations: Vec<_> = self
                            .detail
                            .as_ref()
                            .unwrap()
                            .annotations
                            .iter()
                            .filter(|a| Some(a.asset_id) == self.selected_asset)
                            .cloned()
                            .collect();
                        let action = crate::editor_ui::layers(
                            ui,
                            &annotations,
                            self.selected_annotation,
                            &mut self.layer_view,
                            editable,
                        );
                        if let Some(action) = action {
                            match action {
                                crate::editor_ui::LayerAction::Select(id) => {
                                    self.selected_annotation = Some(id);
                                    self.photo_tool = 5;
                                }
                                crate::editor_ui::LayerAction::Edit(id) => {
                                    self.defect_picker = Some(id)
                                }
                                crate::editor_ui::LayerAction::Delete(id) => {
                                    crate::defects::remove(self.detail.as_mut().unwrap(), id);
                                    self.selected_annotation = None;
                                    self.dirty = true;
                                }
                            }
                        }
                        ui.separator();
                        ui.strong("Обозначения");
                        ui.add_space(6.0);
                        if let Some(choice) =
                            crate::editor_ui::palette(ui, self.defect_choice, editable)
                        {
                            self.defect_choice = choice;
                            self.photo_tool = 6;
                        }
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new("Перетащите на снимок").small().weak());
                    });
            });
        if let Some(id) = self.defect_picker {
            let mut open = true;
            let mut choice = None;
            egui::Window::new("Обозначение дефекта")
                .vscroll(true)
                .max_height(420.0)
                .open(&mut open)
                .collapsible(false)
                .show(ctx, |ui| {
                    for (i, p) in crate::defects::PRESETS.iter().enumerate() {
                        if ui.button(format!("{} — {}", p.short, p.name)).clicked() {
                            choice = Some(i);
                        }
                    }
                });
            if let Some(choice) = choice {
                let result = crate::defects::retag(
                    self.detail.as_mut().unwrap(),
                    self.dictionary.as_ref().unwrap(),
                    id,
                    choice,
                );
                if result.is_ok() {
                    self.dirty = true;
                    self.defect_picker = None;
                }
                self.status(result, "Обозначение изменено");
            }
            if !open {
                self.defect_picker = None;
            }
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        editable && self.background_jobs == 0,
                        egui::Button::new("＋ Добавить снимки"),
                    )
                    .clicked()
                    && let Some(files) = rfd::FileDialog::new()
                        .add_filter("Снимки", &["jpg", "jpeg", "png", "bmp", "tif", "tiff"])
                        .pick_files()
                {
                    self.start_import(detail.study.id, files);
                }
                ui.label(if self.photo_zoom == 1.0 {
                    "Вписан целиком".into()
                } else {
                    format!("{:.0}% от вписанного", self.photo_zoom * 100.0)
                });
                if ui
                    .add_enabled(self.dirty, egui::Button::new("Сохранить разметку"))
                    .clicked()
                {
                    let r = self.save();
                    self.status(r, "Разметка сохранена локально");
                }
            });
            ui.separator();
            if self.texture.is_none()
                && self.selected_asset.is_none()
                && let Some(first) = originals.first()
            {
                let _ = self.load_photo(first.id, ctx);
            }
            if let Some(texture) = &self.texture {
                let available = ui.available_size();
                let fit = (available.x / texture.size_vec2().x)
                    .min((available.y - 25.0).max(50.0) / texture.size_vec2().y);
                let size = texture.size_vec2() * fit * self.photo_zoom;
                let texture_id = texture.id();
                egui::ScrollArea::both()
                    .drag_to_scroll(self.photo_tool == 0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let response = ui
                            .allocate_ui_with_layout(
                                egui::vec2(available.x.max(size.x), available.y.max(size.y)),
                                egui::Layout::top_down(egui::Align::Center),
                                |ui| {
                                    ui.add_space(((available.y - size.y) * 0.5).max(0.0));
                                    ui.add(egui::Image::new((texture_id, size)).sense(
                                        if self.photo_tool == 5 {
                                            egui::Sense::click_and_drag()
                                        } else if matches!(self.photo_tool, 3 | 4) {
                                            egui::Sense::drag()
                                        } else if matches!(self.photo_tool, 1 | 2 | 6) {
                                            egui::Sense::click()
                                        } else {
                                            egui::Sense::hover()
                                        },
                                    ))
                                },
                            )
                            .inner;
                        let rect = response.rect;
                        let color = ui.visuals().selection.stroke.color;
                        if matches!(self.photo_tool, 3 | 4)
                            && response.drag_started()
                            && let Some(pos) = ui.input(|i| i.pointer.press_origin())
                        {
                            self.crop_start = Some(egui::pos2(
                                ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
                                ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0),
                            ));
                        }
                        if matches!(self.photo_tool, 3 | 4)
                            && response.dragged()
                            && let (Some(start), Some(pos)) =
                                (self.crop_start, response.interact_pointer_pos())
                        {
                            let start = egui::pos2(
                                rect.left() + start.x * rect.width(),
                                rect.top() + start.y * rect.height(),
                            );
                            if self.photo_tool == 4 {
                                ui.painter().arrow(
                                    start,
                                    pos - start,
                                    egui::Stroke::new(2.0, color),
                                );
                            }
                            let r = egui::Rect::from_two_pos(start, pos);
                            let corners = [
                                r.left_top(),
                                r.right_top(),
                                r.right_bottom(),
                                r.left_bottom(),
                                r.left_top(),
                            ];
                            for pair in corners.windows(2).filter(|_| self.photo_tool == 3) {
                                ui.painter().line_segment(
                                    [pair[0], pair[1]],
                                    egui::Stroke::new(2.0, color),
                                );
                            }
                        }

                        if editable
                            && let Some(payload) =
                                response.dnd_release_payload::<crate::editor_ui::Stamp>()
                            && let Some(pos) = ui.input(|i| i.pointer.hover_pos())
                            && rect.contains(pos)
                        {
                            let x = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                            let y = ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
                            {
                                let preset = payload.0;
                                let result = crate::defects::place(
                                    self.detail.as_mut().unwrap(),
                                    self.dictionary.as_ref().unwrap(),
                                    preset,
                                    self.selected_asset.unwrap(),
                                    x,
                                    y,
                                );
                                match result {
                                    Ok(id) => {
                                        self.selected_annotation = Some(id);
                                        self.photo_tool = 5;
                                        self.dirty = true;
                                    }
                                    Err(e) => self.message = e,
                                }
                            }
                        }
                        if editable
                            && self.photo_tool == 6
                            && response.clicked()
                            && let Some(pos) = response.interact_pointer_pos()
                        {
                            let x = (pos.x - rect.left()) / rect.width();
                            let y = (pos.y - rect.top()) / rect.height();
                            match crate::defects::place(
                                self.detail.as_mut().unwrap(),
                                self.dictionary.as_ref().unwrap(),
                                self.defect_choice,
                                self.selected_asset.unwrap(),
                                x,
                                y,
                            ) {
                                Ok(id) => {
                                    self.selected_annotation = Some(id);
                                    self.photo_tool = 5;
                                    self.dirty = true;
                                }
                                Err(e) => self.message = e,
                            }
                        }
                        if editable && self.photo_tool == 5 {
                            if (response.drag_started()
                                || response.clicked()
                                || response.double_clicked())
                                && let Some(pos) = ui
                                    .input(|i| i.pointer.press_origin())
                                    .or(response.interact_pointer_pos())
                            {
                                let mut hit = None;
                                for a in self
                                    .detail
                                    .as_ref()
                                    .unwrap()
                                    .annotations
                                    .iter()
                                    .rev()
                                    .filter(|a| {
                                        Some(a.asset_id) == self.selected_asset
                                            && self.layer_view.visible(a)
                                    })
                                {
                                    let center = egui::pos2(
                                        rect.left() + a.x * rect.width(),
                                        rect.top() + a.y * rect.height(),
                                    );
                                    let r = a.radius * rect.width().min(rect.height());
                                    if let AnnotationLayer::Callout { end_x, end_y, .. } = a.layer {
                                        let tip = egui::pos2(
                                            rect.left() + end_x * rect.width(),
                                            rect.top() + end_y * rect.height(),
                                        );
                                        let handle = if tip.distance(pos) < 12.0 {
                                            Some(crate::defects::Handle::Tip)
                                        } else if (center.distance(pos) - r).abs() < 9.0 {
                                            Some(crate::defects::Handle::Radius)
                                        } else if center.distance(pos) < r {
                                            Some(crate::defects::Handle::Body)
                                        } else {
                                            None
                                        };
                                        if let Some(handle) = handle {
                                            hit = Some((a.id, handle));
                                            break;
                                        }
                                    } else if matches!(a.layer, AnnotationLayer::Target { .. })
                                        && ((center.distance(pos) - r).abs() < 9.0
                                            || center.distance(pos) < 15.0)
                                    {
                                        hit = Some((
                                            a.id,
                                            if (center.distance(pos) - r).abs() < 9.0 {
                                                crate::defects::Handle::Radius
                                            } else {
                                                crate::defects::Handle::Body
                                            },
                                        ));
                                        break;
                                    } else if matches!(
                                        a.layer,
                                        AnnotationLayer::Mark | AnnotationLayer::Arrow { .. }
                                    ) && center.distance(pos) < r.max(15.0)
                                    {
                                        hit = Some((a.id, crate::defects::Handle::Body));
                                        break;
                                    }
                                }
                                self.selected_annotation = hit.map(|h| h.0);
                                if let Some((id, handle)) = hit {
                                    self.annotation_handle = handle;
                                    let origin = self
                                        .detail
                                        .as_ref()
                                        .unwrap()
                                        .annotations
                                        .iter()
                                        .find(|a| a.id == id)
                                        .unwrap();
                                    self.annotation_drag_offset =
                                        if handle == crate::defects::Handle::Body {
                                            egui::vec2(
                                                origin.x - (pos.x - rect.left()) / rect.width(),
                                                origin.y - (pos.y - rect.top()) / rect.height(),
                                            )
                                        } else {
                                            egui::Vec2::ZERO
                                        };
                                    if response.drag_started() {
                                        self.moving_annotation = Some(id);
                                    }
                                    if response.double_clicked()
                                        && self.detail.as_ref().unwrap().annotations.iter().any(
                                            |a| {
                                                a.id == id
                                                    && matches!(
                                                        a.layer,
                                                        AnnotationLayer::Callout { .. }
                                                    )
                                            },
                                        )
                                    {
                                        self.defect_picker = Some(id);
                                    }
                                }
                            }
                            if response.dragged()
                                && let Some(pos) = response.interact_pointer_pos()
                                && let Some(id) = self.moving_annotation
                                && let Some(a) = self
                                    .detail
                                    .as_mut()
                                    .unwrap()
                                    .annotations
                                    .iter_mut()
                                    .find(|a| a.id == id)
                            {
                                crate::defects::edit(
                                    a,
                                    self.annotation_handle,
                                    (pos.x - rect.left()) / rect.width()
                                        + self.annotation_drag_offset.x,
                                    (pos.y - rect.top()) / rect.height()
                                        + self.annotation_drag_offset.y,
                                    rect.width() / rect.height(),
                                );
                                self.dirty = true;
                            }
                            if response.drag_stopped() {
                                self.moving_annotation = None;
                            }
                        }
                        let d = self.detail.as_mut().unwrap();
                        let mut layers: Vec<_> = d
                            .annotations
                            .iter()
                            .filter(|a| {
                                Some(a.asset_id) == self.selected_asset
                                    && self.layer_view.visible(a)
                            })
                            .collect();
                        layers.sort_by_key(|a| matches!(a.layer, AnnotationLayer::Callout { .. }));
                        for annotation in layers {
                            crate::editor_ui::paint_layer(ui.painter(), rect, annotation, color);
                            if self.selected_annotation == Some(annotation.id) {
                                let c = egui::pos2(
                                    rect.left() + annotation.x * rect.width(),
                                    rect.top() + annotation.y * rect.height(),
                                );
                                let r = annotation.radius * rect.width().min(rect.height());
                                for pos in [
                                    c + egui::vec2(r, 0.0),
                                    c - egui::vec2(r, 0.0),
                                    c + egui::vec2(0.0, r),
                                    c - egui::vec2(0.0, r),
                                ] {
                                    ui.painter().rect_filled(
                                        egui::Rect::from_center_size(pos, egui::vec2(7.0, 7.0)),
                                        1.0,
                                        color,
                                    );
                                }
                                if let AnnotationLayer::Callout { end_x, end_y, .. } =
                                    annotation.layer
                                {
                                    let tip = egui::pos2(
                                        rect.left() + end_x * rect.width(),
                                        rect.top() + end_y * rect.height(),
                                    );
                                    ui.painter().circle_filled(tip, 5.0, color);
                                }
                            }
                        }
                        if editable
                            && self.photo_tool == 1
                            && response.clicked()
                            && let Some(pos) = response.interact_pointer_pos()
                        {
                            d.annotations.push(Annotation {
                                layer: Default::default(),
                                coordinate_system: Default::default(),
                                id: Uuid::new_v4(),
                                asset_id: self.selected_asset.unwrap(),
                                lesion_id: self.lesion_link,
                                finding_id: None,
                                x: (pos.x - rect.left()) / rect.width(),
                                y: (pos.y - rect.top()) / rect.height(),
                                radius: self.radius,
                                label: self.label.clone(),
                            });
                            self.dirty = true;
                        }
                        if editable
                            && self.photo_tool == 2
                            && response.clicked()
                            && let Some(pos) = response.interact_pointer_pos()
                            && let Some(a) = d.annotations.iter_mut().find(|a| {
                                Some(a.asset_id) == self.selected_asset
                                    && matches!(a.layer, AnnotationLayer::Target { .. })
                            })
                        {
                            a.x = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                            a.y = ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
                            self.dirty = true;
                        }
                        if editable
                            && matches!(self.photo_tool, 3 | 4)
                            && response.drag_stopped()
                            && let Some(pos) = response.interact_pointer_pos()
                            && let Some(start) = self.crop_start.take()
                        {
                            let x1 = start.x;
                            let y1 = start.y;
                            let x2 = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                            let y2 = ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
                            if (self.photo_tool == 4 && (x1 - x2).hypot(y1 - y2) > 0.005)
                                || ((x1 - x2).abs() > 0.005 && (y1 - y2).abs() > 0.005)
                            {
                                let a = Annotation {
                                    layer: if self.photo_tool == 4 {
                                        AnnotationLayer::Arrow {
                                            end_x: x2,
                                            end_y: y2,
                                        }
                                    } else {
                                        AnnotationLayer::OutputRegion {
                                            width: (x1 - x2).abs(),
                                            height: (y1 - y2).abs(),
                                        }
                                    },
                                    coordinate_system: Default::default(),
                                    id: Uuid::new_v4(),
                                    asset_id: self.selected_asset.unwrap(),
                                    lesion_id: None,
                                    finding_id: None,
                                    x: if self.photo_tool == 4 { x1 } else { x1.min(x2) },
                                    y: if self.photo_tool == 4 { y1 } else { y1.min(y2) },
                                    radius: 0.01,
                                    label: if self.photo_tool == 4 {
                                        self.label.clone()
                                    } else {
                                        "Область отчета".into()
                                    },
                                };
                                if self.photo_tool == 4 {
                                    self.detail.as_mut().unwrap().annotations.push(a);
                                    self.dirty = true;
                                } else {
                                    self.replace_image_layer(Some(a), false);
                                }
                            }
                        }
                    });
            } else {
                ui.vertical_centered(|ui| {
                    ui.add_space(80.0);
                    ui.heading(if self.photo_busy {
                        "Открываем снимок…"
                    } else {
                        "Добавьте снимки обследования"
                    });
                    ui.label("Оригиналы и разметка сохраняются локально. Сеть работает в фоне.");
                });
            }
        });
    }
    fn replace_image_layer(&mut self, layer: Option<Annotation>, target: bool) {
        let Some(asset) = self.selected_asset else {
            return;
        };
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        if detail.study.status == "approved" {
            return;
        }
        detail.annotations.retain(|a| {
            a.asset_id != asset
                || if target {
                    !matches!(a.layer, AnnotationLayer::Target { .. })
                } else {
                    !matches!(a.layer, AnnotationLayer::OutputRegion { .. })
                }
        });
        if let Some(layer) = layer {
            detail.annotations.push(layer);
        }
        self.dirty = true;
    }
    fn warm_thumbnails(&mut self, assets: &[Asset], ctx: &egui::Context) {
        for asset in assets.iter().take(30) {
            if self.thumbnail_busy.len() >= 3 {
                break;
            }
            if self.thumbnails.contains_key(&asset.id)
                || self.thumbnail_busy.contains(&asset.id)
                || self
                    .thumbnail_retry
                    .get(&asset.id)
                    .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(5))
            {
                continue;
            }
            let id = asset.id;
            self.thumbnail_busy.insert(id);
            self.thumbnail_retry.insert(id, std::time::Instant::now());
            let connection = self.connection.as_ref().unwrap().clone();
            let ctx = ctx.clone();
            let sender = self.events_tx.clone();
            std::thread::spawn(move || {
                let result = (|| {
                    let bytes = connection.download(id)?;
                    let image = crate::imaging::decode(&bytes)?
                        .thumbnail(160, 100)
                        .to_rgba8();
                    Ok(ctx.load_texture(
                        format!("thumb-{id}"),
                        egui::ColorImage::from_rgba_unmultiplied(
                            [image.width() as usize, image.height() as usize],
                            image.as_raw(),
                        ),
                        egui::TextureOptions::LINEAR,
                    ))
                })();
                let _ = sender.send(UiEvent::Thumbnail(id, result));
                ctx.request_repaint();
            });
        }
    }
}
