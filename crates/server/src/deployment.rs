//! Restricted access to the explicitly installed M53 database.
//! Legacy development/seed commands retain setup::connect and its suffix guard.
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::env;

#[derive(Clone, Copy)]
pub enum Purpose {
    Runtime,
    Migrator,
}

pub async fn connect(purpose: Purpose) -> Result<PgPool, Box<dyn std::error::Error>> {
    if env::var("M53_DEPLOYMENT_DATABASE").is_err() {
        return match purpose {
            Purpose::Runtime => crate::setup::runtime_connect().await,
            Purpose::Migrator => crate::setup::connect().await,
        };
    }
    // This is a single named, installed target, not a switch to accept any DB.
    if env::var("M53_DEPLOYMENT_DATABASE").as_deref() != Ok("m53colpo") {
        return Err("unrecognized_installed_database".into());
    }
    let url = env::var("M53_DATABASE_URL")?;
    let db = match purpose {
        Purpose::Runtime => crate::database::connect(&url).await?,
        // Schema changes have their own explicitly controlled migration budgets.
        Purpose::Migrator => sqlx::postgres::PgPoolOptions::new().max_connections(8).connect(&url).await?,
    };
    let row = sqlx::query("SELECT current_database() AS database,current_user AS role,rolsuper,rolcreatedb,rolcreaterole,rolreplication,rolbypassrls,has_database_privilege(current_user,current_database(),'CREATE') AS database_create,pg_has_role(current_user,'colposcopy_owner','USAGE') AS owner_usage,pg_has_role(current_user,'colposcopy_owner','SET') AS owner_set,has_schema_privilege(current_user,'app','CREATE') AS schema_create FROM pg_roles WHERE rolname=current_user")
        .fetch_one(&db).await?;
    let expected = match purpose {
        Purpose::Runtime => "colposcopy_app",
        Purpose::Migrator => "colposcopy_migrator",
    };
    if row.get::<String, _>("database") != "m53colpo"
        || row.get::<String, _>("role") != expected
        || row.get::<bool, _>("rolsuper")
        || row.get::<bool, _>("rolcreatedb")
        || row.get::<bool, _>("rolcreaterole")
        || row.get::<bool, _>("rolreplication")
        || row.get::<bool, _>("rolbypassrls")
        || (matches!(purpose, Purpose::Runtime) && row.get::<bool, _>("database_create"))
        || (matches!(purpose, Purpose::Runtime)
            && (row.get::<bool, _>("owner_usage")
                || row.get::<bool, _>("owner_set")
                || row.get::<bool, _>("schema_create")))
    {
        return Err("installed_database_or_role_mismatch".into());
    }
    for (name, body) in MIGRATIONS {
        let expected = hex::encode(Sha256::digest(body));
        let stored: Option<String> =
            sqlx::query_scalar("SELECT sha256 FROM app.schema_migrations WHERE version=$1")
                .bind(name.trim_end_matches(".sql"))
                .fetch_optional(&db)
                .await?;
        if stored.as_deref() != Some(&expected) {
            return Err("installed_schema_fingerprint_mismatch".into());
        }
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM app.schema_migrations")
        .fetch_one(&db)
        .await?;
    if count != MIGRATIONS.len() as i64 {
        return Err("unsupported_installed_schema_version".into());
    }
    Ok(db)
}

const MIGRATIONS: &[(&str, &[u8])] = &[
    ("0036_result_diagnoses_snapshot.sql",include_bytes!("../../../migrations/0036_result_diagnoses_snapshot.sql")),
    ("0035_approved_report_photo_selection.sql",include_bytes!("../../../migrations/0035_approved_report_photo_selection.sql")),
    ("0034_study_colposcope_model.sql",include_bytes!("../../../migrations/0034_study_colposcope_model.sql")),
    ("0033_revision_metadata.sql",include_bytes!("../../../migrations/0033_revision_metadata.sql")),
    ("0032_manual_identity_verification.sql",include_bytes!("../../../migrations/0032_manual_identity_verification.sql")),
    ("0031_protocol_template_titles.sql", include_bytes!("../../../migrations/0031_protocol_template_titles.sql")),
    ("0030_protocol_templates.sql", include_bytes!("../../../migrations/0030_protocol_templates.sql")),
    ("0029_diagnosis_priority.sql", include_bytes!("../../../migrations/0029_diagnosis_priority.sql")),
    ("0028_diagnosis_note_phrases.sql", include_bytes!("../../../migrations/0028_diagnosis_note_phrases.sql")),
    ("0027_diagnoses_catalog.sql",include_bytes!("../../../migrations/0027_diagnoses_catalog.sql")),
    ("0026_anamnesis_phrases.sql", include_bytes!("../../../migrations/0026_anamnesis_phrases.sql")),
    ("0025_request_log_retention.sql", include_bytes!("../../../migrations/0025_request_log_retention.sql")),
    (
        "0001_core.sql",
        include_bytes!("../../../migrations/0001_core.sql"),
    ),
    (
        "0002_runtime_permissions.sql",
        include_bytes!("../../../migrations/0002_runtime_permissions.sql"),
    ),
    (
        "0003_reports_and_delivery.sql",
        include_bytes!("../../../migrations/0003_reports_and_delivery.sql"),
    ),
    (
        "0004_working_api.sql",
        include_bytes!("../../../migrations/0004_working_api.sql"),
    ),
    (
        "0005_delivery_and_scoping.sql",
        include_bytes!("../../../migrations/0005_delivery_and_scoping.sql"),
    ),
    (
        "0006_external_versions.sql",
        include_bytes!("../../../migrations/0006_external_versions.sql"),
    ),
    (
        "0007_terminology_catalog.sql",
        include_bytes!("../../../migrations/0007_terminology_catalog.sql"),
    ),
    (
        "0008_completion_notifications.sql",
        include_bytes!("../../../migrations/0008_completion_notifications.sql"),
    ),
    (
        "0009_client_operation_receipts.sql",
        include_bytes!("../../../migrations/0009_client_operation_receipts.sql"),
    ),
    (
        "0010_colposcopy_tasks.sql",
        include_bytes!("../../../migrations/0010_colposcopy_tasks.sql"),
    ),
    (
        "0011_core_sync.sql",
        include_bytes!("../../../migrations/0011_core_sync.sql"),
    ),
    (
        "0012_result_payload_cache.sql",
        include_bytes!("../../../migrations/0012_result_payload_cache.sql"),
    ),
    (
        "0013_auth_access.sql",
        include_bytes!("../../../migrations/0013_auth_access.sql"),
    ),
    (
        "0014_continuity_care.sql",
        include_bytes!("../../../migrations/0014_continuity_care.sql"),
    ),
    (
        "0015_1c_login_bindings.sql",
        include_bytes!("../../../migrations/0015_1c_login_bindings.sql"),
    ),
    (
        "0016_1c_principal.sql",
        include_bytes!("../../../migrations/0016_1c_principal.sql"),
    ),
    (
        "0017_access_generation_revocation.sql",
        include_bytes!("../../../migrations/0017_access_generation_revocation.sql"),
    ),
    (
        "0018_patient_sex.sql",
        include_bytes!("../../../migrations/0018_patient_sex.sql"),
    ),
    (
        "0019_client_change_notifications.sql",
        include_bytes!("../../../migrations/0019_client_change_notifications.sql"),
    ),
    (
        "0020_client_updates.sql",
        include_bytes!("../../../migrations/0020_client_updates.sql"),
    ),
    (
        "0021_workstation_activity_audit.sql",
        include_bytes!("../../../migrations/0021_workstation_activity_audit.sql"),
    ),
    (
        "0022_phrase_templates.sql",
        include_bytes!("../../../migrations/0022_phrase_templates.sql"),
    ),
    (
        "0023_report_letterhead.sql",
        include_bytes!("../../../migrations/0023_report_letterhead.sql"),
    ),
    (
        "0024_organization_clinic_default.sql",
        include_bytes!("../../../migrations/0024_organization_clinic_default.sql"),
    ),
];
