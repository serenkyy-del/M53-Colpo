//! Approval progress follows the exact durable operation, never the global queue count alone.
use std::time::Duration;
use uuid::Uuid;

pub const SAVE_WAIT_LIMIT: Duration = Duration::from_secs(45);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub busy: bool,
    /// A submitted request may still execute. Prevent duplicate approval, not local editing.
    pub pending: bool,
    pub message: String,
}
impl Notice {
    pub fn editing_locked(&self) -> bool { self.busy }
    pub fn saving() -> Self {
        Self {
            busy: true,
            pending: false,
            message: "Сохраняем протокол перед утверждением…".into(),
        }
    }
    pub fn failure(message: impl Into<String>) -> Self {
        Self {
            busy: false,
            pending: false,
            message: message.into(),
        }
    }
    fn queued(message: impl Into<String>, busy: bool) -> Self {
        Self {
            busy,
            pending: true,
            message: message.into(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub own_study: bool,
    pub code: Option<String>,
}
impl Block {
    pub fn message(&self) -> String {
        if !self.own_study {
            return "Не передана необходимая связанная запись. Проверьте её сообщение синхронизации; текущий черновик сохранён.".into();
        }
        match self.code.as_deref() {
            Some("patient_identity_unconfirmed") => "Для ручного приёма требуется сверка пациента врачом. Проверьте ФИО, дату рождения и пол на экране, нажмите «Подтвердить сверку», дождитесь подтверждения сервера, затем повторите «Утвердить». Черновик сохранён; это не конфликт версий.".into(),
            Some("patient_changed" | "patient_version_conflict") => "Данные пациента изменились. Обновите обследование и повторите ручную сверку. Черновик сохранён.".into(),
            Some("protocol_validation_failed") => "Сервер отклонил данные этого обследования: проверьте обязательные поля протокола и готовность справочника. Черновик сохранён; это ошибка проверки данных, а не конфликт версий.".into(),
            Some("version_conflict" | "study_version_conflict" | "revision_conflict" | "expected_version_mismatch") => "Версия обследования на сервере изменилась. Сравните локальный и серверный протокол и разрешите конфликт; черновик сохранён.".into(),
            _ => "Сервер отклонил передачу обследования. Откройте сравнение с серверной версией и устраните указанную ошибку; черновик сохранён.".into(),
        }
    }
}
#[derive(Default)]
pub struct Queue {
    pub study: Option<Uuid>,
    pub approved: bool,
    pub draft_changed: bool,
    pub online: bool,
    pub durable: bool,
    pub pending_count: usize,
    pub blocked: Option<Block>,
    /// Safe transport error category, without response bodies or clinical values.
    pub network_error: Option<String>,
    pub approval_queued: bool,
    pub approval_confirmed: bool,
    pub approval_rejected: Option<Block>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Wait(Notice),
    Submit,
    Finish(Notice),
}
#[derive(Debug)]
pub struct Pending {
    pub study: Uuid,
    pub operation: Uuid,
    pub submitted: bool,
    pub draft: Option<serde_json::Value>,
    pub last_notice: Option<Notice>,
}
impl Pending {
    pub fn new(study: Uuid, operation: Uuid, submitted: bool) -> Self {
        Self {
            study,
            operation,
            submitted,
            draft: None,
            last_notice: None,
        }
    }
    pub fn inspect(&self, q: &Queue, elapsed: Duration) -> Step {
        if q.study != Some(self.study) {
            return Step::Finish(Notice::failure(
                "Ожидание утверждения завершено: открыто другое обследование. Локальная работа сохранена.",
            ));
        }
        // A frozen/rejected request cannot execute; never hide its resolution behind
        // a changed working copy (for example text entered while the ACK was delayed).
        if self.submitted {
            if let Some(rejected) = &q.approval_rejected {
                return Step::Finish(Notice::failure(rejected.message()));
            }
            if let Some(blocked) = q.blocked.as_ref().filter(|b|b.own_study) {
                return Step::Finish(Notice::failure(blocked.message()));
            }
        }
        if q.draft_changed {
            if !self.submitted {
                return Step::Finish(Notice::failure(
                    "Утверждение не отправлено: протокол изменился во время сохранения. Проверьте текущие данные и нажмите «Утвердить» ещё раз.",
                ));
            }
            if q.approved || q.approval_confirmed || !q.approval_queued {
                return Step::Finish(Notice::failure(
                    "После отправки запроса локальный протокол изменился. Обновите обследование и сверьте утверждённую серверную редакцию; текущий текст автоматически не утверждался.",
                ));
            }
            return Step::Wait(Notice::queued(
                "Запрос относится к ранее сохранённой редакции. Текущий черновик можно редактировать и сохранять локально; повторное утверждение — после сверки с сервером.",
                false,
            ));
        }
        if q.approved {
            return Step::Finish(Notice::failure(
                "Протокол утверждён сервером. Можно сформировать отчёт и отправить в МИС.",
            ));
        }
        if let Some(rejected) = &q.approval_rejected {
            return Step::Finish(Notice::failure(rejected.message()));
        }
        if self.submitted {
            if q.approval_confirmed {
                return Step::Finish(Notice::failure(
                    "Сервер подтвердил запрос, но статус утверждения ещё не обновлён. Обновите обследование; печать доступна после подтверждения статуса.",
                ));
            }
            if !q.approval_queued {
                return Step::Finish(Notice::failure(
                    "Утверждение не подтверждено сервером. Черновик сохранён; проверьте сообщения синхронизации и повторите действие.",
                ));
            }
            if let Some(blocked) = &q.blocked {
                let mut notice = Notice::queued(blocked.message(), false);
                // A rejected current-study request is frozen and cannot execute automatically.
                // Its form remains editable for explicit conflict resolution.
                notice.pending = !blocked.own_study;
                return Step::Wait(notice);
            }
            if let Some(problem) = &q.network_error {
                return Step::Wait(Notice::queued(
                    format!(
                        "Запрос утверждения сохранён в очереди, подтверждения сервера нет. {problem} Черновик можно редактировать и сохранять локально. Повторная отправка не создаёт новый запрос."
                    ),
                    false,
                ));
            }
            let message = if elapsed >= SAVE_WAIT_LIMIT {
                "Запрос утверждения остаётся в очереди. Подтверждения сервера пока нет; можно продолжать редактирование и локальное сохранение черновика."
            } else {
                "Ожидаем подтверждения утверждения сервером…"
            };
            return Step::Wait(Notice::queued(message, elapsed < SAVE_WAIT_LIMIT));
        }
        if let Some(blocked) = &q.blocked {
            return Step::Finish(Notice::failure(format!(
                "Утверждение не отправлено. {}",
                blocked.message()
            )));
        }
        if let Some(problem) = &q.network_error {
            return Step::Finish(Notice::failure(format!(
                "Утверждение не отправлено. {problem} Черновик сохранён; после восстановления связи нажмите «Утвердить» ещё раз."
            )));
        }
        if q.online && q.durable && q.pending_count == 0 {
            return Step::Submit;
        }
        if elapsed >= SAVE_WAIT_LIMIT {
            return Step::Finish(Notice::failure(if !q.durable {
                "Утверждение не отправлено: локальная запись ещё не подтверждена. Не закрывайте программу; проверьте сообщение сохранения."
            } else {
                "Утверждение не отправлено: сервер ещё не подтвердил сохранение черновика. Работа сохранена локально; дождитесь передачи и повторите «Утвердить»."
            }));
        }
        Step::Wait(Notice {
            busy: true,
            pending: false,
            message: if !q.durable {
                "Сохраняем протокол на компьютере перед утверждением…".into()
            } else if q.pending_count > 0 {
                format!(
                    "Передаём сохранённые данные перед утверждением… Осталось операций: {}. Протокол пока не утверждён.",
                    q.pending_count
                )
            } else {
                "Проверяем связь с сервером перед утверждением…".into()
            },
        })
    }
}

/// Response bodies can contain clinical data. Only the server's technical code is used here.
pub fn rejection_code(message: &str) -> Option<String> {
    let start = message.find('{')?;
    let reply: serde_json::Value = serde_json::from_str(&message[start..]).ok()?;
    reply["code"]
        .as_str()
        .filter(|code| {
            code.len() <= 96 && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
        .map(str::to_owned)
}
pub fn transport_problem(online: bool, error: &str) -> Option<String> {
    if online || error.is_empty() {
        return None;
    }
    Some(
        if error.contains("Сессия истекла") || error.contains("401") || error.contains("403")
        {
            "Сеанс врача недоступен: выполните повторный вход."
        } else {
            "Нет подтверждённой связи с API."
        }
        .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (Pending, Queue) {
        let study = Uuid::new_v4();
        (
            Pending::new(study, Uuid::new_v4(), false),
            Queue {
                study: Some(study),
                online: true,
                durable: true,
                ..Default::default()
            },
        )
    }
    #[test]
    fn blocked_dependency_explains_failure_and_never_submits() {
        let (p, mut q) = setup();
        q.pending_count = 8;
        q.blocked = Some(Block {
            own_study: false,
            code: None,
        });
        let Step::Finish(n) = p.inspect(&q, Duration::ZERO) else {
            panic!("must stop");
        };
        assert!(!n.busy && !n.pending);
        assert!(n.message.contains("связанная запись"));
    }
    #[test]
    fn queue_is_visible_bounded_and_kept_until_confirmation() {
        let (p, mut q) = setup();
        q.pending_count = 8;
        assert!(
            matches!(p.inspect(&q,Duration::from_secs(1)),Step::Wait(n) if n.busy && n.message.contains("8"))
        );
        assert!(
            matches!(p.inspect(&q,SAVE_WAIT_LIMIT),Step::Finish(n) if !n.busy && n.message.contains("не отправлено"))
        );
        q.pending_count = 0;
        assert_eq!(p.inspect(&q, Duration::ZERO), Step::Submit);
    }
    #[test]
    fn rejection_does_not_become_success_when_global_queue_is_empty() {
        let (mut p, mut q) = setup();
        p.submitted = true;
        q.approval_rejected = Some(Block {
            own_study: true,
            code: Some("protocol_validation_failed".into()),
        });
        assert!(
            matches!(p.inspect(&q,Duration::ZERO),Step::Finish(n) if !n.busy && n.message.contains("Сервер отклонил"))
        );
    }
    #[test]
    fn submitted_transport_failure_is_retained_without_new_request() {
        let (mut p, mut q) = setup();
        p.submitted = true;
        q.approval_queued = true;
        q.online = false;
        q.network_error = Some("Нет связи".into());
        assert!(
            matches!(p.inspect(&q,Duration::ZERO),Step::Wait(n) if !n.busy && n.pending && n.message.contains("в очереди"))
        );
        q.online = true;
        q.network_error = None;
        assert!(matches!(p.inspect(&q,SAVE_WAIT_LIMIT),Step::Wait(n) if !n.busy && n.pending));
        q.approved = true;
        assert!(
            matches!(p.inspect(&q,SAVE_WAIT_LIMIT),Step::Finish(n) if !n.busy && !n.pending && n.message.contains("утверждён сервером"))
        );
    }
    #[test]
    fn a_missing_or_confirmed_request_without_approved_status_never_reports_success() {
        let (mut p, mut q) = setup();
        p.submitted = true;
        assert!(
            matches!(p.inspect(&q,Duration::ZERO),Step::Finish(n) if n.message.contains("не подтверждено"))
        );
        q.approval_confirmed = true;
        assert!(
            matches!(p.inspect(&q,Duration::ZERO),Step::Finish(n) if n.message.contains("статус утверждения ещё не обновлён"))
        );
    }
    #[test]
    fn changed_draft_or_study_never_submits_stale_approval() {
        let (p, mut q) = setup();
        q.draft_changed = true;
        assert!(
            matches!(p.inspect(&q,Duration::ZERO),Step::Finish(n) if n.message.contains("протокол изменился"))
        );
        q.draft_changed = false;
        q.study = Some(Uuid::new_v4());
        assert!(
            matches!(p.inspect(&q,Duration::ZERO),Step::Finish(n) if n.message.contains("другое обследование"))
        );
    }
    #[test]
    fn changed_local_text_is_never_presented_as_server_approved() {
        let (mut p, mut q) = setup();
        p.submitted = true;
        q.draft_changed = true;
        q.approval_queued = true;
        assert!(matches!(p.inspect(&q,Duration::ZERO),Step::Wait(n) if n.pending && !n.busy));
        q.approved = true;
        q.approval_queued = false;
        assert!(
            matches!(p.inspect(&q,Duration::ZERO),Step::Finish(n) if n.message.contains("текущий текст автоматически не утверждался"))
        );
    }
    #[test]
    fn changed_anamnesis_and_frozen_conflict_release_editor() {
        let (mut p,mut q)=setup();p.submitted=true;q.approval_queued=true;q.draft_changed=true;
        q.blocked=Some(Block{own_study:true,code:Some("version_conflict".into())});
        let Step::Finish(n)=p.inspect(&q,Duration::ZERO) else {panic!("frozen conflict must finish");};
        assert!(!n.editing_locked() && !n.pending && n.message.contains("конфликт"));
        q.approval_rejected=q.blocked.take();
        assert!(matches!(p.inspect(&q,Duration::ZERO),Step::Finish(n) if !n.editing_locked()));
    }
    #[test]
    fn offline_or_timed_out_ack_blocks_duplicate_approval_but_not_local_work() {
        let (mut p,mut q)=setup();p.submitted=true;q.approval_queued=true;
        q.network_error=Some("Нет связи".into());q.online=false;
        assert!(matches!(p.inspect(&q,Duration::ZERO),Step::Wait(n) if n.pending && !n.editing_locked()));
        q.network_error=None;q.online=true;
        assert!(matches!(p.inspect(&q,SAVE_WAIT_LIMIT),Step::Wait(n) if n.pending && !n.editing_locked()));
        q.draft_changed=true;
        assert!(matches!(p.inspect(&q,Duration::ZERO),Step::Wait(n) if n.pending && !n.editing_locked()));
    }
    #[test]
    fn only_technical_server_code_is_exposed() {
        assert_eq!(
            rejection_code(
                r#"Backend 400: {"code":"protocol_validation_failed","patient":"PRIVATE"}"#
            ),
            Some("protocol_validation_failed".into())
        );
        assert_eq!(
            rejection_code(r#"Backend 400: {"code":"PRIVATE value"}"#),
            None
        );
        assert_eq!(
            transport_problem(false, "secret clinical value").as_deref(),
            Some("Нет подтверждённой связи с API.")
        );
    }
}

#[cfg(test)]
mod manual_identity_notice_tests {
    use super::*;
    #[test]
    fn identity_failure_requests_explicit_verification_not_version_comparison() {
        let message=Block {own_study:true,code:Some("patient_identity_unconfirmed".into())}.message();
        assert!(message.contains("сверк"));
        assert!(!message.contains("сравнение"));
    }
}
