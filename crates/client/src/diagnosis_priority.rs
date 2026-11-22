//! Confirmed organization-wide diagnosis priorities.
use crate::{cache::Store, diagnosis_cache::Scope};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

const FILE: &str = "diagnoses-priority-v1";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    format: u32,
    scope: Scope,
    group: Group,
}

pub(crate) fn load(store: &Store, scope: &Scope) -> Result<Option<Group>, String> {
    let Some(bytes) = store.read(FILE)? else {
        return Ok(None);
    };
    let env: Envelope =
        serde_json::from_slice(&bytes).map_err(|_| "Повреждён кеш группы диагнозов")?;
    if env.format != 1 || &env.scope != scope {
        return Err("Кеш группы диагнозов относится к другому профилю".into());
    }
    env.group.validate().map_err(str::to_owned)?;
    Ok(Some(env.group))
}

pub(crate) fn save(store: &Store, scope: &Scope, group: &Group) -> Result<(), String> {
    group.validate().map_err(str::to_owned)?;
    let env = Envelope {
        format: 1,
        scope: scope.clone(),
        group: group.clone(),
    };
    let bytes = serde_json::to_vec(&env).map_err(|_| "Не удалось сохранить группу диагнозов")?;
    store.write(FILE, &bytes)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub version: i64,
    #[serde(default)]
    pub catalog_version_id: Option<Uuid>,
    pub diagnosis_ids: Vec<Uuid>,
}

#[derive(Clone, Debug)]
pub struct Draft {
    pub base: Group,
    pub ids: Vec<Uuid>,
}
impl Draft {
    pub fn new(base: Group) -> Self {
        Self {
            ids: base.diagnosis_ids.clone(),
            base,
        }
    }
    pub fn add(&mut self, id: Uuid) {
        if !id.is_nil() && self.ids.len() < 500 && !self.ids.contains(&id) {
            self.ids.push(id);
        }
    }
    pub fn remove(&mut self, index: usize) {
        if index < self.ids.len() {
            self.ids.remove(index);
        }
    }
    pub fn move_up(&mut self, index: usize) {
        if index > 0 && index < self.ids.len() {
            self.ids.swap(index, index - 1);
        }
    }
    pub fn move_down(&mut self, index: usize) {
        if index < self.ids.len().saturating_sub(1) {
            self.ids.swap(index, index + 1);
        }
    }
}

impl Group {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version < 0
            || self.diagnosis_ids.len() > 500
            || (self.version == 0 && self.catalog_version_id.is_none_or(|v| v.is_nil()))
            || self.diagnosis_ids.iter().any(Uuid::is_nil)
            || self.diagnosis_ids.iter().collect::<HashSet<_>>().len() != self.diagnosis_ids.len()
        {
            return Err("Некорректная общая группа диагнозов");
        }
        Ok(())
    }
}

pub(crate) fn decode_response(status: u16, bytes: &[u8]) -> Result<Group, String> {
    if status != 200 {
        return Err("Общая группа диагнозов временно недоступна".into());
    }
    let group: Group =
        serde_json::from_slice(bytes).map_err(|_| "Некорректный ответ группы диагнозов")?;
    group.validate().map_err(str::to_owned)?;
    Ok(group)
}

pub(crate) fn is_stale_poll(current: Option<&Group>, received: &Group) -> bool {
    current.is_some_and(|current| received.version < current.version)
}

pub fn update_body(base: &Group, ids: &[Uuid]) -> Result<serde_json::Value, String> {
    base.validate().map_err(str::to_owned)?;
    let proposed = Group {
        version: base.version,
        catalog_version_id: base.catalog_version_id,
        diagnosis_ids: ids.to_vec(),
    };
    proposed.validate().map_err(str::to_owned)?;
    Ok(
        serde_json::json!({ "expected_version": base.version, "expected_catalog_version": base.catalog_version_id, "diagnosis_ids": ids }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn connection() -> crate::connection::Connection {
        let mut raw =
            crate::connection::Connection::new("https://synthetic.invalid", "test").unwrap();
        raw.user_id = Some(Uuid::from_u128(6));
        raw.identity = Some(crate::ui_guidance::Identity {
            organization_id: Uuid::from_u128(5),
            user_id: Uuid::from_u128(6),
            doctor_id: Uuid::from_u128(7),
            workstation_id: Uuid::from_u128(8),
        });
        raw
    }

    #[test]
    fn late_poll_reply_is_stale_only_if_a_newer_confirmed_version_exists() {
        let old = Group { version: 1, catalog_version_id: None, diagnosis_ids: vec![] };
        let new = Group { version: 2, ..old.clone() };
        assert!(is_stale_poll(Some(&new), &old));
        assert!(!is_stale_poll(Some(&old), &new));
        assert!(!is_stale_poll(Some(&old), &old));
        assert!(!is_stale_poll(None, &old));
    }
    #[test]
    fn maximum_group_fits_network_response_bound() {
        let ids: Vec<Uuid> = (1..=500).map(Uuid::from_u128).collect();
        let response = serde_json::json!({"version": 1, "diagnosis_ids": ids});
        let bytes = serde_json::to_vec(&response).unwrap();
        assert!(bytes.len() <= crate::connection::diagnosis_response_limit("/api/v1/diagnoses/priority") as usize);
        assert!(decode_response(200, &bytes).is_ok());
    }
    #[test]
    fn draft_add_remove_and_reorder_without_touching_confirmed_group() {
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        let base = Group {
            version: 1,
            catalog_version_id: None,
            diagnosis_ids: vec![a, b],
        };
        let mut draft = Draft::new(base.clone());
        draft.add(c);
        draft.add(c);
        draft.move_up(2);
        draft.remove(0);
        assert_eq!(draft.ids, vec![c, b]);
        assert_eq!(draft.base, base);
        draft.move_up(0);
        draft.move_down(1);
        assert_eq!(draft.ids, vec![c, b]);
    }
    #[test]
    fn update_uses_confirmed_version_and_pins_default_catalog() {
        let first = Uuid::from_u128(1);
        let base = Group {
            version: 0,
            catalog_version_id: Some(first),
            diagnosis_ids: vec![first],
        };
        let body = update_body(&base, &[first]).unwrap();
        assert_eq!(body["expected_version"], 0);
        assert_eq!(body["expected_catalog_version"], first.to_string());
        assert!(update_body(&base, &[first, first]).is_err());
        assert!(update_body(&base, &[Uuid::nil()]).is_err());
    }
    #[test]
    fn remote_group_rejects_invalid_payload_and_preserves_confirmed_cache() {
        let first = Uuid::from_u128(1);
        let response =
            serde_json::json!({"version":0,"catalog_version_id":first,"diagnosis_ids":[first]});
        let result = decode_response(200, &serde_json::to_vec(&response).unwrap()).unwrap();
        assert_eq!(result.diagnosis_ids, vec![first]);
        assert!(decode_response(503, b"{}").is_err());
        assert!(decode_response(200, b"{}").is_err());
        assert!(
            decode_response(
                200,
                br#"{"version":1,"diagnosis_ids":["00000000-0000-0000-0000-000000000000"]}"#
            )
            .is_err()
        );
    }
    #[test]
    fn cache_keeps_confirmed_empty_and_rejects_foreign_or_corrupt_data() {
        use std::sync::Arc;
        let path = std::env::temp_dir().join(format!("priority-{}", Uuid::new_v4()));
        let store =
            crate::cache::Store::new(path.clone(), Arc::new(crate::cache::AesProtector([42; 32])))
                .unwrap();
        let scope = crate::diagnosis_cache::Scope::from_connection(&connection()).unwrap();
        let group = Group {
            version: 1,
            catalog_version_id: None,
            diagnosis_ids: vec![],
        };
        save(&store, &scope, &group).unwrap();
        assert_eq!(load(&store, &scope).unwrap(), Some(group));
        let mut other = connection();
        other.identity.as_mut().unwrap().organization_id = Uuid::from_u128(99);
        let other_scope = crate::diagnosis_cache::Scope::from_connection(&other).unwrap();
        assert!(load(&store, &other_scope).is_err());
        store.write(FILE, b"bad").unwrap();
        assert!(load(&store, &scope).is_err());
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn confirmed_snapshot_rejects_invalid_identity_and_accepts_explicit_empty_group() {
        let first = Uuid::from_u128(1);
        let valid = Group {
            version: 1,
            catalog_version_id: None,
            diagnosis_ids: vec![first],
        };
        assert!(valid.validate().is_ok());
        assert!(
            Group {
                diagnosis_ids: vec![],
                ..valid.clone()
            }
            .validate()
            .is_ok()
        );
        assert!(
            Group {
                version: -1,
                ..valid.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            Group {
                diagnosis_ids: vec![Uuid::nil()],
                ..valid.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            Group {
                diagnosis_ids: vec![first, first],
                ..valid.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            Group {
                version: 0,
                ..valid.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            Group {
                version: 0,
                catalog_version_id: Some(first),
                ..valid
            }
            .validate()
            .is_ok()
        );
    }
}
