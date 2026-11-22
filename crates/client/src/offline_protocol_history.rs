use super::*;
impl Offline {
    pub fn protocol_history_document(&self,study:Uuid,revision:Option<Uuid>,before:Option<i64>)->Result<Value,String>{
        if study.is_nil() || revision.is_some_and(|id|id.is_nil()) || before.is_some_and(|n|n<1){return Err("Некорректная редакция протокола".into());}
        let suffix=revision.map(|id|format!("/{id}")).unwrap_or_default();
        let query=before.map(|n|format!("?before_revision={n}")).unwrap_or_default();
        self.read_live_value(&format!("/api/v1/studies/{study}/revisions{suffix}{query}"),"историю протокола")
    }
    pub fn protocol_history_dictionary(&self,version:Uuid)->Result<m53_domain::validation::Dictionary,String>{
        let value=self.read_live_value(&format!("/api/v1/terminology?version_id={version}"),"словарь редакции")?;
        let dictionary:m53_domain::validation::Dictionary=serde_json::from_value(value).map_err(|_|"Некорректный словарь исторической редакции".to_string())?;
        if dictionary.version_id!=version{return Err("Словарь относится к другой редакции".into());}Ok(dictionary)
    }
    pub(crate) fn read_live_value(&self,path:&str,subject:&str)->Result<Value,String>{
        let _writer=self.workers.writer()?;
        let raw=self.transport.lock().unwrap().clone();let scope=crate::diagnosis_cache::Scope::from_connection(&raw);
        let active=||!self.paused.load(std::sync::atomic::Ordering::SeqCst)&&!self.stopping.load(std::sync::atomic::Ordering::SeqCst);
        if !active()||raw.session.is_empty()||scope.is_none(){return Err(format!("Чтобы загрузить {subject}, подключитесь к серверу и войдите в программу"));}
        let response=raw.exchange("GET",path,None,None,None);
        let auth=self.transport.lock().unwrap();
        if !active()||auth.base!=raw.base||auth.key!=raw.key||auth.session!=raw.session||auth.user_id!=raw.user_id||crate::diagnosis_cache::Scope::from_connection(&auth)!=scope{return Err("Вход изменился. Повторите запрос".into());}
        drop(auth);let(status,bytes)=response.map_err(|_|format!("Не удалось загрузить {subject} с сервера"))?;
        if status==401{self.reject_session(&raw);}
        match status{200=>serde_json::from_slice(&bytes).map_err(|_|"Некорректный ответ сервера".into()),401|403=>Err(format!("Нет доступа: не удалось загрузить {subject}")),404=>Err("Запись не найдена или сервер ещё не обновлён".into()),_=>Err(format!("Сервер не смог загрузить {subject}"))}
    }
}
