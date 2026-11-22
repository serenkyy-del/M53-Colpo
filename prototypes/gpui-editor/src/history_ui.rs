use super::*;
impl Workplace {
    pub(super) fn protocol_metadata_bar(&self,cx:&Context<Self>)->Option<AnyElement>{
        if (self.history.is_some() && !self.approved_history())||self.screen==Screen::Patients||self.screen==Screen::Settings{return None;}
        let detail=self.detail.as_ref()?;
        let (metadata,status,revision)=if let Some(history)=self.history.as_ref() {
            (&history.metadata,"approved",history.selected)
        } else {
            (&self.state["protocol_metadata"],detail.study.status.as_str(),detail.revision_id)
        };
        let text=match m53_client::report_identity::validate_status(metadata,status).and_then(|_|m53_client::report_identity::performer(metadata,detail.study.id,revision)){
            Ok(name)=>{
                let mut text=format!("{} · Врач исследования: {name}",crate::protocol_history::revision_label(metadata));
                if let Some(model)=m53_client::report_identity::equipment(metadata,None) {
                    text.push_str(&format!(" · Кольпоскоп: {model}"));
                }
                text
            },
            Err(_)=>"Сведения об исполнителе и редакции ещё не загружены".into(),
        };let tooltip=text.clone();
        Some(div().id("protocol-revision-metadata").test_support().px_4().py_1().text_xs().text_color(cx.theme().muted_foreground)
            .text_ellipsis().child(text).tooltip(move|window,cx|Tooltip::new(tooltip.clone()).build(window,cx)).into_any_element())
    }
    pub(super) fn open_history(&mut self,study:Uuid,cx:&mut Context<Self>){
        if self.history.is_some()||self.workflow_request.is_some()||self.approval_busy||self.patient_editor.is_some()||self.patient_edit_request.is_some(){return;}
        if self.editor.read(cx).io_pending>0 || self.editor.read(cx).photo_pending>0 {
            self.message="Дождитесь локального сохранения снимков перед открытием истории".into();cx.notify();return;
        }
        if self.video_running {self.stop_video(cx);}
        self.editor.update(cx,|editor,cx|{if editor.gesture.is_some(){editor.finish(cx);}});
        self.flush_form(cx,true);
        self.history=Some(crate::protocol_history::HistoryState::new(study));
        self.request_history(None,None,cx);
    }
    pub(super) fn close_history(&mut self,cx:&mut Context<Self>) {
        #[cfg(windows)] self.release_history_pages(cx);
        if let Some(history)=self.history.take()
            && let Some(snapshot)=history.editor_snapshot
        {
            let readonly=self.detail.as_ref().is_some_and(|detail|detail.study.status=="approved")
                || self.logging_out || self.approval_busy || self.workflow_request.is_some()
                || self.state["authenticated"]!=true || self.form.as_ref().is_some_and(|form|form.read(cx).readonly);
            self.editor.update(cx,|editor,cx| {
                editor.session=snapshot.session;
                editor.selected=snapshot.selected;
                editor.export_patient=snapshot.export_patient;
                editor.historical_readonly=snapshot.historical_readonly;
                editor.readonly=snapshot.readonly || readonly;
                editor.zoom=snapshot.zoom;editor.pan=snapshot.pan;
                editor.properties=None;
                editor.photo_demand=None;
                cx.notify();
            });
        }
    }
    #[cfg(windows)]
    pub(super) fn release_history_pages(&mut self,cx:&mut Context<Self>) {
        if let Some(history)=self.history.as_mut()
            && let Some(Ok(pages))=history.pdf_pages.take()
        {
            for image in pages.pages { cx.drop_image(image,None); }
        }
    }

    pub(super) fn request_history(&mut self,revision:Option<Uuid>,before:Option<i64>,cx:&mut Context<Self>){
        if self.report_busy {return;}
        let Some(history)=self.history.as_mut()else{return;};if history.busy(){return;}
        let request=Uuid::new_v4();history.request=Some((request,revision));history.note.clear();
        if revision.is_some(){
            history.selected=revision;history.form=None;history.metadata=Value::Null;history.report_lines=None;history.show_photo=false;
            #[cfg(windows)] {
                history.pdf_request=None;
                if let Some(Ok(pages))=history.pdf_pages.take() {
                    for image in pages.pages { cx.drop_image(image,None); }
                }
            }
        }
        if self.worker.send(Command::ProtocolHistory{request,study:self.history.as_ref().unwrap().study,revision,before}).is_err()
            && let Some(history)=self.history.as_mut()
        {
            history.request=None;history.note="Не работает локальный обработчик истории".into();
        }
        cx.notify();
    }
    pub(super) fn receive_history(&mut self,request:Uuid,study:Uuid,revision:Option<Uuid>,result:Result<(Value,Option<Dictionary>),String>,cx:&mut Context<Self>){
        let Some(history)=self.history.as_mut().filter(|h|h.study==study&&h.request==Some((request,revision)))else{return;};history.request=None;
        #[cfg(windows)]
        let mut pdf = None;
        match result{
            Err(e)=>history.note=e,
            Ok((value,dictionary))=>{
                if let Some(revision)=revision {
                    if value["readonly"]!=true || (history.prefer_approved && value["status"]!="approved")
                        || m53_client::report_identity::performer(&value,study,Some(revision)).is_err(){history.note="Не удалось подтвердить историческую редакцию".into();return;}
                    let Some(dictionary)=dictionary else{history.note="Нет словаря этой редакции".into();return;};
                    if history.prefer_approved {
                        history.report_lines = Some(crate::protocol_history::historical_report(study, revision, &value, &dictionary));
                        #[cfg(windows)]
                        if let Some(template)=self.templates.first().cloned() {
                            history.pdf_request=Some(request);
                            pdf=Some(Command::HistoricalPdf{request,study,revision,document:value.clone(),dictionary:dictionary.clone(),template});
                        } else {
                            history.pdf_pages=Some(Err("Шаблон отчёта недоступен".into()));
                        }
                    }
                    {
                        if !history.prefer_approved && history.editor_snapshot.is_none() {
                            let editor=self.editor.read(cx);
                            history.editor_snapshot=Some(crate::protocol_history::EditorSnapshot {
                                session:editor.session.clone(),selected:editor.selected,export_patient:editor.export_patient.clone(),
                                readonly:editor.readonly,historical_readonly:editor.historical_readonly,zoom:editor.zoom,pan:editor.pan,
                            });
                        }
                        let annotations: Vec<Annotation> = serde_json::from_value(value["annotations"].clone()).unwrap_or_default();
                        let selected: std::collections::BTreeSet<_> = m53_client::report_identity::approved_assets(&value,study)
                            .ok().flatten().unwrap_or_default().into_iter().map(|asset|asset.id).collect();
                        let export_patient=m53_client::report_identity::patient(&value).ok().map(|patient| {
                            let date=value["protocol"]["colpo"]["fields"]["context.exam_date"]["value"]
                                .as_str().unwrap_or("").to_owned();
                            (patient_name(&patient),date)
                        });
                        self.editor.update(cx, |editor,cx|{
                            editor.export_patient=export_patient;
                            editor.historical_readonly=true;
                            editor.readonly=true;
                            for photo in &mut editor.session.photos {
                                photo.annotations=annotations.iter().filter(|annotation|annotation.asset_id==photo.id).cloned().collect();
                                photo.in_report=selected.contains(&photo.id);
                                photo.hidden.clear();
                                if !photo.annotations.is_empty() || photo.in_report {photo.removed=false;}
                            }
                            cx.notify();
                        });
                    }
                    let photo_order=self.editor.read(cx).session.photos.iter().filter(|photo|!photo.removed).map(|photo|photo.id).collect();
                    history.form=Some(cx.new(|_|{
                        let mut form=NativeForm::new(value["protocol"].clone(),dictionary,Default::default(),true);
                        form.photo_order=photo_order;
                        form
                    }));
                    history.metadata=value;
                }else{
                    match crate::protocol_history::history_rows(&value,study){
                        Ok(rows)=>{
                            if rows.iter().any(|r|history.rows.iter().any(|old|old["revision_id"]==r["revision_id"])){history.note="История изменилась. Закройте и откройте её заново".into();return;}
                            let next=value["next_before_revision"].as_i64();
                            if next.is_some_and(|next|next<1||rows.last().and_then(|r|r["revision_number"].as_i64()).is_none_or(|last|next!=last)){history.note="Некорректный курсор истории".into();return;}
                            history.next=next;history.rows.extend(rows);
                            if let Some(selected)=history.refresh_selected {
                                if history.rows.iter().any(|row|row["revision_id"]==json!(selected)) {
                                    history.refresh_selected=None;
                                    self.request_history(Some(selected),None,cx);
                                } else if let Some(before)=next {
                                    self.request_history(None,Some(before),cx);
                                } else {
                                    history.refresh_selected=None;
                                    history.note="Выбранная редакция больше недоступна. Выберите редакцию явно".into();
                                }
                                cx.notify();return;
                            }
                            if history.prefer_approved && history.selected.is_none() {
                                if let Some(id) = crate::protocol_history::latest_approved(&history.rows) {
                                    self.request_history(Some(id), None, cx);
                                } else if let Some(before) = next {
                                    self.request_history(None, Some(before), cx);
                                } else {
                                    self.history = None;
                                    self.editor.update(cx, |editor,cx| {
                                        editor.historical_readonly=false;
                                        editor.readonly=false;
                                        cx.notify();
                                    });
                                    self.screen = Screen::Study;
                                }
                                cx.notify();
                                return;
                            }
                        },Err(e)=>history.note=e,
                    }
                }
            }
        }
        #[cfg(windows)]
        if let Some(pdf)=pdf { self.send(pdf); }
        cx.notify();
    }
    pub(super) fn history_revision_list(&self,cx:&Context<Self>)->AnyElement {
        let history=self.history.as_ref().unwrap();
        let busy=history.busy() || self.report_busy;
        let mut list=v_flex().gap_2();
        for row in &history.rows{
            if history.prefer_approved && row["status"] != "approved" { continue; }
            let Some(id)=row["revision_id"].as_str().and_then(|s|Uuid::parse_str(s).ok())else{continue;};
            list=list.child(Button::new(SharedString::from(format!("history-{id}"))).outline().w_full().h(px(116.)).selected(history.selected==Some(id)).disabled(busy)
                .child(v_flex().w_full().gap_1().items_start()
                    .child(div().font_semibold().text_sm().child(format!("Редакция {} · {}",row["revision_number"].as_i64().unwrap_or(0),if row["status"]=="approved"{"утверждена"}else{"черновик"})))
                    .child(div().whitespace_normal().text_left().text_xs().child(format!("Создана: {}",crate::protocol_history::timestamp(row["created_at"].as_str()))))
                    .child(div().whitespace_normal().text_left().text_xs().child(format!("Утверждена: {}",crate::protocol_history::timestamp(row["approved_at"].as_str())))))
                .on_click(cx.listener(move|s,_,_,cx|s.request_history(Some(id),None,cx))));
        }
        if let Some(before)=history.next{list=list.child(Button::new("more-revisions").outline().label("Ещё редакции").disabled(busy).on_click(cx.listener(move|s,_,_,cx|s.request_history(None,Some(before),cx))));}
        v_flex().w(px(270.)).h_full().flex_shrink_0().gap_2().p_3()
            .border_r_1().border_color(cx.theme().border)
            .child(Button::new("refresh-protocol-history").outline().label("Обновить список")
                .disabled(busy).on_click(cx.listener(|s,_,_,cx|{
                    if s.report_busy || s.history.as_ref().is_some_and(|history|history.busy()) {return;}
                    if let Some(history)=s.history.as_ref(){
                        let mut reset=crate::protocol_history::HistoryState::new(history.study);
                        reset.prefer_approved=history.prefer_approved;
                        reset.revisions_expanded=history.revisions_expanded;
                        reset.selected=history.selected;
                        reset.refresh_selected=history.selected;
                        // The live editor belongs to this history session until it is closed.
                        reset.editor_snapshot=s.history.as_mut().and_then(|history|history.editor_snapshot.take());
                        #[cfg(windows)] s.release_history_pages(cx);
                        s.history=Some(reset);
                        s.request_history(None,None,cx);
                    }
                })))
            .child(div().id("history-revisions-scroll").flex_1().min_h_0().overflow_y_scroll().child(list))
            .into_any_element()
    }

    pub(super) fn history_view(&self,cx:&Context<Self>)->AnyElement {
        let history=self.history.as_ref().unwrap();
        let metadata=&history.metadata;
        let mut content=v_flex().flex_1().h_full().min_w_0().min_h_0().gap_2();
        if metadata.is_object(){
            let performer=m53_client::report_identity::performer(metadata,history.study,history.selected).unwrap_or_else(|_|"Не указан".into());
            let patient=m53_client::report_identity::patient(metadata).map(|p|patient_name(&p)).unwrap_or_else(|_|"Данные пациента не сохранены".into());
            content=content.child(div().font_semibold().child(patient)).child(div().text_sm().child(format!("Врач исследования: {performer}")))
                .child(div().text_xs().child(crate::protocol_history::revision_label(metadata)));
        }
        if history.show_photo {
            content=content.child(Button::new("history-return-to-protocol").outline().label("Вернуться к протоколу редакции")
                .on_click(cx.listener(|s,_,_,cx|{if let Some(history)=s.history.as_mut(){history.show_photo=false;}cx.notify();})))
                .child(self.editor.clone());
        } else if let Some(form)=&history.form {
            content=content.child(div().id("historical-protocol-form").test_support().flex_1().min_h_0().child(form.clone()));
        } else {
            content=content.child(div().child(if history.request.is_some() {"Загружаем редакции…"} else {"Выберите редакцию слева"}));
        }
        v_flex().flex_1().min_h_0().gap_2().p_3()
            .child(h_flex().gap_2().child(div().font_semibold().flex_1().child("История протокола"))
                .child(Button::new("close-protocol-history").outline().label("Вернуться к приёму")
                    .on_click(cx.listener(|s,_,_,cx|{s.close_history(cx);cx.notify();}))))
            .when(!history.note.is_empty(),|d|d.child(div().text_sm().text_color(cx.theme().danger).child(history.note.clone())))
            .child(h_flex().flex_1().min_h_0().gap_3().child(self.history_revision_list(cx)).child(content))
            .into_any_element()
    }
}
