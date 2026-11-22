use serde_json::Value;
use uuid::Uuid;
pub struct HistoryState {
    pub study:Uuid,pub rows:Vec<Value>,pub next:Option<i64>,pub selected:Option<Uuid>,
    pub form:Option<gpui_kit::Entity<crate::native_form::NativeForm>>,pub metadata:Value,
    pub request:Option<(Uuid,Option<Uuid>)>,pub note:String,
    pub prefer_approved: bool,
    pub revisions_expanded: bool,
    pub show_photo: bool,
    pub editor_snapshot: Option<EditorSnapshot>,
    pub refresh_selected: Option<Uuid>,
    pub report_lines: Option<Result<Vec<String>, String>>,
    #[cfg(windows)]
    pub pdf_pages: Option<Result<HistoricalPdf, String>>,
    #[cfg(windows)]
    pub pdf_request: Option<Uuid>,
}
impl HistoryState {pub fn new(study:Uuid)->Self{Self{study,rows:vec![],next:None,selected:None,form:None,metadata:Value::Null,request:None,note:String::new(),prefer_approved:false,revisions_expanded:false,show_photo:false,editor_snapshot:None,refresh_selected:None,report_lines:None,
    #[cfg(windows)] pdf_pages:None,
    #[cfg(windows)] pdf_request:None,
}}}
pub struct EditorSnapshot {
    pub session:m53_gpui_editor::Session,
    pub selected:Option<Uuid>,
    pub export_patient:Option<(String,String)>,
    pub readonly:bool,
    pub historical_readonly:bool,
    pub zoom:f32,
    pub pan:(f32,f32),
}
impl HistoryState {
    pub fn busy(&self) -> bool {
        if self.request.is_some() {return true;}
        #[cfg(windows)]
        if self.pdf_request.is_some() {return true;}
        false
    }
}
fn historical_detail(
    study: Uuid, revision: Uuid, document: &Value,
    dictionary: &m53_domain::validation::Dictionary,
) -> Result<(m53_contracts::workflow::StudyDetail, String), String> {
    use m53_contracts::workflow::{Patient, Study, StudyDetail};
    use m53_client::report_identity;
    report_identity::validate_status(document, "approved")?;
    let performer = report_identity::performer(document, study, Some(revision))?;
    let protocol: m53_domain::protocol::Protocol = serde_json::from_value(document["protocol"].clone())
        .map_err(|_| "Некорректный протокол редакции")?;
    if protocol.terminology_version_id != dictionary.version_id {
        return Err("Словарь относится к другой редакции".into());
    }
    let annotations = serde_json::from_value(document["annotations"].clone())
        .map_err(|_| "Разметка редакции недоступна")?;
    // Missing legacy demographics remain unknown. The header shows only a saved snapshot.
    let patient = report_identity::patient(document).unwrap_or(Patient {
        id: Uuid::nil(), card_number: None, external_id: None, family_name: String::new(),
        given_name: String::new(), patronymic: None, birth_date: None, sex: None,
    });
    let detail = StudyDetail {
        study: Study { id: study, patient_id: patient.id, status: "approved".into(),
            row_version: 0, study_instance_uid: String::new() },
        patient, revision_id: Some(revision), protocol: Some(protocol),
        annotations, assets: vec![], mis_task_bound: false,
    };
    Ok((detail, performer))
}

#[cfg(windows)]
pub struct HistoricalPdf {
    pub bytes: std::sync::Arc<Vec<u8>>,
    pub pages: Vec<std::sync::Arc<gpui_kit::RenderImage>>,
}

#[cfg(windows)]
pub fn historical_pdf(
    study: Uuid, revision: Uuid, document: &Value,
    dictionary: &m53_domain::validation::Dictionary,
    template: &m53_contracts::workflow::Template,
    connection: &m53_client::connection::Connection,
) -> Result<HistoricalPdf, String> {
    let (mut detail, performer) = historical_detail(study, revision, document, dictionary)?;
    detail.assets = m53_client::report_identity::approved_assets(document, study)?
        .ok_or("Сервер не вернул снимки утверждённой редакции")?;
    let selected: Vec<_> = detail.assets.iter().map(|asset| asset.id).collect();
    let photos = m53_client::report_assets::load_selected(connection, &detail.assets, &selected)?;
    let equipment = m53_client::report_identity::equipment(document, None);
    let rendered = m53_client::report::with_equipment(
        &detail, &template.layout, &photos, &performer, &dictionary.terms, equipment, false,
    )?;
    let pages = m53_client::report::chromium::pages(&rendered.pdf)?
        .iter().map(|page| crate::storage::texture(page).map(|(_,_,image)|image))
        .collect::<Result<Vec<_>,_>>()?;
    Ok(HistoricalPdf { bytes: std::sync::Arc::new(rendered.pdf), pages })
}

pub fn historical_report(
    study: Uuid, revision: Uuid, document: &Value,
    dictionary: &m53_domain::validation::Dictionary,
) -> Result<Vec<String>, String> {
    use m53_contracts::workflow::{ReportBlock, ReportLayout};
    let (detail, performer) = historical_detail(study, revision, document, dictionary)?;
    let layout = ReportLayout {
        title: "Протокол кольпоскопии".into(),
        blocks: vec![ReportBlock::Indications, ReportBlock::History, ReportBlock::Assessment,
            ReportBlock::Findings, ReportBlock::Conclusion, ReportBlock::Recommendations],
        photo_columns: 1, letterhead: None,
    };
    m53_client::report::native_lines(&detail, &layout, &performer, &dictionary.terms)
}

pub fn latest_approved(rows: &[Value]) -> Option<Uuid> {
    let row = rows.iter().filter(|row| row["status"] == "approved")
        .max_by_key(|row| row["revision_number"].as_i64())?;
    Uuid::parse_str(row["revision_id"].as_str()?).ok()
}

pub fn timestamp(value:Option<&str>)->String {
    value.and_then(|s|chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d|d.with_timezone(&chrono::Local).format("%d.%m.%Y %H:%M:%S %:z").to_string()).unwrap_or_else(||"время не указано".into())
}
pub fn revision_label(row:&Value)->String {
    let number=row["revision_number"].as_i64().filter(|n|*n>0).map(|n|format!("Редакция {n}")).unwrap_or_else(||"Редакция ещё не сохранена".into());
    let status=if row["status"]=="approved"{"утверждена"}else{"черновик"};
    let mut text=format!("{number} · {status} · создана: {}",timestamp(row["created_at"].as_str()));
    if !row["approved_at"].is_null(){text.push_str(&format!(" · утверждена: {}",timestamp(row["approved_at"].as_str())));}
    text
}
pub fn history_rows(value:&Value,study:Uuid)->Result<Vec<Value>,String> {
    if value["study_id"]!=serde_json::json!(study){return Err("История относится к другому обследованию".into());}
    let rows=value["revisions"].as_array().ok_or("Некорректная история редакций")?;
    let mut ids=std::collections::BTreeSet::new();let mut numbers=std::collections::BTreeSet::new();
    if rows.len()>500 || rows.iter().any(|r|r["revision_id"].as_str().and_then(|s|Uuid::parse_str(s).ok()).filter(|id|!id.is_nil()).is_none_or(|id|!ids.insert(id))||r["revision_number"].as_i64().filter(|n|*n>0).is_none_or(|n|!numbers.insert(n))){return Err("История содержит некорректные или повторные редакции".into());}
    Ok(rows.clone())
}
#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;
    #[cfg(windows)]
    #[test]
    fn pending_historical_pdf_keeps_revision_requests_serialized() {
        let mut history=super::HistoryState::new(Uuid::new_v4());
        history.pdf_request=Some(Uuid::new_v4());
        assert!(history.busy(), "metadata completion must not reopen refresh while PDF renders");
        history.pdf_request=None;
        assert!(!history.busy());
        history.request=Some((Uuid::new_v4(),None));
        assert!(history.busy());
    }
    #[test]
    fn revision_number_is_not_the_study_cas_version_and_unknown_dates_stay_unknown(){
        let row=json!({"revision_number":2,"row_version":41,"created_at":null,"approved_at":null});
        assert!(super::revision_label(&row).contains("Редакция 2"));
        assert!(!super::revision_label(&row).contains("41"));
        assert!(super::revision_label(&row).contains("не указано"));
    }
    #[test]
    fn history_list_is_scoped_and_duplicate_revisions_are_rejected(){
        let study=Uuid::new_v4();let id=Uuid::new_v4();let row=json!({"revision_id":id,"revision_number":2});
        assert!(super::history_rows(&json!({"study_id":study,"revisions":[row.clone()]}),study).is_ok());
        assert!(super::history_rows(&json!({"study_id":Uuid::new_v4(),"revisions":[row.clone()]}),study).is_err());
        assert!(super::history_rows(&json!({"study_id":study,"revisions":[row.clone(),row]}),study).is_err());
    }
}

#[cfg(all(test,windows))]
#[test]
fn historical_snapshot_renders_to_pdf_pages_without_live_study() {
    use m53_contracts::workflow::{ReportBlock, ReportLayout, Template};
    let version=Uuid::new_v4();
    let study=Uuid::new_v4();
    let revision=Uuid::new_v4();
    let mut protocol=m53_client::model::blank_protocol(version);
    protocol.colpo=Some(Default::default());
    let dictionary=m53_domain::validation::Dictionary{version_id:version,clinically_approved:true,terms:vec![]};
    let template=Template{id:Uuid::new_v4(),version_id:Uuid::new_v4(),version:1,
        layout:ReportLayout{title:"Synthetic approved report".into(),blocks:vec![ReportBlock::Conclusion],photo_columns:1,letterhead:None}};
    let mut snapshot=serde_json::json!({"study_id":study,"revision_id":revision,"readonly":true,"status":"approved",
        "performer":{"display_name":"Original doctor"},"patient":null,"protocol":protocol,"annotations":[],
        "report_assets":[],"report_assets_source":"approval_selection"});
    let connection=m53_client::connection::Connection::new("http://127.0.0.1:9","synthetic").unwrap();
    assert!(!historical_pdf(study,revision,&snapshot,&dictionary,&template,&connection).unwrap().pages.is_empty());
    snapshot["status"]=serde_json::json!("draft");
    assert!(historical_pdf(study,revision,&snapshot,&dictionary,&template,&connection).is_err());
}
