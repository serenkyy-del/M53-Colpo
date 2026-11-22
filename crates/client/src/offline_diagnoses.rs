//! Low-priority catalogue lane: one bounded page at a time; no clinical queue changes.
use super::*;
use crate::diagnosis_cache::{self, Scope, Snapshot};
impl Offline {
    /// Called by the clinical worker, never a UI callback. No offline mutation is queued.
    pub fn save_diagnosis_priorities(&self, base: crate::diagnosis_priority::Group, ids: Vec<Uuid>) -> Result<(), String> {
        let _writer = self.workers.writer()?;
        if self.paused.load(std::sync::atomic::Ordering::SeqCst) || self.stopping.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("Для изменения группы нужно подключение к серверу".into());
        }
        let raw = self.transport.lock().unwrap().clone();
        let scope = Scope::from_connection(&raw).ok_or("Войдите для изменения группы диагнозов")?;
        if raw.session.is_empty() { return Err("Для изменения группы диагнозов нужно подключение к серверу".into()); }
        {
            let state = self.diagnosis_catalog.lock().unwrap();
            if state.scope.as_ref() != Some(&scope) || state.snapshot.priority.as_deref() != Some(&base) {
                return Err("Группа диагнозов изменилась. Обновите настройки и повторите изменение".into());
            }
        }
        let body = crate::diagnosis_priority::update_body(&base, &ids)?;
        let op = Operation { id: Uuid::new_v4(), entity_id: raw.identity.as_ref().unwrap().organization_id,
            version_id: None, path: "/api/v1/diagnoses/priority".into(), method: "PUT".into(),
            body: body.clone(), blob: None, started: true, blocked: None, sequence: 0 };
        let (status, bytes) = raw.exchange("PUT", &op.path, Some(&body), None, Some(&op))
            .map_err(|_| "Сервер не подтвердил изменение группы. Обновите настройки перед повтором")?;
        if status == 409 { self.refresh_diagnosis_priorities(); return Err("Группу изменили на другом рабочем месте. Отмените правку и загрузите актуальную группу".into()); }
        let saved = crate::diagnosis_priority::decode_response(status, &bytes)?;
        if saved.version != base.version.checked_add(1).unwrap_or(-1) || saved.diagnosis_ids != ids {
            return Err("Сервер вернул неполное подтверждение изменения группы".into());
        }
        let auth = self.transport.lock().unwrap();
        if !same_auth(&auth, &raw) || self.stopping.load(std::sync::atomic::Ordering::SeqCst) { return Err("Вход изменился; обновите настройки группы".into()); }
        let mut cache = self.diagnosis_catalog.lock().unwrap();
        cache.publish_priority(&self.store, &scope, saved)
    }
    pub(super) fn start_diagnosis_priorities(self: &Arc<Self>) {
        let this = self.clone();
        self.workers.network(move || {
            let mut next = Instant::now();
            loop {
                if this.stopping.load(std::sync::atomic::Ordering::SeqCst) { break; }
                if this.paused.load(std::sync::atomic::Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(100)); continue;
                }
                let raw = this.transport.lock().unwrap().clone();
                let Some(scope) = Scope::from_connection(&raw) else { thread::sleep(Duration::from_millis(100)); continue; };
                if raw.session.is_empty() { thread::sleep(Duration::from_millis(100)); continue; }
                {
                    let mut cache = this.diagnosis_catalog.lock().unwrap();
                    if cache.scope.as_ref() != Some(&scope) || (!cache.priority_refresh && Instant::now() < next) {
                        drop(cache); thread::sleep(Duration::from_millis(100)); continue;
                    }
                    cache.priority_refresh = false;
                }
                let loaded = raw.diagnosis_catalog_exchange("/api/v1/diagnoses/priority")
                    .and_then(|(status, bytes)| {
                        if status == 401 || status == 403 { this.reject_session(&raw); }
                        crate::diagnosis_priority::decode_response(status, &bytes)
                    });
                let auth = this.transport.lock().unwrap();
                if !same_auth(&auth, &raw) || this.stopping.load(std::sync::atomic::Ordering::SeqCst) || this.paused.load(std::sync::atomic::Ordering::SeqCst) { continue; }
                let mut cache = this.diagnosis_catalog.lock().unwrap();
                if cache.scope.as_ref() != Some(&scope) { continue; }
                if loaded.as_ref().is_ok_and(|group| crate::diagnosis_priority::is_stale_poll(cache.snapshot.priority.as_deref(), group)) {
                    next = Instant::now() + Duration::from_secs(30);
                    continue;
                }
                let result = loaded.and_then(|group| cache.publish_priority(&this.store, &scope, group));
                if let Err(error) = result {
                    if cache.snapshot.priority_error != error { cache.snapshot.priority_error = error; cache.snapshot.revision += 1; }
                }
                next = Instant::now() + Duration::from_secs(30);
            }
        });
    }
    pub fn diagnoses_snapshot(&self) -> Snapshot {
        self.diagnosis_catalog.lock().unwrap().snapshot.clone()
    }
    pub fn refresh_diagnoses(&self) {
        self.diagnosis_catalog.lock().unwrap().refresh = true;
    }
    pub fn refresh_diagnosis_priorities(&self) {
        self.diagnosis_catalog.lock().unwrap().priority_refresh = true;
    }
    pub(super) fn start_diagnoses(self: &Arc<Self>) {
        let this = self.clone();
        self.workers.network(move||{
            let mut next=Instant::now();
            loop {
                if this.stopping.load(std::sync::atomic::Ordering::SeqCst){break;}
                if this.paused.load(std::sync::atomic::Ordering::SeqCst){thread::sleep(Duration::from_millis(100));continue;}
                let raw=this.transport.lock().unwrap().clone();
                let scope=Scope::from_connection(&raw);
                if raw.session.is_empty() || scope.is_none(){thread::sleep(Duration::from_millis(100));continue;}
                let scope=scope.unwrap();
                let current={
                    let mut cache=this.diagnosis_catalog.lock().unwrap();
                    if cache.scope.as_ref()!=Some(&scope) || (!cache.refresh&&Instant::now()<next) {
                        drop(cache);thread::sleep(Duration::from_millis(100));continue;
                    }
                    cache.refresh=false;cache.status(true,String::new());cache.snapshot.catalog.clone()
                };
                let cancelled=||this.stopping.load(std::sync::atomic::Ordering::SeqCst)
                    || this.paused.load(std::sync::atomic::Ordering::SeqCst)
                    || !same_auth(&this.transport.lock().unwrap(),&raw);
                let loaded=diagnosis_cache::download(current.as_deref(),|path|{
                    let (status,bytes)=raw.diagnosis_catalog_exchange(path)?;
                    match status {
                        200=>Ok(bytes),
                        401|403=>{this.reject_session(&raw);Err("Для обновления справочника диагнозов войдите снова".into())},
                        404=>Err("Каталог диагнозов пока не подготовлен на сервере".into()),
                        _=>Err("Обновление справочника диагнозов временно недоступно; сохранённый каталог остаётся доступен".into()),
                    }
                },cancelled);
                // Keep auth pinned through the sidecar commit; an old session cannot
                // publish after a newer install_auth or overwrite its profile's cache.
                let auth=this.transport.lock().unwrap();
                if !same_auth(&auth,&raw) || this.stopping.load(std::sync::atomic::Ordering::SeqCst) {continue;}
                let mut cache=this.diagnosis_catalog.lock().unwrap();
                if cache.scope.as_ref()!=Some(&scope){continue;}
                let result=loaded.and_then(|catalog|match catalog {Some(catalog)=>cache.publish(&this.store,&scope,catalog),None=>{cache.status(false,String::new());Ok(())}});
                next=Instant::now()+if result.is_ok(){Duration::from_secs(6*60*60)}else{Duration::from_secs(60)};
                if let Err(error)=result{cache.status(false,error);}
            }
        });
    }
}
fn same_auth(a: &Connection, b: &Connection) -> bool {
    a.base == b.base
        && a.key == b.key
        && a.session == b.session
        && a.user_id == b.user_id
        && Scope::from_connection(a) == Scope::from_connection(b)
}
