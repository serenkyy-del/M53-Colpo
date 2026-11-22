//! Report attribution is bound to the study and selected revision, never the viewer.
use serde_json::Value;
use uuid::Uuid;

pub fn performer(context: &Value, study: Uuid, revision: Option<Uuid>) -> Result<String,String> {
    let id=|key:&str|context[key].as_str().and_then(|v|Uuid::parse_str(v).ok());
    if study.is_nil() || id("study_id")!=Some(study) || id("revision_id")!=revision {
        return Err("Данные отчёта относятся к другой редакции. Обновите обследование".into());
    }
    Ok(context["performer"]["display_name"].as_str().map(str::trim).filter(|s|!s.is_empty()).unwrap_or("Не указан").to_owned())
}

pub fn load(connection: &crate::connection::Connection, detail: &m53_contracts::workflow::StudyDetail) -> Result<(Value,String),String> {
    let suffix=detail.revision_id.map(|id|format!("?revision_id={id}")).unwrap_or_default();
    let path = format!("/api/v1/studies/{}/report-context{suffix}", detail.study.id);
    let context: Value = if detail.study.status == "approved"
        && let Some(offline) = &connection.offline
    {
        match offline.cached(&path).filter(|context| {
            context.get("report_assets").is_some()
                && validate_status(context, &detail.study.status).is_ok()
                && performer(context, detail.study.id, detail.revision_id).is_ok()
        }) {
            Some(context) => context,
            None => offline.read_live_value(&path, "данные утверждённого отчёта")?,
        }
    } else {
        connection.get(&path)?
    };
    let name=performer(&context,detail.study.id,detail.revision_id)?;
    validate_status(&context,&detail.study.status)?;
    Ok((context,name))
}
pub fn validate_status(context:&Value,status:&str)->Result<(),String>{
    if context["status"]!=status || (status=="approved"&&context["readonly"]!=true){return Err("Сведения редакции ещё обновляются. Повторите предпросмотр после подтверждения сервера".into());}
    Ok(())
}
pub fn patient(context: &Value) -> Result<m53_contracts::workflow::Patient,String> {
    let patient:m53_contracts::workflow::Patient=serde_json::from_value(context["patient"].clone()).map_err(|_|"Нет подтверждённых данных пациента для этой редакции".to_string())?;
    if patient.id.is_nil() {return Err("Некорректный пациент в данных редакции".into());}
    Ok(patient)
}
pub fn bind(detail:&m53_contracts::workflow::StudyDetail,context:&Value)->Result<(m53_contracts::workflow::StudyDetail,String),String>{
    let name=performer(context,detail.study.id,detail.revision_id)?;
    validate_status(context,&detail.study.status)?;
    let mut bound=detail.clone();bound.patient=patient(context)?;
    Ok((bound,name))
}

pub fn equipment<'a>(context: &'a Value, local_draft_model: Option<&'a str>) -> Option<&'a str> {
    let model = match context.get("colposcope_model") {
        Some(value) => value.as_str(),
        None if context["status"] == "draft" => local_draft_model,
        None => None,
    };
    model.map(str::trim).filter(|model| !model.is_empty())
}

pub fn approved_assets(context: &Value, study: Uuid) -> Result<Option<Vec<m53_contracts::workflow::Asset>>, String> {
    let Some(raw) = context.get("report_assets") else { return Ok(None) };
    validate_status(context, "approved")?;
    let assets: Vec<m53_contracts::workflow::Asset> = serde_json::from_value(raw.clone())
        .map_err(|_| "Некорректный список снимков утверждённой редакции")?;
    let mut ids = std::collections::BTreeSet::new();
    if !matches!(context["report_assets_source"].as_str(), Some("approval_selection" | "legacy_before_approval"))
        || context["study_id"] != serde_json::json!(study) || assets.iter().any(|asset| {
        asset.study_id != study || asset.kind != "original" || asset.id.is_nil() || !ids.insert(asset.id)
    }) {
        return Err("Список снимков утверждённой редакции не прошёл проверку".into());
    }
    Ok(Some(assets))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;
    #[test]
    fn performer_is_independent_of_editor_approver_and_viewer() {
        let study=Uuid::new_v4();let revision=Uuid::new_v4();
        let context=json!({"study_id":study,"revision_id":revision,"performer":{"display_name":"Исследователь А"},"author":{"display_name":"Редактор Б"},"approver":{"display_name":"Врач В"}});
        assert_eq!(super::performer(&context,study,Some(revision)).unwrap(),"Исследователь А");
        assert!(super::performer(&context,Uuid::new_v4(),Some(revision)).is_err());
        assert!(super::performer(&context,study,Some(Uuid::new_v4())).is_err());
    }
    #[test]
    fn unknown_legacy_performer_does_not_become_current_doctor() {
        let study=Uuid::new_v4();let revision=Uuid::new_v4();
        let context=json!({"study_id":study,"revision_id":revision,"performer":null});
        assert_eq!(super::performer(&context,study,Some(revision)).unwrap(),"Не указан");
    }
    #[test]
    fn report_patient_comes_from_selected_revision_and_missing_snapshot_is_not_inferred() {
        let id=Uuid::new_v4();
        let context=json!({"patient":{"id":id,"family_name":"Сохранённая","given_name":"А","patronymic":null,"birth_date":"2000-01-01"}});
        assert_eq!(super::patient(&context).unwrap().family_name,"Сохранённая");
        assert!(super::patient(&json!({})).is_err());
    }
    #[test]
    fn equipment_uses_recorded_model_and_never_guesses_for_approved_records() {
        assert_eq!(super::equipment(&json!({"status":"approved","colposcope_model":"Model A"}), Some("Model B")), Some("Model A"));
        assert_eq!(super::equipment(&json!({"status":"approved","colposcope_model":null}), Some("Model B")), None);
        assert_eq!(super::equipment(&json!({"status":"approved"}), Some("Model B")), None);
        assert_eq!(super::equipment(&json!({"status":"draft","colposcope_model":null}), Some("Model B")), None);
        assert_eq!(super::equipment(&json!({"status":"draft"}), Some("Model B")), Some("Model B"));
    }
    #[test]
    fn approved_photo_manifest_distinguishes_empty_missing_and_other_studies() {
        let study=Uuid::new_v4();
        assert!(super::approved_assets(&json!({"status":"approved"}), study).unwrap().is_none());
        let empty=json!({"study_id":study,"status":"approved","readonly":true,"report_assets_source":"approval_selection","report_assets":[]});
        assert!(super::approved_assets(&empty, study).unwrap().unwrap().is_empty());
        let mut foreign=empty;
        foreign["report_assets"]=json!([{"id":Uuid::new_v4(),"study_id":Uuid::new_v4(),
            "kind":"original","sha256":"a".repeat(64),"byte_size":12,"mime_type":"image/jpeg"}]);
        assert!(super::approved_assets(&foreign, study).is_err());
    }
    #[test]
    fn approved_report_never_uses_a_cached_draft_context(){
        assert!(super::validate_status(&json!({"status":"draft","readonly":false}),"approved").is_err());
        assert!(super::validate_status(&json!({"status":"approved","readonly":true}),"approved").is_ok());
        assert!(super::validate_status(&json!({"status":"approved","readonly":false}),"approved").is_err());
    }
}
