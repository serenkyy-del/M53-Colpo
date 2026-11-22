//! Synthetic HTTP only; no clinical API or cache.
use super::*;
use crate::{diagnosis_cache::Scope, diagnosis_priority::Group};
use std::io::{Read, Write};
fn configure_accepted_stream(stream: &std::net::TcpStream) {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(4)))
        .unwrap();
}

#[test]
fn accepted_nonblocking_socket_waits_for_request_bytes() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut peer = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    peer.set_write_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    let (mut accepted, _) = listener.accept().unwrap();
    // Reproduce Windows' inherited nonblocking mode independently of the OS.
    accepted.set_nonblocking(true).unwrap();
    configure_accepted_stream(&accepted);
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut byte = [0];
        ready_tx.send(()).unwrap();
        tx.send(
            accepted
                .read_exact(&mut byte)
                .map(|()| byte)
                .map_err(|e| e.kind()),
        )
        .unwrap();
    });
    // The peer deliberately has no bytes available until this observation.
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let premature = rx.recv_timeout(Duration::from_millis(50));
    peer.write_all(b"x").unwrap();
    let result = match premature {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        Err(error) => panic!("synthetic reader disconnected: {error}"),
    };
    reader.join().unwrap();
    assert_eq!(
        result,
        Ok([b'x']),
        "accepted socket must wait for request bytes"
    );
}

fn setup(
    status: u16,
) -> (
    Offline,
    Group,
    std::thread::JoinHandle<()>,
    std::path::PathBuf,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut raw = Connection::new(
        &format!("http://{}", listener.local_addr().unwrap()),
        "synthetic-workstation",
    )
    .unwrap();
    raw.user_id = Some(Uuid::from_u128(6));
    raw.session = "synthetic-session".into();
    raw.identity = Some(crate::ui_guidance::Identity {
        organization_id: Uuid::from_u128(5),
        user_id: Uuid::from_u128(6),
        doctor_id: Uuid::from_u128(7),
        workstation_id: Uuid::from_u128(8),
    });
    let root = std::env::temp_dir().join(format!("m53-priority-contract-{}", Uuid::new_v4()));
    let store = Store::new(root.clone(), Arc::new(crate::cache::AesProtector([7; 32]))).unwrap();
    let profile = Profile {
        server: raw.base.clone(),
        user_id: raw.user_id.unwrap(),
        doctor: "Synthetic".into(),
    };
    let state = LocalState {
        activity: None,
        activity_intents: BTreeMap::new(),
        format_version: 1,
        profile,
        responses: BTreeMap::new(),
        blobs: BTreeMap::new(),
        pending: VecDeque::new(),
        confirmed: BTreeMap::new(),
        journal: Value::Null,
        editors: BTreeMap::new(),
        server_only_studies: BTreeSet::new(),
        remote_conflicts: BTreeMap::new(),
        conflict_history: vec![],
        aliases: BTreeMap::new(),
        patient_sexes: BTreeMap::new(),
        sequence: 1,
        durable_sequence: 1,
        metadata_generations: BTreeMap::new(),
        recovery_errors: BTreeSet::new(),
        online: true,
        error: String::new(),
    };
    let (tx, _) = mpsc::channel();
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
        disk: tx,
        transport: Mutex::new(raw),
        wake: false.into(),
        paused: false.into(),
        stopping: false.into(),
        workers: Workers::default(),
        sync: Mutex::new(SyncSchedule::default()),
    };
    let base = Group {
        version: 4,
        catalog_version_id: Some(Uuid::from_u128(9)),
        diagnosis_ids: vec![Uuid::from_u128(10)],
    };
    let scope = Scope::from_connection(&offline.transport.lock().unwrap()).unwrap();
    {
        let mut cache = offline.diagnosis_catalog.lock().unwrap();
        cache.scope = Some(scope.clone());
        cache
            .publish_priority(&offline.store, &scope, base.clone())
            .unwrap();
    }
    let expected = crate::diagnosis_priority::update_body(&base, &[Uuid::from_u128(11)]).unwrap();
    let server = std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(e) => panic!("synthetic accept: {e}"),
            }
        };
        configure_accepted_stream(&stream);
        let mut request = vec![];
        let mut part = [0; 4096];
        let end = loop {
            let n = stream.read(&mut part).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&part[..n]);
            if let Some(i) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
        assert!(headers.starts_with("put /api/v1/diagnoses/priority http/1.1\r\n"));
        let header = |name: &str| {
            headers
                .lines()
                .find_map(|line| line.strip_prefix(&format!("{name}: ")))
                .unwrap()
                .trim()
        };
        assert_eq!(header("content-type"), "application/json");
        assert_eq!(header("authorization"), "bearer synthetic-session");
        assert_eq!(header("x-workstation-key"), "synthetic-workstation");
        assert_eq!(header("x-m53-protocol-version"), "2");
        assert_eq!(
            Uuid::parse_str(header("x-entity-id")).unwrap(),
            Uuid::from_u128(5)
        );
        assert!(!Uuid::parse_str(header("x-operation-id")).unwrap().is_nil());
        let length = header("content-length").parse::<usize>().unwrap();
        while request.len() < end + length {
            let n = stream.read(&mut part).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&part[..n]);
        }
        assert_eq!(
            serde_json::from_slice::<Value>(&request[end..end + length]).unwrap(),
            expected
        );
        let body = serde_json::to_string(&ack()).unwrap();
        write!(stream,"HTTP/1.1 {status} Reply\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
    });
    (offline, base, server, root)
}
fn ack() -> Group {
    Group {
        version: 5,
        catalog_version_id: Some(Uuid::from_u128(9)),
        diagnosis_ids: vec![Uuid::from_u128(11)],
    }
}
#[test]
fn priority_put_matching_ack_updates_memory_and_durable_cache() {
    let (offline, base, server, root) = setup(200);
    let result = offline.save_diagnosis_priorities(base, ack().diagnosis_ids);
    server.join().unwrap();
    result.unwrap();
    assert_eq!(
        offline.diagnoses_snapshot().priority.as_deref(),
        Some(&ack())
    );
    let scope = Scope::from_connection(&offline.transport.lock().unwrap()).unwrap();
    assert_eq!(
        crate::diagnosis_priority::load(&offline.store, &scope).unwrap(),
        Some(ack())
    );
    assert!(offline.state.lock().unwrap().pending.is_empty());
    drop(offline);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn priority_put_409_keeps_memory_and_durable_cache_and_requests_refresh() {
    let (offline, base, server, root) = setup(409);
    let before = offline.store.read("diagnoses-priority-v1").unwrap();
    let result = offline.save_diagnosis_priorities(base.clone(), ack().diagnosis_ids);
    server.join().unwrap();
    assert!(result.is_err());
    assert_eq!(
        offline.diagnoses_snapshot().priority.as_deref(),
        Some(&base)
    );
    assert_eq!(offline.store.read("diagnoses-priority-v1").unwrap(), before);
    assert!(offline.diagnosis_catalog.lock().unwrap().priority_refresh);
    assert!(offline.state.lock().unwrap().pending.is_empty());
    drop(offline);
    std::fs::remove_dir_all(root).unwrap();
}
