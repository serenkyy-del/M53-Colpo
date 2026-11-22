//! Explicit standalone provisioning; never seeds patients or external identities.
use crate::api::hash;
use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use sqlx::Row;
use std::env;
use uuid::Uuid;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn id(name: &str) -> Result<Uuid> {
    let id = Uuid::parse_str(&env::var(name)?)?;
    if id.is_nil() {
        return Err("nonzero_identifier_required".into());
    }
    Ok(id)
}
fn name(key: &str) -> Result<String> {
    let s = env::var(key)?;
    if s.trim().is_empty() || s.len() > 256 {
        return Err("invalid_name".into());
    }
    Ok(s)
}
fn secret(key: &str, min: usize) -> Result<String> {
    let path = std::path::PathBuf::from(env::var(key)?);
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.is_file() || metadata.len() > 4096 {
        return Err("invalid_secret_file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("secret_file_permissions".into());
        }
    }
    let s = std::fs::read_to_string(path)?
        .trim_end_matches(['\r', '\n'])
        .to_owned();
    if s.len() < min {
        return Err("secret_too_short".into());
    }
    Ok(s)
}
pub fn integration_path(path: &str) -> bool {
    path.starts_with("/api/colposcopy/")
        || path.contains("/external")
        || path.contains("/tasks")
        || path.contains("/results/1c")
        || path.contains("/auth/1c/")
        || path.ends_with("/auth/profiles")
        || path.ends_with("/refresh-from-1c")
        || path.contains("/identity-link")
}
pub async fn init() -> Result<()> {
    let org = id("M53_ORGANIZATION_ID")?;
    let org_name = name("M53_ORGANIZATION_NAME")?;
    let doctor = id("M53_DOCTOR_ID")?;
    let doctor_name = name("M53_DOCTOR_NAME")?;
    let login = name("M53_USER_LOGIN")?;
    let password = secret("M53_USER_PASSWORD_FILE", 12)?;
    let workstation = id("M53_WORKSTATION_ID")?;
    let station_name = name("M53_WORKSTATION_NAME")?;
    let key = secret("M53_WORKSTATION_KEY_FILE", 32)?;
    let db = crate::deployment::connect(crate::deployment::Purpose::Migrator).await?;
    let mut tx = db.begin().await?;
    sqlx::query("SET LOCAL ROLE colposcopy_owner")
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO app.organizations(id,name) VALUES($1,$2) ON CONFLICT DO NOTHING")
        .bind(org)
        .bind(&org_name)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO app.doctors(id,organization_id,display_name,origin) VALUES($1,$2,$3,'local_pending_link') ON CONFLICT DO NOTHING").bind(doctor).bind(org).bind(&doctor_name).execute(&mut *tx).await?;
    let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app.doctors WHERE id=$1 AND organization_id=$2 AND origin='local_pending_link' AND active)").bind(doctor).bind(org).fetch_one(&mut *tx).await?;
    if !valid {
        return Err("doctor_identity_mismatch".into());
    }
    let existing=sqlx::query("SELECT u.organization_id,u.doctor_id FROM app.users u JOIN app.user_auth a ON a.user_id=u.id WHERE a.login=$1 FOR UPDATE OF u").bind(&login).fetch_optional(&mut *tx).await?;
    if let Some(row) = existing {
        if row.get::<Uuid, _>("organization_id") != org
            || row.get::<Option<Uuid>, _>("doctor_id") != Some(doctor)
        {
            return Err("account_identity_mismatch".into());
        }
    } else {
        let user = Uuid::new_v4();
        let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes()).map_err(|_| "salt_failed")?;
        let hashed = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|_| "password_hash_failed")?
            .to_string();
        sqlx::query(
            "INSERT INTO app.users(id,organization_id,doctor_id,display_name) VALUES($1,$2,$3,$4)",
        )
        .bind(user)
        .bind(org)
        .bind(doctor)
        .bind(&doctor_name)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO app.user_auth(user_id,login,password_hash) VALUES($1,$2,$3)")
            .bind(user)
            .bind(login)
            .bind(hashed)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO app.roles(code) VALUES('doctor') ON CONFLICT DO NOTHING")
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO app.user_roles(user_id,role_code) VALUES($1,'doctor')")
            .bind(user)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("INSERT INTO app.workstations(id,organization_id,name) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(workstation).bind(org).bind(station_name).execute(&mut *tx).await?;
    let owner: Uuid =
        sqlx::query_scalar("SELECT organization_id FROM app.workstations WHERE id=$1 FOR UPDATE")
            .bind(workstation)
            .fetch_one(&mut *tx)
            .await?;
    if owner != org {
        return Err("workstation_identity_mismatch".into());
    }
    let existing:Option<String>=sqlx::query_scalar("SELECT verifier FROM app.api_credentials WHERE workstation_id=$1 AND purpose='client' AND revoked_at IS NULL").bind(workstation).fetch_optional(&mut *tx).await?;
    if let Some(existing) = existing {
        if existing != hash(&key) {
            return Err("explicit_workstation_rotation_required".into());
        }
    } else {
        let used:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app.api_credentials WHERE workstation_id=$1 OR verifier=$2)").bind(workstation).bind(hash(&key)).fetch_one(&mut *tx).await?;
        if used {
            return Err("revoked_workstation_requires_explicit_rotation".into());
        }
        sqlx::query("INSERT INTO app.api_credentials(id,workstation_id,purpose,verifier,permissions) VALUES($1,$2,'client',$3,ARRAY['clinical'])").bind(Uuid::new_v4()).bind(workstation).bind(hash(&key)).execute(&mut *tx).await?;
    }
    sqlx::query("INSERT INTO app.clinic_branches(organization_id,guid,name,active) VALUES($1,$1,$2,true) ON CONFLICT DO NOTHING").bind(org).bind(org_name).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO app.organization_clinic_defaults(organization_id,branch_guid) VALUES($1,$1) ON CONFLICT DO NOTHING").bind(org).execute(&mut *tx).await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM app.report_templates WHERE organization_id=$1)",
    )
    .bind(org)
    .fetch_one(&mut *tx)
    .await?;
    if !exists {
        use m53_contracts::workflow::{ReportBlock, ReportClinic, ReportLayout};
        let template = Uuid::new_v4();
        let mut letterhead = crate::clinic::publication_letterhead();
        letterhead.clinic = Some(ReportClinic {
            guid: org,
            name: name("M53_ORGANIZATION_NAME")?,
            license: String::new(),
            phone: String::new(),
            version: 1,
        });
        let layout = ReportLayout {
            letterhead: Some(letterhead),
            title: "Протокол кольпоскопии".into(),
            blocks: vec![
                ReportBlock::Patient,
                ReportBlock::Indications,
                ReportBlock::Assessment,
                ReportBlock::Findings,
                ReportBlock::Photos,
                ReportBlock::Conclusion,
                ReportBlock::Recommendations,
                ReportBlock::Signature,
            ],
            photo_columns: 2,
        };
        sqlx::query("INSERT INTO app.report_templates(id,name,organization_id) VALUES($1,'Протокол кольпоскопии',$2)").bind(template).bind(org).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO app.report_template_versions(id,template_id,version,schema_version,layout,styles,published_at) VALUES($1,$2,1,1,$3,'',now())").bind(Uuid::new_v4()).bind(template).bind(serde_json::to_value(layout)?).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}
