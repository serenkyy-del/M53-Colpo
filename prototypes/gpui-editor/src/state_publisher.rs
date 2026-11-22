//! Build full UI projections only when persisted data or visible status changes.
use m53_client::{activity::Journal, workspace::Workspace};
use serde_json::Value;
use uuid::Uuid;

#[derive(PartialEq)]
struct Revision {
    actor: Option<Uuid>,
    data: (u64, u64, bool, String),
    study: Option<(Uuid, i64, Option<Uuid>)>,
    dirty: bool,
    message: String,
    audit: Value,
    diagnoses: u64,
}

#[derive(Default)]
pub struct StatePublisher {
    last: Option<Revision>,
}
impl StatePublisher {
    pub fn invalidate(&mut self) {
        self.last = None;
    }
    pub fn changed(&mut self, workspace: &Workspace, audit: &Journal) -> bool {
        let diagnoses = workspace
            .connection
            .as_ref()
            .and_then(|c| c.offline.as_ref())
            .map(|offline| offline.diagnoses_snapshot().revision)
            .unwrap_or_default();
        self.changed_with_diagnoses_revision(workspace, audit, diagnoses)
    }
    fn changed_with_diagnoses_revision(
        &mut self,
        workspace: &Workspace,
        audit: &Journal,
        diagnoses: u64,
    ) -> bool {
        let revision = Revision {
            diagnoses,
            actor: workspace.connection.as_ref().and_then(|c| c.user_id),
            data: workspace
                .connection
                .as_ref()
                .and_then(|c| c.offline.as_ref())
                .map(|o| o.state_token())
                .unwrap_or_default(),
            study: workspace
                .detail
                .as_ref()
                .map(|d| (d.study.id, d.study.row_version, d.revision_id)),
            dirty: workspace.dirty,
            message: workspace.message.clone(),
            audit: serde_json::to_value(audit.snapshot()).unwrap_or(Value::Null),
        };
        if self.last.as_ref() == Some(&revision) {
            return false;
        }
        if self.last.as_ref().is_none_or(|previous| previous.actor!=revision.actor || previous.study!=revision.study) {
            if let (Some(detail),Some(offline))=(workspace.detail.as_ref(),workspace.connection.as_ref().and_then(|c|c.offline.as_ref())) {
                let query=detail.revision_id.map(|id|format!("?revision_id={id}")).unwrap_or_default();
                offline.fetch(&format!("/api/v1/studies/{}/report-context{query}",detail.study.id));
            }
        }
        self.last = Some(revision);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalogue_revision_publishes_without_clinical_state_changes() {
        let mut publisher = StatePublisher::default();
        let workspace = Workspace::default();
        let journal = Journal::start();
        assert!(publisher.changed_with_diagnoses_revision(&workspace, &journal, 0));
        assert!(!publisher.changed_with_diagnoses_revision(&workspace, &journal, 0));
        // Background loading, completion and error each change only this revision.
        for revision in 1..=3 {
            assert!(publisher.changed_with_diagnoses_revision(&workspace, &journal, revision));
            assert!(!publisher.changed_with_diagnoses_revision(&workspace, &journal, revision));
        }
    }
    #[test]
    fn identical_idle_status_does_not_rebuild_full_state() {
        let mut publisher = StatePublisher::default();
        let mut workspace = Workspace::default();
        let journal = Journal::start();
        assert!(publisher.changed(&workspace, &journal));
        assert!(!publisher.changed(&workspace, &journal));
        publisher.invalidate();
        assert!(publisher.changed(&workspace, &journal));
        assert!(!publisher.changed(&workspace, &journal));
        workspace.message = "Сохранение…".into();
        assert!(publisher.changed(&workspace, &journal));
        assert!(!publisher.changed(&workspace, &journal));
        workspace.dirty = true;
        assert!(publisher.changed(&workspace, &journal));
    }
}
