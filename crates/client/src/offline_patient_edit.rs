//! Manual patient changes require a live acknowledgement and never replace the clinical outbox.
use super::*;
fn accept_demographics(state:&mut LocalState,id:Uuid,value:&Value){
    if crate::patient_demographics::reconcile_legacy_sex(&mut state.patient_sexes,id,value){state.change();}
}
#[derive(Deserialize)]
struct MatchingPatientPage{items:Vec<Patient>,next_cursor:Option<String>}
fn accept_matching_page(state:&mut LocalState,value:&Value)->Result<MatchingPatientPage,String>{
    let page:MatchingPatientPage=serde_json::from_value(value.clone()).map_err(|_|"Некорректная страница картотеки")?;
    // Validate the whole page before applying anything, then retain the raw
    // distinction between an omitted legacy field and an authoritative null.
    if let Some(rows)=value["items"].as_array(){
        for (patient,row) in page.items.iter().zip(rows){
            if !state.pending.iter().any(|op|op.path=="/api/v1/patients"&&op.entity_id==patient.id){
                accept_demographics(state,patient.id,row);
            }
        }
    }
    Ok(page)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn confirmed_sex_clear_removes_legacy_gender_without_changing_protocol_or_queue(){
        let id=Uuid::new_v4();
        let mut state:LocalState=serde_json::from_value(json!({"format_version":1,"profile":{"server":"http://127.0.0.1","user_id":Uuid::new_v4(),"doctor":"Synthetic"},"responses":{"saved_protocol":{"retained":true}},"blobs":{},"pending":[],"confirmed":{},"journal":{},"editors":{},"remote_conflicts":{},"conflict_history":[],"sequence":1})).unwrap();
        state.patient_sexes.insert(id,PatientSex::Female);let before=state.responses.clone();
        super::accept_demographics(&mut state,id,&json!({"sex":null}));
        assert!(!state.patient_sexes.contains_key(&id));assert_eq!(state.responses,before);assert!(state.pending.is_empty());assert!(state.sequence>1);
    }
    #[test]
    fn omitted_legacy_sex_does_not_clear_the_existing_fallback(){
        let id=Uuid::new_v4();
        let mut state:LocalState=serde_json::from_value(json!({"format_version":1,"profile":{"server":"http://127.0.0.1","user_id":Uuid::new_v4(),"doctor":"Synthetic"},"responses":{},"blobs":{},"pending":[],"confirmed":{},"journal":{},"editors":{},"remote_conflicts":{},"conflict_history":[],"sequence":1})).unwrap();
        state.patient_sexes.insert(id,PatientSex::Female);
        let before=serde_json::to_value(&state).unwrap();
        super::accept_demographics(&mut state,id,&json!({"id":id}));
        assert_eq!(serde_json::to_value(&state).unwrap(),before);
    }
    #[test]
    fn matching_page_uses_raw_null_without_rewriting_local_identity_or_frozen_request(){
        let cleared=Uuid::new_v4();let legacy=Uuid::new_v4();let pending=Uuid::new_v4();
        let mut state:LocalState=serde_json::from_value(json!({"format_version":1,"profile":{"server":"http://127.0.0.1","user_id":Uuid::new_v4(),"doctor":"Synthetic"},"responses":{"saved_protocol":{"retained":true}},"blobs":{},"pending":[],"confirmed":{},"journal":{},"editors":{},"remote_conflicts":{},"conflict_history":[],"sequence":1})).unwrap();
        state.patient_sexes=BTreeMap::from([(cleared,PatientSex::Female),(legacy,PatientSex::Male),(pending,PatientSex::Female)]);
        state.pending.push_back(Operation{id:Uuid::new_v4(),entity_id:pending,version_id:None,path:"/api/v1/patients".into(),method:"POST".into(),body:json!({"family_name":"Local","given_name":"Patient","sex":"F"}),blob:None,started:true,blocked:None,sequence:1});
        let before_queue=serde_json::to_value(&state.pending).unwrap();let before_responses=state.responses.clone();
        let page=json!({"items":[
            {"id":cleared,"family_name":"Synthetic","given_name":"Patient","sex":null},
            {"id":legacy,"family_name":"Legacy","given_name":"Patient"},
            {"id":pending,"family_name":"Remote","given_name":"Patient","sex":null}
        ],"next_cursor":null});
        let parsed=super::accept_matching_page(&mut state,&page).unwrap();
        assert_eq!(parsed.items.len(),3);
        assert!(!state.patient_sexes.contains_key(&cleared),"fresh explicit null must remove the old fallback");
        assert_eq!(state.patient_sexes.get(&legacy),Some(&PatientSex::Male));
        assert_eq!(state.patient_sexes.get(&pending),Some(&PatientSex::Female));
        assert_eq!(serde_json::to_value(&state.pending).unwrap(),before_queue);
        assert_eq!(state.responses,before_responses);
    }
    #[test]
    fn malformed_matching_page_cannot_partially_clear_legacy_demographics(){
        let id=Uuid::new_v4();
        let mut state:LocalState=serde_json::from_value(json!({"format_version":1,"profile":{"server":"http://127.0.0.1","user_id":Uuid::new_v4(),"doctor":"Synthetic"},"responses":{},"blobs":{},"pending":[],"confirmed":{},"journal":{},"editors":{},"remote_conflicts":{},"conflict_history":[],"sequence":1})).unwrap();
        state.patient_sexes.insert(id,PatientSex::Female);
        let before=serde_json::to_value(&state).unwrap();
        let page=json!({"items":[{"id":id,"family_name":"Synthetic","given_name":"Patient","sex":null},{"id":Uuid::new_v4(),"sex":null}]});
        assert!(super::accept_matching_page(&mut state,&page).is_err());
        assert_eq!(serde_json::to_value(&state).unwrap(),before);
    }
}
impl Offline {
    pub fn remember_matched_patient(&self,patient:&Patient)->Result<(),String>{
        let _writer=self.workers.writer()?;
        let mut state=self.state.lock().unwrap();
        let rows=state.responses.entry("/api/v1/patients".into()).or_insert(json!([])).as_array_mut().ok_or("Некорректный кеш картотеки")?;
        if !rows.iter().any(|row|row["id"]==json!(patient.id)) {
            rows.push(json!(patient));state.change();
        }
        Ok(())
    }
    pub fn matching_patients(&self,input:&NewPatient)->Result<Vec<Patient>,String>{
        let Some(date)=input.birth_date.as_deref() else{return Ok(vec![]);};
        if m53_contracts::colposcopy_1c::CalendarDate::parse(date).is_err(){return Err("Проверьте дату рождения".into());}
        let offline=self.transport.lock().unwrap().session.is_empty();
        let mut matches=BTreeMap::new();
        {
            let state=self.state.lock().unwrap();
            if let Some(rows)=state.responses.get("/api/v1/patients").and_then(Value::as_array){
                for row in rows {
                    if let Ok(p)=serde_json::from_value::<Patient>(row.clone()) {
                        if (offline || state.pending.iter().any(|op|op.path=="/api/v1/patients"&&op.entity_id==p.id)) && crate::patient_edit::same_patient(input,&p){matches.insert(p.id,p);}
                    }
                }
            }
        }
        if offline{return Ok(matches.into_values().collect());}
        let mut cursor=None;let mut seen=BTreeSet::new();
        for _ in 0..40 {
            let mut url=reqwest::Url::parse("http://localhost/api/v1/patients/page").unwrap();
            url.query_pairs_mut().append_pair("q",date).append_pair("limit","100");
            if let Some(cursor)=cursor.as_deref(){url.query_pairs_mut().append_pair("cursor",cursor);}
            let path=format!("{}?{}",url.path(),url.query().unwrap_or_default());
            let value=self.read_live_value(&path,"совпадения в картотеке")?;
            let page=accept_matching_page(&mut self.state.lock().unwrap(),&value)?;
            for patient in page.items {if crate::patient_edit::same_patient(input,&patient){matches.insert(patient.id,patient);}}
            let Some(next)=page.next_cursor else{return Ok(matches.into_values().collect());};
            if next.is_empty()||!seen.insert(next.clone()){break;}cursor=Some(next);
        }
        Err("Не удалось проверить все совпадения в картотеке".into())
    }
    pub fn patient_edit_context(&self,id:Uuid)->Result<Value,String> {
        let value=self.patient_edit_exchange("GET",id,None)?;
        if value["id"]!=json!(id) || value["row_version"].as_i64().is_none_or(|v|v<1) {return Err("Некорректная карточка пациента от сервера".into());}
        accept_demographics(&mut self.state.lock().unwrap(),id,&value);
        Ok(value)
    }
    pub fn update_manual_patient(&self,id:Uuid,body:Value)->Result<Value,String> {
        let _writer=self.workers.writer()?;
        self.require_edit_permission(false)?;
        let context=self.patient_edit_context(id)?;
        let version=crate::patient_edit::edit_version(&context,id)?;
        if body["expected_version"]!=json!(version) {return Err("Карточка пациента изменена. Откройте её заново; введённые данные сохранены в форме".into());}
        let value=self.patient_edit_exchange("PUT",id,Some(&body))?;
        crate::patient_edit::check_ack(&value,id,&body)?;
        accept_demographics(&mut self.state.lock().unwrap(),id,&value);
        Ok(value)
    }
    fn patient_edit_exchange(&self,method:&str,id:Uuid,body:Option<&Value>)->Result<Value,String> {
        if id.is_nil() {return Err("Не выбран пациент".into());}
        let _writer=self.workers.writer()?;
        let raw=self.transport.lock().unwrap().clone();
        let active=||!self.paused.load(std::sync::atomic::Ordering::SeqCst)&&!self.stopping.load(std::sync::atomic::Ordering::SeqCst);
        if !active() || raw.session.is_empty() {return Err("Для изменения карточки требуется подключение к серверу".into());}
        let path=format!("/api/v1/patients/{id}{}",if method=="GET" {"/edit-context"}else{""});
        let operation=Operation{id:Uuid::new_v4(),entity_id:id,version_id:None,path:path.clone(),method:method.into(),body:body.cloned().unwrap_or(Value::Null),blob:None,started:true,blocked:None,sequence:0};
        let response=raw.exchange(method,&path,body,None,body.map(|_|&operation));
        let auth=self.transport.lock().unwrap();
        if !active() || auth.base!=raw.base || auth.key!=raw.key || auth.session!=raw.session || auth.user_id!=raw.user_id {return Err("Вход изменился. Откройте карточку пациента повторно".into());}
        drop(auth);
        let(status,bytes)=response.map_err(|_|"Сервер не подтвердил изменение. Данные остаются в форме; обновите карточку перед повтором".to_owned())?;
        if status==401 {self.reject_session(&raw);}
        match status {
            200=>serde_json::from_slice(&bytes).map_err(|_|"Некорректный ответ карточки пациента".into()),
            409=>Err("Карточка пациента изменилась или связана с МИС. Обновите её перед повтором; введённый текст сохранён".into()),
            400=>Err("Проверьте ФИО, дату рождения и пол пациента".into()),
            401|403=>Err("Нет права изменять карточку пациента. Проверьте вход и права врача".into()),
            404=>Err("Пациент не найден или обновление сервера ещё не установлено".into()),
            _=>Err("Карточка пациента временно недоступна на сервере".into()),
        }
    }
}
