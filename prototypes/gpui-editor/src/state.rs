//! Merge local form edits into the current clinical snapshot without replaying stale fields.
use m53_contracts::workflow::StudyDetail;
use m53_domain::protocol::Protocol;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Adopt the current form for editable legacy drafts without translating or dropping data.
pub fn use_current_form(detail: &mut StudyDetail) -> bool {
    if detail.study.status == "approved" {
        return false;
    }
    let Some(protocol) = detail.protocol.as_mut() else {
        return false;
    };
    if protocol.schema_version != 1 || protocol.colpo.is_some() {
        return false;
    }
    protocol.schema_version = 2;
    protocol.colpo = Some(Default::default());
    let sections = protocol.sections.clone();
    m53_client::defects::sync_colpo(detail);
    detail.protocol.as_mut().unwrap().sections = sections;
    true
}

pub fn merge(base: &Value, current: &Value, incoming: &Value) -> Result<Value, String> {
    merge_node(Some(base), Some(current), Some(incoming))?
        .ok_or_else(|| "Нельзя удалить протокол целиком".into())
}
fn index<'a>(values: &'a [Value], field: &str) -> Option<BTreeMap<String, &'a Value>> {
    let mut indexed = BTreeMap::new();
    for value in values {
        let key = value.get(field)?.as_str()?.to_string();
        if indexed.insert(key, value).is_some() {
            return None;
        }
    }
    Some(indexed)
}
fn merge_node(
    base: Option<&Value>,
    current: Option<&Value>,
    incoming: Option<&Value>,
) -> Result<Option<Value>, String> {
    if incoming == base || incoming == current {
        return Ok(current.cloned());
    }
    if current == base {
        return Ok(incoming.cloned());
    }
    if let (Some(Value::Object(b)), Some(Value::Object(c)), Some(Value::Object(i))) =
        (base, current, incoming)
    {
        let keys: BTreeSet<_> = b.keys().chain(c.keys()).chain(i.keys()).collect();
        let mut result = serde_json::Map::new();
        for k in keys {
            if let Some(value) = merge_node(b.get(k), c.get(k), i.get(k))? {
                result.insert(k.clone(), value);
            }
        }
        return Ok(Some(Value::Object(result)));
    }
    if let (Some(Value::Array(b)), Some(Value::Array(c)), Some(Value::Array(i))) =
        (base, current, incoming)
    {
        for field in ["id", "region", "code"] {
            if let (Some(bm), Some(cm), Some(im)) =
                (index(b, field), index(c, field), index(i, field))
            {
                let mut keys = vec![];
                for value in c.iter().chain(i.iter()).chain(b.iter()) {
                    let key = value[field].as_str().unwrap().to_string();
                    if !keys.contains(&key) {
                        keys.push(key);
                    }
                }
                let mut result = vec![];
                for key in keys {
                    if let Some(value) = merge_node(
                        bm.get(&key).copied(),
                        cm.get(&key).copied(),
                        im.get(&key).copied(),
                    )? {
                        result.push(value);
                    }
                }
                return Ok(Some(Value::Array(result)));
            }
        }
    }
    Err("Данные формы изменились одновременно с разметкой. Ввод сохранен; требуется согласовать изменения.".into())
}

pub fn form(detail: &StudyDetail, base: &Value, incoming: &Value) -> Result<StudyDetail, String> {
    if detail.study.status == "approved" {
        return Err("Откройте новую редакцию".into());
    }
    let value = merge(base, &serde_json::json!(detail.protocol), incoming)?;
    let protocol: Protocol =
        serde_json::from_value(value).map_err(|_| "Незавершенная структура протокола")?;
    for annotation in &detail.annotations {
        let finding = annotation.finding_id.and_then(|id| {
            protocol
                .sections
                .iter()
                .flat_map(|s| {
                    s.findings
                        .iter()
                        .chain(s.lesions.iter().flat_map(|l| l.findings.iter()))
                })
                .find(|f| f.id == id)
        });
        if annotation.finding_id.is_some() && finding.is_none() {
            return Err(
                "Признак связан с меткой. Измените или удалите соответствующую метку на снимке."
                    .into(),
            );
        }
        if let Some(id) = annotation.lesion_id {
            let lesion = protocol
                .sections
                .iter()
                .flat_map(|s| &s.lesions)
                .find(|l| l.id == id)
                .ok_or("Очаг связан с меткой и не может быть удален отдельно")?;
            if let Some(finding) = finding
                && !lesion.findings.iter().any(|f| f.id == finding.id)
            {
                return Err("Признак не принадлежит указанному очагу".into());
            }
        }
        if let m53_contracts::workflow::AnnotationLayer::Callout {
            term_code: Some(code),
            ..
        } = &annotation.layer
            && finding.is_none_or(|f| f.term_code != *code)
        {
            return Err("Код признака расходится с меткой на снимке".into());
        }
    }
    m53_contracts::workflow::validate_colpo_links(&protocol, &detail.annotations)
        .map_err(str::to_string)?;
    if protocol
        .colpo
        .as_ref()
        .is_some_and(|c| !c.errors().is_empty())
    {
        return Err("Недопустимое значение характеристики".into());
    }
    let mut result = detail.clone();
    result.protocol = Some(protocol);
    Ok(result)
}

/// Keep the legacy journal key synchronized for old readers; new readers use detail only.
pub fn set_detail(journal: &mut Value, detail: &StudyDetail) {
    journal["detail"] = serde_json::json!(detail);
    journal["native_protocol"] = serde_json::json!(detail.protocol);
}

/// Recover an explicit rejected draft without preferring an obsolete full-form mirror.
pub fn restore_form(detail: &StudyDetail, journal: &Value) -> Value {
    let current = serde_json::json!(detail.protocol);
    let draft = &journal["native_protocol_draft"];
    if detail.study.status != "approved"
        && journal["unfinished_fields"]["schema"].is_string()
        && draft["base"].is_object()
        && draft["value"].is_object()
    {
        return merge(&draft["base"], &current, &draft["value"])
            .unwrap_or_else(|_| draft["value"].clone());
    }
    current
}
