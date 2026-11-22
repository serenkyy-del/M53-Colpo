//! Server-only shared templates. Call on a dedicated UI background task, never the clinical queue.
use super::*;
use m53_domain::visit_template::VisitTemplate;

#[derive(Debug, Clone, Deserialize)]
pub struct VisitTemplateSummary {
    pub id: Uuid,
    pub title: String,
    pub row_version: i64,
    pub updated_at: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct VisitTemplateDetail {
    #[serde(flatten)]
    pub metadata: VisitTemplateSummary,
    pub protocol: VisitTemplate,
}
#[derive(Debug, Clone, Deserialize)]
pub struct VisitTemplatePage {
    pub templates: Vec<VisitTemplateSummary>,
    pub next_after: Option<Uuid>,
}
fn decode<T: serde::de::DeserializeOwned>(status: u16, bytes: &[u8]) -> Result<T, String> {
    let error = match status {
        200 | 201 => return serde_json::from_slice(bytes).map_err(|_| "Сервер вернул некорректный ответ шаблонов приёма".into()),
        401 | 403 => "Для работы с общими шаблонами войдите снова",
        404 => "Шаблон приёма не найден на сервере",
        409 if serde_json::from_slice::<serde_json::Value>(bytes).ok().and_then(|v| v.get("code").and_then(|c| c.as_str()).map(str::to_owned)).as_deref() == Some("protocol_template_title_conflict") => "Шаблон с таким названием уже есть. Обновите список, выберите его и включите перезапись",
        409 => "Шаблон изменён на другом рабочем месте. Загрузите актуальный шаблон перед повтором",
        400 => "Сервер отклонил шаблон приёма",
        _ => "Общие шаблоны приёма временно недоступны",
    };
    Err(error.into())
}
fn valid_metadata(value: &VisitTemplateSummary) -> bool {
    !value.id.is_nil() && value.row_version > 0 && !value.title.trim().is_empty()
        && value.title.chars().count() <= 200 && !value.title.contains('\0')
        && !value.updated_at.is_empty()
}
impl Offline {
    /// Online-only bounded page; invoke on a background task independent of clinical sync.
    pub fn list_visit_templates(&self, after: Option<Uuid>) -> Result<VisitTemplatePage, String> {
        let path = after.map_or_else(|| "/api/v1/protocol-templates".into(), |id| format!("/api/v1/protocol-templates?after={id}"));
        let page: VisitTemplatePage = self.visit_template_exchange("GET", &path, Uuid::nil(), None)?;
        if page.templates.len()>500 || page.templates.iter().any(|v| !valid_metadata(v))
            || page.templates.windows(2).any(|w| w[0].id>=w[1].id)
            || page.templates.first().is_some_and(|v| after.is_some_and(|a| v.id<=a))
            || page.next_after.is_some_and(|next| page.templates.last().is_none_or(|v| v.id!=next)) {
            return Err("Сервер вернул некорректную страницу шаблонов приёма".into());
        }
        Ok(page)
    }
    pub fn get_visit_template(&self, id: Uuid) -> Result<VisitTemplateDetail, String> {
        self.visit_template_detail("GET", id, None)
    }
    pub fn create_visit_template(&self, id: Uuid, title: String, protocol: VisitTemplate) -> Result<VisitTemplateDetail, String> {
        validate_input(&title, &protocol)?;
        self.visit_template_detail("POST", id, Some(json!({"id":id,"title":title,"protocol":protocol})))
    }
    pub fn update_visit_template(&self, id: Uuid, expected_version: i64, title: String, protocol: VisitTemplate) -> Result<VisitTemplateDetail, String> {
        validate_input(&title, &protocol)?;
        if expected_version<1 { return Err("Некорректная версия шаблона приёма".into()); }
        self.visit_template_detail("PUT", id, Some(json!({"title":title,"protocol":protocol,"expected_version":expected_version})))
    }
    fn visit_template_detail(&self, method: &str, id: Uuid, body: Option<Value>) -> Result<VisitTemplateDetail, String> {
        if id.is_nil() { return Err("Некорректный идентификатор шаблона приёма".into()); }
        let path=if method=="POST" { "/api/v1/protocol-templates".into() } else {format!("/api/v1/protocol-templates/{id}")};
        let saved: VisitTemplateDetail=self.visit_template_exchange(method,&path,id,body.as_ref())?;
        if saved.metadata.id!=id || !valid_metadata(&saved.metadata) || saved.protocol.validate().is_err()
            || body.as_ref().is_some_and(|v| v["title"]!=saved.metadata.title || v["protocol"]!=json!(saved.protocol)
                || (method=="POST" && saved.metadata.row_version!=1)
                || (method=="PUT" && v["expected_version"].as_i64().and_then(|n| n.checked_add(1))!=Some(saved.metadata.row_version))) {
            return Err("Сервер вернул неполное подтверждение шаблона приёма".into());
        }
        Ok(saved)
    }
    fn visit_template_exchange<T: serde::de::DeserializeOwned>(&self, method:&str,path:&str,id:Uuid,body:Option<&Value>) -> Result<T,String> {
        let _writer=self.workers.writer()?;
        let available=|| !self.paused.load(std::sync::atomic::Ordering::SeqCst) && !self.stopping.load(std::sync::atomic::Ordering::SeqCst);
        let raw=self.transport.lock().unwrap().clone();
        let scope=crate::diagnosis_cache::Scope::from_connection(&raw);
        if !available() || raw.session.is_empty() || scope.is_none() { return Err("Для общих шаблонов нужно подключение к серверу и действующий вход".into()); }
        let operation=Operation {id:Uuid::new_v4(),entity_id:id,version_id:None,path:path.into(),method:method.into(),body:body.cloned().unwrap_or(Value::Null),blob:None,started:true,blocked:None,sequence:0};
        // No outbox/cache lock or clinical worker is used for this independent request.
        let result=raw.exchange(method,path,body,None,body.map(|_| &operation));
        let auth=self.transport.lock().unwrap();
        if !available() || auth.base!=raw.base || auth.key!=raw.key || auth.session!=raw.session
            || auth.user_id!=raw.user_id || crate::diagnosis_cache::Scope::from_connection(&auth)!=scope {
            return Err("Вход изменился; загрузите шаблоны приёма снова".into());
        }
        drop(auth);
        let (status,bytes)=result.map_err(|_| "Сервер не подтвердил запрос шаблонов приёма. Загрузите актуальный список перед повтором".to_string())?;
        if status==401 || status==403 { self.reject_session(&raw); }
        decode(status,&bytes)
    }
}
fn validate_input(title:&str, protocol:&VisitTemplate)->Result<(),String> {
    if title.trim().is_empty() || title.chars().count()>200 || title.contains('\0') || protocol.validate().is_err()
        || serde_json::to_vec(protocol).map_or(true,|v|v.len()>4*1024*1024) {
        return Err("Некорректный шаблон приёма".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::cache::AesProtector;
    use std::{io::{Read,Write},net::TcpListener};
    fn fixture_recording(replies: Vec<(u16, Value)>, records:Option<mpsc::Sender<Vec<u8>>>) -> (Offline, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut raw = Connection::new(
            &format!("http://{}", listener.local_addr().unwrap()),
            "synthetic-key",
        )
        .unwrap();
        let user=Uuid::new_v4();
        raw.user_id=Some(user);
        raw.session="synthetic-session".into();
        raw.identity=Some(crate::ui_guidance::Identity {organization_id:Uuid::new_v4(),user_id:user,doctor_id:Uuid::new_v4(),workstation_id:Uuid::new_v4()});
        let store = Store::new(
            std::env::temp_dir().join(format!("m53-scoped-receipt-{}", Uuid::new_v4())),
            Arc::new(AesProtector([19; 32])),
        )
        .unwrap();
        let mut state:LocalState=serde_json::from_value(json!({"format_version":1,"profile":{"server":raw.base,"user_id":Uuid::new_v4(),"doctor":"Synthetic"},"responses":{},"blobs":{},"pending":[],"confirmed":{},"journal":{},"editors":{},"remote_conflicts":{},"conflict_history":[],"sequence":1})).unwrap();
        state.online = true;
        state.durable_sequence = 1;
        let (disk, _) = mpsc::channel();
        let offline = Offline {
            diagnosis_catalog: Mutex::new(crate::diagnosis_cache::State::empty()),
            state: Arc::new(Mutex::new(state)),
            store,
            memory: Mutex::new(MemoryCache::default()),
            thumbnails: Mutex::new(MemoryCache::default()),
            fetch: Mutex::new(FetchQueue::default()),
            asset_fetch: Mutex::new(FetchQueue::default()),
            foreground: Mutex::new(None),
            registry_paths: Mutex::new(vec![]),
            registry_epoch: std::sync::atomic::AtomicU64::new(0),
            performance: PerformanceCounters::default(),
            disk,
            transport: Mutex::new(raw),
            wake: false.into(),
            paused: false.into(),
            stopping: false.into(),
            workers: Workers::default(),
            sync: Mutex::new(SyncSchedule::default()),
        };
        let handle = thread::spawn(move || {
            for (status, body) in replies {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(4)))
                    .unwrap();
                let mut request = Vec::new();
                let mut chunk = [0u8; 4096];
                let end = loop {
                    let n = socket.read(&mut chunk).unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&chunk[..n]);
                    if let Some(i) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                let length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .and_then(|s| s.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                while request.len() < end + length {
                    let n = socket.read(&mut chunk).unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&chunk[..n]);
                }
                if let Some(records)=&records {records.send(request.clone()).unwrap();}
                let body = body.to_string();
                write!(socket,"HTTP/1.1 {status} Reply\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        (offline, handle)
    }

    use super::*;
    #[test]
    fn visit_templates_decodes_page_and_detail() {
        let id=Uuid::new_v4();
        let metadata=json!({"id":id,"title":"Общий","row_version":2,"updated_at":"2026-10-28"});
        let page:VisitTemplatePage=decode(200,&serde_json::to_vec(&json!({"templates":[metadata.clone()],"next_after":id})).unwrap()).unwrap();
        assert_eq!(page.templates[0].id,id);
        assert_eq!(page.next_after,Some(id));
        let mut detail=metadata;
        detail["protocol"]=json!({"format_version":1,"colpo_fields":{},"history":null,"conclusion":null,"diagnosis_note":null,"diagnoses":[],"adequacy":[],"transformation_zone":null});
        let detail:VisitTemplateDetail=decode(200,&serde_json::to_vec(&detail).unwrap()).unwrap();
        assert!(detail.protocol.validate().is_ok());
    }
    fn portable() -> VisitTemplate {
        serde_json::from_value(json!({"format_version":1,"colpo_fields":{},"history":null,"conclusion":null,"diagnosis_note":null,"diagnoses":[],"adequacy":[],"transformation_zone":null})).unwrap()
    }
    fn detail(id:Uuid,version:i64)->Value {
        json!({"id":id,"title":"Synthetic","row_version":version,"updated_at":"2026-10-28","protocol":portable()})
    }
    #[test]
    fn visit_templates_loopback_get_post_put_confirmation_and_conflict() {
        let id=Uuid::new_v4();
        let (tx,rx)=mpsc::channel();
        let (offline,server)=fixture_recording(vec![
            (200,json!({"templates":[{"id":id,"title":"Synthetic","row_version":1,"updated_at":"2026-10-28"}],"next_after":null})),
            (200,detail(id,1)),(200,detail(id,1)),(200,detail(id,2)),
            (409,json!({"code":"protocol_template_version_conflict","private":"PRIVATE_CLINICAL_CONTENT"})),
        ],Some(tx));
        assert_eq!(offline.list_visit_templates(None).unwrap().templates[0].id,id);
        assert_eq!(offline.get_visit_template(id).unwrap().metadata.id,id);
        assert_eq!(offline.create_visit_template(id,"Synthetic".into(),portable()).unwrap().metadata.row_version,1);
        assert_eq!(offline.update_visit_template(id,1,"Synthetic".into(),portable()).unwrap().metadata.row_version,2);
        let error=offline.update_visit_template(id,1,"Synthetic".into(),portable()).unwrap_err();
        assert!(error.contains("другом"));
        assert!(!error.contains("PRIVATE_CLINICAL_CONTENT"));
        server.join().unwrap();
        let records:Vec<_>=rx.try_iter().collect();
        assert_eq!(records.len(),5);
        for (index,request) in records.iter().enumerate() {
            let text=String::from_utf8_lossy(request);
            let headers=text.split("\r\n\r\n").next().unwrap().to_ascii_lowercase();
            assert!(headers.contains("authorization: bearer synthetic-session"));
            let expected=match index {0|1=>"GET",2=>"POST",_=>"PUT"};
            assert!(text.starts_with(&format!("{expected} /api/v1/protocol-templates")));
            if index>=2 {
                let op=headers.lines().find_map(|v|v.strip_prefix("x-operation-id: ")).unwrap();
                assert!(!Uuid::parse_str(op).unwrap().is_nil());
                let body:Value=serde_json::from_str(text.split_once("\r\n\r\n").unwrap().1).unwrap();
                assert_eq!(body["protocol"],json!(portable()));
                if index>=3 {assert_eq!(body["expected_version"],1);}
            }
        }
        assert!(offline.state.lock().unwrap().pending.is_empty());
    }
    #[test]
    fn visit_templates_errors_never_expose_response_content() {
        for status in [400,401,403,404,409,500] {
            let error=decode::<VisitTemplatePage>(status,b"PRIVATE_CLINICAL_CONTENT").unwrap_err();
            assert!(!error.contains("PRIVATE_CLINICAL_CONTENT"));
            if status==409 { assert!(error.contains("другом")); }
        }
        assert!(decode::<VisitTemplatePage>(200,b"PRIVATE_CLINICAL_CONTENT").is_err());
    }
    #[test]
    fn duplicate_title_conflict_explains_selection_without_exposing_response() {
        let body = br#"{"code":"protocol_template_title_conflict","private":"PRIVATE_CLINICAL_CONTENT"}"#;
        let error = decode::<VisitTemplatePage>(409, body).unwrap_err();
        assert!(error.contains("названием"));
        assert!(!error.contains("PRIVATE_CLINICAL_CONTENT"));
    }
    #[test]
    fn visit_templates_loopback_rejects_wrong_ack_and_sanitizes_http_errors() {
        let id=Uuid::new_v4();
        let (offline,server)=fixture_recording(vec![
            (200,detail(Uuid::new_v4(),1)),
            (200,detail(id,9)),
            (500,json!({"code":"PRIVATE_CLINICAL_CONTENT","message":"PRIVATE_CLINICAL_CONTENT"})),
        ],None);
        let wrong_id=offline.create_visit_template(id,"Synthetic".into(),portable()).unwrap_err();
        assert!(wrong_id.contains("неполное подтверждение"));
        let wrong_version=offline.update_visit_template(id,1,"Synthetic".into(),portable()).unwrap_err();
        assert!(wrong_version.contains("неполное подтверждение"));
        let error=offline.get_visit_template(id).unwrap_err();
        assert!(!error.contains("PRIVATE_CLINICAL_CONTENT"));
        server.join().unwrap();
        // Listener is closed: network failure must also stay generic.
        let error=offline.get_visit_template(id).unwrap_err();
        assert!(!error.contains("PRIVATE_CLINICAL_CONTENT"));
        assert!(!error.contains("127.0.0.1"));
        assert!(offline.state.lock().unwrap().pending.is_empty());
        offline.paused.store(true,std::sync::atomic::Ordering::SeqCst);
        assert!(offline.create_visit_template(id,"Synthetic".into(),portable()).is_err());
        assert!(offline.state.lock().unwrap().pending.is_empty());
    }
}
