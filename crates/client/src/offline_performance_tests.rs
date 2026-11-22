//! Synthetic data only; these checks never open a doctor's real cache or API.
use super::*;
use crate::cache::AesProtector;
use std::{
    io::{Read, Write},
    net::TcpListener,
};

fn fixture(base: &str) -> (Arc<Offline>, mpsc::Receiver<DiskJob>) {
    let raw = Connection::new(base, "synthetic-key").unwrap();
    let mut state: LocalState = serde_json::from_value(json!({
        "format_version":1,"profile":{"server":base,"user_id":Uuid::new_v4(),"doctor":"Synthetic"},
        "responses":{},"blobs":{},"pending":[],"confirmed":{},"journal":{},
        "editors":{},"remote_conflicts":{},"conflict_history":[],"sequence":1
    }))
    .unwrap();
    state.online = true;
    state.durable_sequence = 1;
    let store = Store::new(
        std::env::temp_dir().join(format!("m53-perf-{}", Uuid::new_v4())),
        Arc::new(AesProtector([23; 32])),
    )
    .unwrap();
    let (disk, receiver) = mpsc::channel();
    (
        Arc::new(Offline {
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
        }),
        receiver,
    )
}

fn queued_patient() -> Operation {
    Operation {
        id: Uuid::new_v4(), entity_id: Uuid::new_v4(), version_id: None,
        path: "/api/v1/patients".into(), method: "POST".into(),
        body: json!({"family_name":"Synthetic","given_name":"Patient","patronymic":null,"birth_date":"2000-01-01"}),
        blob: None, started: true, blocked: None, sequence: 1,
    }
}
fn reply_json(socket: &mut std::net::TcpStream, status: u16, body: Value) {
    let body = body.to_string();
    write!(socket,"HTTP/1.1 {status} Reply\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
}
fn matching_receipt(op: &Operation) -> Value {
    json!({"operation_id":op.id,"scope":op.scope(),"request_sha256":op.request_hash(),"response":{"id":op.entity_id}})
}
fn wait_for_sync_error(offline: &Offline) -> bool {
    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until {
        if !offline.information().3.is_empty() { return true; }
        thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn reachable_metadata_wakes_transport_retry_without_waiting_for_outage_delay() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let (offline, receiver) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let operation = queued_patient();
    offline.state.lock().unwrap().pending.push_back(operation.clone());
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server_stop = stop.clone();
    let (online_tx, online_rx) = mpsc::channel();
    let (receipt_tx, receipt_rx) = mpsc::channel();
    let server_op = operation.clone();
    let server = thread::spawn(move || {
        let until = Instant::now() + Duration::from_secs(6);
        let mut first = true;
        while Instant::now() < until && !server_stop.load(std::sync::atomic::Ordering::SeqCst) {
            let Ok((mut socket, _)) = listener.accept() else {thread::sleep(Duration::from_millis(2));continue;};
            let path = request_path(&mut socket);
            if path == "/api/v1/me" {
                reply_json(&mut socket, 200, json!({}));
                online_tx.send(Instant::now()).unwrap();
            } else {
                assert_eq!(path, format!("/api/v1/operations/{}", server_op.id));
                if first {
                    // Headers arrive, but the body is lost after the server may have committed.
                    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 999\r\nConnection: close\r\n\r\n{").unwrap();
                    first = false;
                } else {
                    reply_json(&mut socket, 200, matching_receipt(&server_op));
                    receipt_tx.send(Instant::now()).unwrap();
                }
            }
        }
    });
    offline.start_disk(receiver);
    offline.start_network(offline.transport.lock().unwrap().clone());
    let failed = wait_for_sync_error(&offline);
    offline.fetch("/api/v1/me");
    offline.start_metadata_network();
    let online = online_rx.recv_timeout(Duration::from_secs(2));
    let received = receipt_rx.recv_timeout(Duration::from_millis(900));
    let until = Instant::now() + Duration::from_millis(500);
    while Instant::now() < until && !offline.state.lock().unwrap().confirmed.contains_key(&operation.id) {
        thread::sleep(Duration::from_millis(5));
    }
    offline.stopping.store(true, std::sync::atomic::Ordering::SeqCst);
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    server.join().unwrap();
    assert!(failed && online.is_ok(), "the fixture observed the outage and a real successful metadata GET");
    assert!(received.is_ok(), "API recovery must retry the exact frozen operation without the stale two-second outage delay");
    let state = offline.state.lock().unwrap();
    assert_eq!(state.confirmed.get(&operation.id), Some(&operation.request_hash()));
    assert!(state.pending.is_empty());
}

#[test]
fn independent_lane_sends_while_another_request_has_http_retry_delay() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let (offline, receiver) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let first = queued_patient();
    offline.state.lock().unwrap().pending.push_back(first.clone());
    let server_offline = offline.clone();
    let first_id = first.id;
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server_stop = stop.clone();
    let (receipt_tx, receipt_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let until = Instant::now() + Duration::from_secs(6);
        let mut failed_calls = 0;
        while Instant::now() < until && !server_stop.load(std::sync::atomic::Ordering::SeqCst) {
            let Ok((mut socket, _)) = listener.accept() else {thread::sleep(Duration::from_millis(2));continue;};
            let path = request_path(&mut socket);
            let id: Uuid = path.strip_prefix("/api/v1/operations/").unwrap().parse().unwrap();
            if id == first_id {
                failed_calls += 1;
                reply_json(&mut socket, 429, json!({"code":"rate_limit"}));
            } else {
                let operation = server_offline.state.lock().unwrap().pending.iter().find(|p|p.id==id).unwrap().clone();
                reply_json(&mut socket, 200, matching_receipt(&operation));
                receipt_tx.send((id, failed_calls)).unwrap();
            }
        }
    });
    offline.start_disk(receiver);
    offline.start_network(offline.transport.lock().unwrap().clone());
    let failed = wait_for_sync_error(&offline);
    let second = offline.mutate("/api/v1/patients", "POST", queued_patient().body).unwrap();
    let received = receipt_rx.recv_timeout(Duration::from_millis(900));
    let until = Instant::now() + Duration::from_millis(500);
    while Instant::now() < until && offline.state.lock().unwrap().pending.len() > 1 {thread::sleep(Duration::from_millis(5));}
    offline.stopping.store(true, std::sync::atomic::Ordering::SeqCst);
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    server.join().unwrap();
    assert!(failed);
    assert!(received.is_ok(), "a failed lane must not impose its global retry delay on a new independent patient");
    let (_, failed_calls) = received.unwrap();
    assert_eq!(failed_calls, 1, "queuing new work must preserve HTTP429 backoff");
    let state = offline.state.lock().unwrap();
    assert_eq!(state.pending.len(), 1);
    assert_eq!(json!(state.pending[0]), json!(first), "frozen failure ID/hash/body are preserved");
    assert!(state.responses["/api/v1/patients"].as_array().unwrap().iter().any(|p|p["id"]==second["id"]));
}
fn request_path(socket: &mut std::net::TcpStream) -> String {
    socket.set_nonblocking(false).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut request = Vec::new();
    let mut byte = [0];
    while !request.ends_with(b"\r\n\r\n") {
        socket.read_exact(&mut byte).unwrap();
        request.push(byte[0]);
        assert!(request.len() < 16384);
    }
    String::from_utf8(request)
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .into()
}

#[test]
fn directory_requests_are_deduplicated_and_obsolete_dates_do_not_retry() {
    let (offline, _disk) = fixture("http://127.0.0.1:9");
    let patients = "/api/v1/patients/page?limit=50".to_string();
    let today = "/api/v1/tasks/page?date=2026-10-21".to_string();
    for _ in 0..1000 {
        offline.set_registry_paths(vec![patients.clone(), today.clone()]);
    }
    assert_eq!(offline.fetch.lock().unwrap().pending.len(), 2);
    let yesterday = "/api/v1/tasks/page?date=2026-10-20".to_string();
    offline.set_registry_paths(vec![patients.clone(), yesterday.clone()]);
    offline.fetch(&today); // late failed response cannot restore an obsolete page
    let queue = offline.fetch.lock().unwrap();
    assert_eq!(queue.pending.len(), 2);
    assert!(!queue.pending.contains(&today));
    assert!(queue.pending.contains(&yesterday));
    assert!(
        !queue
            .pending
            .iter()
            .any(|p| p == "/api/v1/patients" || p == "/api/v1/studies" || p == "/api/v1/tasks")
    );
}

#[test]
fn bounded_queue_prioritizes_and_deduplicates_inflight_requests() {
    let mut queue = FetchQueue::default();
    for index in 0..500 {
        queue.enqueue(&format!("/{index}"), false, false);
    }
    assert_eq!(queue.pending.len(), 200);
    queue.enqueue("/180", true, false);
    let path = queue.take().unwrap();
    assert_eq!(path, "/180");
    for _ in 0..1000 {
        queue.enqueue(&path, true, false);
    }
    assert_eq!(queue.pending.len(), 199);
    queue.complete(&path);
    queue.enqueue(&path, true, false);
    assert_eq!(queue.take().as_deref(), Some("/180"));
}

#[test]
fn invalidation_during_inflight_keeps_one_followup_request() {
    let mut queue = FetchQueue::default();
    queue.enqueue("/selected", true, false);
    assert_eq!(queue.take().as_deref(), Some("/selected"));
    for _ in 0..1000 {
        queue.enqueue("/selected", false, true);
    }
    assert!(queue.pending.is_empty());
    queue.complete("/selected");
    assert_eq!(queue.pending.len(), 1);
    assert_eq!(queue.take().as_deref(), Some("/selected"));
    queue.complete("/selected");
    assert!(queue.pending.is_empty());
}

#[test]
fn cached_ui_reads_do_not_schedule_network_or_dirty_the_index() {
    let (offline, _receiver) = fixture("http://127.0.0.1:9");
    let path = "/api/v1/tasks";
    offline
        .state
        .lock()
        .unwrap()
        .responses
        .insert(path.into(), json!([]));
    let before = offline.state_token();
    let started = Instant::now();
    for _ in 0..10000 {
        assert_eq!(offline.get(path).unwrap(), json!([]));
        assert!(offline.remote(Uuid::nil()).is_none());
    }
    assert!(offline.fetch.lock().unwrap().pending.is_empty());
    assert!(
        offline
            .cached_original_shared(Uuid::nil())
            .unwrap()
            .is_none()
    );
    assert!(offline.asset_fetch.lock().unwrap().pending.is_empty());
    assert_eq!(offline.state_token(), before);
    eprintln!(
        "cached-read benchmark: 10000 reads in {}us; queued GETs=0",
        started.elapsed().as_micros()
    );
}

#[test]
fn heartbeat_refreshes_current_study_without_scanning_the_archive() {
    let (offline, _receiver) = fixture("http://127.0.0.1:9");
    for index in 1..=500 {
        offline.state.lock().unwrap().responses.insert(
            format!("/api/v1/studies/{}", Uuid::from_u128(index)),
            json!({"assets":[]}),
        );
    }
    let selected = Uuid::from_u128(410);
    let patient=Uuid::from_u128(900);
    offline.set_registry_paths(vec![
        "/api/v1/patients/page?limit=50".into(),
        "/api/v1/tasks/page?date=2026-10-21".into(),
        format!("/api/v1/studies?patient_id={patient}"),
        format!("/api/v1/studies/summaries?patient_id={patient}"),
    ]);
    offline.fetch_study_foreground(selected);
    offline.invalidate_lists();
    let queue = offline.fetch.lock().unwrap();
    assert!(queue.pending.contains(&format!("/api/v1/studies/{selected}/context")));
    assert_eq!(queue.pending.len(), 7);
    assert_eq!(
        queue.pending.front().unwrap(),
        &format!("/api/v1/studies/{selected}")
    );
    assert!(offline.asset_fetch.lock().unwrap().pending.is_empty());
    assert!(!queue.pending.iter().any(|p|matches!(p.as_str(),"/api/v1/patients"|"/api/v1/studies"|"/api/v1/studies/summaries"|"/api/v1/tasks")));
}

#[test]
fn manual_patient_lookup_checks_later_server_pages_without_creating_a_record() {
    let listener=TcpListener::bind("127.0.0.1:0").unwrap();
    let (offline,_)=fixture(&format!("http://{}",listener.local_addr().unwrap()));
    let user=Uuid::new_v4();
    { let mut raw=offline.transport.lock().unwrap();raw.session="synthetic-session".into();raw.user_id=Some(user);
      raw.identity=Some(crate::ui_guidance::Identity{user_id:user,organization_id:Uuid::new_v4(),doctor_id:Uuid::new_v4(),workstation_id:Uuid::new_v4()}); }
    let id=Uuid::new_v4();
    let server=thread::spawn(move||{
        let (mut socket,_)=listener.accept().unwrap();
        assert_eq!(request_path(&mut socket),"/api/v1/patients/page?q=2000-01-01&limit=100");
        reply_json(&mut socket,200,json!({"items":[],"next_cursor":"page+2"}));drop(socket);
        let (mut socket,_)=listener.accept().unwrap();
        assert_eq!(request_path(&mut socket),"/api/v1/patients/page?q=2000-01-01&limit=100&cursor=page%2B2");
        reply_json(&mut socket,200,json!({"items":[{"id":id,"family_name":"Synthetic","given_name":"Patient","patronymic":null,"birth_date":"2000-01-01","external_id":Uuid::new_v4()}],"next_cursor":null}));
    });
    let input=NewPatient{family_name:"synthetic".into(),given_name:"PATIENT".into(),patronymic:None,birth_date:Some("2000-01-01".into())};
    let matches=offline.matching_patients(&input).unwrap();
    server.join().unwrap();
    assert_eq!(matches.len(),1);assert_eq!(matches[0].id,id);
    offline.remember_matched_patient(&matches[0]).unwrap();
    assert!(offline.cached("/api/v1/patients").unwrap().as_array().unwrap().iter().any(|p|p["id"]==json!(id)));
    assert!(offline.state.lock().unwrap().pending.is_empty());
}

#[test]
fn server_approved_open_keeps_unsent_editor_and_requires_server_after_restart() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (offline, _) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let user = Uuid::new_v4();
    let mut connection = {
        let mut raw = offline.transport.lock().unwrap();
        raw.session = "synthetic-session".into();
        raw.user_id = Some(user);
        raw.identity = Some(crate::ui_guidance::Identity {
            user_id: user, organization_id: Uuid::new_v4(),
            doctor_id: Uuid::new_v4(), workstation_id: Uuid::new_v4(),
        });
        raw.clone()
    };
    let study = Uuid::new_v4();
    let patient = Uuid::new_v4();
    let version = Uuid::new_v4();
    let path = format!("/api/v1/studies/{study}");
    let protocol = crate::model::blank_protocol(version);
    let approved = json!({
        "study":{"id":study,"patient_id":patient,"status":"approved","row_version":3,"study_instance_uid":"1.2.3"},
        "patient":{"id":patient,"family_name":"Synthetic","given_name":"Example","patronymic":null,"birth_date":null},
        "revision_id":Uuid::new_v4(),"protocol":protocol,"annotations":[],"assets":[]
    });
    let mut draft = approved.clone();
    draft["study"]["status"] = json!("draft");
    draft["revision_id"] = json!(Uuid::new_v4());
    let local = json!({"dirty":true,"detail":draft});
    {
        let mut state = offline.state.lock().unwrap();
        state.journal = json!({"detail":null,"dictionary":null,"dirty":false});
        state.responses.insert(path.clone(), draft);
        state.editors.insert(study, local.clone());
        state.responses.insert(format!("/api/v1/terminology?version_id={version}"),
            json!({"version_id":version,"clinically_approved":true,"terms":[]}));
    }
    let expected = approved.clone();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        assert_eq!(request_path(&mut socket), path);
        reply_json(&mut socket, 200, approved.clone());
        drop(socket);
        let (mut socket, _) = listener.accept().unwrap();
        assert_eq!(request_path(&mut socket), format!("{path}/report-context?revision_id={}",
            approved["revision_id"].as_str().unwrap()));
        reply_json(&mut socket, 200, json!({"study_id":study,"revision_id":approved["revision_id"],
            "status":"approved","readonly":true,"performer":{"display_name":"Original doctor"}}));
    });
    connection.offline = Some(offline.clone());
    let mut workspace = crate::workspace::Workspace::default();
    workspace.attach(connection);
    workspace.open(study, None).unwrap();
    let (_, performer) = crate::report_identity::load(
        workspace.connection().unwrap(), workspace.detail.as_ref().unwrap()).unwrap();
    assert_eq!(performer, "Original doctor");
    server.join().unwrap();
    assert_eq!(json!(workspace.detail.as_ref().unwrap().revision_id), expected["revision_id"]);
    assert!(!workspace.dirty);
    assert_eq!(offline.editor(study), Some(local.clone()));
    let state = offline.state.lock().unwrap().clone();
    offline.write_index(state).unwrap();
    let saved: LocalState = serde_json::from_slice(
        &offline.store.read("workspace").unwrap().unwrap()).unwrap();
    assert!(saved.journal.is_null());
    assert!(!saved.responses.contains_key(&format!("/api/v1/studies/{study}")));
    assert_eq!(saved.editors.get(&study), Some(&local));
    *offline.state.lock().unwrap() = saved;
    offline.state.lock().unwrap().journal = local.clone();
    assert!(offline.journal_for_restore().is_null());
    assert_eq!(offline.journal(), local);
    offline.transport.lock().unwrap().session.clear();
    assert!(offline.study_for_open(study).unwrap_err().starts_with("Утверждённый протокол"));
}

#[test]
fn confirmed_server_draft_retires_only_stale_reopen_conflict() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (offline, _) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let user = Uuid::new_v4();
    {
        let mut raw = offline.transport.lock().unwrap();
        raw.session = "synthetic".into();
        raw.user_id = Some(user);
        raw.identity = Some(crate::ui_guidance::Identity {
            user_id:user, organization_id:Uuid::new_v4(), doctor_id:Uuid::new_v4(), workstation_id:Uuid::new_v4(),
        });
    }
    let study=Uuid::new_v4();
    let patient=Uuid::new_v4();
    let path=format!("/api/v1/studies/{study}");
    let draft=json!({"study":{"id":study,"patient_id":patient,"status":"draft","row_version":26,
        "study_instance_uid":"1.2.3"},"patient":{"id":patient,"family_name":"Synthetic","given_name":"Example",
        "patronymic":null,"birth_date":null},"revision_id":Uuid::new_v4(),
        "protocol":crate::model::blank_protocol(Uuid::new_v4()),"annotations":[],"assets":[]});
    let mut stale=queued_patient();
    stale.path=format!("{path}/reopen");
    stale.body=json!({"expected_version":25});
    stale.blocked=Some(r#"Backend 409: {"code":"version_conflict"}"#.into());
    let mut other=queued_patient();
    other.path=format!("/api/v1/studies/{}/protocol",Uuid::new_v4());
    other.blocked=stale.blocked.clone();
    offline.state.lock().unwrap().pending.extend([stale.clone(),other.clone()]);
    let mut previous = draft.clone();
    previous["study"]["status"]=json!("approved");
    previous["study"]["row_version"]=json!(25);
    previous["revision_id"]=json!(Uuid::new_v4());
    let server=thread::spawn(move||{
        let (mut socket,_)=listener.accept().unwrap();
        assert_eq!(request_path(&mut socket),path);
        reply_json(&mut socket,200,draft);
    });
    let mut connection=offline.transport.lock().unwrap().clone();
    connection.offline=Some(offline.clone());
    let mut workspace=crate::workspace::Workspace::default();
    workspace.attach(connection);
    workspace.detail=Some(serde_json::from_value(previous).unwrap());
    workspace.reopen().unwrap();
    server.join().unwrap();
    let state=offline.state.lock().unwrap();
    assert_eq!(state.pending.len(),1);
    assert_eq!(state.pending[0].id,other.id);
    assert_eq!(state.conflict_history.last().map(|op|op.id),Some(stale.id));
    assert!(state.confirmed.is_empty());
    drop(state);
    let new_study=Uuid::new_v4();
    let reopen=format!("/api/v1/studies/{new_study}/reopen");
    assert_eq!(offline.mutate(&reopen,"POST",json!({"expected_version":1})).unwrap(),json!({"queued":true}));
    let state=offline.state.lock().unwrap();
    assert!(state.pending.iter().any(|op|op.path==reopen));
    assert!(state.confirmed.is_empty());
}

#[test]
fn opening_diverged_local_draft_preserves_both_and_never_queues_stale_cas() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (offline, _) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let user = Uuid::new_v4();
    let mut connection = {
        let mut raw = offline.transport.lock().unwrap();
        raw.session = "synthetic-session".into();
        raw.user_id = Some(user);
        raw.identity = Some(crate::ui_guidance::Identity {
            user_id:user, organization_id:Uuid::new_v4(), doctor_id:Uuid::new_v4(), workstation_id:Uuid::new_v4(),
        });
        raw.clone()
    };
    let study = Uuid::new_v4();
    let patient = Uuid::new_v4();
    let version = Uuid::new_v4();
    let mut protocol = crate::model::blank_protocol(version);
    protocol.history.value = m53_domain::protocol::Assessment::Assessed("Unsent local history".into());
    let local: StudyDetail = serde_json::from_value(json!({
        "study":{"id":study,"patient_id":patient,"status":"draft","row_version":2,"study_instance_uid":"1.2.3"},
        "patient":{"id":patient,"family_name":"Synthetic","given_name":"Example","patronymic":null,"birth_date":null},
        "revision_id":Uuid::new_v4(),"protocol":protocol,"annotations":[],"assets":[]
    })).unwrap();
    let mut remote = local.clone();
    remote.study.row_version = 5;
    remote.revision_id = Some(Uuid::new_v4());
    remote.protocol.as_mut().unwrap().history.value =
        m53_domain::protocol::Assessment::Assessed("Confirmed remote history".into());
    let expected_remote = json!(remote);
    let dictionary = m53_domain::validation::Dictionary {
        version_id:version,clinically_approved:true,terms:vec![],
    };
    let journal = json!({"dirty":true,"detail":local,"dictionary":dictionary});
    {
        let mut state=offline.state.lock().unwrap();
        state.journal=journal.clone();
        state.editors.insert(study,journal);
        state.responses.insert(format!("/api/v1/studies/{study}"),json!(local));
        state.responses.insert(format!("/api/v1/terminology?version_id={version}"),json!(dictionary));
    }
    let server=thread::spawn(move||{
        let (mut socket,_)=listener.accept().unwrap();
        assert_eq!(request_path(&mut socket),format!("/api/v1/studies/{study}"));
        reply_json(&mut socket,200,json!(remote));
    });
    connection.offline=Some(offline.clone());
    let mut workspace=crate::workspace::Workspace::default();
    workspace.attach(connection);
    let deadline=Instant::now()+Duration::from_secs(3);
    loop {
        {let mut state=offline.state.lock().unwrap();state.durable_sequence=state.sequence;}
        match workspace.open_background(study,None,false) {
            Ok(())=>break,
            Err(error) if crate::workspace::opening_pending(&error)&&Instant::now()<deadline=>thread::sleep(Duration::from_millis(10)),
            Err(error)=>panic!("open failed: {error}"),
        }
    }
    server.join().unwrap();
    assert_eq!(json!(workspace.detail.as_ref().unwrap().protocol),json!(local.protocol));
    assert!(workspace.save().is_err(),"divergent input must not enqueue a stale CAS");
    assert!(offline.state.lock().unwrap().pending.is_empty());
    assert_eq!(offline.remote(study),Some(expected_remote));
    assert!(offline.conflict_for_study(study).is_some());
    // A later form checkpoint cannot clear a Rust-owned conflict marker.
    workspace.checkpoint(json!({"detail":workspace.detail,"dictionary":dictionary})).unwrap();
    assert_eq!(workspace.journal["revision_conflict"],true);
    workspace.tick();
    assert!(offline.state.lock().unwrap().pending.is_empty());
    // An explicit compare-and-merge uses the confirmed server CAS, preserving the local text.
    workspace.resolve().unwrap();
    let state=offline.state.lock().unwrap();
    assert!(!state.has_local_revision_conflict(study));
    assert_eq!(state.pending.len(),1);
    assert_eq!(state.pending[0].body["expected_version"],5);
    assert_eq!(state.pending[0].body["protocol"],json!(local.protocol));
}

#[test]
fn background_open_returns_without_waiting_for_the_detail_response() {
    let listener=TcpListener::bind("127.0.0.1:0").unwrap();
    let (offline,_)=fixture(&format!("http://{}",listener.local_addr().unwrap()));
    let user=Uuid::new_v4();
    let mut connection={
        let mut raw=offline.transport.lock().unwrap(); raw.session="synthetic".into();raw.user_id=Some(user);
        raw.identity=Some(crate::ui_guidance::Identity{user_id:user,organization_id:Uuid::new_v4(),doctor_id:Uuid::new_v4(),workstation_id:Uuid::new_v4()});
        raw.clone()
    };
    let study=Uuid::new_v4();let patient=Uuid::new_v4();let version=Uuid::new_v4();
    let detail=json!({"study":{"id":study,"patient_id":patient,"status":"draft","row_version":1,"study_instance_uid":"1.2.3"},
        "patient":{"id":patient,"family_name":"Synthetic","given_name":"Example","patronymic":null,"birth_date":null},
        "revision_id":Uuid::new_v4(),"protocol":crate::model::blank_protocol(version),"annotations":[],"assets":[]});
    {
        let mut state=offline.state.lock().unwrap();
        state.journal=json!({"detail":null,"dictionary":null,"dirty":false});
        state.responses.insert(format!("/api/v1/terminology?version_id={version}"),json!({"version_id":version,"clinically_approved":true,"terms":[]}));
    }
    let server=thread::spawn(move||{
        let(mut socket,_)=listener.accept().unwrap();
        assert_eq!(request_path(&mut socket),format!("/api/v1/studies/{study}"));
        thread::sleep(Duration::from_millis(350));
        reply_json(&mut socket,200,detail);
    });
    connection.offline=Some(offline.clone());
    let mut workspace=crate::workspace::Workspace::default();workspace.attach(connection);
    let started=Instant::now();
    let result=workspace.open_background(study,None,false);
    assert!(started.elapsed()<Duration::from_millis(150),"network must not block command processing");
    assert!(result.as_ref().err().is_some_and(|e|crate::workspace::opening_pending(e)));
    // A command can still checkpoint while the network request is in progress.
    offline.checkpoint(json!({"screen":"patients","detail":null,"dictionary":null,"dirty":false}));
    let deadline=Instant::now()+Duration::from_secs(3);
    loop {
        {let mut state=offline.state.lock().unwrap();state.durable_sequence=state.sequence;}
        match workspace.open_background(study,None,false) {
            Ok(())=>break,
            Err(error) if crate::workspace::opening_pending(&error)&&Instant::now()<deadline=>thread::sleep(Duration::from_millis(10)),
            Err(error)=>panic!("background open failed: {error}"),
        }
    }
    server.join().unwrap();
    assert_eq!(workspace.detail.as_ref().unwrap().study.id,study);
}

#[test]
fn selected_patient_sex_is_frozen_and_requires_an_exact_server_ack() {
    for confirmed_sex in [None,Some("F")] {
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();
        let (offline,_)=fixture(&format!("http://{}",listener.local_addr().unwrap()));
        let local=offline.create_patient(NewPatient {
            family_name:"Synthetic".into(),given_name:"Patient".into(),patronymic:None,birth_date:None,
        },Some(PatientSex::Female)).unwrap();
        assert_eq!(json!(local)["sex"],"F");
        let operation=offline.state.lock().unwrap().pending[0].clone();
        assert_eq!(operation.body["sex"],"F");
        let frozen=operation.request_hash();
        let mut receipt=matching_receipt(&operation);
        if let Some(sex)=confirmed_sex {receipt["response"]["sex"]=json!(sex);}
        let server=thread::spawn(move||{
            let(mut socket,_)=listener.accept().unwrap();
            assert_eq!(request_path(&mut socket),format!("/api/v1/operations/{}",operation.id));
            reply_json(&mut socket,200,receipt);
        });
        let raw=offline.transport.lock().unwrap().clone();
        let op=offline.state.lock().unwrap().pending[0].clone();
        let result=offline.sync_one(&raw,&op);
        server.join().unwrap();
        let state=offline.state.lock().unwrap();
        if confirmed_sex.is_some() {
            result.unwrap();
            assert!(state.pending.is_empty());
            assert_eq!(state.confirmed.get(&op.id),Some(&frozen));
        } else {
            assert!(result.is_err());
            assert_eq!(state.pending[0].request_hash(),frozen);
            assert!(state.confirmed.is_empty());
        }
    }
}

#[test]
fn explicit_null_from_patient_page_clears_legacy_sex_without_changing_outbox() {
    for field in [Some(Value::Null),None] {
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();
        let (offline,_)=fixture(&format!("http://{}",listener.local_addr().unwrap()));
        let id=Uuid::new_v4();
        let mut row=json!({"id":id,"family_name":"Synthetic","given_name":"Patient","patronymic":null,"birth_date":null});
        if let Some(value)=field.clone() {row["sex"]=value;}
        let server=thread::spawn(move||{
            let(mut socket,_)=listener.accept().unwrap();
            let _=request_path(&mut socket);
            reply_json(&mut socket,200,json!({"items":[row],"next_cursor":null}));
        });
        {
            let mut state=offline.state.lock().unwrap();
            state.patient_sexes.insert(id,PatientSex::Female);
            state.journal=json!({"clinical_input":"retained"});
        }
        let raw=offline.transport.lock().unwrap().clone();
        offline.fetch_one(&raw,"/api/v1/patients/page?limit=50").unwrap();
        server.join().unwrap();
        let state=offline.state.lock().unwrap();
        assert_eq!(state.patient_sexes.contains_key(&id),field.is_none());
        assert!(state.pending.is_empty());
        assert_eq!(state.journal["clinical_input"],"retained");
    }
}

#[test]
fn switching_background_open_does_not_show_the_late_first_response() {
    let listener=TcpListener::bind("127.0.0.1:0").unwrap();
    let (offline,_)=fixture(&format!("http://{}",listener.local_addr().unwrap()));
    let user=Uuid::new_v4();
    let mut connection={
        let mut raw=offline.transport.lock().unwrap();raw.session="synthetic".into();raw.user_id=Some(user);
        raw.identity=Some(crate::ui_guidance::Identity{user_id:user,organization_id:Uuid::new_v4(),doctor_id:Uuid::new_v4(),workstation_id:Uuid::new_v4()});
        raw.clone()
    };
    let first=Uuid::new_v4();let second=Uuid::new_v4();let patient=Uuid::new_v4();let version=Uuid::new_v4();
    {
        let mut state=offline.state.lock().unwrap();
        state.journal=json!({"detail":null,"dictionary":null,"dirty":false});
        state.responses.insert(format!("/api/v1/terminology?version_id={version}"),json!({"version_id":version,"clinically_approved":true,"terms":[]}));
    }
    let server=thread::spawn(move||{
        let mut workers=vec![];
        for _ in 0..2 {
            let(mut socket,_)=listener.accept().unwrap();
            workers.push(thread::spawn(move||{
                let path=request_path(&mut socket);
                let id=if path==format!("/api/v1/studies/{first}") {first} else {
                    assert_eq!(path,format!("/api/v1/studies/{second}"));second
                };
                if id==first {thread::sleep(Duration::from_millis(250));}
                reply_json(&mut socket,200,json!({"study":{"id":id,"patient_id":patient,"status":"draft",
                    "row_version":1,"study_instance_uid":"1.2.3"},"patient":{"id":patient,"family_name":"Synthetic",
                    "given_name":"Example","patronymic":null,"birth_date":null},
                    "revision_id":Uuid::new_v4(),"protocol":crate::model::blank_protocol(version),"annotations":[],"assets":[]}));
            }));
        }
        for worker in workers {worker.join().unwrap();}
    });
    connection.offline=Some(offline.clone());
    let mut workspace=crate::workspace::Workspace::default();workspace.attach(connection);
    assert!(workspace.open_background(first,None,false).is_err());
    workspace.cancel_background_open();
    let deadline=Instant::now()+Duration::from_secs(3);
    loop {
        {let mut state=offline.state.lock().unwrap();state.durable_sequence=state.sequence;}
        match workspace.open_background(second,None,false) {
            Ok(())=>break,
            Err(error) if crate::workspace::opening_pending(&error)&&Instant::now()<deadline=>thread::sleep(Duration::from_millis(10)),
            Err(error)=>panic!("open failed: {error}"),
        }
    }
    server.join().unwrap();
    assert_eq!(workspace.detail.as_ref().unwrap().study.id,second);
}

#[test]
fn cancelled_background_reopen_never_admits_a_late_operation() {
    let listener=TcpListener::bind("127.0.0.1:0").unwrap();
    let (offline,_)=fixture(&format!("http://{}",listener.local_addr().unwrap()));
    let user=Uuid::new_v4();
    let mut connection={
        let mut raw=offline.transport.lock().unwrap();raw.session="synthetic".into();raw.user_id=Some(user);
        raw.identity=Some(crate::ui_guidance::Identity{user_id:user,organization_id:Uuid::new_v4(),doctor_id:Uuid::new_v4(),workstation_id:Uuid::new_v4()});raw.clone()
    };
    let study=Uuid::new_v4();let patient=Uuid::new_v4();let version=Uuid::new_v4();
    let detail:StudyDetail=serde_json::from_value(json!({"study":{"id":study,"patient_id":patient,"status":"approved","row_version":7,"study_instance_uid":"1.2.3"},"patient":{"id":patient,"family_name":"Synthetic","given_name":"Example","patronymic":null,"birth_date":null},"revision_id":Uuid::new_v4(),"protocol":crate::model::blank_protocol(version),"annotations":[],"assets":[]})).unwrap();
    let remote=json!(detail);
    let (entered_tx,entered_rx)=mpsc::channel();let (release_tx,release_rx)=mpsc::channel();
    let server=thread::spawn(move||{
        let(mut socket,_)=listener.accept().unwrap();
        assert_eq!(request_path(&mut socket),format!("/api/v1/studies/{study}"));
        entered_tx.send(()).unwrap();release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        reply_json(&mut socket,200,remote);
        listener.set_nonblocking(true).unwrap();
        let deadline=Instant::now()+Duration::from_millis(300);
        while Instant::now()<deadline {
            if let Ok((mut socket,_))=listener.accept(){let _=request_path(&mut socket);reply_json(&mut socket,503,json!({}));}
            thread::sleep(Duration::from_millis(5));
        }
    });
    connection.offline=Some(offline.clone());
    let mut workspace=crate::workspace::Workspace::default();workspace.attach(connection);workspace.detail=Some(detail);
    assert!(workspace.open_background(study,None,true).is_err());
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();workspace.cancel_background_open();
    release_tx.send(()).unwrap();server.join().unwrap();
    assert!(offline.state.lock().unwrap().pending.is_empty(),"cancelled preflight must not enqueue a new revision");
    assert_eq!(workspace.detail.as_ref().unwrap().study.status,"approved");
}

#[test]
fn cancelling_an_admitted_direct_reopen_never_waits_for_the_http_response() {
    let listener=TcpListener::bind("127.0.0.1:0").unwrap();
    let connection=Connection::new(&format!("http://{}",listener.local_addr().unwrap()),"synthetic-key").unwrap();
    let study=Uuid::new_v4();let patient=Uuid::new_v4();
    let detail:StudyDetail=serde_json::from_value(json!({"study":{"id":study,"patient_id":patient,"status":"approved","row_version":7,"study_instance_uid":"1.2.3"},"patient":{"id":patient,"family_name":"Synthetic","given_name":"Example","patronymic":null,"birth_date":null},"revision_id":Uuid::new_v4(),"protocol":null,"annotations":[],"assets":[]})).unwrap();
    let (entered_tx,entered_rx)=mpsc::channel();
    let server=thread::spawn(move||{
        let(mut socket,_)=listener.accept().unwrap();
        assert_eq!(request_path(&mut socket),format!("/api/v1/studies/{study}/reopen"));
        entered_tx.send(()).unwrap();thread::sleep(Duration::from_millis(500));
        reply_json(&mut socket,200,json!({"row_version":8}));
    });
    let mut workspace=crate::workspace::Workspace::default();workspace.attach(connection);workspace.detail=Some(detail);
    assert!(workspace.open_background(study,None,true).is_err());
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let started=Instant::now();workspace.cancel_background_open();let elapsed=started.elapsed();
    server.join().unwrap();
    assert!(elapsed<Duration::from_millis(200),"cancellation held a mutex through the network response: {elapsed:?}");
}

#[test]
fn unchanged_refreshes_write_the_encrypted_index_once() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (offline, _receiver) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let server = thread::spawn(move || {
        for _ in 0..3 {
            let (mut socket, _) = listener.accept().unwrap();
            assert_eq!(request_path(&mut socket), "/api/v1/tasks");
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]")
                .unwrap();
        }
    });
    let raw = offline.transport.lock().unwrap().clone();
    for _ in 0..3 {
        offline.fetch_one(&raw, "/api/v1/tasks").unwrap();
        offline.flush_on_writer().unwrap();
    }
    server.join().unwrap();
    let metrics = offline.performance();
    assert_eq!(metrics.metadata_requests, 3);
    assert_eq!(metrics.unchanged_responses, 2);
    assert_eq!(metrics.index_writes, 1);
}

#[test]
fn a_late_metadata_response_cannot_replace_a_newer_same_study_generation() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (offline, _receiver) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let path = "/api/v1/studies/11111111-1111-4111-8111-111111111111";
    offline
        .state
        .lock()
        .unwrap()
        .responses
        .insert(path.into(), json!(["cached"]));
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        assert_eq!(request_path(&mut socket), path);
        started_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]")
            .unwrap();
    });
    let reader = {
        let offline = offline.clone();
        thread::spawn(move || {
            let raw = offline.transport.lock().unwrap().clone();
            offline.fetch_one(&raw, path).unwrap();
        })
    };
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    offline.checkpoint(json!({"detail":{"study":{"id":"11111111-1111-4111-8111-111111111111"},"protocol":"synthetic local draft"}}));
    let before = offline.state_token();
    release_tx.send(()).unwrap();
    server.join().unwrap();
    reader.join().unwrap();
    assert_eq!(offline.cached(path).unwrap()["protocol"], json!("synthetic local draft"));
    assert_eq!(offline.state_token(), before);
    assert_eq!(offline.performance().stale_responses, 1);
    assert_eq!(
        offline
            .fetch
            .lock()
            .unwrap()
            .pending
            .front()
            .map(String::as_str),
        Some(path)
    );
}

#[test]
fn shared_original_reuses_memory_without_a_full_image_copy() {
    let (offline, _receiver) = fixture("http://127.0.0.1:9");
    let id = Uuid::new_v4();
    let bytes = Arc::new(vec![7; 8 * 1024 * 1024]);
    offline.memory.lock().unwrap().insert(id, bytes.clone());
    let one = offline.download_shared(id).unwrap();
    let two = offline.download_shared(id).unwrap();
    assert!(Arc::ptr_eq(&bytes, &one) && Arc::ptr_eq(&one, &two));
    assert_eq!(offline.performance().memory_hits, 2);
}

#[test]
fn lru_limits_evict_old_bytes_but_preserve_unsaved_originals() {
    let (one, two, three) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let mut cache = MemoryCache::default();
    cache.insert(one, Arc::new(vec![1; 4]));
    cache.insert(two, Arc::new(vec![2; 4]));
    assert!(cache.get(&one).is_some());
    cache.insert(three, Arc::new(vec![3; 4]));
    cache.trim(8, 2, |_| true);
    assert!(!cache.contains_key(&two));
    assert!(cache.contains_key(&one) && cache.contains_key(&three));
    cache.trim(0, 0, |id| *id != one);
    assert!(cache.contains_key(&one), "unsaved original is pinned");
    assert!(!cache.contains_key(&three));
}

#[test]
fn encrypted_thumbnail_cache_is_bound_to_asset_without_dirtying_clinical_state() {
    let (offline, receiver) = fixture("http://127.0.0.1:9");
    let id = Uuid::new_v4();
    let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        32,
        20,
        image::Rgb([13, 29, 45]),
    ));
    let bytes = Arc::new(crate::imaging::jpeg(&image).unwrap());
    let before = offline.state_token();
    offline.start_disk(receiver);
    offline
        .disk
        .send(DiskJob::Thumbnail {
            asset: id,
            bytes: bytes.clone(),
        })
        .unwrap();
    let started = Instant::now();
    while offline.store.read(&thumbnail_key(id)).unwrap().is_none() {
        assert!(started.elapsed() < Duration::from_secs(3));
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        offline.thumbnail_shared(id).unwrap().as_ref(),
        bytes.as_ref()
    );
    assert_eq!(offline.state_token(), before);
    assert_eq!(offline.performance().index_writes, 0);
    let another = Uuid::new_v4();
    offline
        .store
        .write(
            &thumbnail_key(another),
            &offline.store.read(&thumbnail_key(id)).unwrap().unwrap(),
        )
        .unwrap();
    assert!(
        offline.thumbnail_shared(another).is_err(),
        "a different asset cannot reuse the envelope"
    );
    assert_eq!(offline.state_token(), before);
    offline
        .stopping
        .store(true, std::sync::atomic::Ordering::SeqCst);
}

fn report_assets_fixture(
    offline: &Arc<Offline>,
    bytes: &[u8],
    count: usize,
) -> (Connection, Vec<Asset>) {
    let study = Uuid::new_v4();
    let assets: Vec<_> = (0..count)
        .map(|_| Asset {
            id: Uuid::new_v4(),
            study_id: study,
            kind: "original".into(),
            sha256: digest(bytes),
            byte_size: bytes.len() as i64,
            mime_type: "image/jpeg".into(),
        })
        .collect();
    offline
        .state
        .lock()
        .unwrap()
        .responses
        .insert(format!("/api/v1/studies/{study}"), json!({"assets":assets}));
    let mut connection = offline.transport.lock().unwrap().clone();
    connection.offline = Some(offline.clone());
    (connection, assets)
}

#[test]
fn report_waits_for_only_selected_chunked_originals_with_the_shared_bounded_pool() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (offline, _receiver) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let bytes = crate::imaging::jpeg(&image::DynamicImage::new_rgb8(32, 20)).unwrap();
    let (connection, assets) = report_assets_fixture(&offline, &bytes, 5);
    let selected: Vec<_> = [0, 2, 3, 4].iter().map(|index| assets[*index].id).collect();
    let paths = Arc::new(Mutex::new(Vec::new()));
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server = {
        let paths = paths.clone();
        let active = active.clone();
        let peak = peak.clone();
        thread::spawn(move || {
            let mut handlers = Vec::new();
            for _ in 0..4 {
                let (mut socket, _) = listener.accept().unwrap();
                let bytes = bytes.clone();
                let paths = paths.clone();
                let active = active.clone();
                let peak = peak.clone();
                handlers.push(thread::spawn(move || {
                    paths.lock().unwrap().push(request_path(&mut socket));
                    let concurrent=active.fetch_add(1,std::sync::atomic::Ordering::SeqCst)+1;
                    peak.fetch_max(concurrent,std::sync::atomic::Ordering::SeqCst);
                    socket.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").unwrap();
                    for chunk in bytes.chunks(64) {
                        write!(socket,"{:x}\r\n",chunk.len()).unwrap();
                        socket.write_all(chunk).unwrap();socket.write_all(b"\r\n").unwrap();
                    }
                    socket.write_all(b"0\r\n\r\n").unwrap();
                    active.fetch_sub(1,std::sync::atomic::Ordering::SeqCst);
                }));
            }
            for handler in handlers {
                handler.join().unwrap();
            }
        })
    };
    offline.state.lock().unwrap().error = "synthetic unrelated metadata failure".into();
    offline.start_asset_network();
    let photos = crate::report_assets::load_until(
        &connection,
        &assets,
        &selected,
        Instant::now() + Duration::from_secs(3),
        || false,
    )
    .unwrap();
    server.join().unwrap();
    assert_eq!(photos.iter().map(|p| p.id).collect::<Vec<_>>(), selected);
    assert_eq!(
        photos
            .iter()
            .map(|p| p.caption.as_str())
            .collect::<Vec<_>>(),
        ["Снимок 1", "Снимок 3", "Снимок 4", "Снимок 5"]
    );
    assert!(photos.iter().all(|p| digest(&p.bytes) == assets[0].sha256));
    assert!(
        !paths
            .lock()
            .unwrap()
            .iter()
            .any(|path| path.contains(&assets[1].id.to_string()))
    );
    assert_eq!(paths.lock().unwrap().len(), 4);
    assert!(peak.load(std::sync::atomic::Ordering::SeqCst) <= 2);
    assert!(offline.state.lock().unwrap().pending.is_empty());
    offline
        .stopping
        .store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn report_stops_on_a_selected_original_404_and_refresh_clears_the_scoped_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (offline, _receiver) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let (connection, assets) = report_assets_fixture(&offline, b"synthetic", 1);
    let id = assets[0].id;
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        assert_eq!(request_path(&mut socket), format!("/api/v1/assets/{id}"));
        socket
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
    });
    offline.start_asset_network();
    let started = Instant::now();
    let error = crate::report_assets::load_until(
        &connection,
        &assets,
        &[id],
        started + Duration::from_secs(3),
        || false,
    )
    .err()
    .unwrap();
    assert!(error.contains("не найден"));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(connection.asset_failure(id), Some(AssetFailure::Http(404)));
    assert!(offline.asset_fetch.lock().unwrap().pending.is_empty());
    offline.clear_asset_failures();
    assert_eq!(connection.asset_failure(id), None);
    server.join().unwrap();
    offline
        .stopping
        .store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn report_cancellation_and_deadline_stop_before_requesting_unneeded_originals() {
    let (offline, _receiver) = fixture("http://127.0.0.1:9");
    let (connection, assets) = report_assets_fixture(&offline, b"synthetic", 1);
    assert!(
        crate::report_assets::load_until(
            &connection,
            &assets,
            &[assets[0].id],
            Instant::now() + Duration::from_secs(3),
            || true
        )
        .is_err()
    );
    assert!(
        crate::report_assets::load_until(
            &connection,
            &assets,
            &[assets[0].id],
            Instant::now(),
            || false
        )
        .is_err()
    );
    assert!(offline.asset_fetch.lock().unwrap().pending.is_empty());
}

#[test]
fn report_wait_is_bounded_even_when_no_file_worker_can_make_progress() {
    let (offline, _receiver) = fixture("http://127.0.0.1:9");
    let (connection, assets) = report_assets_fixture(&offline, b"synthetic", 2);
    let started = Instant::now();
    let error = crate::report_assets::load_until(
        &connection,
        &assets,
        &[assets[0].id],
        started + Duration::from_millis(75),
        || false,
    )
    .err()
    .unwrap();
    assert!(error.contains("45 секунд"));
    assert!(started.elapsed() < Duration::from_secs(2));
    let queue = offline.asset_fetch.lock().unwrap();
    assert_eq!(queue.pending.len(), 1);
    assert_eq!(
        queue.pending.front().unwrap(),
        &format!("/api/v1/assets/{}", assets[0].id)
    );
}

#[test]
fn report_aliases_reuse_the_cached_original_and_keep_snapshot_ids_and_captions() {
    let (offline, _receiver) = fixture("http://127.0.0.1:9");
    let (connection, assets) = report_assets_fixture(&offline, b"synthetic", 2);
    let canonical = assets[1].id;
    offline
        .state
        .lock()
        .unwrap()
        .aliases
        .insert(assets[0].id, canonical);
    offline
        .memory
        .lock()
        .unwrap()
        .insert(canonical, Arc::new(b"synthetic".to_vec()));
    let photos = crate::report_assets::load_selected(&connection, &assets, &[canonical]).unwrap();
    assert_eq!(photos.len(), 1);
    assert_eq!(photos[0].id, assets[0].id);
    assert_eq!(photos[0].caption, "Снимок 1");
    assert!(offline.asset_fetch.lock().unwrap().pending.is_empty());
}

#[test]
fn thumbnail_requires_matching_digest_and_bounded_jpeg_dimensions() {
    let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        32,
        20,
        image::Rgb([13, 29, 45]),
    ));
    let bytes = crate::imaging::jpeg(&image).unwrap();
    assert!(crate::connection::validate_thumbnail(&bytes, Some(&digest(&bytes))).is_ok());
    assert!(crate::connection::validate_thumbnail(&bytes, Some(&"0".repeat(64))).is_err());
    assert!(crate::connection::validate_thumbnail(&bytes, None).is_err());
    let oversized = crate::imaging::jpeg(&image::DynamicImage::new_rgb8(257, 20)).unwrap();
    assert!(crate::connection::validate_thumbnail(&oversized, Some(&digest(&oversized))).is_err());
}

#[test]
fn delivered_reports_and_draft_prints_do_not_create_background_polls() {
    let (offline, _receiver) = fixture("http://127.0.0.1:9");
    let (draft, delivered, waiting) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let mut state = offline.state.lock().unwrap();
    state.responses.insert(
        format!("local-pdf/{draft}/synthetic/draft-false"),
        json!({}),
    );
    state.responses.insert(
        format!("local-pdf/{delivered}/synthetic/approved-true"),
        json!({}),
    );
    state.responses.insert(
        format!("local-pdf/{waiting}/synthetic/approved-true"),
        json!({}),
    );
    state.responses.insert(
        format!("/api/v1/studies/{delivered}/results/1c"),
        json!([{"state":"sent","delivered":true}]),
    );
    state.responses.insert(
        format!("/api/v1/studies/{waiting}/results/1c"),
        json!([{"state":"pending"}]),
    );
    assert_eq!(delivery_poll_studies(&state), BTreeSet::from([waiting]));
}

#[test]
fn a_slow_original_download_does_not_block_metadata() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (offline, _receiver) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let asset = Uuid::new_v4();
    let image = vec![31u8; 96 * 1024];
    offline.state.lock().unwrap().responses.insert(
        "synthetic-detail".into(),
        json!({
            "assets":[{"id":asset,"kind":"original","sha256":digest(&image)}]
        }),
    );
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut slow, _) = listener.accept().unwrap();
        assert_eq!(request_path(&mut slow), format!("/api/v1/assets/{asset}"));
        write!(
            slow,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            image.len()
        )
        .unwrap();
        slow.flush().unwrap();
        started_tx.send(()).unwrap();
        let (mut fast, _) = listener.accept().unwrap();
        assert_eq!(request_path(&mut fast), "/api/v1/tasks");
        fast.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]")
            .unwrap();
        drop(fast);
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        slow.write_all(&image).unwrap();
    });
    offline.fetch_priority(&format!("/api/v1/assets/{asset}"));
    offline.start_asset_network();
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    offline.fetch_priority("/api/v1/tasks");
    offline.start_metadata_network();
    let started = Instant::now();
    while offline.cached("/api/v1/tasks").is_none() {
        assert!(started.elapsed() < Duration::from_secs(3));
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        offline.performance().asset_downloads,
        0,
        "metadata finished while the image body was still held"
    );
    eprintln!(
        "parallel transfer benchmark: metadata completed in {}ms while asset was blocked",
        started.elapsed().as_millis()
    );
    release_tx.send(()).unwrap();
    server.join().unwrap();
    while offline.performance().asset_downloads == 0 {
        assert!(started.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(5));
    }
    offline
        .stopping
        .store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn review_logout_and_session_rejection_never_invert_state_and_transport_locks() {
    let (offline, _disk) = fixture("http://127.0.0.1:9");
    let raw = offline.transport.lock().unwrap().clone();
    let held = offline.state.lock().unwrap();
    let (tx, rx) = mpsc::channel();
    let logout = offline.clone();
    thread::spawn(move || { let _ = tx.send(logout.mutate("/api/v1/logout", "POST", json!({}))); });
    thread::sleep(Duration::from_millis(50));
    let rejected = offline.clone();
    thread::spawn(move || rejected.reject_session(&raw));
    thread::sleep(Duration::from_millis(50));
    drop(held);
    assert!(rx.recv_timeout(Duration::from_secs(1)).is_ok(), "logout must not wait for transport while holding editor state");
}

#[test]
fn review_shutdown_waits_for_network_and_releases_the_cache_before_success() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (offline, receiver) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let root = offline.store.root().to_path_buf();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        assert_eq!(request_path(&mut socket), "/api/v1/me");
        entered_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        reply_json(&mut socket, 200, json!({"server":"synthetic"}));
    });
    offline.start_disk(receiver);
    offline.fetch("/api/v1/me");
    offline.start_metadata_network();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let premature = offline.shutdown().is_ok();
    release_tx.send(()).unwrap();
    server.join().unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    while offline.shutdown().is_err() { assert!(Instant::now() < until); thread::sleep(Duration::from_millis(10)); }
    drop(offline);
    assert!(Store::new(root, Arc::new(AesProtector([23;32]))).is_ok(), "a successful shutdown must leave no background owner of the cache");
    assert!(!premature, "shutdown cannot report success while a GET may still change local state");
}

#[test]
fn review_independent_lane_advances_during_an_inflight_file_transfer() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let (offline, receiver) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let blob = Uuid::new_v4();
    let study = Uuid::new_v4();
    let bytes = b"synthetic frozen upload".to_vec();
    offline.store.write(&blob.simple().to_string(), &bytes).unwrap();
    let mut first = queued_patient();
    first.entity_id = blob;
    first.path = format!("/api/v1/studies/{study}/assets?kind=derived&sha256={}", digest(&bytes));
    first.body = json!({"kind":"derived","sha256":digest(&bytes),"parent":null});
    first.blob = Some(blob);
    let mut dependent = queued_patient();
    dependent.path = format!("/api/v1/studies/{study}/protocol");
    let dependent_id = dependent.id;
    {
        let mut state = offline.state.lock().unwrap();
        state.blobs.insert(blob, Blob {file_id:blob,sha256:digest(&bytes),size:bytes.len(),persisted:true,backend_confirmed:false});
        state.pending.push_back(first.clone());
        state.pending.push_back(dependent);
    }
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let gate = Arc::new(Mutex::new(Some(release_rx)));
    let (second_tx, second_rx) = mpsc::channel();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server_stop = stop.clone();
    let server_offline = offline.clone();
    let first_id = first.id;
    let committed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server = thread::spawn(move || {
        let mut handlers = Vec::new();
        while !server_stop.load(std::sync::atomic::Ordering::SeqCst) {
            let Ok((mut socket, _)) = listener.accept() else { thread::sleep(Duration::from_millis(1)); continue; };
            let offline = server_offline.clone(); let gate = gate.clone(); let entered = entered_tx.clone(); let second = second_tx.clone(); let committed = committed.clone();
            handlers.push(thread::spawn(move || {
                let path = request_path(&mut socket);
                if path.contains("/assets?") {
                    // Drain the small immutable body before withholding its reply.
                    let mut bytes = vec![0; b"synthetic frozen upload".len()]; socket.read_exact(&mut bytes).unwrap();
                    entered.send(()).unwrap(); gate.lock().unwrap().take().unwrap().recv_timeout(Duration::from_secs(3)).unwrap();
                    committed.store(true, std::sync::atomic::Ordering::SeqCst); reply_json(&mut socket, 200, json!({}));
                } else {
                    let id:Uuid = path.strip_prefix("/api/v1/operations/").unwrap().parse().unwrap();
                    let op = offline.state.lock().unwrap().pending.iter().find(|p|p.id==id).unwrap().clone();
                    if id==first_id && !committed.load(std::sync::atomic::Ordering::SeqCst) { reply_json(&mut socket,404,json!({})); }
                    else { if id==dependent_id {assert!(committed.load(std::sync::atomic::Ordering::SeqCst), "same-study operation overtook its upload");} else if id!=first_id { second.send(()).unwrap(); } let mut receipt=matching_receipt(&op); if op.blob.is_some() {receipt["response"]["sha256"]=op.body["sha256"].clone();} reply_json(&mut socket,200,receipt); }
                }
            }));
        }
        for handler in handlers { handler.join().unwrap(); }
    });
    offline.start_disk(receiver); offline.start_network(offline.transport.lock().unwrap().clone());
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    offline.mutate("/api/v1/patients","POST",queued_patient().body).unwrap();
    let advanced = second_rx.recv_timeout(Duration::from_millis(700)).is_ok();
    release_tx.send(()).unwrap();
    let until=Instant::now()+Duration::from_secs(2); while !offline.state.lock().unwrap().pending.is_empty() {assert!(Instant::now()<until);thread::sleep(Duration::from_millis(5));}
    offline.stopping.store(true,std::sync::atomic::Ordering::SeqCst);stop.store(true,std::sync::atomic::Ordering::SeqCst);server.join().unwrap();
    assert!(advanced,"an independent durable operation must reach the server before the other lane's upload completes");
}

#[test]
fn review_unrelated_checkpoint_does_not_discard_metadata() {
    let listener=TcpListener::bind("127.0.0.1:0").unwrap();
    let (offline,_receiver)=fixture(&format!("http://{}",listener.local_addr().unwrap()));
    let (entered_tx,entered_rx)=mpsc::channel();let (release_tx,release_rx)=mpsc::channel();
    let server=thread::spawn(move||{let (mut socket,_)=listener.accept().unwrap();assert_eq!(request_path(&mut socket),"/api/v1/templates");entered_tx.send(()).unwrap();release_rx.recv_timeout(Duration::from_secs(2)).unwrap();reply_json(&mut socket,200,json!([]));});
    let reader={let offline=offline.clone();thread::spawn(move||{let raw=offline.transport.lock().unwrap().clone();offline.fetch_one(&raw,"/api/v1/templates").unwrap();})};
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();for n in 0..25 {offline.checkpoint(json!({"unrelated_ui_counter":n}));}
    release_tx.send(()).unwrap();server.join().unwrap();reader.join().unwrap();
    assert_eq!(offline.cached("/api/v1/templates"),Some(json!([])));assert_eq!(offline.performance().stale_responses,0);
}

#[test]
fn review_shutdown_persists_a_receipt_that_arrives_after_shutdown_started() {
    let listener=TcpListener::bind("127.0.0.1:0").unwrap();
    let (offline,receiver)=fixture(&format!("http://{}",listener.local_addr().unwrap()));
    let op=queued_patient();offline.state.lock().unwrap().pending.push_back(op.clone());
    let root=offline.store.root().to_path_buf();
    let (entered_tx,entered_rx)=mpsc::channel();let (release_tx,release_rx)=mpsc::channel();let receipt=matching_receipt(&op);
    let server=thread::spawn(move||{let(mut socket,_)=listener.accept().unwrap();request_path(&mut socket);entered_tx.send(()).unwrap();release_rx.recv_timeout(Duration::from_secs(2)).unwrap();reply_json(&mut socket,200,receipt);});
    offline.start_disk(receiver);offline.start_network(offline.transport.lock().unwrap().clone());
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();assert!(offline.shutdown().is_err());
    assert!(offline.mutate("/api/v1/patients","POST",queued_patient().body).is_err(), "shutdown cannot accept new clinical operations");
    release_tx.send(()).unwrap();server.join().unwrap();
    let until=Instant::now()+Duration::from_secs(3);while offline.shutdown().is_err(){assert!(Instant::now()<until);thread::sleep(Duration::from_millis(10));}
    let saved:LocalState=serde_json::from_slice(&offline.store.read("workspace").unwrap().unwrap()).unwrap();
    assert!(saved.pending.is_empty());assert_eq!(saved.confirmed.get(&op.id),Some(&op.request_hash()));
    drop(offline);assert!(Store::new(root,Arc::new(AesProtector([23;32]))).is_ok());
}

#[test]
fn review_late_checkpoint_is_rejected_without_dirtying_a_closed_workspace() {
    let (offline, receiver) = fixture("http://127.0.0.1:9");
    offline.start_disk(receiver);
    let mut connection = offline.transport.lock().unwrap().clone();
    connection.offline = Some(offline.clone());
    let mut workspace = crate::workspace::Workspace::default();
    workspace.attach(connection);
    workspace.checkpoint(json!({"unfinished":"before closing"})).unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    while offline.shutdown().is_err() { assert!(Instant::now() < until); thread::sleep(Duration::from_millis(5)); }
    let token = offline.state_token();
    let before = workspace.journal.clone();
    let accepted = workspace.checkpoint(json!({"unfinished":"late form message"})).is_ok();
    workspace.persist();
    offline.checkpoint(json!({"unfinished":"late direct checkpoint"}));
    assert!(!accepted, "closing must reject a late form before changing the workspace journal");
    assert_eq!(workspace.journal, before);
    assert_eq!(offline.state_token(), token);
    assert!(offline.information().2);
}

#[test]
fn review_shutdown_releases_lease_with_readonly_handles_and_rejects_old_store_writes() {
    let (offline, receiver) = fixture("http://127.0.0.1:9");
    let old_store = offline.store.clone();
    let readonly = offline.clone();
    let root = old_store.root().to_path_buf();
    offline.start_disk(receiver);
    let until = Instant::now() + Duration::from_secs(2);
    while offline.shutdown().is_err() { assert!(Instant::now() < until); thread::sleep(Duration::from_millis(5)); }
    let reopened = Store::new(root, Arc::new(AesProtector([23;32])));
    let stale_write = old_store.write("stale-writer", b"must not replace a new session's files");
    assert!(reopened.is_ok(), "readonly Offline/Connection handles cannot retain the writer lease after successful shutdown");
    assert!(stale_write.is_err(), "all clones of the old store must reject writes after lease release");
    assert!(readonly.information().2);
}

#[test]
fn review_shutdown_waits_for_an_external_render_write_and_its_index_flush() {
    struct GatedCipher { entered:mpsc::Sender<()>, release:Mutex<mpsc::Receiver<()>> }
    impl crate::cache::Protector for GatedCipher {
        fn protect(&self, bytes:&[u8]) -> Result<Vec<u8>,String> {
            if bytes == b"synthetic external PDF" { self.entered.send(()).unwrap(); self.release.lock().unwrap().recv_timeout(Duration::from_secs(3)).unwrap(); }
            crate::cache::Protector::protect(&AesProtector([23;32]),bytes)
        }
        fn unprotect(&self, bytes:&[u8]) -> Result<Vec<u8>,String> { crate::cache::Protector::unprotect(&AesProtector([23;32]),bytes) }
    }
    let (mut offline, receiver) = fixture("http://127.0.0.1:9");
    let (entered_tx,entered_rx)=mpsc::channel();let(release_tx,release_rx)=mpsc::channel();
    Arc::get_mut(&mut offline).unwrap().store=Store::new(std::env::temp_dir().join(format!("m53-external-write-{}",Uuid::new_v4())),Arc::new(GatedCipher {entered:entered_tx,release:Mutex::new(release_rx)})).unwrap();
    offline.start_disk(receiver);
    let writer = {let offline=offline.clone();thread::spawn(move||offline.cache_render("external-review",b"synthetic external PDF"))};
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let mut premature=offline.shutdown().is_ok();thread::sleep(Duration::from_millis(100));premature |= offline.shutdown().is_ok();
    release_tx.send(()).unwrap();let written=writer.join().unwrap();
    assert!(!premature,"an external render writer must keep shutdown in progress until its index is durable");
    assert!(written.is_ok());
    let until=Instant::now()+Duration::from_secs(2);while offline.shutdown().is_err(){assert!(Instant::now()<until);thread::sleep(Duration::from_millis(5));}
    let saved:LocalState=serde_json::from_slice(&offline.store.read("workspace").unwrap().unwrap()).unwrap();
    assert!(saved.responses.contains_key("render/external-review"));
}
