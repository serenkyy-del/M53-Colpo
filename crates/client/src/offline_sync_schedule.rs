//! Outage backoff is shared; a reachable server's rejected request only delays its own lane.
//! Recovery never rewrites an operation or treats reachability as an acknowledgement.
use std::{collections::{BTreeMap, BTreeSet}, time::{Duration, Instant}};
use uuid::Uuid;

struct Retry {
    until: Instant,
    attempts: u32,
    outage: bool,
}

#[derive(Default)]
pub(super) struct SyncSchedule {
    outage_until: Option<Instant>,
    outage_attempts: u32,
    retries: BTreeMap<Uuid, Retry>,
    inflight: BTreeSet<Uuid>,
}
impl SyncSchedule {
    /// Called once for offline -> reachable, fresh authentication, or new online work.
    /// HTTP/receipt errors keep their per-operation delay even if many GETs succeed.
    pub(super) fn wake(&mut self) {
        self.outage_until = None;
        self.outage_attempts = 0;
        self.retries.retain(|_, retry| !retry.outage);
    }
    pub(super) fn ready(&self, now: Instant) -> bool {
        self.outage_until.is_none_or(|until| now >= until)
    }
    pub(super) fn deferred(&self, now: Instant) -> BTreeSet<Uuid> {
        self.retries.iter().filter_map(|(id, retry)| (retry.until > now).then_some(*id)).chain(self.inflight.iter().copied()).collect()
    }
    pub(super) fn reserve(&mut self, id: Uuid) -> bool { self.inflight.insert(id) }
    pub(super) fn release(&mut self, id: Uuid) { self.inflight.remove(&id); }
    pub(super) fn failed(&mut self, id: Uuid, now: Instant, outage: bool) {
        self.inflight.remove(&id);
        let attempts = self.retries.get(&id).map_or(1, |r| r.attempts.saturating_add(1).min(5));
        self.retries.insert(id, Retry { until: now + Duration::from_secs(1 << attempts), attempts, outage });
        if outage {
            self.outage_attempts = self.outage_attempts.saturating_add(1).min(5);
            self.outage_until = Some(now + Duration::from_secs(1 << self.outage_attempts));
        }
    }
    pub(super) fn succeeded(&mut self, id: Uuid) {
        self.inflight.remove(&id);
        self.retries.remove(&id);
        // An authenticated matching receipt also demonstrates that connectivity recovered.
        self.wake();
    }
}

pub(super) fn transport_outage(error: &str) -> bool {
    matches!(error,
        "Связь с API прервана; работа сохраняется локально" |
        "Ответ API потерян; операция остается в локальной очереди")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inflight_reservations_survive_wakeup_and_release_only_after_completion() {
        let id=Uuid::new_v4();let now=Instant::now();let mut schedule=SyncSchedule::default();
        assert!(schedule.reserve(id));assert!(!schedule.reserve(id));schedule.wake();
        assert!(schedule.deferred(now).contains(&id));schedule.succeeded(id);
        assert!(!schedule.deferred(now).contains(&id));assert!(schedule.reserve(id));
        schedule.failed(id,now,false);assert!(schedule.deferred(now).contains(&id));
        assert!(!schedule.deferred(now+Duration::from_secs(2)).contains(&id));
    }
    #[test]
    fn recovery_releases_outage_delay_but_keeps_individual_http_retry() {
        let now = Instant::now();
        let (lost, limited) = (Uuid::new_v4(), Uuid::new_v4());
        let mut schedule = SyncSchedule::default();
        schedule.failed(lost, now, true);
        schedule.failed(limited, now, false);
        assert!(!schedule.ready(now));
        assert_eq!(schedule.deferred(now).len(), 2);
        schedule.wake();
        assert!(schedule.ready(now));
        assert_eq!(schedule.deferred(now), BTreeSet::from([limited]));
        for _ in 0..100 { schedule.wake(); }
        assert!(schedule.deferred(now + Duration::from_secs(1)).contains(&limited));
        assert!(schedule.deferred(now + Duration::from_secs(2)).is_empty());
    }
    #[test]
    fn a_server_rejection_leaves_other_lanes_ready_and_retries_are_bounded() {
        let now = Instant::now();
        let id = Uuid::new_v4();
        let mut schedule = SyncSchedule::default();
        for attempt in 1..=20u32 {
            schedule.failed(id, now, false);
            assert!(schedule.ready(now));
            let seconds = 1u64 << attempt.min(5);
            assert!(schedule.deferred(now + Duration::from_secs(seconds - 1)).contains(&id));
            assert!(schedule.deferred(now + Duration::from_secs(seconds)).is_empty());
        }
    }
    #[test]
    fn repeated_offline_attempts_back_off_without_a_busy_loop() {
        let now = Instant::now();
        let id = Uuid::new_v4();
        let mut schedule = SyncSchedule::default();
        for attempt in 1..=20u32 {
            schedule.failed(id, now, true);
            let seconds = 1u64 << attempt.min(5);
            assert!(!schedule.ready(now + Duration::from_secs(seconds - 1)));
            assert!(schedule.ready(now + Duration::from_secs(seconds)));
        }
        schedule.succeeded(id);
        assert!(schedule.ready(now));
        assert!(schedule.deferred(now).is_empty());
    }
    #[test]
    fn auth_validation_and_mismatched_receipts_are_not_transport_outages() {
        assert!(transport_outage("Ответ API потерян; операция остается в локальной очереди"));
        assert!(transport_outage("Связь с API прервана; работа сохраняется локально"));
        for error in ["Backend временно недоступен (429)", "Квитанция не совпадает с операцией; удаление локальных данных запрещено", "Сессия истекла: нужен повторный вход; локальная работа сохраняется", "Missing encrypted file; local operation retained"] {
            assert!(!transport_outage(error));
        }
    }
}
