//! Native form schema helpers. No JavaScript or browser is used.
use serde_json::{Value, json};
use uuid::Uuid;
pub fn document() -> Value {
    serde_json::from_str(include_str!("../../../docs/schemas/save-draft.schema.json")).unwrap()
}
pub fn resolve(root: &Value, s: &Value) -> Value {
    let mut out = s
        .get("$ref")
        .and_then(Value::as_str)
        .map(|r| resolve(root, root.pointer(&r[1..]).unwrap_or(&Value::Null)))
        .unwrap_or_else(|| json!({}));
    if let Some(all) = s["allOf"].as_array() {
        for s in all {
            merge(&mut out, resolve(root, s));
        }
    }
    merge(&mut out, s.clone());
    if let Some(o) = out.as_object_mut() {
        o.remove("$ref");
        o.remove("allOf");
    }
    out
}
fn merge(a: &mut Value, b: Value) {
    if let Some(o) = b.as_object() {
        for (k, v) in o {
            a[k] = v.clone();
        }
    }
}
pub fn variants(s: &Value) -> Option<&Vec<Value>> {
    s.get("oneOf")
        .or_else(|| s.get("anyOf"))
        .and_then(Value::as_array)
}
pub fn branch(root: &Value, s: &Value, v: &Value) -> Value {
    let s = resolve(root, s);
    if let Some(vs) = variants(&s) {
        if v.is_null()
            && let Some(n) = vs.iter().find(|x| x["type"] == "null")
        {
            return n.clone();
        }
        for b in vs {
            let b = resolve(root, b);
            if b["properties"].as_object().is_some_and(|p| {
                p.iter().all(|(k, s)| {
                    s["enum"]
                        .as_array()
                        .is_none_or(|values| values.len() != 1 || v.get(k) == values.first())
                })
            }) {
                return b;
            }
        }
        return resolve(
            root,
            vs.iter().find(|b| b["type"] != "null").unwrap_or(&vs[0]),
        );
    }
    s
}
pub fn defaults(root: &Value, s: &Value, region: &str) -> Value {
    let s = resolve(root, s);
    if let Some(vs) = variants(&s) {
        let variants: Vec<_> = vs
            .iter()
            .map(|v| resolve(root, v))
            .filter(|v| v["type"] != "null")
            .collect();
        if let Some(b) = variants
            .iter()
            .find(|v| {
                v["properties"]["state"]["enum"]
                    .as_array()
                    .is_some_and(|e| e.contains(&json!("not_assessed")))
            })
            .or_else(|| {
                variants.iter().find(|v| {
                    v["properties"]["region"]["enum"]
                        .as_array()
                        .is_some_and(|e| e.contains(&json!(region)))
                })
            })
            .or_else(|| variants.first())
        {
            return defaults(root, b, region);
        }
        return Value::Null;
    }
    if let Some(e) = s["enum"].as_array() {
        return e
            .iter()
            .find(|v| **v == "not_assessed")
            .or_else(|| e.first())
            .cloned()
            .unwrap_or(Value::Null);
    }
    match s["type"].as_str() {
        Some("object") => {
            let mut v = json!({});
            if let Some(props) = s["properties"].as_object() {
                for (k, s) in props {
                    v[k] = match k.as_str() {
                        "origin" => json!({"kind":"physician"}),
                        "id" => json!(Uuid::new_v4()),
                        _ => defaults(root, s, region),
                    };
                }
            }
            v
        }
        Some("array") => json!([]),
        Some("boolean") => json!(false),
        Some("number" | "integer") => json!(0),
        _ => {
            if s["type"]
                .as_array()
                .is_some_and(|v| v.contains(&json!("null")))
            {
                Value::Null
            } else {
                json!("")
            }
        }
    }
}
pub fn label(key: &str) -> String {
    static LABELS: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    let labels =
        LABELS.get_or_init(|| serde_json::from_str(include_str!("form-labels.json")).unwrap());
    labels[key].as_str().unwrap_or(key).to_string()
}
pub fn set(value: &mut Value, path: &str, new: Value) -> Result<(), String> {
    let target = value.pointer_mut(path).ok_or("Поле не найдено")?;
    *target = new;
    let mut prefix = String::new();
    for key in path.split('/').filter(|s| !s.is_empty()) {
        prefix.push('/');
        prefix.push_str(key);
        if let Some(finding) = value
            .pointer_mut(&prefix)
            .filter(|v| v.get("term_code").is_some() && v.get("observation").is_some())
        {
            finding["observation"]["origin"] = json!({"kind":"physician"});
        }
        if let Some(parent) = value
            .pointer_mut(&prefix)
            .filter(|v| v.get("origin").is_some())
        {
            parent["origin"] = json!({"kind":"physician"});
        }
    }
    Ok(())
}
pub fn ordered_properties(properties: &serde_json::Map<String, Value>) -> Vec<(&String, &Value)> {
    let order = [
        "state",
        "region",
        "term_code",
        "observation",
        "physician_confirmed",
        "adequacy",
        "zone_relation",
        "clock_start",
        "clock_end",
        "quadrants",
        "affected_percent",
        "location",
        "findings",
        "lesions",
    ];
    let mut fields: Vec<_> = properties.iter().collect();
    fields.sort_by_key(|(name, _)| order.iter().position(|key| key == name).unwrap_or(100));
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_preserve_explicit_missing_and_native_model() {
        let root = document();
        let p = defaults(&root, &root["properties"]["protocol"], "cervix");
        assert_eq!(p["conclusion"]["value"]["state"], "not_assessed");
        let s = resolve(&root, &root["definitions"]["RegionSection"]);
        let v = defaults(&root, &s, "cervix");
        assert!(serde_json::from_value::<m53_domain::protocol::RegionSection>(v).is_ok());
    }
}
