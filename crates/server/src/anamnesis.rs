//! Compatibility is limited to a genuinely absent legacy history field.
use serde_json::Value;
pub(crate) fn snapshot_matches(snapshot: Value, stored: &Value) -> bool {
    let mut stored = stored.clone();
    if let Some(form) = stored.as_object_mut() {
        form.entry("history").or_insert_with(|| serde_json::to_value(m53_domain::protocol::empty_history()).unwrap());
    }
    snapshot == stored
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn common_note_snapshot_never_relaxes_text_presence_or_explicit_clear() {
        let stored=json!({"history":m53_domain::protocol::empty_history(),"diagnosis_note":{"value":{"state":"assessed","value":"exact\ntext"},"origin":{"kind":"physician"}}});
        assert!(snapshot_matches(stored.clone(),&stored));
        for note in [Value::Null,json!(m53_domain::protocol::empty_history()),json!({"value":{"state":"assessed","value":"different"},"origin":{"kind":"physician"}})] {
            let mut changed=stored.clone();changed["diagnosis_note"]=note;
            assert!(!snapshot_matches(changed,&stored));
        }
        let mut omitted=stored.clone();omitted.as_object_mut().unwrap().remove("diagnosis_note");
        assert!(!snapshot_matches(omitted,&stored));
    }
    #[test]
    fn diagnoses_snapshot_preserves_values_presence_and_array_order() {
        let legacy=json!({"conclusion":"exact"});
        let mut snapshot=legacy.clone();
        snapshot["history"]=json!(m53_domain::protocol::empty_history());
        assert!(snapshot_matches(snapshot.clone(),&legacy));
        snapshot["diagnoses"]=json!([]);
        assert!(!snapshot_matches(snapshot.clone(),&legacy));
        assert!(snapshot_matches(snapshot.clone(),&snapshot));
        let rows=json!([
            {"id":"one","diagnosis_id":"guid1","code":"N72","name":"Synthetic","additional_text":"first"},
            {"id":"two","diagnosis_id":"guid2","code":"N73","name":"Synthetic2","additional_text":"second"}
        ]);
        snapshot["diagnoses"]=rows;
        let stored=snapshot.clone();
        assert!(snapshot_matches(snapshot.clone(),&stored));
        snapshot["diagnoses"].as_array_mut().unwrap().reverse();
        assert!(!snapshot_matches(snapshot,&stored));
        for key in ["id","diagnosis_id","code","name","additional_text"] {
            let mut changed=stored.clone();changed["diagnoses"][0][key]=json!("changed");
            assert!(!snapshot_matches(changed,&stored),"{key}");
        }
    }
    #[test]
    fn only_absent_history_matches_default_and_nothing_else_is_relaxed() {
        let legacy=json!({"conclusion":"exact","colpo":{"fields":{}}});
        let mut snapshot=legacy.clone();
        snapshot["history"]=json!(m53_domain::protocol::empty_history());
        assert!(snapshot_matches(snapshot.clone(), &legacy));
        for history in [json!({"value":{"state":"not_applicable"},"origin":{"kind":"physician"}}),json!({"value":{"state":"assessed","value":"text"},"origin":{"kind":"physician"}}),Value::Null] {
            let mut altered=snapshot.clone();altered["history"]=history;
            assert!(!snapshot_matches(altered.clone(), &legacy));
            assert!(snapshot_matches(altered.clone(), &altered));
        }
        snapshot["conclusion"]=json!("different");
        assert!(!snapshot_matches(snapshot,&legacy));
    }
}
