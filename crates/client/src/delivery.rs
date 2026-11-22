//! Delivery acknowledgement belongs to exact immutable report bytes and revision.
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub fn require_mis_task(detail: &m53_contracts::workflow::StudyDetail) -> Result<(), String> {
    if detail.mis_task_bound { Ok(()) }
    else { Err("Отправка в МИС доступна только для обследования по заданию из МИС. Связь с заданием не подтверждена.".into()) }
}

pub fn pdf_hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
pub fn matching<'a>(
    jobs: &'a Value,
    revision: Uuid,
    template: Uuid,
    hash: &str,
) -> Option<&'a Value> {
    jobs.as_array()?
        .iter()
        .filter(|job| {
            job["revision_id"]
                .as_str()
                .and_then(|v| Uuid::parse_str(v).ok())
                == Some(revision)
                && job["template_version_id"]
                    .as_str()
                    .and_then(|v| Uuid::parse_str(v).ok())
                    == Some(template)
                && job["pdf_sha256"].as_str() == Some(hash)
        })
        .max_by_key(|job| (delivered(job), job["sequence"].as_u64().unwrap_or(0)))
}
pub fn delivered(job: &Value) -> bool {
    job["delivered"] == true && job["state"] == "sent"
}
pub fn prevents_resend(job: &Value) -> bool {
    delivered(job) || matches!(job["state"].as_str(), Some("pending" | "retry"))
}
pub fn status(job: &Value) -> &'static str {
    if delivered(job) {
        "Доставлен в МИС"
    } else {
        match job["state"].as_str() {
            Some("pending") => "М53 принял PDF · ожидает отправки в МИС",
            Some("retry") => "Повторная отправка в МИС",
            Some("blocked") => "Не доставлен в МИС",
            _ => "Доставка в МИС не подтверждена",
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn receipt_alone_is_not_delivery() {
        assert!(!delivered(
            &json!({"state":"sent","receipt":{"success":true}})
        ));
        assert!(!delivered(&json!({"state":"blocked","delivered":true})));
        assert!(delivered(&json!({"state":"sent","delivered":true})));
        assert!(prevents_resend(&json!({"state":"pending"})));
    }
    #[test]
    fn exact_version_and_bytes_only() {
        let r = Uuid::new_v4();
        let t = Uuid::new_v4();
        let jobs = json!([{"revision_id":r,"template_version_id":t,"pdf_sha256":"a","state":"sent","delivered":true}]);
        assert!(matching(&jobs, r, t, "a").is_some());
        assert!(matching(&jobs, Uuid::new_v4(), t, "a").is_none());
        assert!(matching(&jobs, r, Uuid::new_v4(), "a").is_none());
        assert!(matching(&jobs, r, t, "b").is_none());
    }
}
