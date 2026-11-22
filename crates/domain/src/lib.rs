//! Clinical documentation states; no diagnostic inference or IO.
use serde::{Deserialize, Serialize};
pub mod approval_fields;
pub mod colpo;
pub mod protocol;
pub mod validation;
pub mod visit_template;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExaminationState {
    Examined,
    NotExamined,
    NotApplicable,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeatureState {
    Present,
    Absent,
    NotAssessed,
    NotApplicable,
}
/// Optimistic concurrency: stale writes must be rejected.
pub fn next_version(current: u64, expected: u64) -> Option<u64> {
    (current == expected)
        .then(|| current.checked_add(1))
        .flatten()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_stale_and_overflowing_versions() {
        assert_eq!(next_version(3, 3), Some(4));
        assert_eq!(next_version(3, 2), None);
        assert_eq!(next_version(u64::MAX, u64::MAX), None);
    }
}
