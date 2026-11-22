//! Wire snapshots belong to the exact queued approved revision, never a live catalog.
use serde_json::{Value, json};
fn from_form(form: &Value, enabled: bool) -> Result<Value, &'static str> {
    use m53_domain::protocol::{ProtocolDiagnosis, DIAGNOSES_MAX_ROWS, DIAGNOSIS_CODE_MAX_BYTES, DIAGNOSIS_NAME_MAX_BYTES};
    if !enabled { return Ok(json!([])); }
    let selected = match form.get("diagnoses") {
        None | Some(Value::Null) => return Ok(json!([])),
        Some(value) => value,
    };
    let rows: Vec<ProtocolDiagnosis> = serde_json::from_value(selected.clone()).map_err(|_| "invalid_result_diagnoses")?;
    if rows.len() > DIAGNOSES_MAX_ROWS || rows.iter().any(|row| row.diagnosis_id.is_nil() || row.code.trim().is_empty() || row.name.trim().is_empty() || row.code.len()>DIAGNOSIS_CODE_MAX_BYTES || row.name.len()>DIAGNOSIS_NAME_MAX_BYTES) {
        return Err("invalid_result_diagnoses");
    }
    Ok(Value::Array(rows.into_iter().map(|row| json!({"diagnosis_id":row.diagnosis_id,"code":row.code,"name":row.name})).collect()))
}

pub(crate) async fn freeze(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, revision: uuid::Uuid) -> Result<Value, crate::api::Error> {
    use axum::http::StatusCode;
    // Only enqueue consults the flag. No existing job is reinterpreted at delivery.
    if std::env::var("M53_1C_DIAGNOSES_ENABLED").as_deref()!=Ok("1") {return Ok(json!([]));}
    let form: Option<Value> = sqlx::query_scalar("SELECT form FROM app.protocol_revisions WHERE id=$1 AND approved_at IS NOT NULL").bind(revision).fetch_optional(&mut **tx).await?;
    let form=form.ok_or(crate::api::Error(StatusCode::CONFLICT,"approved_report_required"))?;
    from_form(&form,true).map_err(|code| crate::api::Error(StatusCode::CONFLICT,code))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn row(n: u128) -> Value { json!({"id":uuid::Uuid::new_v4(),"diagnosis_id":uuid::Uuid::from_u128(n),"code":format!("C{n}"),"name":format!("Synthetic {n}"),"additional_text":"private text excluded"}) }
    #[test]
    fn freezes_only_wire_fields_in_physician_order() {
        let form=json!({"diagnoses":[row(2),row(1)],"diagnosis_note":"not for structured exchange"});
        let frozen=from_form(&form,true).unwrap();
        assert_eq!(frozen,json!([{"diagnosis_id":uuid::Uuid::from_u128(2),"code":"C2","name":"Synthetic 2"},{"diagnosis_id":uuid::Uuid::from_u128(1),"code":"C1","name":"Synthetic 1"}]));
    }
    #[test]
    fn absent_empty_and_disabled_are_legacy_even_with_bad_hidden_data() {
        for form in [json!({}),json!({"diagnoses":null}),json!({"diagnoses":[]})] {assert_eq!(from_form(&form,true).unwrap(),json!([]));}
        assert_eq!(from_form(&json!({"diagnoses":"malformed but disabled"}),false).unwrap(),json!([]));
    }
    #[test]
    fn rejects_invalid_selected_snapshot_instead_of_guessing() {
        assert!(from_form(&json!({"diagnoses":[row(0)]}),true).is_err());
        assert!(from_form(&json!({"diagnoses":vec![row(1);101]}),true).is_err());
    }
}
