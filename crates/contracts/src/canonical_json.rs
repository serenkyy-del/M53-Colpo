//! Stable JSON for operation receipts, independent of serde_json/preserve_order feature unification.
use serde::{Serialize, Serializer, ser::SerializeMap};
use serde_json::Value;
struct Canonical<'a>(&'a Value);
impl Serialize for Canonical<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Value::Object(values) => {
                let sorted: std::collections::BTreeMap<_, _> = values.iter().collect();
                let mut out = serializer.serialize_map(Some(sorted.len()))?;
                for (key, value) in sorted {
                    out.serialize_entry(key, &Canonical(value))?;
                }
                out.end()
            }
            Value::Array(values) => serializer.collect_seq(values.iter().map(Canonical)),
            value => value.serialize(serializer),
        }
    }
}
pub fn to_vec(value: &Value) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&Canonical(value))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recursively_sorts_keys_without_changing_values_or_array_order() {
        let input: Value =
            serde_json::from_str(r#"{"z":[{"y":2,"a":"тест"},1],"a":null}"#).unwrap();
        assert_eq!(
            String::from_utf8(to_vec(&input).unwrap()).unwrap(),
            r#"{"a":null,"z":[{"a":"тест","y":2},1]}"#
        );
        let reordered: Value =
            serde_json::from_str(r#"{"a":null,"z":[{"a":"тест","y":2},1]}"#).unwrap();
        assert_eq!(to_vec(&input).unwrap(), to_vec(&reordered).unwrap());
    }
}
