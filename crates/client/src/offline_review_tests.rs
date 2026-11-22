//! Regression checks use only synthetic data and an isolated AES-protected cache.
use super::*;
use crate::cache::AesProtector;
use m53_contracts::access::OfflinePermissions;

fn connection() -> Connection {
    // A listener reserved solely by this test: no external or clinical service.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    // The caller normally does not start network workers for these fixtures.
    drop(listener);
    Connection::new(&base, "synthetic-test-key").unwrap()
}
fn permissions(create: bool, edit: bool) -> OfflinePermissions {
    OfflinePermissions {
        allow_new_manual_encounter: create,
        allow_continue_open_encounter: edit,
        allow_local_print: true,
    }
}
fn store() -> Store {
    Store::new(
        std::env::temp_dir().join(format!("m53-review-regression-{}", Uuid::new_v4())),
        Arc::new(AesProtector([73; 32])),
    )
    .unwrap()
}
fn empty_state(raw: &Connection) -> LocalState {
    serde_json::from_value(json!({"format_version":1,"profile":{"server":raw.base,"user_id":Uuid::new_v4(),"doctor":"Synthetic"},"responses":{},"blobs":{},"pending":[],"confirmed":{},"journal":{"synthetic":"preserve"},"editors":{},"remote_conflicts":{},"conflict_history":[],"sequence":1})).unwrap()
}
fn offline_without_workers(
    raw: Connection,
    state: LocalState,
) -> (Offline, mpsc::Receiver<DiskJob>) {
    let (disk, receiver) = mpsc::channel();
    (
        Offline {
            diagnosis_catalog: Mutex::new(crate::diagnosis_cache::State::empty()),
            state: Arc::new(Mutex::new(state)),
            store: store(),
            memory: Mutex::new(MemoryCache::default()),
            thumbnails: Mutex::new(MemoryCache::default()),
            fetch: Mutex::new(FetchQueue::default()),
            asset_fetch: Mutex::new(FetchQueue::default()),
            foreground: Mutex::new(None),
            registry_paths: Mutex::new(vec![]),
            registry_epoch: 0.into(),
            performance: PerformanceCounters::default(),
            disk,
            transport: Mutex::new(raw),
            wake: false.into(),
            paused: false.into(),
            stopping: false.into(),
            workers: Workers::default(),
            sync: Mutex::new(SyncSchedule::default()),
        },
        receiver,
    )
}
fn detail() -> StudyDetail {
    let patient = Patient {
        card_number: None,
        sex: None,
        id: Uuid::new_v4(),
        external_id: None,
        family_name: "Synthetic".into(),
        given_name: "Patient".into(),
        patronymic: None,
        birth_date: None,
    };
    StudyDetail {
        mis_task_bound: false,
        study: Study {
            id: Uuid::new_v4(),
            patient_id: patient.id,
            status: "draft".into(),
            row_version: 1,
            study_instance_uid: "2.25.1".into(),
        },
        patient,
        revision_id: Some(Uuid::new_v4()),
        protocol: Some(crate::model::blank_protocol(Uuid::new_v4())),
        annotations: vec![],
        assets: vec![],
    }
}
fn operation(path: String, body: Value) -> Operation {
    Operation {
        id: Uuid::new_v4(),
        entity_id: Uuid::new_v4(),
        version_id: None,
        path,
        method: "PUT".into(),
        body,
        blob: None,
        started: true,
        blocked: Some("Backend 409: version_conflict".into()),
        sequence: 1,
    }
}

#[test]
fn retired_pacs_publication_cannot_enter_the_clinical_outbox() {
    let mut raw=connection();let mut state=empty_state(&raw);let detail=detail();
    raw.user_id=Some(state.profile.user_id);raw.session="synthetic-session".into();state.online=true;
    state.responses.insert(format!("/api/v1/studies/{}",detail.study.id),json!(detail));
    let (offline,_)=offline_without_workers(raw,state);
    let before=state_snapshot(&offline);
    assert!(offline.mutate(&format!("/api/v1/studies/{}/publications",detail.study.id),"POST",json!({"expected_version":1})).is_err());
    assert!(state_snapshot(&offline)==before,"a retired publication must not create any queued operation");
}

#[test]
fn retired_pacs_queue_is_archived_without_ack_or_loss_of_frozen_request() {
    for started in [false,true] {
    let raw=connection();let mut state=empty_state(&raw);
    let study=Uuid::new_v4();
    let mut legacy=operation(format!("/api/v1/studies/{study}/publications?legacy=1"),json!({"expected_version":7}));
    legacy.method="POST".into();legacy.blocked=Some("previous diagnostic".into());
    legacy.started=started;
    let unrelated=operation(format!("/api/v1/studies/{study}/protocol"),json!({"expected_version":4}));
    let wrong_route=operation(format!("/api/v1/studies/{study}/publications/extra"),json!({}));
    let mut wrong_method=legacy.clone();wrong_method.id=Uuid::new_v4();wrong_method.method="PUT".into();
    state.pending.extend([legacy.clone(),unrelated.clone(),wrong_route.clone(),wrong_method.clone()]);
    let disk=store();disk.write("workspace",&serde_json::to_vec(&state).unwrap()).unwrap();
    let offline=StopOnDrop(Offline::attach(raw,disk.clone(),state.profile).unwrap());
    let current=offline.state.lock().unwrap();
    assert_eq!(current.pending.len(),3);
    assert_eq!(serde_json::to_value(&current.pending[0]).unwrap(),serde_json::to_value(&unrelated).unwrap());
    assert!(current.confirmed.is_empty());
    let archived=current.conflict_history.iter().find(|op|op.id==legacy.id).unwrap();
    assert_eq!(archived.request_hash(),legacy.request_hash());
    assert_eq!(archived.body,legacy.body);assert_eq!(archived.path,legacy.path);
    assert_eq!(archived.entity_id,legacy.entity_id);assert_eq!(archived.started,started);
    assert!(archived.blocked.as_ref().unwrap().contains("previous diagnostic"));
    assert_eq!(serde_json::to_value(&current.pending[1]).unwrap(),serde_json::to_value(&wrong_route).unwrap());
    assert_eq!(serde_json::to_value(&current.pending[2]).unwrap(),serde_json::to_value(&wrong_method).unwrap());
    let mut eligible=current.clone();
    for operation in &mut eligible.pending {operation.blocked=None;}
    assert_eq!(crate::outbox::next_index(&eligible,&BTreeSet::new()),Some(0),"the retired command cannot hold the next operation in the same study");
    drop(current);
    let persisted:LocalState=serde_json::from_slice(&disk.read("workspace").unwrap().unwrap()).unwrap();
    assert_eq!(persisted.conflict_history.len(),1,"retirement must be durable before workers start");
    let deadline=Instant::now()+Duration::from_secs(3);
    while offline.shutdown().is_err(){assert!(Instant::now()<deadline);thread::sleep(Duration::from_millis(10));}
    let raw=offline.transport.lock().unwrap().clone();
    let fresh=Store::new(disk.root().to_path_buf(),Arc::new(AesProtector([73;32]))).unwrap();
    let again=StopOnDrop(Offline::attach(raw,fresh,persisted.profile).unwrap());
    assert_eq!(again.state.lock().unwrap().conflict_history.len(),1);
    }
}

#[test]
fn retired_pacs_archive_must_be_durable_before_any_worker_starts() {
    let raw=connection();let mut state=empty_state(&raw);
    let mut op=operation(format!("/api/v1/studies/{}/publications",Uuid::new_v4()),json!({"expected_version":7}));op.method="POST".into();
    state.pending.push_back(op);
    let fail=Arc::new(std::sync::atomic::AtomicBool::new(false));
    let disk=Store::new(std::env::temp_dir().join(format!("m53-retired-dicom-{}",Uuid::new_v4())),Arc::new(ToggleProtector{inner:AesProtector([74;32]),fail_writes:fail.clone()})).unwrap();
    let original=serde_json::to_vec(&state).unwrap();disk.write("workspace",&original).unwrap();
    fail.store(true,std::sync::atomic::Ordering::SeqCst);
    let result=Offline::attach(raw,disk.clone(),state.profile);
    if let Ok(offline)=&result {pause_workers(offline);}
    assert!(result.is_err(),"retirement cannot continue after a failed archive write");
    assert_eq!(disk.read("workspace").unwrap().unwrap(),original);
}
fn state_snapshot(offline: &Offline) -> (Value, u64, u64, bool, String) {
    let s = offline.state.lock().unwrap();
    (
        serde_json::to_value(&*s).unwrap(),
        s.sequence,
        s.durable_sequence,
        s.online,
        s.error.clone(),
    )
}
fn conflict_fixture(raw: Connection) -> (Offline, StudyDetail) {
    let detail = detail();
    let mut state = empty_state(&raw);
    state.responses.insert(
        format!("/api/v1/studies/{}", detail.study.id),
        json!(detail),
    );
    state
        .remote_conflicts
        .insert(detail.study.id, json!(detail));
    state.pending.push_back(operation(
        format!("/api/v1/studies/{}/protocol", detail.study.id),
        json!({"expected_version":1,"protocol":detail.protocol,"annotations":[]}),
    ));
    (offline_without_workers(raw, state).0, detail)
}
#[test]
fn review_resolve_rejects_denied_and_expired_without_any_state_change() {
    for mode in ["denied", "signed_expired", "monotonic_expired"] {
        let mut raw = connection();
        raw.offline_permissions = Some(permissions(true, mode != "denied"));
        let now = crate::one_c_auth::now();
        let instant = Instant::now();
        raw.offline_deadline = Some(match mode {
            "signed_expired" => (now - 1, instant + Duration::from_secs(60)),
            "monotonic_expired" => (now + 60, instant - Duration::from_secs(1)),
            _ => (now + 60, instant + Duration::from_secs(60)),
        });
        let (offline, detail) = conflict_fixture(raw);
        let before = state_snapshot(&offline);
        assert!(
            offline.resolve_protocol(&detail).is_err(),
            "{mode} allowed conflict resolution"
        );
        assert_eq!(
            state_snapshot(&offline),
            before,
            "{mode} changed local state"
        );
    }
}
#[test]
fn review_resolve_valid_grant_archives_frozen_request_and_queues_new_cas() {
    let mut raw = connection();
    raw.offline_permissions = Some(permissions(true, true));
    raw.offline_deadline = Some((
        crate::one_c_auth::now() + 60,
        Instant::now() + Duration::from_secs(60),
    ));
    let (offline, detail) = conflict_fixture(raw);
    let old = offline.state.lock().unwrap().pending[0].clone();
    offline.resolve_protocol(&detail).unwrap();
    let state = offline.state.lock().unwrap().clone();
    assert_eq!(
        serde_json::to_value(&state.conflict_history[0]).unwrap(),
        serde_json::to_value(&old).unwrap()
    );
    let new = &state.pending[0];
    assert_ne!(new.id, old.id);
    assert_eq!(new.body["expected_version"], 1);
    assert!(!new.started && new.blocked.is_none());
}
#[test]
fn review_revise_failed_applies_path_sensitive_permissions_and_expiry_before_changes() {
    for (path, create, edit, expired) in [
        ("/api/v1/patients", false, true, false),
        ("/api/v1/studies", false, true, false),
        ("/api/v1/templates", true, false, false),
        ("/api/v1/patients", true, true, true),
        ("/api/v1/templates", true, true, true),
    ] {
        let mut raw = connection();
        raw.offline_permissions = Some(permissions(create, edit));
        let now = crate::one_c_auth::now();
        raw.offline_deadline = Some((
            if expired { now - 1 } else { now + 60 },
            Instant::now() + Duration::from_secs(60),
        ));
        let mut state = empty_state(&raw);
        state
            .pending
            .push_back(operation(path.into(), json!({"original":"preserved"})));
        let (offline, _) = offline_without_workers(raw, state);
        let before = state_snapshot(&offline);
        assert!(
            offline.revise_failed(json!({"new":"data"})).is_err(),
            "policy bypass at {path}"
        );
        assert_eq!(
            state_snapshot(&offline),
            before,
            "state mutated on rejection at {path}"
        );
    }
}
#[test]
fn review_revise_failed_valid_policy_preserves_old_request_and_new_id() {
    let mut raw = connection();
    raw.offline_permissions = Some(permissions(true, true));
    raw.offline_deadline = Some((
        crate::one_c_auth::now() + 60,
        Instant::now() + Duration::from_secs(60),
    ));
    let mut state = empty_state(&raw);
    state
        .pending
        .push_back(operation("/api/v1/patients".into(), json!({"old":1})));
    let (offline, _) = offline_without_workers(raw, state);
    let old = offline.state.lock().unwrap().pending[0].clone();
    offline.revise_failed(json!({"new":2})).unwrap();
    let state = offline.state.lock().unwrap().clone();
    assert_eq!(
        serde_json::to_value(&state.conflict_history[0]).unwrap(),
        serde_json::to_value(&old).unwrap()
    );
    assert_ne!(state.pending[0].id, old.id);
    assert_eq!(state.pending[0].body, json!({"new":2}));
}
#[test]
fn review_derived_identical_bytes_keep_both_parent_relations() {
    let raw = connection();
    let detail = detail();
    let mut state = empty_state(&raw);
    state.responses.insert(
        format!("/api/v1/studies/{}", detail.study.id),
        json!(detail),
    );
    let (offline, _receiver) = offline_without_workers(raw, state);
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let first = offline
        .upload(detail.study.id, "derived", Some(a), vec![1, 2, 3])
        .unwrap();
    let repeated = offline
        .upload(detail.study.id, "derived", Some(a), vec![1, 2, 3])
        .unwrap();
    assert_eq!(
        first.id, repeated.id,
        "same known parent and bytes should reuse the existing upload"
    );
    let second = offline
        .upload(detail.study.id, "derived", Some(b), vec![1, 2, 3])
        .unwrap();
    assert_ne!(
        first.id, second.id,
        "distinct parent relations were deduplicated"
    );
    let state = offline.state.lock().unwrap().clone();
    let ops: Vec<_> = state
        .pending
        .iter()
        .filter(|o| o.path.contains("/assets?"))
        .collect();
    assert_eq!(ops.len(), 2);
    assert_eq!(ops[0].body["parent"], json!(a));
    assert_eq!(ops[1].body["parent"], json!(b));
    assert!(
        ops[0].path.contains(&format!("parent={a}"))
            && ops[1].path.contains(&format!("parent={b}"))
    );
}

// Reconstruct the reachable crash state: durable index says false, but the file
// was completely written before the process died. A blocked operation keeps
// network workers from attempting delivery while disk durability is inspected.
fn recovered_blob_case(
    file: Option<&[u8]>,
    expected_size: usize,
    expected_hash: String,
) -> (
    Result<Arc<Offline>, String>,
    Store,
    Operation,
    Uuid,
    std::net::TcpListener,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let raw = Connection::new(
        &format!("http://{}", listener.local_addr().unwrap()),
        "synthetic-test-key",
    )
    .unwrap();
    let mut state = empty_state(&raw);
    let id = Uuid::new_v4();
    state.blobs.insert(
        id,
        Blob {
            file_id: id,
            sha256: expected_hash.clone(),
            size: expected_size,
            persisted: false,
            backend_confirmed: false,
        },
    );
    let mut op = operation(
        format!(
            "/api/v1/studies/{}/assets?kind=original&sha256={expected_hash}",
            Uuid::new_v4()
        ),
        json!({"kind":"original","sha256":expected_hash,"parent":null}),
    );
    op.method = "POST".into();
    op.entity_id = id;
    op.blob = Some(id);
    state.pending.push_back(op.clone());
    let profile = state.profile.clone();
    let disk = store();
    disk.write("workspace", &serde_json::to_vec(&state).unwrap())
        .unwrap();
    if let Some(bytes) = file {
        disk.write(&id.simple().to_string(), bytes).unwrap();
    }
    let result = Offline::attach(raw, disk.clone(), profile);
    (result, disk, op, id, listener)
}
struct StopOnDrop(Arc<Offline>);
impl std::ops::Deref for StopOnDrop {
    type Target = Offline;
    fn deref(&self) -> &Offline {
        &self.0
    }
}
impl Drop for StopOnDrop {
    fn drop(&mut self) {
        // Disposable test-only in-memory cleanup lets the disk worker exit even
        // when this fixture deliberately contains an unrecoverable blob. No
        // sequence increment: the original encrypted index remains untouched.
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        for blob in state.blobs.values_mut() {
            blob.persisted = true;
        }
        state.durable_sequence = state.sequence;
        drop(state);
        pause_workers(&self.0);
    }
}
fn pause_workers(offline: &Offline) {
    offline
        .paused
        .store(true, std::sync::atomic::Ordering::SeqCst);
    offline
        .stopping
        .store(true, std::sync::atomic::Ordering::SeqCst);
}
#[test]
fn review_recovery_verifies_blob_before_durable_ready_and_keeps_frozen_request() {
    let bytes = b"synthetic image bytes";
    let (result, disk, op, id, _listener) =
        recovered_blob_case(Some(bytes), bytes.len(), digest(bytes));
    let offline = StopOnDrop(result.expect("valid recovered file must be accepted"));
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let s = offline.state.lock().unwrap().clone();
        if s.blobs[&id].persisted && s.durable_sequence >= s.sequence {
            break;
        }
        drop(s);
        assert!(
            Instant::now() < until,
            "valid blob was not recovered and persisted"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let s = offline.state.lock().unwrap().clone();
    assert_eq!(
        serde_json::to_value(&s.pending[0]).unwrap(),
        serde_json::to_value(&op).unwrap()
    );
    assert_eq!(s.pending[0].request_hash(), op.request_hash());
    let mut ready = s.clone();
    ready.pending[0].blocked = None;
    assert!(crate::outbox::next_index(&ready, &BTreeSet::new()).is_some());
    drop(s);
    let durable: LocalState =
        serde_json::from_slice(&disk.read("workspace").unwrap().unwrap()).unwrap();
    assert!(
        durable.blobs[&id].persisted,
        "eligibility must follow persisted recovery index"
    );
    assert_eq!(
        serde_json::to_value(&durable.pending[0]).unwrap(),
        serde_json::to_value(&op).unwrap()
    );
    assert_eq!(
        disk.read(&id.simple().to_string()).unwrap().unwrap(),
        bytes.to_vec()
    );
    pause_workers(&offline);
}
#[test]
fn review_recovery_missing_or_damaged_blob_never_marks_ready_or_discards_input() {
    let expected = b"synthetic image bytes";
    for (label, file, size, sha) in [
        ("missing", None, expected.len(), digest(expected)),
        (
            "wrong_size",
            Some(&expected[..]),
            expected.len() + 1,
            digest(expected),
        ),
        (
            "wrong_sha",
            Some(&expected[..]),
            expected.len(),
            digest(b"different bytes"),
        ),
    ] {
        let (result, disk, op, id, _listener) = recovered_blob_case(file, size, sha);
        match result {
            Err(error) => assert!(!error.is_empty(), "{label} needs an actionable error"),
            Ok(offline) => {
                let offline = StopOnDrop(offline);
                let s = offline.state.lock().unwrap().clone();
                assert!(!s.blobs[&id].persisted, "{label} was treated as verified");
                assert_eq!(
                    serde_json::to_value(&s.pending[0]).unwrap(),
                    serde_json::to_value(&op).unwrap()
                );
                let mut candidate = s.clone();
                candidate.pending[0].blocked = None;
                assert!(crate::outbox::next_index(&candidate, &BTreeSet::new()).is_none());
                assert!(
                    !s.status_error().is_empty(),
                    "{label} must have an explicit recovery error"
                );
                drop(s);
                pause_workers(&offline);
            }
        }
        let stored: LocalState =
            serde_json::from_slice(&disk.read("workspace").unwrap().unwrap()).unwrap();
        assert!(!stored.blobs[&id].persisted);
        assert_eq!(
            serde_json::to_value(&stored.pending[0]).unwrap(),
            serde_json::to_value(&op).unwrap()
        );
        assert_eq!(
            disk.read(&id.simple().to_string()).unwrap().as_deref(),
            file
        );
    }
}

#[test]
fn review_recovery_rejects_truncated_encrypted_blob_without_overwriting_it() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let raw = Connection::new(
        &format!("http://{}", listener.local_addr().unwrap()),
        "synthetic-test-key",
    )
    .unwrap();
    let mut state = empty_state(&raw);
    let id = Uuid::new_v4();
    let bytes = b"synthetic";
    state.blobs.insert(
        id,
        Blob {
            file_id: id,
            sha256: digest(bytes),
            size: bytes.len(),
            persisted: false,
            backend_confirmed: false,
        },
    );
    let mut op = operation(
        format!("/api/v1/studies/{}/assets?kind=original", Uuid::new_v4()),
        json!({"sha256":digest(bytes)}),
    );
    op.entity_id = id;
    op.blob = Some(id);
    state.pending.push_back(op.clone());
    let disk = store();
    disk.write("workspace", &serde_json::to_vec(&state).unwrap())
        .unwrap();
    let path = disk.root().join(format!("{}.dpapi", id.simple()));
    let damaged = b"truncated";
    std::fs::write(&path, damaged).unwrap();
    let attached = Offline::attach(raw, disk.clone(), state.profile.clone());
    if let Ok(offline) = attached {
        let offline = StopOnDrop(offline);
        let s = offline.state.lock().unwrap().clone();
        assert!(!s.blobs[&id].persisted);
        assert!(
            !s.status_error().is_empty(),
            "truncated ciphertext requires explicit diagnostic"
        );
        assert_eq!(
            serde_json::to_value(&s.pending[0]).unwrap(),
            serde_json::to_value(&op).unwrap()
        );
        drop(s);
        pause_workers(&offline);
    }
    let after: LocalState =
        serde_json::from_slice(&disk.read("workspace").unwrap().unwrap()).unwrap();
    assert!(!after.blobs[&id].persisted);
    assert_eq!(
        serde_json::to_value(&after.pending[0]).unwrap(),
        serde_json::to_value(&op).unwrap()
    );
    assert_eq!(std::fs::read(path).unwrap(), damaged.to_vec());
}

struct ToggleProtector {
    inner: AesProtector,
    fail_writes: Arc<std::sync::atomic::AtomicBool>,
}
impl crate::cache::Protector for ToggleProtector {
    fn protect(&self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        if self.fail_writes.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("synthetic disk encryption failure".into());
        }
        crate::cache::Protector::protect(&self.inner, bytes)
    }
    fn unprotect(&self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        crate::cache::Protector::unprotect(&self.inner, bytes)
    }
}
#[test]
fn review_recovery_index_write_failure_keeps_index_and_file_and_starts_no_workers() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let raw = Connection::new(
        &format!("http://{}", listener.local_addr().unwrap()),
        "synthetic-test-key",
    )
    .unwrap();
    let mut state = empty_state(&raw);
    let id = Uuid::new_v4();
    let bytes = b"synthetic image";
    state.blobs.insert(
        id,
        Blob {
            file_id: id,
            sha256: digest(bytes),
            size: bytes.len(),
            persisted: false,
            backend_confirmed: false,
        },
    );
    let mut op = operation(
        format!("/api/v1/studies/{}/assets?kind=original", Uuid::new_v4()),
        json!({"sha256":digest(bytes)}),
    );
    op.method = "POST".into();
    op.entity_id = id;
    op.blob = Some(id);
    state.pending.push_back(op.clone());
    let fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let disk = Store::new(
        std::env::temp_dir().join(format!("m53-review-failed-write-{}", Uuid::new_v4())),
        Arc::new(ToggleProtector {
            inner: AesProtector([74; 32]),
            fail_writes: fail.clone(),
        }),
    )
    .unwrap();
    let original = serde_json::to_vec(&state).unwrap();
    disk.write("workspace", &original).unwrap();
    disk.write(&id.simple().to_string(), bytes).unwrap();
    fail.store(true, std::sync::atomic::Ordering::SeqCst);
    let result = Offline::attach(raw, disk.clone(), state.profile.clone());
    assert!(
        result.is_err(),
        "recovery must fail closed if index cannot be persisted"
    );
    assert_eq!(disk.read("workspace").unwrap().unwrap(), original);
    assert_eq!(
        disk.read(&id.simple().to_string()).unwrap().unwrap(),
        bytes.to_vec()
    );
    assert!(
        listener.accept().is_err(),
        "no network worker may start before recovery index is durable"
    );
}
#[test]
fn review_recovery_second_attach_is_idempotent_and_preserves_frozen_operation() {
    let bytes = b"synthetic restart image";
    let (result, disk, op, id, _listener) =
        recovered_blob_case(Some(bytes), bytes.len(), digest(bytes));
    let first = StopOnDrop(result.expect("first recovery"));
    let persisted: LocalState =
        serde_json::from_slice(&disk.read("workspace").unwrap().unwrap()).unwrap();
    assert!(persisted.blobs[&id].persisted);
    let raw = first.transport.lock().unwrap().clone();
    let until = Instant::now() + Duration::from_secs(12);
    loop {
        if first.shutdown().is_ok() {
            break;
        }
        assert!(Instant::now() < until, "first session did not shut down");
        thread::sleep(Duration::from_millis(20));
    }
    let second = StopOnDrop(
        Offline::attach(raw, disk.clone(), persisted.profile.clone()).expect("second attach"),
    );
    let (same, was_persisted) = {
        let state = second.state.lock().unwrap();
        (
            serde_json::to_value(&state.pending[0]).unwrap() == serde_json::to_value(&op).unwrap(),
            state.blobs[&id].persisted,
        )
    };
    pause_workers(&first);
    pause_workers(&second);
    assert!(
        same && was_persisted,
        "second attach changed the frozen operation or lost recovery"
    );
}
#[test]
fn review_resolve_missing_protocol_fails_before_archiving_queue() {
    let mut raw = connection();
    raw.offline_permissions = Some(permissions(true, true));
    let (offline, mut local) = conflict_fixture(raw);
    local.protocol = None;
    let before = state_snapshot(&offline);
    let result = offline.resolve_protocol(&local);
    let after = state_snapshot(&offline);
    assert!(result.is_err());
    assert_eq!(
        after, before,
        "failed validation must not mutate pending or history"
    );
}

#[test]
fn review_derived_does_not_reuse_cached_asset_without_known_parent() {
    let raw = connection();
    let mut detail = detail();
    let bytes = vec![4, 5, 6];
    let cached = Uuid::new_v4();
    detail.assets.push(Asset {
        id: cached,
        study_id: detail.study.id,
        kind: "derived".into(),
        sha256: digest(&bytes),
        byte_size: bytes.len() as i64,
        mime_type: "application/x-m53-rgb8".into(),
    });
    let mut state = empty_state(&raw);
    state.responses.insert(
        format!("/api/v1/studies/{}", detail.study.id),
        json!(detail),
    );
    let (offline, _receiver) = offline_without_workers(raw, state);
    let parent = Uuid::new_v4();
    let result = offline
        .upload(detail.study.id, "derived", Some(parent), bytes)
        .unwrap();
    assert_ne!(
        result.id, cached,
        "cached DTO has no authoritative parent relation"
    );
    let pending = offline.state.lock().unwrap().pending.clone();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].body["parent"], json!(parent));
}

#[test]
fn review_recovery_error_survives_transient_sync_error_change_and_clear() {
    let bytes = b"synthetic missing file";
    let (result, _disk, _op, _id, _listener) =
        recovered_blob_case(None, bytes.len(), digest(bytes));
    let offline = StopOnDrop(result.expect("missing blob should remain available for diagnosis"));
    let recovery = offline.information().3;
    assert!(
        !recovery.is_empty(),
        "missing file must have a recovery diagnostic"
    );
    assert!(offline.state_token().3.contains(&recovery));
    for transient in ["synthetic temporary network failure", ""] {
        offline.state.lock().unwrap().error = transient.into();
        let information = offline.information().3;
        let token = offline.state_token().3;
        assert!(
            information.contains(&recovery),
            "recovery diagnostic lost after transient change: {transient:?}"
        );
        assert!(
            token.contains(&recovery),
            "publisher token lost recovery diagnostic: {transient:?}"
        );
        if !transient.is_empty() {
            assert!(information.contains(transient) && token.contains(transient));
        }
    }
}

#[test]
fn review_recovery_diagnostic_clears_after_verified_blob_is_written() {
    let bytes = b"synthetic missing file";
    let (result, _disk, _op, id, _listener) = recovered_blob_case(None, bytes.len(), digest(bytes));
    let offline = StopOnDrop(result.expect("missing blob should remain available for diagnosis"));
    assert!(!offline.information().3.is_empty());

    offline
        .disk
        .send(DiskJob::Blob(id, Arc::new(bytes.to_vec())))
        .unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        let (persisted, error) = {
            let state = offline.state.lock().unwrap();
            (state.blobs[&id].persisted, state.status_error())
        };
        if persisted {
            assert!(
                error.is_empty(),
                "repaired blob must not retain obsolete recovery diagnostic"
            );
            break;
        }
        assert!(Instant::now() < until, "synthetic blob was not written");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn recovery_checkpoint_can_close_and_restart_without_losing_missing_blob_or_queue() {
    let bytes=b"synthetic lost original";
    let (result,disk,op,id,listener)=recovered_blob_case(None,bytes.len(),digest(bytes));
    // This test inspects disk recovery, not an unanswered synthetic HTTP request.
    drop(listener);
    let offline=StopOnDrop(result.unwrap());
    assert!(!offline.information().2,"a missing original must never be advertised as saved");
    assert!(offline.checkpoint_ready(),"a known missing file must not block unrelated navigation forever");
    let until=Instant::now()+Duration::from_secs(3);
    while offline.shutdown().is_err(){assert!(Instant::now()<until,"shutdown must drain without discarding the missing file");thread::sleep(Duration::from_millis(10));}
    let persisted:LocalState=serde_json::from_slice(&disk.read("workspace").unwrap().unwrap()).unwrap();
    assert!(!persisted.blobs[&id].persisted);
    assert_eq!(serde_json::to_value(&persisted.pending[0]).unwrap(),serde_json::to_value(&op).unwrap());
    assert_eq!(persisted.journal["synthetic"],"preserve");
    let fresh_store=Store::new(disk.root().to_path_buf(),Arc::new(AesProtector([73;32]))).unwrap();
    let raw=offline.transport.lock().unwrap().clone();
    let restored=StopOnDrop(Offline::attach(raw,fresh_store,persisted.profile).unwrap());
    assert!(!restored.information().2);
    assert!(!restored.information().3.is_empty());
    assert!(restored.checkpoint_ready());
}

#[test]
fn recovery_blocks_only_affected_draft_and_new_writes_keep_the_close_barrier() {
    let raw=connection();let mut state=empty_state(&raw);let mut affected=detail();let other=detail();
    let id=Uuid::new_v4();
    affected.assets.push(Asset{id,study_id:affected.study.id,kind:"original".into(),sha256:digest(b"lost"),byte_size:4,mime_type:"image/jpeg".into()});
    state.responses.insert(format!("/api/v1/studies/{}",affected.study.id),json!(affected));
    state.responses.insert(format!("/api/v1/studies/{}",other.study.id),json!(other));
    state.blobs.insert(id,Blob{file_id:id,sha256:digest(b"lost"),size:4,persisted:false,backend_confirmed:false});
    state.recovery_errors.insert(id);state.durable_sequence=state.sequence;
    assert!(!state.study_durable(affected.study.id),"lost original referenced by the editor blocks its approval even without a pending upload");
    assert!(state.study_durable(other.study.id));
    let mut missing=operation(format!("/api/v1/studies/{}/assets",affected.study.id),json!({"sha256":digest(b"lost")}));
    missing.blob=Some(id);missing.blocked=None;
    let mut independent=operation(format!("/api/v1/studies/{}/protocol",other.study.id),json!({}));independent.blocked=None;
    state.pending.extend([missing.clone(),independent]);
    assert_eq!(crate::outbox::next_index(&state,&BTreeSet::new()),Some(1));
    assert_eq!(state.pending[0].request_hash(),missing.request_hash());
    let (offline,_)=offline_without_workers(raw,state);
    assert!(offline.checkpoint_ready());
    let fresh=Uuid::new_v4();
    offline.state.lock().unwrap().blobs.insert(fresh,Blob{file_id:fresh,sha256:digest(b"new"),size:3,persisted:false,backend_confirmed:false});
    assert!(!offline.checkpoint_ready(),"an active unpersisted write still blocks close");
}

#[test]
fn recovery_allows_unrelated_open_and_close_while_preserving_dirty_input() {
    let raw=connection();let mut state=empty_state(&raw);let affected=detail();let other=detail();
    let lost=Uuid::new_v4();
    state.blobs.insert(lost,Blob{file_id:lost,sha256:digest(b"lost"),size:4,persisted:false,backend_confirmed:false});
    state.recovery_errors.insert(lost);
    let dictionary=json!({"version_id":other.protocol.as_ref().unwrap().terminology_version_id,"clinically_approved":true,"terms":[]});
    let dirty=json!({"detail":affected,"dirty":true,"unfinished_fields":{"kept":"synthetic input"}});
    state.editors.insert(affected.study.id,dirty.clone());
    state.journal=dirty.clone();
    state.responses.insert(format!("/api/v1/studies/{}",other.study.id),json!(other));
    state.responses.insert(format!("/api/v1/terminology?version_id={}",other.protocol.as_ref().unwrap().terminology_version_id),dictionary);
    let (offline,receiver)=offline_without_workers(raw.clone(),state);let offline=Arc::new(offline);offline.start_disk(receiver);
    let mut connection=raw;connection.offline=Some(offline.clone());
    let mut workspace=crate::workspace::Workspace::default();workspace.attach(connection);
    let deadline=Instant::now()+Duration::from_secs(3);
    loop {match workspace.open(other.study.id,None) {
        Ok(())=>break,
        Err(error) if crate::workspace::opening_pending(&error) && Instant::now()<deadline=>thread::sleep(Duration::from_millis(10)),
        Err(error)=>panic!("unrelated open blocked: {error}"),
    }}
    assert!(!offline.information().2);assert!(offline.study_file_error(other.study.id).is_none());
    while !offline.checkpoint_ready(){assert!(Instant::now()<deadline);thread::sleep(Duration::from_millis(10));}
    let snapshot=workspace.snapshot();
    assert!(snapshot["sync"]["server_status"].as_str().unwrap().starts_with("Сохранено на сервере"),"another study's missing file must not mislabel this confirmed copy");
    assert!(snapshot["sync"]["server_status"].as_str().unwrap().contains("Не переданы локальные черновики: 1"));
    assert_eq!(snapshot["sync"]["durable"],false);
    assert_eq!(snapshot["sync"]["checkpoint_ready"],true);
    loop {match workspace.close_study() {
        Ok(())=>break,
        Err(_) if Instant::now()<deadline=>thread::sleep(Duration::from_millis(10)),
        Err(error)=>panic!("close blocked: {error}"),
    }}
    let editor=offline.editor(affected.study.id).unwrap();
    assert_eq!(editor["unfinished_fields"],dirty["unfinished_fields"]);
    assert_eq!(editor["detail"],dirty["detail"]);
    assert!(editor["dirty"]==true);
    while offline.shutdown().is_err(){assert!(Instant::now()<deadline);thread::sleep(Duration::from_millis(10));}
}

#[test]
fn recovery_download_reenters_the_active_write_barrier_after_hash_verification() {
    use std::io::{Read,Write};
    let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let raw=Connection::new(&format!("http://{}",listener.local_addr().unwrap()),"synthetic-key").unwrap();
    let mut state=empty_state(&raw);let id=Uuid::new_v4();let bytes=b"synthetic verified original";
    state.responses.insert("synthetic-detail".into(),json!({"assets":[{"id":id,"sha256":digest(bytes)}]}));
    state.blobs.insert(id,Blob{file_id:id,sha256:digest(bytes),size:bytes.len(),persisted:false,backend_confirmed:true});
    state.recovery_errors.insert(id);state.durable_sequence=state.sequence;
    let (offline,_receiver)=offline_without_workers(raw.clone(),state);
    assert!(offline.checkpoint_ready());
    let server=thread::spawn(move||{let(mut stream,_)=listener.accept().unwrap();let mut header=Vec::new();while !header.ends_with(b"\r\n\r\n"){let mut b=[0];stream.read_exact(&mut b).unwrap();header.push(b[0]);}write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len()).unwrap();stream.write_all(bytes).unwrap();});
    offline.fetch_one(&raw,&format!("/api/v1/assets/{id}")).unwrap();server.join().unwrap();
    let mut state=offline.state.lock().unwrap();
    assert!(!state.recovery_errors.contains(&id));
    assert!(!state.blobs[&id].persisted);
    // Even a subsequent durable index cannot stand in for the still queued blob write.
    state.durable_sequence=state.sequence;drop(state);
    assert!(!offline.checkpoint_ready());
}
