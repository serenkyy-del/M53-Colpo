//! Presentation of delivery receipts. Clinical send/approval/idempotency guards stay in m53-client.
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub approved: bool,
    pub revision_id: Option<Uuid>,
    pub template_version_id: Option<Uuid>,
    pub preview_identity: Option<&'a (Uuid, Uuid, String)>,
}
impl<'a> Context<'a> {
    fn exact_identity(self) -> Option<&'a (Uuid, Uuid, String)> {
        let identity = self.preview_identity?;
        (self.approved
            && !identity.0.is_nil()
            && !identity.1.is_nil()
            && self.revision_id == Some(identity.0)
            && self.template_version_id == Some(identity.1)
            && identity.2.len() == 64
            && identity.2.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(identity)
    }

    fn is_current(self, job: &Value) -> bool {
        self.exact_identity().is_some_and(|identity| {
            job_uuid(job, "revision_id") == Some(identity.0)
                && job_uuid(job, "template_version_id") == Some(identity.1)
                && job["pdf_sha256"].as_str() == Some(identity.2.as_str())
        })
    }

    fn is_previous(self, job: &Value) -> bool {
        if !self.approved {
            return true;
        }
        if self.exact_identity().is_some() {
            return !self.is_current(job);
        }
        // Without current PDF bytes, matching revision/template metadata is not proof
        // that a receipt is old. Keep it neutral until the preview establishes its hash.
        self.revision_id
            .zip(job_uuid(job, "revision_id"))
            .is_some_and(|(current, delivered)| current != delivered)
            || self
                .template_version_id
                .zip(job_uuid(job, "template_version_id"))
                .is_some_and(|(current, delivered)| current != delivered)
    }
}

fn job_uuid(job: &Value, field: &str) -> Option<Uuid> {
    job[field]
        .as_str()
        .and_then(|value| Uuid::parse_str(value).ok())
}

/// Only fixed, reviewed copy leaves this boundary. The jobs API exposes
/// `safe_error_code`; receipt bodies and arbitrary transport errors are never UI text.
fn delivery_problem(job: &Value) -> &'static str {
    let code = job["safe_error_code"].as_str().unwrap_or("");
    if job["state"] == "retry" {
        return match code {
            "result_transport_failed" => "Не удалось установить связь с МИС. М53 автоматически повторяет передачу; доставка пока не подтверждена. Повторно отправлять PDF не нужно.",
            "invalid_result_receipt" | "result_receipt_too_large" => "МИС не прислала корректное подтверждение. М53 автоматически повторяет передачу; доставка пока не подтверждена. Если ожидание затянется, сообщите администратору.",
            _ => "М53 автоматически повторяет передачу; доставка пока не подтверждена. Повторно отправлять PDF не нужно. Если ожидание затянется, сообщите администратору.",
        };
    }
    match code {
        "one_c_access_denied" => "МИС отклонила доступ к передаче. Сообщите администратору для проверки доступа; доставка не подтверждена.",
        "task_cancelled" => "Задание в МИС отменено. Уточните задание у администратора; доставка не подтверждена.",
        "one_c_task_not_found" => "МИС не нашла задание. Попросите администратора проверить задание; доставка не подтверждена.",
        "one_c_file_recovery_required" => "МИС требуется восстановление файла. Сообщите администратору; доставка не подтверждена.",
        "stale_identity_snapshot" | "superseded_result_snapshot" => "Данные этой передачи устарели. Попросите администратора проверить актуальное задание и результат; доставка этого PDF не подтверждена.",
        "one_c_conflict" => "МИС отклонила передачу из-за конфликта данных. Попросите администратора проверить результат в МИС; доставка не подтверждена.",
        "one_c_result_too_large" | "result_json_too_large" => "Размер результата превышает допустимый для передачи. Сообщите администратору; доставка не подтверждена.",
        "result_transport_configuration_invalid" => "Настройки передачи в МИС требуют проверки администратором. Доставка не подтверждена.",
        "one_c_invalid_request" | "invalid_result_payload" | "invalid_snapshot_date" => "МИС не может принять подготовленный результат. Сообщите администратору для проверки данных передачи; доставка не подтверждена.",
        _ => "Доставка этого PDF в МИС не подтверждена. Сообщите администратору для проверки передачи.",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    CurrentDelivered,
    CurrentPending,
    CurrentFailed,
    PreviousDelivered,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Success,
    Pending,
    Danger,
    Muted,
}

pub struct Presentation<'a> {
    pub state: State,
    pub current_job: Option<&'a Value>,
    pub previous_delivery: Option<&'a Value>,
    pub title: &'static str,
    pub explanation: &'static str,
}
impl Presentation<'_> {
    pub fn tone(&self) -> Tone {
        match self.state {
            State::CurrentDelivered => Tone::Success,
            State::CurrentPending => Tone::Pending,
            State::CurrentFailed => Tone::Danger,
            State::PreviousDelivered | State::None => Tone::Muted,
        }
    }
    /// Presentation of the existing exact-byte guard; never replaces the actual send handler guard.
    #[cfg(test)]
    pub fn resend_locked(&self) -> bool {
        self.current_job
            .is_some_and(m53_client::delivery::prevents_resend)
    }
}

pub fn present<'a>(jobs: &'a Value, context: Context<'_>) -> Presentation<'a> {
    let current_job = context.exact_identity().and_then(|identity| {
        // Keep the core receipt preference and exact revision/template/hash matching.
        m53_client::delivery::matching(jobs, identity.0, identity.1, &identity.2)
    });
    let previous_delivery = jobs
        .as_array()
        .into_iter()
        .flatten()
        .filter(|job| m53_client::delivery::delivered(job) && context.is_previous(job))
        .max_by_key(|job| job["sequence"].as_u64().unwrap_or(0));

    let (state, title, explanation) = if let Some(job) = current_job {
        if m53_client::delivery::delivered(job) {
            (
                State::CurrentDelivered,
                "Текущий отчёт доставлен в МИС",
                "Получение этого PDF подтверждено МИС 1С.",
            )
        } else if matches!(job["state"].as_str(), Some("pending" | "retry")) {
            let explanation = if job["state"] == "retry" {
                delivery_problem(job)
            } else {
                "М53 принял этот PDF в очередь. Доставка в МИС пока не подтверждена."
            };
            (
                State::CurrentPending,
                "Текущий отчёт ожидает доставки",
                explanation,
            )
        } else {
            (
                State::CurrentFailed,
                "Текущий отчёт не доставлен",
                delivery_problem(job),
            )
        }
    } else {
        let explanation = if !context.approved {
            "Текущий ответ не утверждён. Эта доставка не подтверждает текущую редакцию."
        } else if context.exact_identity().is_none() {
            "Доставка текущего PDF ещё не проверена: дождитесь готового предпросмотра."
        } else {
            "Текущий отчёт не отправлен. Подтверждение относится к другой версии PDF."
        };
        if previous_delivery.is_some() {
            (
                State::PreviousDelivered,
                "Предыдущая версия доставлена в МИС",
                explanation,
            )
        } else if !context.approved {
            (
                State::None,
                "Текущий ответ не утверждён",
                "Текущий ответ не утверждён. Печать, экспорт и отправка недоступны.",
            )
        } else if context.exact_identity().is_none() {
            (
                State::None,
                "Доставка текущего PDF не проверена",
                explanation,
            )
        } else {
            (
                State::None,
                "Текущий отчёт не отправлен",
                "Этот PDF ещё не принят в очередь передачи в МИС.",
            )
        }
    };
    Presentation {
        state,
        current_job,
        previous_delivery,
        title,
        explanation,
    }
}

/// Only borrowed rows. Call when opening the muted history, not to clone receipts each render.
pub fn history<'a>(jobs: &'a Value, context: Context<'_>) -> Vec<&'a Value> {
    jobs.as_array()
        .into_iter()
        .flatten()
        .filter(|job| !context.is_current(job))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn identity() -> (Uuid, Uuid, String) {
        (Uuid::from_u128(1), Uuid::from_u128(2), "a".repeat(64))
    }
    fn context(identity: &(Uuid, Uuid, String)) -> Context<'_> {
        Context {
            approved: true,
            revision_id: Some(identity.0),
            template_version_id: Some(identity.1),
            preview_identity: Some(identity),
        }
    }
    fn job(identity: &(Uuid, Uuid, String), state: &str, delivered: bool, sequence: u64) -> Value {
        json!({"job_id":Uuid::from_u128(sequence as u128+10),"revision_id":identity.0,
            "template_version_id":identity.1,"pdf_sha256":identity.2,
            "state":state,"delivered":delivered,"sequence":sequence,
            "delivered_at":"2026-10-24T00:00:00Z",
            "receipt":{"success":true,"action":"created","reception_number":"SYNTHETIC"}})
    }

    #[test]
    fn green_success_requires_approved_exact_current_revision_template_and_pdf_bytes() {
        let id = identity();
        let jobs = json!([job(&id, "sent", true, 1)]);
        let shown = present(&jobs, context(&id));
        assert_eq!(shown.state, State::CurrentDelivered);
        assert_eq!(shown.tone(), Tone::Success);
        assert!(shown.resend_locked());
        assert!(shown.title.contains("Текущий"));
        assert!(shown.previous_delivery.is_none());
        assert!(history(&jobs, context(&id)).is_empty());
    }

    #[test]
    fn a_draft_never_borrows_green_success_even_if_stale_identity_and_receipt_match() {
        let id = identity();
        let jobs = json!([job(&id, "sent", true, 1)]);
        let mut ctx = context(&id);
        ctx.approved = false;
        let shown = present(&jobs, ctx);
        assert_eq!(shown.state, State::PreviousDelivered);
        assert_eq!(shown.tone(), Tone::Muted);
        assert!(shown.title.starts_with("Предыдущая версия"));
        assert!(shown.explanation.contains("не утверждён"));
        assert!(shown.current_job.is_none());
        assert!(shown.previous_delivery.is_some());
        assert!(!shown.resend_locked());
        assert_eq!(history(&jobs, ctx).len(), 1);
    }

    #[test]
    fn another_revision_template_or_hash_is_only_previous_delivery() {
        let id = identity();
        let jobs = json!([job(&id, "sent", true, 1)]);
        let variants = [
            (Uuid::from_u128(3), id.1, id.2.clone()),
            (id.0, Uuid::from_u128(4), id.2.clone()),
            (id.0, id.1, "b".repeat(64)),
        ];
        for current in variants {
            let shown = present(&jobs, context(&current));
            assert_eq!(shown.state, State::PreviousDelivered);
            assert_eq!(shown.tone(), Tone::Muted);
            assert!(shown.explanation.contains("не отправлен"));
            assert!(shown.current_job.is_none());
            assert!(!shown.resend_locked());
        }
    }

    #[test]
    fn pending_or_retry_current_pdf_wins_over_an_old_confirmed_receipt_without_turning_green() {
        let id = identity();
        let old = (Uuid::from_u128(3), id.1, "b".repeat(64));
        for state in ["pending", "retry"] {
            let jobs = json!([job(&old, "sent", true, 1), job(&id, state, false, 2)]);
            let shown = present(&jobs, context(&id));
            assert_eq!(shown.state, State::CurrentPending);
            assert_eq!(shown.tone(), Tone::Pending);
            assert!(shown.resend_locked());
            assert!(shown.previous_delivery.is_some());
            assert_eq!(history(&jobs, context(&id)).len(), 1);
            assert!(shown.explanation.contains("не подтверждена"));
        }
    }

    #[test]
    fn blocked_and_unconfirmed_sent_current_jobs_never_use_receipt_success_as_confirmation() {
        let id = identity();
        for state in ["blocked", "sent", "unknown"] {
            let jobs = json!([job(&id, state, false, 1)]);
            let shown = present(&jobs, context(&id));
            assert_eq!(shown.state, State::CurrentFailed);
            assert_eq!(shown.tone(), Tone::Danger);
            assert!(!shown.resend_locked());
            assert!(shown.previous_delivery.is_none());
        }
    }

    #[test]
    fn blocked_delivery_explains_safe_reason_and_required_action() {
        let id = identity();
        for (code, expected) in [
            ("one_c_access_denied", "доступ"),
            ("task_cancelled", "отменено"),
            ("one_c_file_recovery_required", "восстановление файла"),
        ] {
            let mut current = job(&id, "blocked", false, 1);
            current["safe_error_code"] = json!(code);
            let jobs = json!([current]);
            let shown = present(&jobs, context(&id));
            assert!(shown.explanation.contains(expected), "{}", shown.explanation);
            assert!(shown.explanation.contains("администратор"));
            assert!(!shown.explanation.contains("автоматически"));
            assert_eq!(shown.state, State::CurrentFailed);
        }
    }

    #[test]
    fn retry_is_automatic_but_unknown_blocked_error_never_echoes_untrusted_details() {
        let id = identity();
        let mut retry = job(&id, "retry", false, 1);
        retry["safe_error_code"] = json!("result_transport_failed");
        let jobs = json!([retry]);
        let shown = present(&jobs, context(&id));
        assert!(shown.explanation.contains("автоматически"));
        assert!(shown.explanation.contains("связь"));
        assert!(shown.resend_locked());
        let mut blocked = job(&id, "blocked", false, 2);
        blocked["safe_error_code"] = json!("https://secret.invalid/token?patient=synthetic");
        blocked["error"] = json!("private body");
        let jobs = json!([blocked]);
        let shown = present(&jobs, context(&id));
        assert!(shown.explanation.contains("администратор"));
        for private in ["https", "token", "patient", "synthetic", "private", "автоматически"] {
            assert!(!shown.explanation.contains(private));
        }
    }

    #[test]
    fn missing_preview_identity_is_unverified_instead_of_false_success_or_not_sent() {
        let id = identity();
        let jobs = json!([job(&id, "sent", true, 1)]);
        let mut ctx = context(&id);
        ctx.preview_identity = None;
        let shown = present(&jobs, ctx);
        assert_eq!(shown.state, State::None);
        assert_eq!(shown.tone(), Tone::Muted);
        assert!(shown.current_job.is_none());
        assert!(shown.previous_delivery.is_none());
        assert!(shown.explanation.contains("ещё не проверена"));
        assert!(!shown.title.contains("не отправлен"));
    }

    #[test]
    fn selected_template_or_live_revision_mismatch_invalidates_an_older_preview_identity() {
        let id = identity();
        let jobs = json!([job(&id, "sent", true, 1)]);
        let mut revision = context(&id);
        revision.revision_id = Some(Uuid::from_u128(8));
        let mut template = context(&id);
        template.template_version_id = Some(Uuid::from_u128(9));
        for ctx in [revision, template] {
            let shown = present(&jobs, ctx);
            assert_eq!(shown.state, State::PreviousDelivered);
            assert_eq!(shown.tone(), Tone::Muted);
            assert!(shown.current_job.is_none());
            assert!(shown.explanation.contains("ещё не проверена"));
        }
    }

    #[test]
    fn malformed_or_empty_hash_cannot_establish_exact_current_bytes() {
        for hash in ["".to_string(), "synthetic".into(), "z".repeat(64)] {
            let id = (Uuid::from_u128(1), Uuid::from_u128(2), hash);
            let jobs = json!([job(&id, "sent", true, 1)]);
            let shown = present(&jobs, context(&id));
            assert_eq!(shown.state, State::None);
            assert_ne!(shown.tone(), Tone::Success);
            assert!(shown.current_job.is_none());
        }
    }

    #[test]
    fn no_jobs_distinguishes_draft_unverified_preview_and_ready_unsent_pdf() {
        let id = identity();
        let jobs = json!([]);
        let ready = present(&jobs, context(&id));
        assert_eq!(ready.state, State::None);
        assert_eq!(ready.title, "Текущий отчёт не отправлен");
        let mut ctx = context(&id);
        ctx.approved = false;
        assert!(present(&jobs, ctx).explanation.contains("не утверждён"));
        ctx.approved = true;
        ctx.preview_identity = None;
        assert!(present(&jobs, ctx).explanation.contains("ещё не проверена"));
        assert_eq!(present(&Value::Null, ctx).state, State::None);
    }

    #[test]
    fn exact_delivered_receipt_keeps_existing_priority_and_history_excludes_duplicate_current_bytes()
     {
        let id = identity();
        let old = (Uuid::from_u128(3), id.1, "b".repeat(64));
        let jobs = json!([
            job(&old, "sent", true, 20),
            job(&id, "sent", true, 1),
            job(&id, "retry", false, 2)
        ]);
        let shown = present(&jobs, context(&id));
        assert_eq!(shown.state, State::CurrentDelivered);
        assert_eq!(shown.current_job.unwrap()["sequence"], 1);
        assert_eq!(shown.previous_delivery.unwrap()["sequence"], 20);
        let history = history(&jobs, context(&id));
        assert_eq!(history.len(), 1);
        assert_eq!(history[0]["sequence"], 20);
    }
}
