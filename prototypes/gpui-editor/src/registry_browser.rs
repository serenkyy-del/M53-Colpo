//! Bounded directory requests; no archive-wide patient/study preload.
use m53_client::offline::Offline;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Clone)]
pub enum Request {
    Query(String),
    Sort(usize, bool),
    Date(String),
    Patient(Uuid),
    MorePatients,
    MoreTasks,
    Refresh,
}
pub struct Browser {
    query: String,
    column: usize,
    descending: bool,
    pub date: String,
    pub patient: Option<Uuid>,
    patients: Vec<String>,
    tasks: Vec<String>,
    epoch: u64,
}
impl Default for Browser {
    fn default() -> Self {
        let mut this = Self {
            query: String::new(),
            column: 0,
            descending: false,
            date: chrono::Local::now().format("%Y-%m-%d").to_string(),
            patient: None,
            patients: vec![],
            tasks: vec![],
            epoch: 0,
        };
        this.reset_patients();
        this.reset_tasks();
        this
    }
}
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn rows(offline: &Offline, paths: &[String]) -> Vec<Value> {
    let mut rows: Vec<Value> = vec![];
    let mut positions=std::collections::BTreeMap::new();
    for path in paths {
        if let Some(page) = offline.cached(path) {
            for row in page["items"].as_array().into_iter().flatten() {
                let Some(id)=row["id"].as_str() else {continue;};
                if let Some(index) = positions.get(id).copied() {
                    rows[index] = row.clone();
                } else {
                    positions.insert(id.to_string(),rows.len());
                    rows.push(row.clone());
                }
            }
        }
    }
    rows
}
fn next(offline: &Offline, paths: &[String]) -> Option<String> {
    offline.cached(paths.last()?)?["next_cursor"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}
// Pure projection of the selected patient's history; does not schedule requests.
fn study_snapshot(patient: Option<Uuid>, online: bool, responses: &std::collections::BTreeMap<String, Value>) -> Value {
    let scoped = patient.and_then(|id| responses.get(&format!("/api/v1/studies?patient_id={id}"))).and_then(Value::as_array);
    let has_rows = scoped.is_some_and(|rows| !rows.is_empty()) || patient.is_some_and(|id| {
        responses.get("/api/v1/studies").and_then(Value::as_array).into_iter().flatten()
            .any(|row| row["patient_id"].as_str().and_then(|s| s.parse::<Uuid>().ok()) == Some(id))
    });
    // The aggregate study list contains local overlays, not evidence that this
    // patient's full server history has loaded. Only a scoped array proves that.
    let missing = scoped.is_none();
    let state = if !online && missing {
        crate::registry_view::DirectoryState::OfflineUncached
    } else {
        crate::registry_view::DirectoryState::classify(online, missing, missing, has_rows, false)
    };
    json!({"studies_patient_id":patient,"studies_cache_missing":scoped.is_none(),"studies_has_rows":has_rows,
        "studies_state":state.key(),"studies_status":if patient.is_none() {"Выберите пациента в таблице"} else {state.study_message(has_rows)}})
}
impl Browser {
    pub fn sync_refresh(&mut self, offline: &Offline) -> bool {
        if !self.observe_epoch(offline.registry_epoch()) {
            return false;
        }
        self.fetch(offline);
        true
    }
    fn observe_epoch(&mut self, epoch: u64) -> bool {
        if epoch == self.epoch {
            return false;
        }
        self.epoch = epoch;
        // Rows can move between pages after a sync. Retained middle pages can
        // overwrite fresh first-page patients or keep tasks removed by an ACK.
        self.reset_patients();
        self.reset_tasks();
        true
    }
    fn reset_patients(&mut self) {
        let sort = ["name", "birth_date", "sex", "last_exam_date"][self.column.min(3)];
        self.patients = vec![format!(
            "/api/v1/patients/page?limit=50&q={}&sort={sort}&descending={}",
            encode(&self.query),
            self.descending
        )];
    }
    fn reset_tasks(&mut self) {
        if !cfg!(feature = "standalone") {
            self.tasks = vec![format!("/api/v1/tasks/page?limit=50&date={}", self.date)];
        }
    }
    pub fn study_paths(&self) -> Option<(String, String)> {
        self.patient.map(|id| {
            (
                format!("/api/v1/studies?patient_id={id}"),
                format!("/api/v1/studies/summaries?patient_id={id}"),
            )
        })
    }
    pub fn request(&mut self, request: Request, offline: &Offline) {
        match request {
            Request::Query(q) => {
                self.query = q.trim().chars().take(200).collect();
                self.reset_patients();
            }
            Request::Sort(column, descending) => {
                self.column = column.min(3);
                self.descending = descending;
                self.reset_patients();
            }
            Request::Date(date) => {
                if m53_gpui_editor::registry::iso_date(&date).is_none() {
                    return;
                }
                self.date = date;
                self.reset_tasks();
            }
            Request::Patient(id) => self.patient = Some(id),
            Request::MorePatients => Self::more(offline, &mut self.patients),
            Request::MoreTasks => Self::more(offline, &mut self.tasks),
            Request::Refresh => {
                self.reset_patients();
                self.reset_tasks();
                offline.refresh_registry();
            }
        }
        self.fetch(offline);
    }
    fn more(offline: &Offline, paths: &mut Vec<String>) {
        if let Some(cursor) = next(offline, paths) {
            let path = format!("{}&cursor={}", paths[0], encode(&cursor));
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    pub fn fetch(&self, offline: &Offline) {
        let mut active = vec![self.patients[0].clone()];
        active.extend(self.tasks.first().cloned());
        for path in self
            .patients
            .iter()
            .rev()
            .take(1)
            .chain(self.tasks.iter().rev().take(1))
        {
            if !active.contains(path) {
                active.push(path.clone());
            }
        }
        if let Some((studies, summaries)) = self.study_paths() {
            active.push(studies);
            active.push(summaries);
        }
        offline.set_registry_paths(active);
    }
    pub fn snapshot(&self, offline: &Offline) -> Value {
        let mut patients = rows(offline, &self.patients);
        let page_missing = offline.cached(&self.patients[0]).is_none();
        let patients_last_missing = self.patients.last().is_some_and(|p| offline.cached(p).is_none());
        let tasks = rows(offline, &self.tasks);
        let tasks_first_missing = self.tasks.first().is_some_and(|path| offline.cached(path).is_none());
        let tasks_last_missing = self.tasks.last().is_some_and(|p| offline.cached(p).is_none());
        let state = offline.state.lock().unwrap();
        let study_view = study_snapshot(self.patient, state.online, &state.responses);
        let online = state.online;
        let fallback = patients.is_empty() && !state.online && page_missing;
        let pending: std::collections::BTreeSet<_> = state
            .pending
            .iter()
            .filter(|op| op.scope() == "/api/v1/patients")
            .map(|op| op.entity_id)
            .collect();
        let query = self.query.to_lowercase().replace('ё', "е");
        for patient in state
            .responses
            .get("/api/v1/patients")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let id = patient["id"].as_str().and_then(|v| v.parse().ok());
            if !fallback && !id.is_some_and(|id| pending.contains(&id) || self.patient==Some(id)) {
                continue;
            }
            let birth = patient["birth_date"].as_str().unwrap_or("");
            let searchable = format!(
                "{} {} {} {} {} {}",
                patient["family_name"].as_str().unwrap_or(""),
                patient["given_name"].as_str().unwrap_or(""),
                patient["patronymic"].as_str().unwrap_or(""),
                patient["external_id"].as_str().unwrap_or(""),
                birth,
                m53_gpui_editor::registry::display_date(Some(birth))
            )
            .to_lowercase()
            .replace('ё', "е");
            if searchable.contains(&query) && !patients.iter().any(|p| p["id"] == patient["id"]) {
                patients.push(patient.clone());
            }
            if fallback && patients.len() >= 50 {
                break;
            }
        }
        drop(state);
        let has_patients = !patients.is_empty();
        let has_tasks = !tasks.is_empty();
        let has_query = !self.query.is_empty();
        let patients_state = crate::registry_view::DirectoryState::classify(online, page_missing, patients_last_missing, has_patients, has_query);
        let tasks_state = crate::registry_view::DirectoryState::classify(online, tasks_first_missing, tasks_last_missing, has_tasks, false);
        let mut snapshot = json!({"patients":patients,"tasks":tasks,"date":self.date,
            "query":self.query,"column":self.column,"descending":self.descending,
            "patients_more":next(offline,&self.patients).is_some(),"tasks_more":next(offline,&self.tasks).is_some(),
            "patients_loading":online && patients_last_missing,"tasks_loading":online && tasks_last_missing,
            "online":online,"patients_cached":!page_missing || (!online && has_patients),"tasks_cached":!tasks_first_missing,
            "patients_cache_missing":page_missing,"tasks_cache_missing":tasks_first_missing,
            "patients_state":patients_state.key(),"tasks_state":tasks_state.key(),
            "patients_status":patients_state.patient_message(has_patients,has_query),"tasks_status":tasks_state.task_message(has_tasks)});
        snapshot.as_object_mut().unwrap().extend(study_view.as_object().unwrap().clone());
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_epoch_restarts_loaded_pages_and_preserves_view() {
        let mut browser = Browser::default();
        browser.query = "Иван &".into();
        browser.column = 3;
        browser.descending = true;
        browser.date = "2026-11-11".into();
        browser.patient = Some(Uuid::from_u128(17));
        browser.reset_patients();
        browser.reset_tasks();
        let first_patient = browser.patients[0].clone();
        let first_task = browser.tasks[0].clone();
        for cursor in ["middle", "last"] {
            browser.patients.push(format!("{first_patient}&cursor={cursor}"));
            browser.tasks.push(format!("{first_task}&cursor={cursor}"));
        }
        // A patient can move from a stale middle page to a fresh first page.
        // Keeping that middle path lets its older row overwrite the fresh one.
        assert!(browser.observe_epoch(1));
        assert_eq!(browser.patients, vec![first_patient.clone()]);
        assert_eq!(browser.tasks, vec![first_task]);
        assert_eq!(browser.query, "Иван &");
        assert_eq!((browser.column, browser.descending), (3, true));
        assert_eq!(browser.date, "2026-11-11");
        assert_eq!(browser.patient, Some(Uuid::from_u128(17)));
        // Loading another page remains explicit and survives unchanged epochs.
        browser.patients.push(format!("{first_patient}&cursor=new"));
        assert!(!browser.observe_epoch(1));
        assert_eq!(browser.patients.len(), 2);
    }
    #[test]
    fn study_history_never_infers_an_empty_server_history_from_missing_cache() {
        let patient = Uuid::from_u128(1);
        let mut responses = std::collections::BTreeMap::new();
        let online = study_snapshot(Some(patient), true, &responses);
        assert_eq!(online["studies_state"], "loading");
        assert_eq!(online["studies_patient_id"], patient.to_string());
        let offline = study_snapshot(Some(patient), false, &responses);
        assert_eq!(offline["studies_state"], "offline_uncached");
        assert!(offline["studies_status"].as_str().unwrap().contains("Сервер недоступен"));
        responses.insert("/api/v1/studies".into(), json!([{"id":Uuid::from_u128(10),"patient_id":patient}]));
        let local = study_snapshot(Some(patient), false, &responses);
        assert_eq!(local["studies_state"], "offline_uncached");
        assert_eq!(local["studies_has_rows"], true);
        assert!(local["studies_status"].as_str().unwrap().contains("локальные"));
        assert_eq!(study_snapshot(Some(patient), true, &responses)["studies_state"], "loading");
        let other = study_snapshot(Some(Uuid::from_u128(2)), false, &responses);
        assert_eq!(other["studies_has_rows"], false);
        assert_eq!(other["studies_patient_id"], Uuid::from_u128(2).to_string());
    }

    #[test]
    fn only_a_scoped_array_can_establish_loaded_study_history() {
        let patient = Uuid::from_u128(1);
        let path = format!("/api/v1/studies?patient_id={patient}");
        let mut responses = std::collections::BTreeMap::from([(path.clone(), json!([]))]);
        assert_eq!(study_snapshot(Some(patient), true, &responses)["studies_state"], "empty");
        let cached_empty = study_snapshot(Some(patient), false, &responses);
        assert_eq!(cached_empty["studies_state"], "cached");
        assert_eq!(cached_empty["studies_cache_missing"], false);
        assert!(cached_empty["studies_status"].as_str().unwrap().contains("сохранённых"));
        // A newly created local study can precede its appearance in the scoped
        // server response. Keep that visible row instead of announcing emptiness.
        responses.insert("/api/v1/studies".into(), json!([{"id":Uuid::from_u128(10),"patient_id":patient}]));
        assert_eq!(study_snapshot(Some(patient), true, &responses)["studies_state"], "ready");
        responses.remove("/api/v1/studies");
        responses.insert(path.clone(), json!([{"id":Uuid::from_u128(10),"patient_id":patient}]));
        assert_eq!(study_snapshot(Some(patient), true, &responses)["studies_state"], "ready");
        assert_eq!(study_snapshot(Some(patient), false, &responses)["studies_state"], "cached");
        responses.insert(path, json!({"error":"synthetic"}));
        assert_eq!(study_snapshot(Some(patient), true, &responses)["studies_state"], "loading");
        assert_eq!(study_snapshot(None, true, &responses)["studies_patient_id"], Value::Null);
    }
    #[test]
    fn query_is_encoded_and_initial_requests_are_bounded() {
        assert_eq!(encode("а &?"), "%D0%B0%20%26%3F");
        let browser = Browser::default();
        assert_eq!(browser.patients.len(), 1);
        assert_eq!(browser.tasks.len(), 1);
        assert!(browser.patients[0].contains("limit=50"));
        assert!(browser.tasks[0].contains("date="));
        assert!(browser.study_paths().is_none());
    }
}
