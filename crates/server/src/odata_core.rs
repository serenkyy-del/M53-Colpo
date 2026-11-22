//! Read-only catalogue adapter for the existing OData Core.
//!
//! Tasks arrive through the M53 REST API, never through this adapter. The Core
//! owns its upstream 1C credentials. Its public catalogue GET routes do not use
//! the admin token. All URLs and field names below were checked against its
//! source and XML metadata. The employee eligibility fields were additionally
//! confirmed by an explicitly authorized, exact employee lookup; no patient
//! catalogue records were read during that verification.

use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{collections::HashSet, fmt, sync::Arc, time::Duration};
use tokio::sync::OnceCell;
use uuid::Uuid;

const MAX_BODY: usize = 1024 * 1024;
const MAX_PAGE: u16 = 500;
const PATIENT_ENTITY: &str = "Catalog_Клиенты";
const DOCTOR_ENTITY: &str = "Catalog_Сотрудники";
const POSITION_ENTITY: &str = "Catalog_Должности";
const USER_ENTITY: &str = "Catalog_Пользователи";
// Verified in the existing Core catalogue, not inferred from specialization.
const REQUIRED_POSITION_NAME: &str = "Врач-акушер-гинеколог";
const SERVICE_ENTITY: &str = "Catalog_Номенклатура";
const PERSON_FIELDS: &str =
    "Ref_Key,Description,Фамилия,Имя,Отчество,DeletionMark,Архив,IsFolder,DataVersion";
const DOCTOR_EXTRA_FIELDS: &str =
    "Подразделение_Key,Должность_Key,Специализация_Key,УчаствуетВКалендарномПланировании";

/// Errors deliberately contain no upstream body, URL, credentials or names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreError {
    Configuration,
    Transport,
    Http(u16),
    BodyTooLarge,
    InvalidResponse,
    InvalidRecord,
    InvalidPage,
}

impl CoreError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Configuration => "odata_core_configuration",
            Self::Transport => "odata_core_unavailable",
            Self::Http(_) => "odata_core_http_error",
            Self::BodyTooLarge => "odata_core_response_too_large",
            Self::InvalidResponse => "odata_core_invalid_response",
            Self::InvalidRecord => "odata_core_invalid_record",
            Self::InvalidPage => "odata_core_invalid_page",
        }
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for CoreError {}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Patient {
    pub guid: Uuid,
    pub family_name: String,
    pub given_name: String,
    pub patronymic: Option<String>,
    pub display_name: String,
    pub birth_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sex: Option<String>,
    pub active: bool,
    /// Opaque 1C token, not an incrementing assignment version.
    pub source_version: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Doctor {
    pub guid: Uuid,
    pub family_name: String,
    pub given_name: String,
    pub patronymic: Option<String>,
    pub display_name: String,
    pub active: bool,
    pub department_guid: Option<Uuid>,
    pub position_guid: Option<Uuid>,
    pub specialization_guid: Option<Uuid>,
    pub participates_in_calendar_planning: bool,
    pub source_version: Option<String>,
}

/// A verified employee-to-1C-user catalogue link, never an authentication call.
/// Code is the 1C login name confirmed by the user; no passwords are requested.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OdataUserAccount {
    pub guid: Uuid,
    pub doctor_guid: Uuid,
    pub login: String,
    pub active: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Service {
    pub guid: Uuid,
    pub name: String,
    pub full_name: Option<String>,
    pub code: Option<String>,
    pub active: bool,
    pub source_version: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Diagnosis {
    pub guid: Uuid,
    pub code: String,
    pub name: String,
    pub full_name: String,
    pub active: bool,
    pub is_folder: bool,
    /// Opaque source token, never an incrementing version.
    pub source_version: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// Explicit stable `$orderby=Ref_Key` / `$skip` pagination, no fetched links.
    pub next_skip: Option<u32>,
}

#[derive(Clone)]
pub struct CoreClient {
    base: Url,
    client: Client,
    required_position: Arc<OnceCell<Uuid>>,
}

impl CoreClient {
    /// Read only the verified branch catalogue. No patient catalogue scan.
    pub async fn clinic_branches(&self) -> Result<Vec<serde_json::Value>, CoreError> {
        let mut all = Vec::new();
        for page in 0..20u32 {
            let mut url = self.entity_url("Catalog_Филиалы")?;
            url.query_pairs_mut().append_pair("$format", "json")
                .append_pair("$select", "Ref_Key,Description,НазваниеОрганизации,Лицензия,DataVersion,DeletionMark")
                .append_pair("$orderby", "Ref_Key").append_pair("$top", "100")
                .append_pair("$skip", &(page * 100).to_string());
            let rows: List<serde_json::Value> = self.get(url).await?.ok_or(CoreError::InvalidResponse)?;
            if rows.next_link.is_some() || rows.value.len() > 100 { return Err(CoreError::InvalidPage); }
            let done = rows.value.len() < 100;
            all.extend(rows.value);
            if done { return Ok(all); }
        }
        Err(CoreError::InvalidPage)
    }

    /// Filter polymorphic contact information by exact branch GUID/type first.
    pub async fn clinic_phone(&self, guid: Uuid) -> Result<String, CoreError> {
        let mut values=Vec::new();
        for page in 0..10u32 {
        let mut url = self.entity_url("InformationRegister_КонтактнаяИнформация")?;
        url.query_pairs_mut().append_pair("$format", "json")
            .append_pair("$select", "Представление,ЗначениеПоУмолчанию")
            .append_pair("$filter", &format!("Объект eq '{guid}' and Объект_Type eq 'StandardODATA.Catalog_Филиалы' and Тип eq 'Телефон'"))
            .append_pair("$orderby", "ЗначениеПоУмолчанию desc,Представление")
            .append_pair("$skip", &(page*100).to_string()).append_pair("$top", "100");
        let rows: List<serde_json::Value> = self.get(url).await?.ok_or(CoreError::InvalidResponse)?;
        if rows.next_link.is_some() || rows.value.len() > 100 { return Err(CoreError::InvalidPage); }
        let done=rows.value.len()<100;values.extend(rows.value);
        if !done { if page==9 {return Err(CoreError::InvalidPage)} continue }
        let mut rows = values;
        rows.sort_by_key(|v| (!v["ЗначениеПоУмолчанию"].as_bool().unwrap_or(false), v["Представление"].as_str().unwrap_or("").to_owned()));
        let mut phones=Vec::new();
        for phone in rows.iter().filter_map(|v|v["Представление"].as_str()).filter(|v|!v.trim().is_empty()) {
            if !phones.contains(&phone) {phones.push(phone)}
        }
        return Ok(phones.join(", "))
        }
        Err(CoreError::InvalidPage)
    }
    /// An absent URL disables enrichment. No default connection to production.
    pub fn from_env() -> Result<Option<Self>, CoreError> {
        match std::env::var("M53_ODATA_CORE_URL") {
            Ok(value) if value.trim().is_empty() => Err(CoreError::Configuration),
            Ok(value) => Self::new(&value).map(Some),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err(CoreError::Configuration),
        }
    }

    /// HTTP is allowed only for a literal loopback IP or `localhost`. External
    /// Core connections require HTTPS with normal certificate validation.
    pub fn new(base: &str) -> Result<Self, CoreError> {
        let mut base = Url::parse(base).map_err(|_| CoreError::Configuration)?;
        let local = base.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !matches!(base.scheme(), "https" | "http")
            || (base.scheme() == "http" && !local)
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(CoreError::Configuration);
        }
        let path = format!("{}/", base.path().trim_end_matches('/'));
        base.set_path(&path);
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(5))
            .no_proxy();
        if let Some(path) = std::env::var_os("M53_TLS_CA_FILE") {
            let bytes = std::fs::read(path).map_err(|_| CoreError::Configuration)?;
            let certificate =
                reqwest::Certificate::from_pem(&bytes).map_err(|_| CoreError::Configuration)?;
            builder = builder.add_root_certificate(certificate);
        }
        let client = builder.build().map_err(|_| CoreError::Configuration)?;
        Ok(Self {
            base,
            client,
            required_position: Arc::new(OnceCell::new()),
        })
    }

    pub async fn patient(&self, guid: Uuid) -> Result<Option<Patient>, CoreError> {
        let fields = format!("{PERSON_FIELDS},ДатаРождения,Пол");
        self.record::<PersonRow>(PATIENT_ENTITY, guid, &fields)
            .await?
            .map(|row| row.patient(guid))
            .transpose()
    }

    pub async fn doctor(&self, guid: Uuid) -> Result<Option<Doctor>, CoreError> {
        if guid.is_nil() {
            return Err(CoreError::InvalidRecord);
        }
        let position = self.required_position().await?;
        let fields = format!("{PERSON_FIELDS},{DOCTOR_EXTRA_FIELDS}");
        self.record::<PersonRow>(DOCTOR_ENTITY, guid, &fields)
            .await?
            .map(|row| row.doctor(Some(guid), position))
            .transpose()
    }

    /// Read the complete bounded list for this exact employee. Inactive records
    /// must not hide a second active login behind a truncated first page.
    /// Callers never pick the first user or infer a login from the full name.
    pub async fn user_accounts(&self, employee: Uuid) -> Result<Vec<OdataUserAccount>, CoreError> {
        if employee.is_nil() {
            return Err(CoreError::InvalidRecord);
        }
        let mut seen = HashSet::new();
        let mut accounts = Vec::new();
        for page_index in 0..10 {
            let mut url = self.entity_url(USER_ENTITY)?;
            url.query_pairs_mut()
                .append_pair("$format", "json")
                .append_pair(
                    "$select",
                    "Ref_Key,Code,Сотрудник_Key,DeletionMark,IsFolder",
                )
                .append_pair("$filter", &format!("Сотрудник_Key eq guid'{employee}'"))
                .append_pair("$orderby", "Ref_Key")
                .append_pair("$skip", &(page_index * 100).to_string())
                .append_pair("$top", "100");
            let page: List<UserAccountRow> =
                self.get(url).await?.ok_or(CoreError::InvalidResponse)?;
            if page.next_link.is_some() || page.value.len() > 100 {
                return Err(CoreError::InvalidPage);
            }
            let complete = page.value.len() < 100;
            for row in page.value {
                if row.guid.is_nil() || row.employee != employee || !seen.insert(row.guid) {
                    return Err(CoreError::InvalidRecord);
                }
                let login = text(Some(&row.code))?.unwrap_or_default();
                if login.len() > 256 {
                    return Err(CoreError::InvalidRecord);
                }
                accounts.push(OdataUserAccount {
                    guid: row.guid,
                    doctor_guid: row.employee,
                    active: !row.deleted && !row.folder && !login.is_empty(),
                    login,
                });
            }
            if complete {
                return Ok(accounts);
            }
        }
        Err(CoreError::InvalidPage)
    }

    /// Resolve a real source GUID on every new synchronization client. Zero,
    /// duplicate, deleted or mismatched positions fail closed, without guessing.
    async fn required_position(&self) -> Result<Uuid, CoreError> {
        self.required_position
            .get_or_try_init(|| async {
                let mut url = self.entity_url(POSITION_ENTITY)?;
                url.query_pairs_mut()
                    .append_pair("$format", "json")
                    .append_pair("$select", "Ref_Key,Description,DeletionMark")
                    .append_pair(
                        "$filter",
                        &format!(
                            "Description eq '{REQUIRED_POSITION_NAME}' and DeletionMark eq false"
                        ),
                    )
                    .append_pair("$orderby", "Ref_Key")
                    .append_pair("$top", "2");
                let mut page: List<PositionRow> =
                    self.get(url).await?.ok_or(CoreError::InvalidResponse)?;
                if page.next_link.is_some() || page.value.len() != 1 {
                    return Err(CoreError::InvalidRecord);
                }
                let position = page.value.pop().ok_or(CoreError::InvalidRecord)?;
                if position.guid.is_nil()
                    || position.deleted
                    || position.name != REQUIRED_POSITION_NAME
                {
                    return Err(CoreError::InvalidRecord);
                }
                Ok(position.guid)
            })
            .await
            .copied()
    }

    /// `task_name` is not assumed to identify this entity. Call only after an
    /// explicit source mapping has established that it is a nomenclature GUID.
    pub async fn service(&self, guid: Uuid) -> Result<Option<Service>, CoreError> {
        self.record::<ServiceRow>(
            SERVICE_ENTITY,
            guid,
            "Ref_Key,Description,НаименованиеПолное,Code,DeletionMark,Архив,IsFolder,DataVersion",
        )
        .await?
        .map(|row| row.service(guid))
        .transpose()
    }

    /// Exact confirmed eligibility predicate: non-folder, not deleted, not
    /// archived, participates in calendar planning, and the source position
    /// named Врач-акушер-гинеколог. Previously linked profiles absent from these
    /// pages require a separate exact-GUID lookup before deactivation.
    pub async fn doctors_page(&self, skip: u32, page_size: u16) -> Result<Page<Doctor>, CoreError> {
        if page_size == 0 || page_size > MAX_PAGE {
            return Err(CoreError::InvalidPage);
        }
        let position = self.required_position().await?;
        let mut url = self.entity_url(DOCTOR_ENTITY)?;
        url.query_pairs_mut()
            .append_pair("$format", "json")
            .append_pair(
                "$select",
                &format!("{PERSON_FIELDS},{DOCTOR_EXTRA_FIELDS}"),
            )
            .append_pair("$filter", &format!("IsFolder eq false and DeletionMark eq false and Архив eq false and УчаствуетВКалендарномПланировании eq true and Должность_Key eq guid'{position}'"))
            .append_pair("$orderby", "Ref_Key")
            .append_pair("$skip", &skip.to_string())
            .append_pair("$top", &page_size.to_string());
        let page: List<PersonRow> = self.get(url).await?.ok_or(CoreError::InvalidResponse)?;
        if page.value.len() > usize::from(page_size) {
            return Err(CoreError::InvalidPage);
        }
        // A next-link cursor is not silently approximated with skip: doing so
        // could skip records when a server applies its own page-size limit.
        if page.next_link.is_some() {
            return Err(CoreError::InvalidPage);
        }
        let mut seen = HashSet::new();
        let mut items = Vec::with_capacity(page.value.len());
        for row in page.value {
            let doctor = row.doctor(None, position)?;
            if !seen.insert(doctor.guid) {
                return Err(CoreError::InvalidPage);
            }
            items.push(doctor);
        }
        let next_skip = if items.len() == usize::from(page_size) {
            Some(
                skip.checked_add(u32::from(page_size))
                    .ok_or(CoreError::InvalidPage)?,
            )
        } else {
            None
        };
        Ok(Page { items, next_skip })
    }

    /// Includes archived, deleted and folder records so the cache can reconcile
    /// source flags without treating absence as deletion.
    pub async fn diagnoses_page(
        &self,
        skip: u32,
        page_size: u16,
    ) -> Result<Page<Diagnosis>, CoreError> {
        if page_size == 0 || page_size > MAX_PAGE {
            return Err(CoreError::InvalidPage);
        }
        let mut url = self.entity_url("Catalog_Диагнозы")?;
        url.query_pairs_mut()
            .append_pair("$format", "json")
            .append_pair("$select", "Ref_Key,КодДиагноза,Description,НаименованиеПолное,DeletionMark,IsFolder,Архив,DataVersion")
            .append_pair("$orderby", "Ref_Key")
            .append_pair("$skip", &skip.to_string())
            .append_pair("$top", &page_size.to_string());
        let page: List<DiagnosisRow> = self.get(url).await?.ok_or(CoreError::InvalidResponse)?;
        if page.next_link.is_some() || page.value.len() > usize::from(page_size) {
            return Err(CoreError::InvalidPage);
        }
        let mut seen = HashSet::new();
        let mut items = Vec::with_capacity(page.value.len());
        for row in page.value {
            let diagnosis = row.diagnosis()?;
            if !seen.insert(diagnosis.guid) {
                return Err(CoreError::InvalidPage);
            }
            items.push(diagnosis);
        }
        let next_skip = if items.len() == usize::from(page_size) {
            Some(
                skip.checked_add(u32::from(page_size))
                    .ok_or(CoreError::InvalidPage)?,
            )
        } else {
            None
        };
        Ok(Page { items, next_skip })
    }

    fn entity_url(&self, entity: &str) -> Result<Url, CoreError> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|_| CoreError::Configuration)?
            .pop_if_empty()
            .push(entity);
        Ok(url)
    }

    async fn record<T: DeserializeOwned>(
        &self,
        entity: &str,
        guid: Uuid,
        fields: &str,
    ) -> Result<Option<T>, CoreError> {
        if guid.is_nil() {
            return Err(CoreError::InvalidRecord);
        }
        let mut url = self.entity_url(&format!("{entity}(guid'{guid}')"))?;
        url.query_pairs_mut()
            .append_pair("$format", "json")
            .append_pair("$select", fields);
        self.get(url).await
    }

    async fn get<T: DeserializeOwned>(&self, url: Url) -> Result<Option<T>, CoreError> {
        let mut response = self
            .client
            .get(url)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|_| CoreError::Transport)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if response.status() != StatusCode::OK {
            return Err(CoreError::Http(response.status().as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|len| len > MAX_BODY as u64)
        {
            return Err(CoreError::BodyTooLarge);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| CoreError::Transport)? {
            if body.len().saturating_add(chunk.len()) > MAX_BODY {
                return Err(CoreError::BodyTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body)
            .map(Some)
            .map_err(|_| CoreError::InvalidResponse)
    }
}

#[derive(Deserialize)]
struct List<T> {
    value: Vec<T>,
    #[serde(
        default,
        rename = "@odata.nextLink",
        alias = "odata.nextLink",
        alias = "__next"
    )]
    next_link: Option<String>,
}

#[derive(Deserialize)]
struct PersonRow {
    #[serde(rename = "Ref_Key")]
    guid: Uuid,
    #[serde(rename = "Description", default)]
    description: Option<String>,
    #[serde(rename = "Фамилия", default)]
    family_name: Option<String>,
    #[serde(rename = "Имя", default)]
    given_name: Option<String>,
    #[serde(rename = "Отчество", default)]
    patronymic: Option<String>,
    #[serde(rename = "Пол", default)]
    sex: Option<String>,
    #[serde(rename = "ДатаРождения", default)]
    birth_date: Option<String>,
    #[serde(rename = "DeletionMark")]
    deleted: bool,
    #[serde(rename = "Архив")]
    archived: bool,
    #[serde(rename = "IsFolder")]
    folder: bool,
    #[serde(rename = "DataVersion", default)]
    source_version: Option<String>,
    #[serde(rename = "Подразделение_Key", default)]
    department_guid: Option<Uuid>,
    #[serde(rename = "Должность_Key", default)]
    position_guid: Option<Uuid>,
    #[serde(rename = "Специализация_Key", default)]
    specialization_guid: Option<Uuid>,
    #[serde(rename = "УчаствуетВКалендарномПланировании", default)]
    participates_in_calendar_planning: Option<bool>,
}

#[derive(Deserialize)]
struct PositionRow {
    #[serde(rename = "Ref_Key")]
    guid: Uuid,
    #[serde(rename = "Description")]
    name: String,
    #[serde(rename = "DeletionMark")]
    deleted: bool,
}

#[derive(Deserialize)]
struct UserAccountRow {
    #[serde(rename = "Ref_Key")]
    guid: Uuid,
    #[serde(rename = "Code")]
    code: String,
    #[serde(rename = "Сотрудник_Key")]
    employee: Uuid,
    #[serde(rename = "DeletionMark")]
    deleted: bool,
    #[serde(rename = "IsFolder")]
    folder: bool,
}

impl PersonRow {
    fn names(
        &self,
        expected: Option<Uuid>,
    ) -> Result<(String, String, Option<String>, String), CoreError> {
        if self.guid.is_nil() || expected.is_some_and(|id| id != self.guid) {
            return Err(CoreError::InvalidRecord);
        }
        let family = text(self.family_name.as_deref())?.unwrap_or_default();
        let given = text(self.given_name.as_deref())?.unwrap_or_default();
        let patronymic = text(self.patronymic.as_deref())?;
        let display = [
            family.as_str(),
            given.as_str(),
            patronymic.as_deref().unwrap_or(""),
        ]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
        let display = if display.is_empty() {
            text(self.description.as_deref())?.ok_or(CoreError::InvalidRecord)?
        } else {
            display
        };
        Ok((family, given, patronymic, display))
    }

    fn patient(self, expected: Uuid) -> Result<Patient, CoreError> {
        if self.folder {
            return Err(CoreError::InvalidRecord);
        }
        let (family_name, given_name, patronymic, display_name) = self.names(Some(expected))?;
        Ok(Patient {
            guid: self.guid,
            family_name,
            given_name,
            patronymic,
            display_name,
            birth_date: birth_date(self.birth_date.as_deref())?,
            sex: match self.sex.as_deref() {
                None | Some("") => None,
                Some("Женский" | "F") => Some("F".into()),
                Some("Мужской" | "M") => Some("M".into()),
                Some("НеУказан" | "O") => Some("O".into()),
                _ => return Err(CoreError::InvalidRecord),
            },
            active: !self.deleted && !self.archived,
            source_version: version(self.source_version)?,
        })
    }

    fn doctor(self, expected: Option<Uuid>, required_position: Uuid) -> Result<Doctor, CoreError> {
        let (family_name, given_name, patronymic, display_name) = self.names(expected)?;
        let participates_in_calendar_planning = self
            .participates_in_calendar_planning
            .ok_or(CoreError::InvalidRecord)?;
        Ok(Doctor {
            guid: self.guid,
            family_name,
            given_name,
            patronymic,
            display_name,
            active: !self.folder
                && !self.deleted
                && !self.archived
                && participates_in_calendar_planning
                && self.position_guid == Some(required_position),
            department_guid: non_nil(self.department_guid),
            position_guid: non_nil(self.position_guid),
            specialization_guid: non_nil(self.specialization_guid),
            participates_in_calendar_planning,
            source_version: version(self.source_version)?,
        })
    }
}

#[derive(Deserialize)]
struct ServiceRow {
    #[serde(rename = "Ref_Key")]
    guid: Uuid,
    #[serde(rename = "Description")]
    name: String,
    #[serde(rename = "НаименованиеПолное", default)]
    full_name: Option<String>,
    #[serde(rename = "Code", default)]
    code: Option<String>,
    #[serde(rename = "DeletionMark")]
    deleted: bool,
    #[serde(rename = "Архив")]
    archived: bool,
    #[serde(rename = "IsFolder")]
    folder: bool,
    #[serde(rename = "DataVersion", default)]
    source_version: Option<String>,
}

impl ServiceRow {
    fn service(self, expected: Uuid) -> Result<Service, CoreError> {
        if self.guid != expected || self.guid.is_nil() || self.folder {
            return Err(CoreError::InvalidRecord);
        }
        Ok(Service {
            guid: self.guid,
            name: text(Some(&self.name))?.ok_or(CoreError::InvalidRecord)?,
            full_name: text(self.full_name.as_deref())?,
            code: text(self.code.as_deref())?,
            active: !self.deleted && !self.archived,
            source_version: version(self.source_version)?,
        })
    }
}

#[derive(Deserialize)]
struct DiagnosisRow {
    #[serde(rename = "Ref_Key")]
    guid: Uuid,
    #[serde(rename = "КодДиагноза")]
    code: Option<String>,
    #[serde(rename = "Description")]
    name: Option<String>,
    #[serde(rename = "НаименованиеПолное")]
    full_name: Option<String>,
    #[serde(rename = "DeletionMark")]
    deleted: bool,
    #[serde(rename = "IsFolder")]
    folder: bool,
    #[serde(rename = "Архив")]
    archived: bool,
    #[serde(rename = "DataVersion")]
    source_version: Option<String>,
}

impl DiagnosisRow {
    fn diagnosis(self) -> Result<Diagnosis, CoreError> {
        fn bounded(value: &str, maximum: usize) -> Result<(), CoreError> {
            if value.len() > maximum || value.chars().any(char::is_control) {
                return Err(CoreError::InvalidRecord);
            }
            Ok(())
        }
        if self.guid.is_nil() {
            return Err(CoreError::InvalidRecord);
        }
        let code = self.code.unwrap_or_default();
        let name = self.name.unwrap_or_default();
        let source_version = self.source_version.unwrap_or_default();
        bounded(&code, 32)?;
        bounded(&name, 4096)?;
        bounded(&source_version, 256)?;
        if let Some(full_name) = &self.full_name {
            bounded(full_name, 4096)?;
        }
        let name = name.trim().to_owned();
        let full_name = self
            .full_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(&name)
            .to_owned();
        Ok(Diagnosis {
            guid: self.guid,
            code: code.trim().to_owned(),
            name,
            full_name,
            active: !self.deleted && !self.archived,
            is_folder: self.folder,
            source_version,
        })
    }
}

fn non_nil(value: Option<Uuid>) -> Option<Uuid> {
    value.filter(|guid| !guid.is_nil())
}

fn version(value: Option<String>) -> Result<Option<String>, CoreError> {
    if value
        .as_ref()
        .is_some_and(|value| value.len() > 4096 || value.chars().any(char::is_control))
    {
        return Err(CoreError::InvalidRecord);
    }
    Ok(value.filter(|value| !value.is_empty()))
}

fn text(value: Option<&str>) -> Result<Option<String>, CoreError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.len() > 4096 || value.chars().any(char::is_control) {
        return Err(CoreError::InvalidRecord);
    }
    Ok(Some(value.to_owned()))
}

fn birth_date(value: Option<&str>) -> Result<Option<String>, CoreError> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let bytes = value.as_bytes();
    if bytes.len() < 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[..10]
            .iter()
            .enumerate()
            .any(|(i, b)| i != 4 && i != 7 && !b.is_ascii_digit())
        || (bytes.len() > 10 && bytes[10] != b'T')
    {
        return Err(CoreError::InvalidRecord);
    }
    let year: u32 = value[..4].parse().map_err(|_| CoreError::InvalidRecord)?;
    let month: u32 = value[5..7].parse().map_err(|_| CoreError::InvalidRecord)?;
    let day: u32 = value[8..10].parse().map_err(|_| CoreError::InvalidRecord)?;
    // The 1C empty date is not a patient's birth date.
    if (year, month, day) == (1, 1, 1) {
        return Ok(None);
    }
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    if year < 2 || day == 0 || day > days {
        return Err(CoreError::InvalidRecord);
    }
    Ok(Some(value[..10].to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::oneshot,
    };

    const ID: Uuid = Uuid::from_u128(100);
    const POSITION_ID: Uuid = Uuid::from_u128(200);

    fn person(id: Uuid) -> Value {
        json!({
            "Ref_Key": id, "Description": "Тестовый врач", "Фамилия": "Тестовый",
            "Имя": "Врач", "Отчество": null, "DeletionMark": false,
            "Архив": false, "IsFolder": false, "DataVersion": "opaque+/==",
            "ДатаРождения": "2000-02-29T00:00:00",
            "Должность_Key": POSITION_ID,
            "УчаствуетВКалендарномПланировании": true
        })
    }

    async fn mock(
        status: &str,
        headers: &str,
        body: Vec<u8>,
    ) -> (CoreClient, oneshot::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let response_head = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
            body.len()
        );
        let (send, receive) = oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                let read = stream.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    break;
                }
            }
            let _ = send.send(String::from_utf8(request).unwrap());
            let _ = stream.write_all(response_head.as_bytes()).await;
            let _ = stream.write_all(&body).await;
        });
        (
            CoreClient::new(&format!("http://{address}/odata")).unwrap(),
            receive,
        )
    }

    #[test]
    fn configuration_rejects_embedded_credentials_and_external_plain_http() {
        for endpoint in [
            "http://192.168.1.1/odata",
            "http://example.org/odata",
            "ftp://127.0.0.1/odata",
            "https://username:secret@example.org/odata",
            "https://example.org/odata?token=secret",
            "https://example.org/odata#secret",
        ] {
            assert!(matches!(
                CoreClient::new(endpoint),
                Err(CoreError::Configuration)
            ));
        }
        for endpoint in [
            "http://localhost:8930/odata",
            "http://[::1]:8930/odata",
            "https://example.org/odata",
        ] {
            assert!(CoreClient::new(endpoint).is_ok());
        }
        assert!(!CoreError::Http(500).to_string().contains("500"));
    }

    #[tokio::test]
    async fn patient_lookup_is_get_encoded_and_validates_returned_guid() {
        let (client, request) = mock(
            "200 OK",
            "Content-Type: application/json\r\n",
            serde_json::to_vec(&person(ID)).unwrap(),
        )
        .await;
        let patient = client.patient(ID).await.unwrap().unwrap();
        assert_eq!(patient.guid, ID);
        assert_eq!(patient.birth_date.as_deref(), Some("2000-02-29"));
        assert_eq!(patient.source_version.as_deref(), Some("opaque+/=="));
        assert_eq!(patient.patronymic, None);
        let request = request.await.unwrap();
        assert!(request.starts_with("GET /odata/Catalog_%"));
        assert!(!request.to_lowercase().contains("authorization:"));
        let target = request.split_whitespace().nth(1).unwrap();
        let url = Url::parse(&format!("http://localhost{target}")).unwrap();
        let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(query.get("$format").unwrap(), "json");
        assert!(query.get("$select").unwrap().contains("ДатаРождения"));
        let (client, _) = mock(
            "200 OK",
            "",
            serde_json::to_vec(&person(Uuid::from_u128(101))).unwrap(),
        )
        .await;
        assert_eq!(
            client.patient(ID).await.unwrap_err(),
            CoreError::InvalidRecord
        );
    }

    #[tokio::test]
    async fn doctor_pages_use_exact_position_and_calendar_eligibility() {
        let mut archived = person(ID);
        archived["Архив"] = json!(true);
        let body = json!({"value": [archived, person(Uuid::from_u128(101))]});
        let (client, request) = mock("200 OK", "", serde_json::to_vec(&body).unwrap()).await;
        client.required_position.set(POSITION_ID).unwrap();
        let page = client.doctors_page(4, 2).await.unwrap();
        assert_eq!(page.next_skip, Some(6));
        assert_eq!(page.items.len(), 2);
        assert!(!page.items[0].active);
        assert_eq!(page.items[0].position_guid, Some(POSITION_ID));
        assert!(page.items[1].active);
        let request = request.await.unwrap();
        let target = request.split_whitespace().nth(1).unwrap();
        let url = Url::parse(&format!("http://localhost{target}")).unwrap();
        let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(query.get("$orderby").unwrap(), "Ref_Key");
        assert_eq!(query.get("$skip").unwrap(), "4");
        assert_eq!(query.get("$top").unwrap(), "2");
        assert_eq!(
            query.get("$filter").unwrap(),
            &format!(
                "IsFolder eq false and DeletionMark eq false and Архив eq false and УчаствуетВКалендарномПланировании eq true and Должность_Key eq guid'{POSITION_ID}'"
            )
        );
        assert!(
            query
                .get("$select")
                .unwrap()
                .contains("УчаствуетВКалендарномПланировании")
        );
        assert!(!request.contains("/schedule"));
    }

    #[tokio::test]
    async fn malformed_or_partial_pages_do_not_silently_look_complete() {
        for body in [
            json!({"value":[person(ID),person(ID)]}),
            json!({"value":[person(ID)],"odata.nextLink":"http://other-host/secret"}),
            json!({"error":"missing value envelope"}),
        ] {
            let (client, _) = mock("200 OK", "", serde_json::to_vec(&body).unwrap()).await;
            client.required_position.set(POSITION_ID).unwrap();
            assert!(client.doctors_page(0, 2).await.is_err());
        }
        let (client, _) = mock(
            "200 OK",
            "",
            serde_json::to_vec(&json!({"value":[]})).unwrap(),
        )
        .await;
        client.required_position.set(POSITION_ID).unwrap();
        assert!(client.doctors_page(0, 2).await.unwrap().next_skip.is_none());
        assert_eq!(
            client.doctors_page(0, 0).await.unwrap_err(),
            CoreError::InvalidPage
        );
    }

    #[tokio::test]
    async fn obgyn_position_requires_one_exact_active_source_record() {
        let valid = json!({"Ref_Key":POSITION_ID,"Description":REQUIRED_POSITION_NAME,"DeletionMark":false});
        for body in [
            json!({"value":[]}),
            json!({"value":[valid.clone(),valid.clone()]}),
            json!({"value":[{"Ref_Key":POSITION_ID,"Description":"Other position","DeletionMark":false}]}),
            json!({"value":[{"Ref_Key":POSITION_ID,"Description":REQUIRED_POSITION_NAME,"DeletionMark":true}]}),
            json!({"value":[{"Ref_Key":Uuid::nil(),"Description":REQUIRED_POSITION_NAME,"DeletionMark":false}]}),
        ] {
            let (client, _) = mock("200 OK", "", serde_json::to_vec(&body).unwrap()).await;
            assert_eq!(
                client.required_position().await.unwrap_err(),
                CoreError::InvalidRecord
            );
        }
        let (client, request) = mock(
            "200 OK",
            "",
            serde_json::to_vec(&json!({"value":[valid]})).unwrap(),
        )
        .await;
        assert_eq!(client.required_position().await.unwrap(), POSITION_ID);
        // Cached for this synchronization client; no second HTTP request.
        assert_eq!(client.required_position().await.unwrap(), POSITION_ID);
        let request = request.await.unwrap();
        let target = request.split_whitespace().nth(1).unwrap();
        let url = Url::parse(&format!("http://localhost{target}")).unwrap();
        assert_eq!(
            url.path_segments()
                .unwrap()
                .next_back()
                .map(percent_decode_test),
            Some(POSITION_ENTITY.to_string())
        );
        let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(
            query.get("$filter").unwrap(),
            &format!("Description eq '{REQUIRED_POSITION_NAME}' and DeletionMark eq false")
        );
        assert_eq!(query.get("$top").unwrap(), "2");
    }

    fn percent_decode_test(value: &str) -> String {
        let parsed = Url::parse(&format!("http://localhost/?value={value}")).unwrap();
        parsed.query_pairs().next().unwrap().1.to_string()
    }

    #[test]
    fn doctor_eligibility_never_infers_missing_planning_or_position() {
        let valid: PersonRow = serde_json::from_value(person(ID)).unwrap();
        assert!(valid.doctor(Some(ID), POSITION_ID).unwrap().active);
        for (field, value) in [
            ("DeletionMark", json!(true)),
            ("Архив", json!(true)),
            ("IsFolder", json!(true)),
            ("УчаствуетВКалендарномПланировании", json!(false)),
            ("Должность_Key", json!(Uuid::from_u128(201))),
            ("Должность_Key", json!(Uuid::nil())),
        ] {
            let mut row = person(ID);
            row[field] = value;
            let row: PersonRow = serde_json::from_value(row).unwrap();
            assert!(!row.doctor(Some(ID), POSITION_ID).unwrap().active);
        }
        let mut row = person(ID);
        row.as_object_mut()
            .unwrap()
            .remove("УчаствуетВКалендарномПланировании");
        let row: PersonRow = serde_json::from_value(row).unwrap();
        assert_eq!(
            row.doctor(Some(ID), POSITION_ID).unwrap_err(),
            CoreError::InvalidRecord
        );
    }

    #[tokio::test]
    async fn user_account_lookup_uses_employee_guid_and_trimmed_code_without_credentials() {
        let body = json!({"value":[{"Ref_Key":Uuid::from_u128(300),"Сотрудник_Key":ID,
            "Code":"  synthetic.login      ","DeletionMark":false,"IsFolder":false}]});
        let (client, request) = mock("200 OK", "", serde_json::to_vec(&body).unwrap()).await;
        let accounts = client.user_accounts(ID).await.unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].doctor_guid, ID);
        assert_eq!(accounts[0].login, "synthetic.login");
        assert!(accounts[0].active);
        let request = request.await.unwrap();
        assert!(!request.to_lowercase().contains("authorization:"));
        let target = request.split_whitespace().nth(1).unwrap();
        let url = Url::parse(&format!("http://localhost{target}")).unwrap();
        let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(
            query.get("$filter").unwrap(),
            &format!("Сотрудник_Key eq guid'{ID}'")
        );
        assert_eq!(
            query.get("$select").unwrap(),
            "Ref_Key,Code,Сотрудник_Key,DeletionMark,IsFolder"
        );
        assert_eq!(query.get("$top").unwrap(), "100");
        assert_eq!(query.get("$skip").unwrap(), "0");
        for (field, value) in [
            ("Code", json!("   ")),
            ("DeletionMark", json!(true)),
            ("IsFolder", json!(true)),
        ] {
            let mut body = body.clone();
            body["value"][0][field] = value;
            let (client, _) = mock("200 OK", "", serde_json::to_vec(&body).unwrap()).await;
            assert!(!client.user_accounts(ID).await.unwrap()[0].active);
        }
        let mut wrong = body.clone();
        wrong["value"][0]["Сотрудник_Key"] = json!(Uuid::from_u128(101));
        let (client, _) = mock("200 OK", "", serde_json::to_vec(&wrong).unwrap()).await;
        assert_eq!(
            client.user_accounts(ID).await.unwrap_err(),
            CoreError::InvalidRecord
        );
        let duplicate = json!({"value":[body["value"][0].clone(),body["value"][0].clone()]});
        let (client, _) = mock("200 OK", "", serde_json::to_vec(&duplicate).unwrap()).await;
        assert_eq!(
            client.user_accounts(ID).await.unwrap_err(),
            CoreError::InvalidRecord
        );
        let (client, _) = mock(
            "200 OK",
            "",
            serde_json::to_vec(&json!({"value":[]})).unwrap(),
        )
        .await;
        assert!(client.user_accounts(ID).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn missing_records_redirects_and_size_limits_are_explicit() {
        let (client, _) = mock("404 Not Found", "", Vec::new()).await;
        assert!(client.patient(ID).await.unwrap().is_none());
        let (client, _) = mock(
            "302 Found",
            "Location: http://other-host/private\r\n",
            Vec::new(),
        )
        .await;
        assert_eq!(client.patient(ID).await.unwrap_err(), CoreError::Http(302));
        let (client, _) = mock("200 OK", "", vec![b' '; MAX_BODY + 1]).await;
        assert_eq!(
            client.patient(ID).await.unwrap_err(),
            CoreError::BodyTooLarge
        );
        let (client, _) = mock("200 OK", "", b"not-json".to_vec()).await;
        assert_eq!(
            client.patient(ID).await.unwrap_err(),
            CoreError::InvalidResponse
        );
        assert_eq!(
            client.patient(Uuid::nil()).await.unwrap_err(),
            CoreError::InvalidRecord
        );
    }

    #[test]
    fn empty_one_c_date_and_invalid_calendar_dates_are_not_birth_dates() {
        assert_eq!(birth_date(Some("0001-01-01T00:00:00")).unwrap(), None);
        assert_eq!(birth_date(None).unwrap(), None);
        assert_eq!(
            birth_date(Some("2026-02-30T00:00:00")),
            Err(CoreError::InvalidRecord)
        );
        assert_eq!(
            birth_date(Some("2025-02-29")),
            Err(CoreError::InvalidRecord)
        );
        assert_eq!(
            birth_date(Some("2000-02-29")).unwrap(),
            Some("2000-02-29".into())
        );
        assert_eq!(
            birth_date(Some("я\u{2003}2000-01-01")),
            Err(CoreError::InvalidRecord)
        );
    }

    #[tokio::test]
    async fn service_mapping_is_explicit_and_does_not_infer_from_task_name() {
        let body = json!({"Ref_Key":ID,"Description":"Тестовая услуга","НаименованиеПолное":"Полное название","Code":"001","DeletionMark":false,"Архив":false,"IsFolder":false,"DataVersion":"not-a-number"});
        let (client, request) = mock("200 OK", "", serde_json::to_vec(&body).unwrap()).await;
        let service = client.service(ID).await.unwrap().unwrap();
        assert_eq!(service.guid, ID);
        assert_eq!(service.name, "Тестовая услуга");
        assert_eq!(service.source_version.as_deref(), Some("not-a-number"));
        let request = request.await.unwrap();
        assert!(request.starts_with("GET /odata/Catalog_%"));
    }
    fn diagnosis(id: Uuid) -> Value {
        json!({"Ref_Key":id,"КодДиагноза":" A01 ","Code":"WRONG","Description":" Диагноз ","НаименованиеПолное":null,"DeletionMark":false,"IsFolder":true,"Архив":false,"DataVersion":"opaque+/=="})
    }

    #[tokio::test]
    async fn diagnoses_mapping_and_query_are_exact() {
        let mut archived = diagnosis(Uuid::from_u128(101));
        archived["Архив"] = json!(true);
        archived["КодДиагноза"] = json!("");
        let (client, request) = mock(
            "200 OK",
            "",
            serde_json::to_vec(&json!({"value":[diagnosis(ID),archived]})).unwrap(),
        )
        .await;
        let page = client.diagnoses_page(4, 2).await.unwrap();
        assert_eq!(page.next_skip, Some(6));
        assert_eq!(page.items[0].code, "A01");
        assert_eq!(page.items[0].name, "Диагноз");
        assert_eq!(page.items[0].full_name, "Диагноз");
        assert_eq!(page.items[0].source_version, "opaque+/==");
        assert!(page.items[0].is_folder && page.items[0].active);
        assert!(!page.items[1].active);
        assert_eq!(page.items[1].code, "");
        for deleted in [false, true] {
            for archived in [false, true] {
                let mut row = diagnosis(ID);
                row["DeletionMark"] = json!(deleted);
                row["Архив"] = json!(archived);
                let parsed: DiagnosisRow = serde_json::from_value(row).unwrap();
                assert_eq!(parsed.diagnosis().unwrap().active, !deleted && !archived);
            }
        }
        let request = request.await.unwrap();
        assert!(request.starts_with("GET /odata/Catalog_%"));
        assert!(!request.to_lowercase().contains("authorization:"));
        let target = request.split_whitespace().nth(1).unwrap();
        let url = Url::parse(&format!("http://localhost{target}")).unwrap();
        assert_eq!(
            url.path(),
            "/odata/Catalog_%D0%94%D0%B8%D0%B0%D0%B3%D0%BD%D0%BE%D0%B7%D1%8B"
        );
        let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(
            query.get("$select").unwrap(),
            "Ref_Key,КодДиагноза,Description,НаименованиеПолное,DeletionMark,IsFolder,Архив,DataVersion"
        );
        assert_eq!(query.get("$orderby").unwrap(), "Ref_Key");
        assert_eq!(query.get("$skip").unwrap(), "4");
        assert_eq!(query.get("$top").unwrap(), "2");
        assert!(!query.contains_key("$filter"));
    }

    #[tokio::test]
    async fn diagnoses_pages_fail_closed() {
        for body in [
            json!({"value":[diagnosis(ID),diagnosis(ID)]}),
            json!({"value":[diagnosis(ID),diagnosis(Uuid::from_u128(101))]}),
            json!({"value":[],"odata.nextLink":"http://other-host/secret"}),
            json!({"value":[],"@odata.nextLink":"http://other-host/secret"}),
            json!({"error":"missing envelope"}),
        ] {
            let (client, _) = mock("200 OK", "", serde_json::to_vec(&body).unwrap()).await;
            assert!(client.diagnoses_page(0, 1).await.is_err());
        }
        let (client, _) = mock(
            "200 OK",
            "",
            serde_json::to_vec(&json!({"value":[]})).unwrap(),
        )
        .await;
        assert!(
            client
                .diagnoses_page(0, 200)
                .await
                .unwrap()
                .next_skip
                .is_none()
        );
        // 1C GUID ordering need not match the textual UUID ordering.
        let rows: Vec<_> = (1..=200)
            .rev()
            .map(|id| diagnosis(Uuid::from_u128(id)))
            .collect();
        let (large_client, _) = mock(
            "200 OK",
            "",
            serde_json::to_vec(&json!({"value":rows})).unwrap(),
        )
        .await;
        assert_eq!(
            large_client.diagnoses_page(0, 200).await.unwrap().next_skip,
            Some(200)
        );
        for size in [0, 501] {
            assert_eq!(
                client.diagnoses_page(0, size).await.unwrap_err(),
                CoreError::InvalidPage
            );
        }
        let (client, _) = mock(
            "200 OK",
            "",
            serde_json::to_vec(&json!({"value":[diagnosis(ID)]})).unwrap(),
        )
        .await;
        assert_eq!(
            client.diagnoses_page(u32::MAX, 1).await.unwrap_err(),
            CoreError::InvalidPage
        );
    }

    #[tokio::test]
    async fn diagnoses_nullable_and_blank_source_fields_are_preserved() {
        for value in [Value::Null, json!(""), json!("  ")] {
            let mut row = diagnosis(ID);
            row["КодДиагноза"] = value.clone();
            row["Description"] = value;
            row["НаименованиеПолное"] = Value::Null;
            row["DataVersion"] = Value::Null;
            let (client, _) = mock(
                "200 OK",
                "",
                serde_json::to_vec(&json!({"value":[row]})).unwrap(),
            )
            .await;
            let page = client.diagnoses_page(0, 200).await.unwrap();
            let item = &page.items[0];
            assert_eq!(
                (
                    &*item.code,
                    &*item.name,
                    &*item.full_name,
                    &*item.source_version
                ),
                ("", "", "", "")
            );
            assert!(item.active && item.is_folder);
        }
        let mut row = diagnosis(ID);
        row["DataVersion"] = json!("x".repeat(256));
        let (client, _) = mock(
            "200 OK",
            "",
            serde_json::to_vec(&json!({"value":[row]})).unwrap(),
        )
        .await;
        assert_eq!(
            client.diagnoses_page(0, 200).await.unwrap().items[0]
                .source_version
                .len(),
            256
        );
    }

    #[tokio::test]
    async fn diagnoses_invalid_records_are_rejected() {
        for (field, value) in [
            ("Ref_Key", json!(Uuid::nil())),
            ("КодДиагноза", json!("x".repeat(33))),
            ("КодДиагноза", json!(42)),
            ("Description", json!(42)),
            ("Description", json!("x".repeat(4097))),
            ("НаименованиеПолное", json!("x".repeat(4097))),
            ("Description", json!("bad\nname")),
            ("DataVersion", json!("x".repeat(257))),
            ("DataVersion", json!(42)),
            ("DeletionMark", Value::Null),
            ("IsFolder", Value::Null),
            ("Архив", Value::Null),
        ] {
            let mut row = diagnosis(ID);
            row[field] = value;
            let (client, _) = mock(
                "200 OK",
                "",
                serde_json::to_vec(&json!({"value":[row]})).unwrap(),
            )
            .await;
            assert!(client.diagnoses_page(0, 200).await.is_err(), "{field}");
        }
        for field in ["DeletionMark", "IsFolder", "Архив"] {
            let mut row = diagnosis(ID);
            row.as_object_mut().unwrap().remove(field);
            let (client, _) = mock(
                "200 OK",
                "",
                serde_json::to_vec(&json!({"value":[row]})).unwrap(),
            )
            .await;
            assert!(client.diagnoses_page(0, 200).await.is_err(), "{field}");
        }
    }
}
