//! Validated server permission for manual demographic editing.
use serde_json::Value;
use uuid::Uuid;
use m53_contracts::workflow::{NewPatient,Patient};

/// Capture the chosen value once, before the operation is added to the outbox.
/// Omission remains valid for patients created by older clients.
pub fn create_body(input: &NewPatient, sex: Option<crate::patient_demographics::PatientSex>) -> Result<Value, String> {
    let mut body = serde_json::to_value(input).map_err(|_| "Неверные данные пациента")?;
    if let Some(sex) = sex {
        body["sex"] = serde_json::json!(sex.wire());
    }
    Ok(body)
}

/// Validate the additional field without changing the frozen request or the legacy DTO.
pub fn create_input(body: &Value) -> Result<(NewPatient, Option<m53_contracts::colposcopy_1c::Sex>), String> {
    let mut demographics = body.clone();
    let fields = demographics.as_object_mut().ok_or("Неверные данные пациента")?;
    let sex = fields.remove("sex").map(serde_json::from_value).transpose()
        .map_err(|_| "Проверьте пол пациента")?.flatten();
    let input = serde_json::from_value(demographics).map_err(|_| "Неверные данные пациента")?;
    Ok((input, sex))
}

/// Old receipts did not include sex. New requests require its exact confirmation.
pub fn check_create_ack(ack: &Value, body: &Value) -> Result<(), String> {
    if let Some(sex) = body.get("sex") {
        let valid = sex.is_null() || matches!(sex.as_str(), Some("F" | "M" | "O"));
        if !valid || ack.get("sex") != Some(sex) {
            return Err("Сервер не подтвердил пол пациента. Локальные данные остаются в очереди".into());
        }
    }
    Ok(())
}

pub fn same_patient(input:&NewPatient,patient:&Patient)->bool {
    let normalize=|s:&str|s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let family=normalize(&input.family_name);let given=normalize(&input.given_name);
    !patient.id.is_nil() && !family.is_empty() && !given.is_empty()
        && input.birth_date.as_deref().is_some_and(|d|!d.is_empty()&&patient.birth_date.as_deref()==Some(d))
        && family==normalize(&patient.family_name) && given==normalize(&patient.given_name)
        && normalize(input.patronymic.as_deref().unwrap_or_default())==normalize(patient.patronymic.as_deref().unwrap_or_default())
}
pub fn edit_version(context:&Value,id:Uuid)->Result<i64,String> {
    if context["id"]!=serde_json::json!(id) || id.is_nil() || context["manual_edit_allowed"]!=true || context["origin"]!="manual" {
        return Err("Редактирование доступно только для пациента, заведённого вручную и не связанного с МИС".into());
    }
    context["row_version"].as_i64().filter(|v|*v>0).ok_or_else(||"Обновите карточку пациента перед изменением".into())
}
pub fn check_ack(ack:&Value,id:Uuid,body:&Value)->Result<(),String> {
    let version=body["expected_version"].as_i64().filter(|v|*v>0);
    if ack["id"]!=serde_json::json!(id) || version.is_none() || ack["row_version"].as_i64().is_none_or(|actual|version.is_none_or(|expected|actual<expected))
        || ["family_name","given_name","patronymic","birth_date","sex"].iter().any(|key|ack.get(*key).is_none()||body.get(*key).is_none()||ack[*key]!=body[*key]) {
        return Err("Сервер не подтвердил точные изменения пациента. Обновите карточку перед повтором".into());
    }
    Ok(())
}
pub fn prefill_manual_sex(context: &mut Value, local: Option<crate::patient_demographics::PatientSex>) {
    if context.get("sex").is_none() && let Some(sex) = local {
        context["sex"] = serde_json::json!(sex.wire());
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;
    fn new_patient() -> m53_contracts::workflow::NewPatient {
        serde_json::from_value(json!({"family_name":"Synthetic","given_name":"Patient","birth_date":"1998-01-20"})).unwrap()
    }
    #[test]
    fn patient_creation_captures_the_selected_sex_in_the_request() {
        use crate::patient_demographics::PatientSex;
        for (selected, wire) in [(PatientSex::Female,"F"),(PatientSex::Male,"M")] {
            let body=super::create_body(&new_patient(),Some(selected)).unwrap();
            assert_eq!(body["sex"],wire);
            let (input, parsed)=super::create_input(&body).unwrap();
            assert_eq!(input.family_name,"Synthetic");
            assert_eq!(parsed,Some(selected.wire()));
        }
        assert!(super::create_body(&new_patient(),None).unwrap().get("sex").is_none());
    }
    #[test]
    fn patient_creation_validates_sex_without_rewriting_legacy_requests() {
        let legacy=json!(new_patient());
        let before=legacy.clone();
        assert!(super::create_input(&legacy).unwrap().1.is_none());
        assert_eq!(legacy,before);
        for valid in [json!(null),json!("F"),json!("M"),json!("O")] {
            let mut body=legacy.clone();body["sex"]=valid;
            let frozen=body.clone();
            assert!(super::create_input(&body).is_ok());
            assert_eq!(body,frozen);
        }
        for invalid in [json!(""),json!("female"),json!(false),json!(1),json!([])] {
            let mut body=legacy.clone();body["sex"]=invalid;
            assert!(super::create_input(&body).is_err());
        }
        let mut unknown=legacy;unknown["unsupported"]=json!(true);
        assert!(super::create_input(&unknown).is_err());
    }
    #[test]
    fn new_creation_ack_must_confirm_sex_but_old_receipts_remain_usable() {
        let body=super::create_body(&new_patient(),Some(crate::patient_demographics::PatientSex::Female)).unwrap();
        assert!(super::check_create_ack(&json!({"sex":"F"}),&body).is_ok());
        for wrong in [json!({}),json!({"sex":null}),json!({"sex":"M"}),json!({"sex":"O"})] {
            assert!(super::check_create_ack(&wrong,&body).is_err());
        }
        let legacy=json!(new_patient());
        assert!(super::check_create_ack(&json!({}),&legacy).is_ok());
        let mut explicit_clear=legacy;explicit_clear["sex"]=json!(null);
        assert!(super::check_create_ack(&json!({}),&explicit_clear).is_err());
        assert!(super::check_create_ack(&json!({"sex":null}),&explicit_clear).is_ok());
    }
    #[test]
    fn duplicate_lookup_matches_complete_name_and_birth_date_for_either_origin() {
        let input=m53_contracts::workflow::NewPatient{family_name:" ИВАНОВА ".into(),given_name:"Анна".into(),patronymic:Some("Мария  Ивановна".into()),birth_date:Some("1998-01-20".into())};
        let mut row:m53_contracts::workflow::Patient=serde_json::from_value(json!({"id":Uuid::new_v4(),"family_name":"Иванова","given_name":"анна","patronymic":"Мария Ивановна","birth_date":"1998-01-20"})).unwrap();
        assert!(super::same_patient(&input,&row));
        row.external_id=Some(Uuid::new_v4().to_string());assert!(super::same_patient(&input,&row));
        row.birth_date=Some("1998-01-21".into());assert!(!super::same_patient(&input,&row));
        row.birth_date=input.birth_date.clone();row.patronymic=None;assert!(!super::same_patient(&input,&row));
    }
    #[test]
    fn legacy_sex_prefills_only_an_omitted_field_and_server_values_win() {
        use crate::patient_demographics::PatientSex;
        let id=Uuid::new_v4();
        let mut context=json!({"id":id,"row_version":3,"origin":"manual","manual_edit_allowed":true});
        super::prefill_manual_sex(&mut context,Some(PatientSex::Female));
        assert_eq!(context["sex"],"F");
        context["sex"]=json!("O");
        super::prefill_manual_sex(&mut context,Some(PatientSex::Female));
        assert_eq!(context["sex"],"O");
        context["sex"]=json!(null);
        super::prefill_manual_sex(&mut context,None);
        assert!(context["sex"].is_null());
    }
    #[test]
    fn explicit_server_sex_clear_is_not_replaced_with_legacy_local_value() {
        let mut context=json!({"sex":null});
        super::prefill_manual_sex(&mut context,Some(crate::patient_demographics::PatientSex::Female));
        assert!(context["sex"].is_null(),"authoritative null must remain unknown in the edit form");
    }
    #[test]
    fn manual_edit_permission_is_explicit_and_bound_to_patient_version() {
        let id=Uuid::new_v4();
        let mut context=json!({"id":id,"row_version":3,"origin":"manual","manual_edit_allowed":true});
        assert_eq!(super::edit_version(&context,id).unwrap(),3);
        context["manual_edit_allowed"]=json!(false);assert!(super::edit_version(&context,id).is_err());
        context["manual_edit_allowed"]=json!(true);context["origin"]=json!("1c");assert!(super::edit_version(&context,id).is_err());
        assert!(super::edit_version(&json!({"id":id,"external_id":null}),id).is_err());
    }
    #[test]
    fn update_ack_must_match_exact_patient_version_and_requested_demographics() {
        let id=Uuid::new_v4();let body=json!({"expected_version":3,"family_name":"А","given_name":"Б","patronymic":null,"birth_date":"2000-01-01","sex":"F"});
        let ack=json!({"id":id,"row_version":4,"family_name":"А","given_name":"Б","patronymic":null,"birth_date":"2000-01-01","sex":"F"});
        assert!(super::check_ack(&ack,id,&body).is_ok());
        let mut wrong=ack.clone();wrong["id"]=json!(Uuid::new_v4());assert!(super::check_ack(&wrong,id,&body).is_err());
        wrong=ack.clone();wrong["row_version"]=json!(3);assert!(super::check_ack(&wrong,id,&body).is_ok(),"точный no-op подтверждается без увеличения версии");
        wrong["row_version"]=json!(2);assert!(super::check_ack(&wrong,id,&body).is_err());
        wrong=ack;wrong["family_name"]=json!("Другой");assert!(super::check_ack(&wrong,id,&body).is_err());
    }
    #[test]
    fn explicit_nullable_clear_requires_an_explicit_ack_field(){
        let id=Uuid::new_v4();let body=json!({"expected_version":3,"family_name":"А","given_name":"Б","patronymic":null,"birth_date":null,"sex":null});
        let ack=json!({"id":id,"row_version":4,"family_name":"А","given_name":"Б","patronymic":null,"birth_date":null});
        assert!(super::check_ack(&ack,id,&body).is_err());
    }
}
