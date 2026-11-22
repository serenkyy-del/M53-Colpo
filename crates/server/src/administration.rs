//! Explicit migration-role commands. Directory synchronization never provisions passwords.
use crate::api::hash;
use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use sqlx::Row;
use std::env;
use uuid::Uuid;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn id(name: &str) -> Result<Uuid> {
    let value = Uuid::parse_str(&env::var(name)?)?;
    if value.is_nil() {
        return Err("nonzero_identifier_required".into());
    }
    Ok(value)
}
fn secret_file(name: &str) -> Result<String> {
    let bytes = std::fs::read(env::var(name)?)?;
    if bytes.len() > 4096 {
        return Err("secret_file_too_large".into());
    }
    Ok(String::from_utf8(bytes)?
        .trim_end_matches(['\r', '\n'])
        .to_string())
}
pub async fn source() -> Result<()> {
    let org = id("M53_ORGANIZATION_ID")?;
    let source = id("M53_1C_SOURCE_ID")?;
    let name = env::var("M53_ORGANIZATION_NAME")?;
    let code = env::var("M53_1C_SOURCE_CODE")?;
    let key = secret_file("M53_1C_TASK_KEY_FILE")?;
    if name.trim().is_empty()
        || name.len() > 256
        || code.trim().is_empty()
        || code.len() > 128
        || key.len() < 32
    {
        return Err("invalid_source_configuration".into());
    }
    let db = crate::deployment::connect(crate::deployment::Purpose::Migrator).await?;
    let mut tx = db.begin().await?;
    sqlx::query("SET LOCAL ROLE colposcopy_owner")
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO app.organizations(id,name) VALUES($1,$2) ON CONFLICT(id) DO NOTHING")
        .bind(org)
        .bind(name)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO app.integration_sources(id,organization_id,code) VALUES($1,$2,$3) ON CONFLICT(id) DO NOTHING").bind(source).bind(org).bind(code).execute(&mut *tx).await?;
    let valid: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM app.integration_sources WHERE id=$1 AND organization_id=$2)",
    )
    .bind(source)
    .bind(org)
    .fetch_one(&mut *tx)
    .await?;
    if !valid {
        return Err("source_organization_mismatch".into());
    }
    sqlx::query("INSERT INTO app.integration_task_credentials(id,source_id,organization_id,key_sha256) VALUES($1,$2,$3,$4) ON CONFLICT(key_sha256) DO NOTHING")
        .bind(Uuid::new_v4()).bind(source).bind(org).bind(hash(&key)).execute(&mut *tx).await?;
    let valid: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app.integration_task_credentials WHERE source_id=$1 AND organization_id=$2 AND key_sha256=$3 AND revoked_at IS NULL)").bind(source).bind(org).bind(hash(&key)).fetch_one(&mut *tx).await?;
    if !valid {
        return Err("credential_source_mismatch".into());
    }
    tx.commit().await?;
    Ok(())
}
pub async fn user() -> Result<()> {
    if env::var("M53_AUTH_PROVIDER").as_deref() == Ok("1c_web") {
        return Err("own_password_provisioning_disabled_for_1c_web".into());
    }
    let org = id("M53_ORGANIZATION_ID")?;
    let source = id("M53_1C_SOURCE_ID")?;
    let guid = id("M53_DOCTOR_GUID")?;
    let login = env::var("M53_USER_LOGIN")?;
    let password = secret_file("M53_USER_PASSWORD_FILE")?;
    if login.trim().is_empty() || login.len() > 256 || password.len() < 12 {
        return Err("invalid_account_configuration".into());
    }
    let db = crate::deployment::connect(crate::deployment::Purpose::Migrator).await?;
    let mut tx = db.begin().await?;
    sqlx::query("SET LOCAL ROLE colposcopy_owner")
        .execute(&mut *tx)
        .await?;
    let doctor=sqlx::query("SELECT d.id,d.display_name FROM app.doctors d JOIN app.doctor_external_ids e ON e.doctor_id=d.id AND e.organization_id=d.organization_id WHERE e.source_id=$1 AND e.external_guid=$2 AND d.organization_id=$3 AND d.active")
        .bind(source).bind(guid).bind(org).fetch_one(&mut *tx).await?;
    let doctor_id: Uuid = doctor.get("id");
    let existing=sqlx::query("SELECT u.id,u.organization_id,u.doctor_id FROM app.users u JOIN app.user_auth a ON a.user_id=u.id WHERE a.login=$1 FOR UPDATE OF u")
        .bind(&login).fetch_optional(&mut *tx).await?;
    let user = if let Some(row) = existing {
        if row.get::<Uuid, _>("organization_id") != org
            || row.get::<Option<Uuid>, _>("doctor_id") != Some(doctor_id)
        {
            return Err("account_identity_mismatch".into());
        }
        if env::var("M53_REPLACE_USER_PASSWORD").as_deref() != Ok("1") {
            tx.commit().await?;
            return Ok(());
        }
        row.get("id")
    } else {
        let user = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO app.users(id,organization_id,doctor_id,display_name) VALUES($1,$2,$3,$4)",
        )
        .bind(user)
        .bind(org)
        .bind(doctor_id)
        .bind(doctor.get::<String, _>("display_name"))
        .execute(&mut *tx)
        .await?;
        user
    };
    let salt =
        SaltString::encode_b64(Uuid::new_v4().as_bytes()).map_err(|_| "password_salt_failed")?;
    let password_hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| "password_hash_failed")?
        .to_string();
    sqlx::query("INSERT INTO app.user_auth(user_id,login,password_hash) VALUES($1,$2,$3) ON CONFLICT(user_id) DO UPDATE SET password_hash=excluded.password_hash")
        .bind(user).bind(login).bind(password_hash).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO app.roles(code) VALUES('doctor') ON CONFLICT DO NOTHING")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO app.user_roles(user_id,role_code) VALUES($1,'doctor') ON CONFLICT DO NOTHING",
    )
    .bind(user)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Provision a real workstation independently of any clinical/demo seed.
pub async fn workstation() -> Result<()> {
    let org = id("M53_ORGANIZATION_ID")?;
    let workstation = id("M53_WORKSTATION_ID")?;
    let name = env::var("M53_WORKSTATION_NAME")?;
    let key = secret_file("M53_WORKSTATION_KEY_FILE")?;
    if name.trim().is_empty() || name.len() > 256 || key.len() < 32 {
        return Err("invalid_workstation_configuration".into());
    }
    let db = crate::deployment::connect(crate::deployment::Purpose::Migrator).await?;
    let mut tx = db.begin().await?;
    sqlx::query("SET LOCAL ROLE colposcopy_owner")
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO app.workstations(id,organization_id,name) VALUES($1,$2,$3) ON CONFLICT(id) DO NOTHING")
        .bind(workstation).bind(org).bind(name).execute(&mut *tx).await?;
    let owner: Uuid =
        sqlx::query_scalar("SELECT organization_id FROM app.workstations WHERE id=$1 FOR UPDATE")
            .bind(workstation)
            .fetch_one(&mut *tx)
            .await?;
    if owner != org {
        return Err("workstation_organization_mismatch".into());
    }
    let existing: Option<Uuid> = sqlx::query_scalar("SELECT id FROM app.api_credentials WHERE verifier=$1 AND workstation_id=$2 AND purpose='client' AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at>now()) AND 'clinical'=ANY(permissions)")
        .bind(hash(&key)).bind(workstation).fetch_optional(&mut *tx).await?;
    if existing.is_some() {
        tx.commit().await?;
        return Ok(());
    }
    let used: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app.api_credentials WHERE verifier=$1)")
            .bind(hash(&key))
            .fetch_one(&mut *tx)
            .await?;
    if used {
        return Err("workstation_key_already_used".into());
    }
    let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app.api_credentials WHERE workstation_id=$1 AND purpose='client' AND revoked_at IS NULL)")
        .bind(workstation).fetch_one(&mut *tx).await?;
    if active && env::var("M53_REPLACE_WORKSTATION_KEY").as_deref() != Ok("1") {
        return Err("explicit_workstation_rotation_required".into());
    }
    if active {
        sqlx::query("UPDATE app.api_credentials SET revoked_at=now() WHERE workstation_id=$1 AND purpose='client' AND revoked_at IS NULL")
            .bind(workstation).execute(&mut *tx).await?;
    }
    sqlx::query("INSERT INTO app.api_credentials(id,workstation_id,purpose,verifier,permissions) VALUES($1,$2,'client',$3,ARRAY['clinical'])")
        .bind(Uuid::new_v4()).bind(workstation).bind(hash(key)).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Generate a signing key once, with restrictive permissions and no overwrite.
pub fn offline_key() -> Result<()> {
    use std::io::Write;
    let path = std::path::PathBuf::from(
        env::var_os("M53_OFFLINE_SIGNING_KEY_FILE").ok_or("offline_key_path_required")?,
    );
    if !path.is_absolute() {
        return Err("offline_key_path_must_be_absolute".into());
    }
    let bytes = crate::auth_access::generate_signing_key().map_err(std::io::Error::other)?;
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}
