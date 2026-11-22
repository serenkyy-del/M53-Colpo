//! Shutdown joins owned workers and drains admitted writes before releasing the cache lease.
use std::{sync::Mutex, thread::{self, JoinHandle}};

#[derive(Default)]
pub(super) struct Workers {
    network: Mutex<Vec<JoinHandle<()>>>,
    disk: Mutex<Option<JoinHandle<()>>>,
    writers: Mutex<WriterState>,
}
#[derive(Default)]
struct WriterState { closing: bool, active: usize }
pub(super) struct WriteGuard<'a>(&'a Workers);
impl Drop for WriteGuard<'_> {
    fn drop(&mut self) { self.0.writers.lock().unwrap().active -= 1; }
}
impl Workers {
    pub(super) fn writer(&self) -> Result<WriteGuard<'_>, String> {
        let mut state = self.writers.lock().unwrap();
        if state.closing { return Err("Рабочее место закрывается; новые изменения не приняты".into()); }
        state.active += 1;
        Ok(WriteGuard(self))
    }
    pub(super) fn close(&self) { self.writers.lock().unwrap().closing = true; }
    pub(super) fn writers_finished(&self) -> bool { self.writers.lock().unwrap().active == 0 }

    pub(super) fn network(&self, work: impl FnOnce() + Send + 'static) {
        self.network.lock().unwrap().push(thread::spawn(work));
    }
    pub(super) fn disk(&self, work: impl FnOnce() + Send + 'static) {
        *self.disk.lock().unwrap() = Some(thread::spawn(work));
    }
    pub(super) fn network_finished(&self) -> bool {
        self.network.lock().unwrap().iter().all(JoinHandle::is_finished)
    }
    pub(super) fn finish(&self) -> Result<bool, String> {
        let mut network = self.network.lock().unwrap();
        let mut disk = self.disk.lock().unwrap();
        if !self.writers_finished() || network.iter().any(|job| !job.is_finished()) || disk.as_ref().is_some_and(|job| !job.is_finished()) {
            return Ok(false);
        }
        let mut failed = false;
        for job in network.drain(..).chain(disk.take()) { failed |= job.join().is_err(); }
        if failed { return Err("Фоновая задача завершилась с ошибкой; локальный кеш сохранён".into()); }
        Ok(true)
    }
}
