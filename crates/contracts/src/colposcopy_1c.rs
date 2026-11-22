//! The supplied 1C colposcopy wire contract. No invented revision or physician fields.
//! Validation errors contain paths/codes only; result secrets and PDF bodies are not Debug.
use m53_domain::validation::ValidationError;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use uuid::Uuid;

pub const MAX_PDF_BYTES: usize = 50 * 1024 * 1024;
pub const MAX_RESULT_JSON_BYTES: usize = 70 * 1024 * 1024;

fn error(path: &str, code: &str) -> ValidationError {
    ValidationError {
        path: path.into(),
        code: code.into(),
    }
}

/// A real proleptic Gregorian date, with exactly the YYYY-MM-DD wire spelling.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CalendarDate(String);
impl schemars::JsonSchema for CalendarDate {
    fn schema_name() -> String {
        "CalendarDate".into()
    }
    fn json_schema(_: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        let year = "(?:[0-9]{3}[1-9]|[0-9]{2}[1-9][0-9]|[0-9][1-9][0-9]{2}|[1-9][0-9]{3})";
        let leap =
            "(?:[0-9]{2}(?:0[48]|[2468][048]|[13579][26])|(?:0[48]|[2468][048]|[13579][26])00)";
        let pattern = format!(
            "^(?:{year}-(?:(?:01|03|05|07|08|10|12)-(?:0[1-9]|[12][0-9]|3[01])|(?:04|06|09|11)-(?:0[1-9]|[12][0-9]|30)|02-(?:0[1-9]|1[0-9]|2[0-8]))|{leap}-02-29)$"
        );
        serde_json::from_value(serde_json::json!({"type":"string","format":"date","minLength":10,"maxLength":10,"pattern":pattern,"description":"Real Gregorian date YYYY-MM-DD, years 0001–9999. Pattern enforces calendar validity even when format validation is disabled."})).unwrap()
    }
}
impl CalendarDate {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        let b = value.as_bytes();
        if b.len() != 10
            || b[4] != b'-'
            || b[7] != b'-'
            || b.iter()
                .enumerate()
                .any(|(i, b)| i != 4 && i != 7 && !b.is_ascii_digit())
        {
            return Err("invalid_date");
        }
        let year = value[..4].parse::<u32>().map_err(|_| "invalid_date")?;
        let month = value[5..7].parse::<u32>().map_err(|_| "invalid_date")?;
        let day = value[8..].parse::<u32>().map_err(|_| "invalid_date")?;
        let leap =
            year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
        let max = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if leap {
                    29
                } else {
                    28
                }
            }
            _ => 0,
        };
        if year == 0 || day == 0 || day > max {
            return Err("invalid_date");
        }
        Ok(Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl Serialize for CalendarDate {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}
impl<'de> Deserialize<'de> for CalendarDate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(D::Error::custom)
    }
}

/// Required upsert field. An empty wire string means unknown; null/missing are errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BirthDate(pub Option<CalendarDate>);
impl schemars::JsonSchema for BirthDate {
    fn schema_name() -> String {
        "BirthDate".into()
    }
    fn json_schema(g: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        serde_json::from_value(serde_json::json!({"description":"Required string: empty means unknown, otherwise a real YYYY-MM-DD date. Null and missing are forbidden.","anyOf":[{"type":"string","const":""},g.subschema_for::<CalendarDate>()]})).unwrap()
    }
}
impl Serialize for BirthDate {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.as_ref().map(CalendarDate::as_str).unwrap_or(""))
    }
}
impl<'de> Deserialize<'de> for BirthDate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.is_empty() {
            Ok(Self(None))
        } else {
            CalendarDate::parse(&value)
                .map(|d| Self(Some(d)))
                .map_err(D::Error::custom)
        }
    }
}

fn guid<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Uuid, D::Error> {
    let raw = String::deserialize(deserializer)?;
    let value = Uuid::parse_str(&raw).map_err(|_| D::Error::custom("invalid_guid"))?;
    if value.is_nil() || raw != value.hyphenated().to_string() {
        return Err(D::Error::custom("invalid_guid"));
    }
    Ok(value)
}
fn guid_schema(_: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
    serde_json::from_value(serde_json::json!({"type":"string","format":"uuid","pattern":"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$","not":{"const":"00000000-0000-0000-0000-000000000000"}})).unwrap()
}
fn check_guid(errors: &mut Vec<ValidationError>, path: &str, value: Uuid) {
    if value.is_nil() {
        errors.push(error(path, "nil_id"));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Upsert,
    Cancel,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum Sex {
    F,
    M,
    O,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpsertTask {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_number: Option<String>,
    #[serde(deserialize_with = "guid")]
    #[schemars(schema_with = "guid_schema")]
    pub application_key: Uuid,
    /// GUID of the 1C application, equal to application_key. Equality is checked by the server.
    #[serde(deserialize_with = "guid")]
    #[schemars(schema_with = "guid_schema")]
    pub task_name: Uuid,
    pub appointment_date: CalendarDate,
    #[serde(deserialize_with = "guid")]
    #[schemars(schema_with = "guid_schema")]
    pub patient_key: Uuid,
    pub last_name: String,
    pub first_name: String,
    pub middle_name: String,
    pub birth_date: BirthDate,
    pub sex: Sex,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelTask {
    #[serde(deserialize_with = "guid")]
    #[schemars(schema_with = "guid_schema")]
    pub application_key: Uuid,
    /// GUID of the 1C application, equal to application_key. Equality is checked by the server.
    #[serde(deserialize_with = "guid")]
    #[schemars(schema_with = "guid_schema")]
    pub task_name: Uuid,
    pub appointment_date: CalendarDate,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskRequest {
    Upsert(UpsertTask),
    Cancel(CancelTask),
}
impl TaskRequest {
    pub fn operation(&self) -> Operation {
        match self {
            Self::Upsert(_) => Operation::Upsert,
            Self::Cancel(_) => Operation::Cancel,
        }
    }
    pub fn application_key(&self) -> Uuid {
        match self {
            Self::Upsert(t) => t.application_key,
            Self::Cancel(t) => t.application_key,
        }
    }
    pub fn task_name(&self) -> Uuid {
        match self {
            Self::Upsert(t) => t.task_name,
            Self::Cancel(t) => t.task_name,
        }
    }
    pub fn appointment_date(&self) -> &CalendarDate {
        match self {
            Self::Upsert(t) => &t.appointment_date,
            Self::Cancel(t) => &t.appointment_date,
        }
    }
    pub fn validate(&self) -> Vec<ValidationError> {
        let mut errors = vec![];
        check_guid(&mut errors, "application_key", self.application_key());
        check_guid(&mut errors, "task_name", self.task_name());
        if self.task_name() != self.application_key() {
            errors.push(error("task_name", "must_equal_application_key"));
        }
        if let Self::Upsert(t) = self {
            check_guid(&mut errors, "patient_key", t.patient_key);
        }
        errors
    }
    /// Feed these bytes to the server's existing SHA-256 helper. Object key order and
    /// JSON whitespace do not alter the digest; patient strings are never guessed/trimmed.
    pub fn normalized_body(&self) -> Result<Vec<u8>, serde_json::Error> {
        crate::canonical_json::to_vec(&serde_json::to_value(self)?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskAck {
    pub success: bool,
    #[serde(deserialize_with = "guid")]
    #[schemars(schema_with = "guid_schema")]
    pub application_key: Uuid,
    pub operation: Operation,
    pub appointment_date: CalendarDate,
}
impl TaskAck {
    /// Construct only after the request and this response have committed durably.
    pub fn accepted(request: &TaskRequest) -> Self {
        Self {
            success: true,
            application_key: request.application_key(),
            operation: request.operation(),
            appointment_date: request.appointment_date().clone(),
        }
    }
    pub fn validate_for(&self, http_status: u16, request: &TaskRequest) -> Vec<ValidationError> {
        let mut errors = vec![];
        if !matches!(http_status, 200 | 201) {
            errors.push(error("http_status", "not_acknowledged"));
        }
        if !self.success {
            errors.push(error("success", "not_acknowledged"));
        }
        if self.application_key.is_nil() || self.application_key != request.application_key() {
            errors.push(error("application_key", "receipt_mismatch"));
        }
        if self.operation != request.operation() {
            errors.push(error("operation", "receipt_mismatch"));
        }
        if self.appointment_date != *request.appointment_date() {
            errors.push(error("appointment_date", "receipt_mismatch"));
        }
        errors
    }
}

/// API key of the 1C ColposcopyREST_IN receiver; sent only in X-API-Key.
/// Deliberately has no Debug or Display implementation.
#[derive(Clone, PartialEq, Eq)]
pub struct ResultKey(String);
impl schemars::JsonSchema for ResultKey {
    fn schema_name() -> String {
        "ResultKey".into()
    }
    fn json_schema(_: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        serde_json::from_value(serde_json::json!({"type":"string","minLength":32,"maxLength":32}))
            .unwrap()
    }
}
impl ResultKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn new(value: String) -> Result<Self, ValidationError> {
        if value.chars().count() != 32 {
            return Err(error("Key", "invalid_key_length"));
        }
        Ok(Self(value))
    }
}
impl Serialize for ResultKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}
impl<'de> Deserialize<'de> for ResultKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?)
            .map_err(|_| D::Error::custom("invalid_key_length"))
    }
}

/// Frozen catalog snapshots in protocol order; diagnosis_id is the 1C Ref_Key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResultDiagnosis {
    #[serde(deserialize_with = "guid")]
    #[schemars(schema_with = "guid_schema")]
    pub diagnosis_id: Uuid,
    #[schemars(schema_with = "diagnosis_code_schema")]
    pub code: String,
    #[schemars(schema_with = "diagnosis_name_schema")]
    pub name: String,
}
fn diagnosis_text_schema(max: usize) -> schemars::schema::Schema {
    serde_json::from_value(serde_json::json!({"type":"string","minLength":1,"maxLength":max,"pattern":r"\S","description":format!("Nonblank frozen snapshot; maximum {max} UTF-8 bytes (validated by sender/receiver)." )})).unwrap()
}
fn diagnosis_code_schema(_: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
    diagnosis_text_schema(m53_domain::protocol::DIAGNOSIS_CODE_MAX_BYTES)
}
fn diagnosis_name_schema(_: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
    diagnosis_text_schema(m53_domain::protocol::DIAGNOSIS_NAME_MAX_BYTES)
}
fn result_diagnoses_schema(g: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
    serde_json::from_value(serde_json::json!({"type":"array","minItems":1,"maxItems":m53_domain::protocol::DIAGNOSES_MAX_ROWS,"items":g.subschema_for::<ResultDiagnosis>()})).unwrap()
}

/// Secret-bearing wire payload: intentionally no Debug implementation.
#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResultRequest {
    #[serde(deserialize_with = "guid")]
    #[schemars(schema_with = "guid_schema")]
    pub application_key: Uuid,
    pub appointment_date: CalendarDate,
    #[serde(deserialize_with = "guid")]
    #[schemars(schema_with = "guid_schema")]
    pub patient_key: Uuid,
    /// application_key followed by .pdf; the relationship is checked by the sender/receiver.
    #[schemars(schema_with = "pdf_filename_schema")]
    pub file_name: String,
    /// Base64 PDF bytes without data: prefix; decoded PDF limit 50 MiB and JSON limit 70 MiB.
    #[schemars(schema_with = "pdf_base64_schema")]
    pub pdf_base64: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(schema_with = "result_diagnoses_schema")]
    pub diagnoses: Vec<ResultDiagnosis>,
}
fn pdf_base64_schema(_: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
    serde_json::from_value(serde_json::json!({"type":"string","minLength":1,"maxLength":4*MAX_PDF_BYTES.div_ceil(3),"pattern":"^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$","not":{"pattern":"[^A-Za-z0-9+/=]"},"contentEncoding":"base64","contentMediaType":"application/pdf","description":"Base64 of exact PDF bytes, no data: prefix; decoded size 1–52428800 bytes. Receiver verifies PDF signature and decoded contents."})).unwrap()
}
fn pdf_filename_schema(_: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
    serde_json::from_value(serde_json::json!({"type":"string","minLength":40,"maxLength":40,"pattern":r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\.pdf$","description":"Must equal application_key followed by .pdf; equality is checked outside JSON Schema."})).unwrap()
}
impl ResultRequest {
    /// PDF byte/signature and Base64 canonicality checks belong to the sender using
    /// its existing codec; pass the verified decoded byte count here before sending.
    pub fn validate(&self, verified_pdf_bytes: usize) -> Vec<ValidationError> {
        let mut errors = vec![];
        use m53_domain::protocol::{
            DIAGNOSES_MAX_ROWS, DIAGNOSIS_CODE_MAX_BYTES, DIAGNOSIS_NAME_MAX_BYTES,
        };
        if self.diagnoses.len() > DIAGNOSES_MAX_ROWS {
            errors.push(error("diagnoses", "too_many_diagnoses"));
        }
        for (i, row) in self.diagnoses.iter().enumerate() {
            check_guid(
                &mut errors,
                &format!("diagnoses[{i}].diagnosis_id"),
                row.diagnosis_id,
            );
            for (key, text, limit) in [
                ("code", row.code.as_str(), DIAGNOSIS_CODE_MAX_BYTES),
                ("name", row.name.as_str(), DIAGNOSIS_NAME_MAX_BYTES),
            ] {
                let path = format!("diagnoses[{i}].{key}");
                if text.trim().is_empty() {
                    errors.push(error(&path, "empty_value"));
                }
                if text.len() > limit {
                    errors.push(error(&path, "text_too_long"));
                }
            }
        }
        check_guid(&mut errors, "application_key", self.application_key);
        check_guid(&mut errors, "patient_key", self.patient_key);
        if self.file_name != format!("{}.pdf", self.application_key) {
            errors.push(error("file_name", "invalid_file_name"));
        }
        if verified_pdf_bytes == 0 {
            errors.push(error("pdf_base64", "empty_pdf"));
        }
        if verified_pdf_bytes > MAX_PDF_BYTES {
            errors.push(error("pdf_base64", "pdf_too_large"));
        }
        if self.pdf_base64.is_empty() {
            errors.push(error("pdf_base64", "empty_pdf"));
        }
        errors
    }
    pub fn validate_json_size(byte_size: usize) -> Vec<ValidationError> {
        if byte_size > MAX_RESULT_JSON_BYTES {
            vec![error("body", "json_too_large")]
        } else {
            vec![]
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResultAction {
    Created,
    Attached,
    Replaced,
    Unchanged,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResultReceipt {
    pub success: bool,
    #[serde(deserialize_with = "guid")]
    #[schemars(schema_with = "guid_schema")]
    pub application_key: Uuid,
    #[serde(deserialize_with = "guid")]
    #[schemars(schema_with = "guid_schema")]
    pub reception_ref_key: Uuid,
    pub reception_number: String,
    #[serde(deserialize_with = "guid")]
    #[schemars(schema_with = "guid_schema")]
    pub file_ref_key: Uuid,
    pub action: ResultAction,
}
impl ResultReceipt {
    pub fn validate_for(&self, http_status: u16, application_key: Uuid) -> Vec<ValidationError> {
        let mut errors = vec![];
        if !matches!(http_status, 200 | 201) {
            errors.push(error("http_status", "not_acknowledged"));
        }
        if !self.success {
            errors.push(error("success", "not_acknowledged"));
        }
        if application_key.is_nil() || self.application_key != application_key {
            errors.push(error("application_key", "receipt_mismatch"));
        }
        check_guid(&mut errors, "application_key", self.application_key);
        check_guid(&mut errors, "reception_ref_key", self.reception_ref_key);
        check_guid(&mut errors, "file_ref_key", self.file_ref_key);
        if self.reception_number.trim().is_empty() {
            errors.push(error("reception_number", "empty"));
        }
        errors
    }
}
/// Safe error envelope returned by the 1C result endpoint; do not add the request body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResultFailure {
    pub success: bool,
    pub error_code: String,
    pub diagnostic_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    fn task(operation: Operation) -> Value {
        let mut value = json!({"operation":operation,"application_key":Uuid::from_u128(1),"task_name":Uuid::from_u128(1),"appointment_date":"2028-02-29"});
        if operation == Operation::Upsert {
            for (key, field) in json!({"patient_key":Uuid::from_u128(3),"last_name":"","first_name":"","middle_name":"","birth_date":"","sex":"O"}).as_object().unwrap() { value[key] = field.clone(); }
        }
        value
    }
    #[test]
    fn task_name_is_the_application_guid() {
        let mut body = task(Operation::Upsert);
        body["task_name"] = json!(Uuid::from_u128(2));
        let request: TaskRequest = serde_json::from_value(body).unwrap();
        assert!(
            request
                .validate()
                .iter()
                .any(|e| e.path == "task_name" && e.code == "must_equal_application_key")
        );
    }
    #[test]
    fn exact_upsert_and_minimal_cancel_have_different_required_fields() {
        let value = task(Operation::Upsert);
        let request: TaskRequest = serde_json::from_value(value.clone()).unwrap();
        assert!(request.validate().is_empty());
        let TaskRequest::Upsert(ref upsert) = request else {
            panic!()
        };
        assert!(upsert.birth_date.0.is_none());
        assert!(upsert.last_name.is_empty());
        assert_eq!(serde_json::to_value(&request).unwrap(), value);
        for key in [
            "patient_key",
            "last_name",
            "first_name",
            "middle_name",
            "birth_date",
            "sex",
        ] {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(key);
            assert!(
                serde_json::from_value::<TaskRequest>(missing).is_err(),
                "required: {key}"
            );
        }
        let cancel: TaskRequest = serde_json::from_value(task(Operation::Cancel)).unwrap();
        assert!(matches!(cancel, TaskRequest::Cancel(_)));
        assert!(cancel.validate().is_empty());
    }
    #[test]
    fn invalid_guid_enum_unknown_fields_and_invalid_dates_are_rejected() {
        for (key, bad) in [
            ("application_key", json!(Uuid::nil())),
            ("task_name", json!("not-a-guid")),
            ("patient_key", json!(3)),
            ("birth_date", Value::Null),
            ("appointment_date", json!("2027-02-29")),
            ("operation", json!("delete")),
            ("sex", json!("female")),
            ("source_revision", json!(1)),
        ] {
            let mut value = task(Operation::Upsert);
            value[key] = bad;
            assert!(
                serde_json::from_value::<TaskRequest>(value).is_err(),
                "field: {key}"
            );
        }
        let mut full = task(Operation::Cancel);
        full["patient_key"] = json!(Uuid::from_u128(3));
        assert!(serde_json::from_value::<TaskRequest>(full).is_err());
        for date in [
            "0000-01-01",
            "1900-02-29",
            "2028-04-31",
            "2028-1-01",
            "2028-01-01T00:00:00Z",
            "２０２８-01-01",
        ] {
            assert!(CalendarDate::parse(date).is_err());
        }
        assert!(CalendarDate::parse("2000-02-29").is_ok());
        let raw = Uuid::from_u128(0xabcdef).to_string();
        for id in [raw.to_uppercase(), raw.replace('-', "")] {
            let mut value = task(Operation::Cancel);
            value["application_key"] = json!(id);
            assert!(serde_json::from_value::<TaskRequest>(value).is_err());
        }
    }
    #[test]
    fn normalized_body_ignores_json_order_but_keeps_clinical_content() {
        let value = task(Operation::Upsert);
        let first: TaskRequest = serde_json::from_value(value.clone()).unwrap();
        let reversed = value
            .as_object()
            .unwrap()
            .iter()
            .rev()
            .map(|(key, value)| format!("{key:?}: {value}"))
            .collect::<Vec<_>>()
            .join(",\n");
        let second: TaskRequest = serde_json::from_str(&format!("{{\n{reversed}\n}}")).unwrap();
        assert_eq!(
            first.normalized_body().unwrap(),
            second.normalized_body().unwrap()
        );
        let mut changed = value;
        changed["last_name"] = json!("Тест");
        let changed: TaskRequest = serde_json::from_value(changed).unwrap();
        assert_ne!(
            first.normalized_body().unwrap(),
            changed.normalized_body().unwrap()
        );
    }
    #[test]
    fn acknowledgement_requires_exact_task_operation_date_and_200_or_201() {
        let request: TaskRequest = serde_json::from_value(task(Operation::Cancel)).unwrap();
        let ack = TaskAck::accepted(&request);
        assert!(ack.validate_for(200, &request).is_empty());
        assert!(ack.validate_for(201, &request).is_empty());
        for status in [202, 204, 301, 400, 500] {
            assert!(!ack.validate_for(status, &request).is_empty());
        }
        let mut bad = ack.clone();
        bad.success = false;
        assert!(!bad.validate_for(200, &request).is_empty());
        let mut bad = ack.clone();
        bad.application_key = Uuid::from_u128(4);
        assert!(!bad.validate_for(200, &request).is_empty());
        let mut bad = ack.clone();
        bad.operation = Operation::Upsert;
        assert!(!bad.validate_for(200, &request).is_empty());
        let mut bad = ack;
        bad.appointment_date = CalendarDate::parse("2028-03-01").unwrap();
        assert!(!bad.validate_for(200, &request).is_empty());
    }
    #[test]
    fn result_diagnoses_schema_matches_optional_nonempty_wire() {
        let schema = serde_json::to_value(schemars::schema_for!(ResultRequest)).unwrap();
        assert!(
            !schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("diagnoses"))
        );
        let diagnoses = &schema["properties"]["diagnoses"];
        assert_eq!(diagnoses["type"], "array");
        assert_eq!(diagnoses["minItems"], 1);
        assert_eq!(diagnoses["maxItems"], 100);
        let row = &schema["definitions"]["ResultDiagnosis"];
        assert_eq!(row["additionalProperties"], false);
        assert_eq!(row["required"], json!(["code", "diagnosis_id", "name"]));
        assert_eq!(row["properties"].as_object().unwrap().len(), 3);
        assert_eq!(row["properties"]["code"]["pattern"], r"\S");
        assert_eq!(
            row["properties"]["diagnosis_id"]["not"]["const"],
            Uuid::nil().to_string()
        );
    }
    fn result_wire() -> serde_json::Value {
        json!({"application_key":Uuid::from_u128(1),"appointment_date":"2028-02-29","patient_key":Uuid::from_u128(3),"file_name":format!("{}.pdf",Uuid::from_u128(1)),"pdf_base64":"JVBERi0xLjcK"})
    }
    #[test]
    fn result_diagnoses_are_optional_ordered_and_strict() {
        let legacy = result_wire();
        let parsed: ResultRequest = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), legacy);
        let mut empty = legacy.clone();
        empty["diagnoses"] = json!([]);
        let parsed: ResultRequest = serde_json::from_value(empty).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), legacy);
        let rows = json!([
            {"diagnosis_id":Uuid::from_u128(9),"code":"N87.1","name":"Первый"},
            {"diagnosis_id":Uuid::from_u128(8),"code":"N87.0","name":"Второй"}
        ]);
        let mut populated = legacy.clone();
        populated["diagnoses"] = rows.clone();
        let parsed: ResultRequest = serde_json::from_value(populated.clone()).unwrap();
        assert!(parsed.validate(9).is_empty());
        assert_eq!(serde_json::to_value(parsed).unwrap(), populated);
        for bad in [json!(null), json!({}), json!("invalid")] {
            let mut value = legacy.clone();
            value["diagnoses"] = bad;
            assert!(serde_json::from_value::<ResultRequest>(value).is_err());
        }
        for key in ["id", "additional_text", "notes", "Ref_Key"] {
            let mut value = populated.clone();
            value["diagnoses"][0][key] = json!("forbidden");
            assert!(serde_json::from_value::<ResultRequest>(value).is_err());
        }
        for bad in [json!(Uuid::nil()), json!("invalid")] {
            let mut value = populated.clone();
            value["diagnoses"][0]["diagnosis_id"] = bad;
            assert!(serde_json::from_value::<ResultRequest>(value).is_err());
        }
    }
    #[test]
    fn result_diagnoses_validate_count_and_utf8_snapshot_limits() {
        use m53_domain::protocol::{
            DIAGNOSES_MAX_ROWS, DIAGNOSIS_CODE_MAX_BYTES, DIAGNOSIS_NAME_MAX_BYTES,
        };
        let mut value = result_wire();
        let row = json!({"diagnosis_id":Uuid::from_u128(9),"code":"A","name":"Название"});
        value["diagnoses"] = json!(vec![row.clone(); DIAGNOSES_MAX_ROWS]);
        let parsed: ResultRequest = serde_json::from_value(value.clone()).unwrap();
        assert!(parsed.validate(9).is_empty());
        value["diagnoses"] = json!(vec![row.clone(); DIAGNOSES_MAX_ROWS + 1]);
        let parsed: ResultRequest = serde_json::from_value(value).unwrap();
        assert!(parsed.validate(9).iter().any(|e| e.path == "diagnoses"));
        for (key, limit) in [
            ("code", DIAGNOSIS_CODE_MAX_BYTES),
            ("name", DIAGNOSIS_NAME_MAX_BYTES),
        ] {
            for (text, valid) in [
                ("я".repeat(limit / 2), true),
                ("я".repeat(limit / 2 + 1), false),
                (" ".into(), false),
            ] {
                let mut value = result_wire();
                value["diagnoses"] = json!([row.clone()]);
                value["diagnoses"][0][key] = json!(text);
                let parsed: ResultRequest = serde_json::from_value(value).unwrap();
                assert_eq!(parsed.validate(9).is_empty(), valid, "{key}");
            }
        }
    }
    #[test]
    fn result_key_spelling_limits_and_receipts_are_validated() {
        let request = ResultRequest {
            application_key: Uuid::from_u128(1),
            appointment_date: CalendarDate::parse("2028-02-29").unwrap(),
            patient_key: Uuid::from_u128(3),
            file_name: format!("{}.pdf", Uuid::from_u128(1)),
            pdf_base64: "JVBERi0xLjcK".into(),
            diagnoses: vec![],
        };
        assert!(request.validate(9).is_empty());
        let json = serde_json::to_value(&request).unwrap();
        assert!(json.get("Key").is_none());
        assert!(json.get("key").is_none());
        assert!(ResultKey::new("x".repeat(31)).is_err());
        assert!(!request.validate(MAX_PDF_BYTES + 1).is_empty());
        assert!(ResultRequest::validate_json_size(MAX_RESULT_JSON_BYTES).is_empty());
        assert!(!ResultRequest::validate_json_size(MAX_RESULT_JSON_BYTES + 1).is_empty());
        let mut bad = json;
        bad["Key"] = serde_json::json!("x".repeat(32));
        assert!(serde_json::from_value::<ResultRequest>(bad).is_err());
        let receipt = ResultReceipt {
            success: true,
            application_key: request.application_key,
            reception_ref_key: Uuid::from_u128(4),
            reception_number: "test".into(),
            file_ref_key: Uuid::from_u128(5),
            action: ResultAction::Unchanged,
        };
        assert!(
            receipt
                .validate_for(200, request.application_key)
                .is_empty()
        );
        assert!(
            !receipt
                .validate_for(202, request.application_key)
                .is_empty()
        );
        assert!(!receipt.validate_for(200, Uuid::from_u128(9)).is_empty());
        let mut value = serde_json::to_value(receipt).unwrap();
        value["action"] = json!("updated");
        assert!(serde_json::from_value::<ResultReceipt>(value).is_err());
    }
}
