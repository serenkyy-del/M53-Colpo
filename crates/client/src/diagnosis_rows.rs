//! Stable row identities protect additions from delayed input events after reorder.
use m53_contracts::diagnoses::Entry;
use m53_domain::protocol::ProtocolDiagnosis;
use serde_json::Value;
use uuid::Uuid;

pub enum Edit {
    Select { id: Uuid, entry: Entry, new: bool },
    Additional { id: Uuid, text: String },
    Remove(Uuid),
    Move { id: Uuid, up: bool },
}
pub fn rows(value: &Value) -> Vec<ProtocolDiagnosis> {
    serde_json::from_value(value["diagnoses"].clone()).unwrap_or_default()
}
/// User-approved removal of obsolete per-row notes in editable test drafts.
/// Keep approved snapshots, row identities, order and the common note untouched.
pub fn prepare_common_note(value: &mut Value, readonly: bool) -> bool {
    if readonly { return false; }
    let Some(rows) = value.get_mut("diagnoses").and_then(Value::as_array_mut) else { return false; };
    let mut changed = false;
    for row in rows {
        if row.get("additional_text").and_then(Value::as_str).is_some_and(|text| !text.is_empty()) {
            row["additional_text"] = Value::String(String::new());
            changed = true;
        }
    }
    changed
}
pub fn apply(value: &mut Value, edit: Edit, readonly: bool) -> bool {
    use m53_domain::protocol::{
        DIAGNOSES_MAX_ROWS, DIAGNOSIS_ADDITIONAL_MAX_BYTES, DIAGNOSIS_CODE_MAX_BYTES,
        DIAGNOSIS_NAME_MAX_BYTES,
    };
    if readonly || !value.is_object() {
        return false;
    }
    let mut next = rows(value);
    let old = next.clone();
    match edit {
        Edit::Select { id, entry, new } => {
            if id.is_nil()
                || entry.guid.is_nil()
                || !entry.active
                || entry.code.trim().is_empty()
                || entry.name.trim().is_empty()
                || entry.code.len() > DIAGNOSIS_CODE_MAX_BYTES
                || entry.name.len() > DIAGNOSIS_NAME_MAX_BYTES
            {
                return false;
            }
            if let Some(row) = next.iter_mut().find(|r| r.id == id) {
                row.diagnosis_id = entry.guid;
                row.code = entry.code;
                row.name = entry.name;
            } else if new && next.len() < DIAGNOSES_MAX_ROWS {
                next.push(ProtocolDiagnosis {
                    id,
                    diagnosis_id: entry.guid,
                    code: entry.code,
                    name: entry.name,
                    additional_text: String::new(),
                });
            } else {
                return false;
            }
        }
        Edit::Additional { id, text } => {
            if text.len() > DIAGNOSIS_ADDITIONAL_MAX_BYTES {
                return false;
            }
            let Some(row) = next.iter_mut().find(|r| r.id == id) else {
                return false;
            };
            row.additional_text = text;
        }
        Edit::Remove(id) => {
            next.retain(|r| r.id != id);
        }
        Edit::Move { id, up } => {
            let Some(index) = next.iter().position(|r| r.id == id) else {
                return false;
            };
            let target = if up {
                index.checked_sub(1)
            } else {
                (index + 1 < next.len()).then_some(index + 1)
            };
            let Some(target) = target else {
                return false;
            };
            next.swap(index, target);
        }
    }
    if old == next {
        return false;
    }
    value["diagnoses"] = serde_json::to_value(next).expect("diagnosis serialization");
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn common_note_preparation_clears_only_authorized_editable_legacy_additions() {
        let mut value = json!({"diagnoses":[{"id":Uuid::new_v4(),"diagnosis_id":Uuid::new_v4(),"code":"Z00","name":"Synthetic","additional_text":"Old test note"}],"diagnosis_note":{"value":{"state":"assessed","value":"Keep common"},"origin":{"kind":"physician"}}});
        let original=value.clone();
        assert!(!prepare_common_note(&mut value,true));
        assert_eq!(value,original,"approved/readonly content is immutable");
        assert!(prepare_common_note(&mut value,false));
        assert_eq!(value["diagnoses"][0]["additional_text"],json!(""));
        assert_eq!(value["diagnosis_note"],original["diagnosis_note"]);
        assert_eq!(value["diagnoses"][0]["id"],original["diagnoses"][0]["id"]);
        assert!(!prepare_common_note(&mut value,false),"normalization must be idempotent");
        let mut blank=json!({"conclusion":{"value":"keep"}});
        let old=blank.clone();
        assert!(!prepare_common_note(&mut blank,false));
        assert_eq!(blank,old,"clean drafts must not acquire empty fields or changes");
    }
    fn entry() -> Entry {
        Entry {
            guid: Uuid::from_u128(10),
            code: "O06".into(),
            name: "Синтетическая группа".into(),
            full_name: String::new(),
            active: true,
            is_folder: true,
            source_version: "v1".into(),
        }
    }
    #[test]
    fn diagnoses_optional_groups_snapshot_reorder_and_clear() {
        let mut value = json!({"conclusion":{"value":"unchanged"}});
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        assert!(apply(
            &mut value,
            Edit::Select {
                id: a,
                entry: entry(),
                new: true
            },
            false
        ));
        assert!(apply(
            &mut value,
            Edit::Select {
                id: b,
                entry: entry(),
                new: true
            },
            false
        ));
        assert!(apply(
            &mut value,
            Edit::Additional {
                id: a,
                text: "первое дополнение".into()
            },
            false
        ));
        assert!(apply(&mut value, Edit::Move { id: b, up: true }, false));
        // Deferred input still addresses the same row after array indices change.
        assert!(apply(
            &mut value,
            Edit::Additional {
                id: a,
                text: "первое после перемещения".into()
            },
            false
        ));
        assert_eq!(
            rows(&value).iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![b, a]
        );
        assert_eq!(rows(&value)[1].additional_text, "первое после перемещения");
        let mut renamed = entry();
        renamed.name = "новое имя".into();
        assert_eq!(rows(&value)[1].name, "Синтетическая группа");
        assert!(apply(
            &mut value,
            Edit::Select {
                id: a,
                entry: renamed,
                new: false
            },
            false
        ));
        assert_eq!(rows(&value)[1].additional_text, "первое после перемещения");
        assert!(apply(&mut value, Edit::Remove(a), false));
        assert!(apply(&mut value, Edit::Remove(b), false));
        assert_eq!(value["diagnoses"], json!([]));
        assert_eq!(value["conclusion"]["value"], "unchanged");
    }
    #[test]
    fn diagnoses_readonly_inactive_deleted_and_late_events_never_write() {
        let id = Uuid::new_v4();
        let mut value = json!({});
        let original = value.clone();
        assert!(!apply(
            &mut value,
            Edit::Select {
                id,
                entry: entry(),
                new: true
            },
            true
        ));
        let mut inactive = entry();
        inactive.active = false;
        assert!(!apply(
            &mut value,
            Edit::Select {
                id,
                entry: inactive,
                new: true
            },
            false
        ));
        assert!(!apply(
            &mut value,
            Edit::Select {
                id,
                entry: entry(),
                new: false
            },
            false
        ));
        assert!(!apply(
            &mut value,
            Edit::Additional {
                id,
                text: "late".into()
            },
            false
        ));
        assert_eq!(value, original);
        assert!(apply(
            &mut value,
            Edit::Select {
                id,
                entry: entry(),
                new: true
            },
            false
        ));
        let selected = value.clone();
        assert!(!apply(&mut value, Edit::Remove(id), true));
        assert!(!apply(&mut value, Edit::Move { id, up: true }, false));
        assert_eq!(value, selected);
        assert!(apply(&mut value, Edit::Remove(id), false));
        let cleared = value.clone();
        assert!(!apply(
            &mut value,
            Edit::Additional {
                id,
                text: "late".into()
            },
            false
        ));
        assert!(!apply(
            &mut value,
            Edit::Select {
                id,
                entry: entry(),
                new: false
            },
            false
        ));
        assert_eq!(value, cleared);
    }
}
