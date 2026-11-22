//! Independent study queues with explicit creation/reference dependencies.
//! No operation is discarded, rewritten after start, or acknowledged by queue position.
use crate::offline::{LocalState, Operation};
use serde_json::Value;
use std::collections::BTreeSet;
use uuid::Uuid;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lane {
    Study(Uuid),
    Patient(Uuid),
    Template(Uuid),
    Global,
}
fn study(path: &str) -> Option<Uuid> {
    path.strip_prefix("/api/v1/studies/")?
        .split('/')
        .next()?
        .parse()
        .ok()
}
fn lane(op: &Operation) -> Lane {
    let scope = op.scope();
    if let Some(id) = study(scope) {
        Lane::Study(id)
    } else {
        match scope {
            "/api/v1/studies" => Lane::Study(op.entity_id),
            "/api/v1/patients" => Lane::Patient(op.entity_id),
            "/api/v1/templates" => Lane::Template(op.entity_id),
            _ => Lane::Global,
        }
    }
}
fn references(op: &Operation) -> BTreeSet<Uuid> {
    fn visit(v: &Value, out: &mut BTreeSet<Uuid>) {
        match v {
            Value::String(s) => {
                if let Ok(id) = s.parse() {
                    out.insert(id);
                }
            }
            Value::Object(m) => m.values().for_each(|v| visit(v, out)),
            Value::Array(a) => a.iter().for_each(|v| visit(v, out)),
            _ => {}
        }
    }
    let mut ids = BTreeSet::new();
    visit(&op.body, &mut ids);
    for part in op.path.split(['/', '?', '&', '=']) {
        if let Ok(id) = part.parse() {
            ids.insert(id);
        }
    }
    ids
}
fn depends(previous: &Operation, next: &Operation, next_refs: &BTreeSet<Uuid>) -> bool {
    let a = lane(previous);
    let b = lane(next);
    a == Lane::Global
        || b == Lane::Global
        || a == b
        || next_refs.contains(&previous.entity_id)
        || previous
            .version_id
            .is_some_and(|id| next_refs.contains(&id))
}
/// Earliest ready lane head. Earlier blocked/deferred operations only hold their dependents.
pub fn next_index(state: &LocalState, deferred: &BTreeSet<Uuid>) -> Option<usize> {
    state.pending.iter().enumerate().find_map(|(i, op)| {
        if op.blocked.is_some()
            || state.operation_has_missing_file(op)
            || (op.path.ends_with("/approve") && study(&op.path).is_some_and(|id| state.study_has_missing_file(id)))
            || matches!(lane(op), Lane::Study(id) if state.has_local_revision_conflict(id))
            || deferred.contains(&op.id)
            || op
                .blob
                .is_some_and(|id| !state.blobs.get(&id).is_some_and(|b| b.persisted))
        {
            return None;
        }
        let refs = references(op);
        (!state.pending.iter().take(i).any(|p| depends(p, op, &refs))).then_some(i)
    })
}
/// Operations whose outcome this study needs, including local patient/study/template creation.
pub fn study_indices(state: &LocalState, id: Uuid) -> Vec<usize> {
    let patient = state
        .responses
        .get(&format!("/api/v1/studies/{id}"))
        .and_then(|v| v.pointer("/study/patient_id"))
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<Uuid>().ok());
    let mut selected: Vec<bool> = state
        .pending
        .iter()
        .map(|op| {
            lane(op) == Lane::Study(id) || lane(op) == Lane::Global || patient == Some(op.entity_id)
        })
        .collect();
    for i in (0..selected.len()).rev() {
        if selected[i] {
            let refs = references(&state.pending[i]);
            for j in 0..i {
                if depends(&state.pending[j], &state.pending[i], &refs) {
                    selected[j] = true;
                }
            }
        }
    }
    selected
        .into_iter()
        .enumerate()
        .filter_map(|(i, yes)| yes.then_some(i))
        .collect()
}
impl LocalState {
    pub fn study_pending_count(&self, id: Uuid) -> usize {
        study_indices(self, id).len()
    }
    pub fn study_blocked(&self, id: Uuid) -> Option<&Operation> {
        study_indices(self, id)
            .into_iter()
            .map(|i| &self.pending[i])
            .find(|op| op.blocked.is_some())
    }
    pub fn study_durable(&self, id: Uuid) -> bool {
        self.durable_sequence >= self.sequence
            && !self.study_has_missing_file(id)
            && study_indices(self, id).into_iter().all(|i| {
                self.pending[i]
                    .blob
                    .is_none_or(|id| self.blobs.get(&id).is_some_and(|b| b.persisted))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn operation(study: Uuid, suffix: &str) -> Operation {
        Operation {
            id: Uuid::new_v4(),
            entity_id: Uuid::new_v4(),
            version_id: None,
            path: format!("/api/v1/studies/{study}/{suffix}"),
            method: "PUT".into(),
            body: json!({"expected_version":1}),
            blob: None,
            started: false,
            blocked: None,
            sequence: 1,
        }
    }
    fn state(ops: Vec<Operation>) -> LocalState {
        let mut state:LocalState=serde_json::from_value(json!({"format_version":1,"profile":{"server":"http://127.0.0.1","user_id":Uuid::new_v4(),"doctor":"Synthetic"},"responses":{},"blobs":{},"pending":ops,"confirmed":{},"journal":{},"editors":{},"remote_conflicts":{},"conflict_history":[],"sequence":1})).unwrap();
        state.durable_sequence = state.sequence;
        state
    }
    #[test]
    fn diverged_local_editor_blocks_only_its_own_outbox_lane() {
        let study=Uuid::new_v4();
        let other=Uuid::new_v4();
        let blocked=operation(study,"protocol");
        let independent=operation(other,"protocol");
        let mut s=state(vec![blocked,independent]);
        s.editors.insert(study,json!({"revision_conflict":true}));
        assert_eq!(next_index(&s,&BTreeSet::new()),Some(1));
        s.editors.get_mut(&study).unwrap()["revision_conflict"]=json!(false);
        assert_eq!(next_index(&s,&BTreeSet::new()),Some(0));
    }

    #[test]
    fn rejected_other_study_is_retained_while_current_advances_in_order() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut first = operation(a, "protocol");
        first.started = true;
        first.blocked = Some("protocol_validation_failed".into());
        let mut state = state(vec![
            first,
            operation(a, "assets"),
            operation(b, "protocol"),
            operation(b, "assets"),
            operation(b, "approve"),
        ]);
        let original = serde_json::to_value(&state.pending[0]).unwrap();
        assert_eq!(state.study_pending_count(b), 3);
        assert!(state.study_blocked(b).is_none());
        assert!(state.study_blocked(a).is_some());
        for _ in 0..3 {
            let index = next_index(&state, &BTreeSet::new()).unwrap();
            assert_eq!(index, 2);
            state.pending.remove(index);
        }
        assert_eq!(next_index(&state, &BTreeSet::new()), None);
        assert_eq!(state.pending.len(), 2);
        assert_eq!(serde_json::to_value(&state.pending[0]).unwrap(), original);
    }
    #[test]
    fn deferred_transport_failure_yields_only_to_independent_studies() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let first = operation(a, "protocol");
        let deferred = BTreeSet::from([first.id]);
        let state = state(vec![
            first,
            operation(a, "approve"),
            operation(b, "protocol"),
        ]);
        assert_eq!(next_index(&state, &deferred), Some(2));
        assert_eq!(next_index(&state, &BTreeSet::new()), Some(0));
    }
    #[test]
    fn local_patient_and_study_creation_remain_dependencies() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let patient = Uuid::new_v4();
        let mut create_patient = operation(a, "unused");
        create_patient.path = "/api/v1/patients".into();
        create_patient.entity_id = patient;
        create_patient.blocked = Some("validation".into());
        let mut create_study = operation(b, "unused");
        create_study.path = "/api/v1/studies".into();
        create_study.entity_id = b;
        create_study.body = json!({"patient_id":patient});
        let state = state(vec![
            create_patient,
            create_study,
            operation(b, "protocol"),
            operation(a, "protocol"),
        ]);
        assert_eq!(state.study_pending_count(b), 3);
        assert!(state.study_blocked(b).is_some());
        assert_eq!(next_index(&state, &BTreeSet::new()), Some(3));
    }
    #[test]
    fn shared_patient_cached_reference_and_template_version_hold_dependent_study() {
        let b = Uuid::new_v4();
        let patient = Uuid::new_v4();
        let version = Uuid::new_v4();
        let mut create_patient = operation(b, "unused");
        create_patient.path = "/api/v1/patients".into();
        create_patient.entity_id = patient;
        create_patient.blocked = Some("validation".into());
        let mut template = operation(b, "unused");
        template.path = "/api/v1/templates".into();
        template.version_id = Some(version);
        template.blocked = Some("validation".into());
        let mut pdf = operation(b, "results/1c/pdf");
        pdf.path = format!("/api/v1/studies/{b}/results/1c/pdf?template_version_id={version}");
        let mut state = state(vec![create_patient, template, pdf]);
        state.responses.insert(
            format!("/api/v1/studies/{b}"),
            json!({"study":{"patient_id":patient}}),
        );
        assert_eq!(state.study_pending_count(b), 3);
        assert_eq!(next_index(&state, &BTreeSet::new()), None);
    }
    #[test]
    fn unknown_global_operation_is_a_conservative_barrier() {
        let b = Uuid::new_v4();
        let mut global = operation(b, "unused");
        global.path = "/api/v1/future-mutation".into();
        global.blocked = Some("validation".into());
        let state = state(vec![global, operation(b, "protocol")]);
        assert_eq!(next_index(&state, &BTreeSet::new()), None);
        assert_eq!(state.study_pending_count(b), 2);
    }
    #[test]
    fn missing_other_blob_does_not_hold_current_but_own_blob_does() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut missing = operation(a, "assets");
        missing.blob = Some(Uuid::new_v4());
        let mut state = state(vec![
            missing,
            operation(a, "protocol"),
            operation(b, "protocol"),
        ]);
        assert!(!state.study_durable(a));
        assert!(state.study_durable(b));
        assert_eq!(next_index(&state, &BTreeSet::new()), Some(2));
        state.durable_sequence = 0;
        assert!(!state.study_durable(b));
    }
    #[test]
    fn body_references_hold_unstarted_asset_dependents_across_studies() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut asset = operation(a, "assets");
        asset.blocked = Some("upload_validation".into());
        let mut protocol = operation(b, "protocol");
        protocol.body = json!({"annotations":[{"asset_id":asset.entity_id}]});
        let state = state(vec![asset, protocol]);
        assert!(state.study_blocked(b).is_some());
        assert_eq!(next_index(&state, &BTreeSet::new()), None);
    }
}
