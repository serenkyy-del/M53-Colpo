//! Clinic stationery: 1C is authoritative; failed sync keeps the complete old snapshot.
use crate::{
    api::{App, Error, actor},
    odata_core::CoreClient,
};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use m53_contracts::workflow::{ReportClinic, ReportLayout};
use serde_json::{Value, json};
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

pub async fn list(State(app): State<Arc<App>>, h: HeaderMap) -> Result<Response, Error> {
    let a = actor(&app, &h).await?;
    let rows=sqlx::query("SELECT guid,name,license,phone,row_version,active FROM app.clinic_branches WHERE organization_id=$1 ORDER BY guid").bind(a.org).fetch_all(&app.db).await?;
    let data:Vec<Value>=rows.iter().map(|r|json!({"guid":r.get::<Uuid,_>("guid"),"name":r.get::<String,_>("name"),"license":r.get::<String,_>("license"),"phone":r.get::<String,_>("phone"),"version":r.get::<i64,_>("row_version"),"active":r.get::<bool,_>("active")})).collect();
    let bytes = serde_json::to_vec(&data)
        .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "clinic_serialization"))?;
    let etag = format!("\"{}\"", crate::api::hash(&bytes));
    let mut response = if h.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(&etag)
    {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        ([(header::CONTENT_TYPE, "application/json")], bytes).into_response()
    };
    response
        .headers_mut()
        .insert(header::ETAG, etag.parse().unwrap());
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "private, max-age=0, must-revalidate".parse().unwrap(),
    );
    Ok(response)
}
pub async fn sync(app: &App, org: Uuid) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut tx = app.db.begin().await?;
    // Fence the whole read/import, including network I/O: a slower old request
    // must never overwrite a newer imported snapshot. Explicit CLI only.
    sqlx::query("SET LOCAL lock_timeout='4s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("clinic_sync:{org}"))
        .execute(&mut *tx)
        .await?;
    let core = CoreClient::from_env()?.ok_or("core_not_configured")?;
    let branches = core.clinic_branches().await?;
    let mut normalized = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in branches {
        let guid: Uuid = row["Ref_Key"].as_str().ok_or("branch_guid")?.parse()?;
        if guid.is_nil() || !seen.insert(guid) {
            return Err("branch_guid".into());
        }
        let name = row["НазваниеОрганизации"]
            .as_str()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| row["Description"].as_str())
            .ok_or("branch_name")?
            .to_string();
        let license = row["Лицензия"].as_str().unwrap_or("").to_string();
        let phone = core.clinic_phone(guid).await?;
        if name.trim().is_empty() || name.len() > 512 || license.len() > 4096 || phone.len() > 1024
        {
            return Err("branch_limits".into());
        }
        normalized.push((
            guid,
            name,
            license,
            phone,
            !row["DeletionMark"].as_bool().ok_or("branch_active")?,
            row["DataVersion"].as_str().map(str::to_owned),
        ));
    }
    // Never guess first-row. Ambiguous/empty complete sources keep the old
    // organization snapshot intact; neither GET nor printing changes defaults.
    let default = unique_active_branch(normalized.iter().map(|r| (r.0, r.4)))?;
    let seen: Vec<Uuid> = seen.into_iter().collect();
    sqlx::query("UPDATE app.clinic_branches SET active=false,row_version=row_version+1,synced_at=now() WHERE organization_id=$1 AND active AND NOT(guid=ANY($2))").bind(org).bind(&seen).execute(&mut *tx).await?;
    for (guid, name, license, phone, active, version) in normalized {
        sqlx::query("INSERT INTO app.clinic_branches(organization_id,guid,name,license,phone,active,source_version) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(organization_id,guid) DO UPDATE SET name=excluded.name,license=excluded.license,phone=excluded.phone,active=excluded.active,source_version=excluded.source_version,synced_at=now(),row_version=app.clinic_branches.row_version+CASE WHEN (app.clinic_branches.name,app.clinic_branches.license,app.clinic_branches.phone,app.clinic_branches.active) IS DISTINCT FROM (excluded.name,excluded.license,excluded.phone,excluded.active) THEN 1 ELSE 0 END")
 .bind(org).bind(guid).bind(name).bind(license).bind(phone).bind(active).bind(version).execute(&mut *tx).await?;
    }
    sqlx::query("INSERT INTO app.organization_clinic_defaults(organization_id,branch_guid) VALUES($1,$2) ON CONFLICT(organization_id) DO UPDATE SET branch_guid=excluded.branch_guid,synced_at=now(),row_version=app.organization_clinic_defaults.row_version+CASE WHEN app.organization_clinic_defaults.branch_guid IS DISTINCT FROM excluded.branch_guid THEN 1 ELSE 0 END")
        .bind(org).bind(default).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO app.audit_events(id,action,entity_id,safe_description) VALUES($1,'clinic.snapshot_synchronized',$2,$3)")
        .bind(Uuid::new_v4()).bind(org).bind(format!("default_branch_guid:{default}")).execute(&mut *tx).await?;
    publish_branch_versions(&mut tx, org).await?;
    tx.commit().await?;
    Ok(())
}

pub fn valid_letterhead(layout: &ReportLayout) -> bool {
    let Some(h) = &layout.letterhead else {
        return true;
    };
    h.logo_data_uri.len() <= 256 * 1024
        && (h.logo_data_uri.is_empty()
            || h.logo_data_uri.starts_with("data:image/svg+xml;base64,")
            || h.logo_data_uri.starts_with("data:image/png;base64,"))
        && h.patient_rows.len() <= 12
        && h.patient_rows.iter().all(|r| {
            r.label.len() <= 200
                && [
                    "full_name",
                    "card_number",
                    "age",
                    "sex",
                    "birth_date",
                    "exam_date",
                ]
                .contains(&r.field.as_str())
        })
}
/// New stationery keeps patient fields while standalone builds embed no private branding.
pub(crate) fn publication_letterhead() -> m53_contracts::workflow::ReportLetterhead {
    #[cfg(not(feature = "standalone"))]
    use base64::Engine;
    m53_contracts::workflow::ReportLetterhead {
        #[cfg(not(feature = "standalone"))]
        logo_data_uri: format!(
            "data:image/svg+xml;base64,{}",
            base64::engine::general_purpose::STANDARD
                .encode(include_bytes!("../../../assets/report/clinic-logo.svg"))
        ),
        #[cfg(feature = "standalone")]
        logo_data_uri: String::new(),
        patient_rows: [
            ("Пациент", "full_name"),
            ("Медицинская карта", "card_number"),
            ("Возраст", "age"),
            ("Пол", "sex"),
            ("Дата рождения", "birth_date"),
            ("Дата осмотра", "exam_date"),
        ]
        .into_iter()
        .map(|(label, field)| m53_contracts::workflow::ReportHeaderRow {
            label: label.into(),
            field: field.into(),
        })
        .collect(),
        clinic: None,
    }
}

/// Explicit administrator publication creates a new immutable template version.
pub async fn publish() -> Result<(), Box<dyn std::error::Error>> {
    let db = crate::deployment::connect(crate::deployment::Purpose::Migrator).await?;
    let org: Uuid = std::env::var("M53_ORGANIZATION_ID")?.parse()?;
    let id: Uuid = std::env::var("M53_TEMPLATE_ID")?.parse()?;
    let mut tx = db.begin().await?;
    org_lock(&mut tx, org).await?;
    sqlx::query(
        "SELECT id FROM app.report_templates WHERE id=$1 AND organization_id=$2 FOR UPDATE",
    )
    .bind(id)
    .bind(org)
    .fetch_one(&mut *tx)
    .await?;
    let r=sqlx::query("SELECT layout,version FROM app.report_template_versions WHERE template_id=$1 AND clinic_branch_guid IS NULL ORDER BY version DESC LIMIT 1").bind(id).fetch_one(&mut *tx).await?;
    let mut layout: ReportLayout = serde_json::from_value(r.get("layout"))?;
    layout.letterhead = Some(publication_letterhead());
    if !valid_letterhead(&layout) {
        return Err("invalid_letterhead".into());
    }
    sqlx::query("INSERT INTO app.report_template_versions(id,template_id,version,schema_version,layout,styles,published_at) SELECT $1,$2,coalesce(max(version),0)+1,1,$3,'',now() FROM app.report_template_versions WHERE template_id=$2").bind(Uuid::new_v4()).bind(id).bind(json!(layout)).execute(&mut *tx).await?;
    publish_branch_versions(&mut tx, org).await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_requires_exactly_one_active_branch() {
        let first = Uuid::new_v4();
        let other = Uuid::new_v4();
        assert_eq!(
            unique_active_branch([(first, true), (other, false)]),
            Ok(first)
        );
        assert!(unique_active_branch([]).is_err());
        assert!(unique_active_branch([(first, false)]).is_err());
        assert!(unique_active_branch([(first, true), (other, true)]).is_err());
    }
    #[cfg(feature = "standalone")]
    #[test]
    fn standalone_publication_has_no_clinic_logo() {
        let letterhead = publication_letterhead();
        assert!(letterhead.logo_data_uri.is_empty());
        assert_eq!(letterhead.patient_rows.len(), 6);
        let layout = ReportLayout {
            title: "Кольпоскопия".into(), blocks: vec![], photo_columns: 1,
            letterhead: Some(letterhead),
        };
        assert!(valid_letterhead(&layout));
    }
    #[test]
    fn stationery_limits_and_known_fields() {
        let mut layout = ReportLayout {
            title: "test".into(),
            blocks: vec![],
            photo_columns: 1,
            letterhead: None,
        };
        assert!(valid_letterhead(&layout));
        layout.letterhead = Some(m53_contracts::workflow::ReportLetterhead {
            logo_data_uri: "https://invalid".into(),
            patient_rows: vec![],
            clinic: None,
        });
        assert!(!valid_letterhead(&layout));
        layout.letterhead.as_mut().unwrap().logo_data_uri.clear();
        assert!(valid_letterhead(&layout), "a clinic letterhead may omit its logo");
        layout.letterhead.as_mut().unwrap().logo_data_uri = "data:image/png;base64,AA==".into();
        assert!(valid_letterhead(&layout));
        layout.letterhead.as_mut().unwrap().patient_rows.push(
            m53_contracts::workflow::ReportHeaderRow {
                label: "x".into(),
                field: "unknown".into(),
            },
        );
        assert!(!valid_letterhead(&layout));
    }
}

pub(crate) async fn publish_branch_versions(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: Uuid,
) -> Result<(), sqlx::Error> {
    let templates = sqlx::query(
        "SELECT id FROM app.report_templates WHERE organization_id=$1 ORDER BY id FOR UPDATE",
    )
    .bind(org)
    .fetch_all(&mut **tx)
    .await?;
    let branches=sqlx::query("SELECT b.guid,b.name,b.license,b.phone,b.row_version FROM app.clinic_branches b JOIN app.organization_clinic_defaults d ON d.organization_id=b.organization_id AND d.branch_guid=b.guid WHERE b.organization_id=$1 AND b.active ORDER BY b.guid").bind(org).fetch_all(&mut **tx).await?;
    for t in templates {
        let id: Uuid = t.get("id");
        let base:Option<Value>=sqlx::query_scalar("SELECT layout FROM app.report_template_versions WHERE template_id=$1 AND clinic_branch_guid IS NULL AND published_at IS NOT NULL ORDER BY version DESC LIMIT 1").bind(id).fetch_optional(&mut **tx).await?;
        let Some(base) = base else { continue };
        for b in &branches {
            let mut layout: ReportLayout = serde_json::from_value(base.clone())
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
            let Some(h) = layout.letterhead.as_mut() else {
                continue;
            };
            let guid: Uuid = b.get("guid");
            h.clinic = Some(ReportClinic {
                guid,
                name: b.get("name"),
                license: b.get("license"),
                phone: b.get("phone"),
                version: b.get("row_version"),
            });
            let layout = json!(layout);
            let last:Option<Value>=sqlx::query_scalar("SELECT layout FROM app.report_template_versions WHERE template_id=$1 AND clinic_branch_guid=$2 AND published_at IS NOT NULL ORDER BY version DESC LIMIT 1").bind(id).bind(guid).fetch_optional(&mut **tx).await?;
            if last.as_ref() == Some(&layout) {
                continue;
            }
            sqlx::query("INSERT INTO app.report_template_versions(id,template_id,version,schema_version,layout,styles,published_at,clinic_branch_guid) SELECT $1,$2,coalesce(max(version),0)+1,1,$3,'',now(),$4 FROM app.report_template_versions WHERE template_id=$2")
   .bind(Uuid::new_v4()).bind(id).bind(layout).bind(guid).execute(&mut **tx).await?;
        }
    }
    Ok(())
}

/// Retained CLI spelling fails explicitly instead of writing obsolete bindings.
pub async fn bind() -> Result<(), Box<dyn std::error::Error>> {
    Err("workstation_binding_obsolete_use_organization_sync".into())
}

fn unique_active_branch(
    rows: impl IntoIterator<Item = (Uuid, bool)>,
) -> Result<Uuid, &'static str> {
    let mut active = rows
        .into_iter()
        .filter_map(|(id, active)| active.then_some(id));
    let one = active
        .next()
        .ok_or("clinic_default_requires_one_active_branch")?;
    if active.next().is_some() {
        return Err("clinic_default_requires_one_active_branch");
    }
    Ok(one)
}

pub(crate) async fn org_lock(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("SET LOCAL lock_timeout='4s'")
        .execute(&mut **tx)
        .await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("clinic_sync:{org}"))
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub(crate) async fn check_template_scope(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: Uuid,
    credential: Uuid,
    version: Uuid,
) -> Result<(), Error> {
    let allowed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app.report_template_versions v JOIN app.report_templates t ON t.id=v.template_id WHERE v.id=$1 AND t.organization_id=$2 AND v.published_at IS NOT NULL AND (v.clinic_branch_guid IS NULL OR EXISTS(SELECT 1 FROM app.api_credentials c JOIN app.workstations w ON w.id=c.workstation_id JOIN app.organization_clinic_defaults x ON x.organization_id=w.organization_id JOIN app.clinic_branches b ON b.organization_id=x.organization_id AND b.guid=x.branch_guid AND b.active WHERE c.id=$3 AND x.organization_id=$2 AND x.branch_guid=v.clinic_branch_guid FOR SHARE OF x,b)))")
 .bind(version).bind(org).bind(credential).fetch_one(&mut **tx).await?;
    if allowed {
        Ok(())
    } else {
        Err(Error(StatusCode::CONFLICT, "template_clinic_mismatch"))
    }
}
