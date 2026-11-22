//! Legacy local demographics and authoritative patient API values.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use uuid::Uuid;

/// Unknown sex is represented by None, never inferred from the examination or name.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatientSex {
    Female,
    Male,
}
impl PatientSex {
    pub fn wire(self) -> m53_contracts::colposcopy_1c::Sex {
        match self {
            Self::Female => m53_contracts::colposcopy_1c::Sex::F,
            Self::Male => m53_contracts::colposcopy_1c::Sex::M,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Female => "Женский",
            Self::Male => "Мужской",
        }
    }
}
/// An omitted field is a legacy response; an explicit null clears the old fallback.
/// This updates only the compatibility map, never a clinical record or queued request.
pub fn reconcile_legacy_sex(cache: &mut BTreeMap<Uuid, PatientSex>, id: Uuid, value: &Value) -> bool {
    match value.get("sex") {
        Some(Value::String(sex)) if sex == "F" => cache.insert(id, PatientSex::Female) != Some(PatientSex::Female),
        Some(Value::String(sex)) if sex == "M" => cache.insert(id, PatientSex::Male) != Some(PatientSex::Male),
        Some(Value::Null) => cache.remove(&id).is_some(),
        Some(Value::String(sex)) if sex == "O" => cache.remove(&id).is_some(),
        _ => false,
    }
}
pub fn display_sex(sex: Option<PatientSex>) -> &'static str {
    sex.map(PatientSex::label).unwrap_or("Не указан")
}
pub fn display_wire_sex(sex:Option<m53_contracts::colposcopy_1c::Sex>,fallback:Option<PatientSex>)->&'static str {
    use m53_contracts::colposcopy_1c::Sex;
    match sex{Some(Sex::F)=>"Женский",Some(Sex::M)=>"Мужской",Some(Sex::O)=>"Другой",None=>display_sex(fallback)}
}
#[cfg(test)]
mod tests {
    #[test]
    fn explicit_api_unknown_removes_legacy_fallback_but_omission_preserves_it() {
        use super::*;
        use serde_json::json;
        let id=Uuid::new_v4();
        let mut cache=BTreeMap::from([(id,PatientSex::Female)]);
        assert!(!reconcile_legacy_sex(&mut cache,id,&json!({})));
        assert_eq!(cache.get(&id),Some(&PatientSex::Female));
        assert!(reconcile_legacy_sex(&mut cache,id,&json!({"sex":null})));
        assert!(!cache.contains_key(&id));
        assert!(!reconcile_legacy_sex(&mut cache,id,&json!({"sex":null})));
        assert!(reconcile_legacy_sex(&mut cache,id,&json!({"sex":"M"})));
        assert_eq!(cache.get(&id),Some(&PatientSex::Male));
        assert!(reconcile_legacy_sex(&mut cache,id,&json!({"sex":"O"})));
        assert!(!cache.contains_key(&id));
    }
    #[test]
    fn server_sex_takes_precedence_over_legacy_cache_and_other_is_not_unknown(){
        use m53_contracts::colposcopy_1c::Sex;
        assert_eq!(super::display_wire_sex(Some(Sex::O),Some(super::PatientSex::Female)),"Другой");
        assert_eq!(super::display_wire_sex(Some(Sex::M),Some(super::PatientSex::Female)),"Мужской");
        assert_eq!(super::display_wire_sex(None,None),"Не указан");
    }
}
