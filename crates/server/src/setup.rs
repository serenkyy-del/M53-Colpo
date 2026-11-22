use crate::api::{App, hash};
use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use sqlx::{PgPool, Row};
use std::{env, path::PathBuf};
use uuid::Uuid;

pub async fn connect() -> Result<PgPool, Box<dyn std::error::Error>> {
    development_connection(false).await
}
pub(crate) async fn runtime_connect() -> Result<PgPool, Box<dyn std::error::Error>> {
    development_connection(true).await
}
async fn development_connection(runtime: bool) -> Result<PgPool, Box<dyn std::error::Error>> {
    let url = env::var("M53_DATABASE_URL")?;
    let db = if runtime { crate::database::connect(&url).await? } else {
        sqlx::postgres::PgPoolOptions::new().max_connections(8).connect(&url).await?
    };
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&db)
        .await?;
    if !name.ends_with("_dev") && !name.ends_with("_test") {
        return Err("This development build only accepts databases ending _dev or _test".into());
    }
    Ok(db)
}
pub async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let db = crate::deployment::connect(crate::deployment::Purpose::Runtime).await?;
    let privileges=sqlx::query("SELECT rolsuper,rolcreatedb,rolcreaterole,pg_has_role(current_user,'colposcopy_owner','USAGE') AS is_owner FROM pg_roles WHERE rolname=current_user").fetch_one(&db).await?;
    if privileges.get::<bool, _>("rolsuper")
        || privileges.get::<bool, _>("rolcreatedb")
        || privileges.get::<bool, _>("rolcreaterole")
        || privileges.get::<bool, _>("is_owner")
    {
        return Err("Runtime requires a restricted app role".into());
    }
    let storage = PathBuf::from(env::var("M53_STORAGE_DIR")?);
    if !storage.is_absolute() {
        return Err("M53_STORAGE_DIR must be absolute".into());
    }
    tokio::fs::create_dir_all(&storage).await?;
    let address: std::net::SocketAddr = env::var("M53_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:8787".into())
        .parse()?;
    if !address.ip().is_loopback() && env::var("M53_DEV_ALLOW_REMOTE_HTTP").as_deref() != Ok("1") {
        return Err("Remote HTTP requires explicit M53_DEV_ALLOW_REMOTE_HTTP=1 for synthetic development; otherwise use a TLS reverse proxy".into());
    }
    let listener = tokio::net::TcpListener::bind(address).await?;
    eprintln!("M53 service listening on {}", listener.local_addr()?);
    let app = App { db, storage };
    let transport = crate::transport::Prepared::from_env(listener).await.map_err(|_| "TLS transport configuration rejected")?;
    let result_worker = if !cfg!(feature="standalone") && env::var("M53_1C_RESULT_WORKER").as_deref() == Ok("1") {
        let state = app.clone();
        Some(tokio::spawn(async move {
            loop {
                let _ = crate::colposcopy_results::run_once(&state).await;
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }))
    } else {
        None
    };
    let core_worker = if !cfg!(feature="standalone") && env::var("M53_CORE_SYNC_WORKER").as_deref() == Ok("1") {
        let state = app.clone();
        Some(tokio::spawn(async move {
            loop {
                let _ = crate::core_sync::run_once(&state).await;
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }))
    } else {
        None
    };
    let doctor_worker = if !cfg!(feature="standalone") && env::var("M53_CORE_DOCTOR_SYNC_WORKER").as_deref() == Ok("1") {
        let source = Uuid::parse_str(&env::var("M53_ODATA_CORE_SOURCE_ID")?)?;
        if source.is_nil() {
            return Err("A real configured Core source is required".into());
        }
        let seconds = env::var("M53_CORE_DOCTOR_SYNC_INTERVAL_SECONDS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(900);
        if !(60..=86400).contains(&seconds) {
            return Err("Invalid doctor synchronization interval".into());
        }
        let state = app.clone();
        Some(tokio::spawn(async move {
            loop {
                // No access/clinical request depends on Core availability.
                let _ = crate::core_sync::sync_doctors(&state, source).await;
                tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
            }
        }))
    } else {
        None
    };
    let clinic_worker = if !cfg!(feature="standalone") && env::var("M53_CORE_CLINIC_SYNC_WORKER").as_deref() == Ok("1") {
        let org=Uuid::parse_str(&env::var("M53_ORGANIZATION_ID")?)?;
        let seconds=env::var("M53_CORE_CLINIC_SYNC_INTERVAL_SECONDS").ok().and_then(|v|v.parse::<u64>().ok()).unwrap_or(900);
        if org.is_nil() || !(60..=86400).contains(&seconds) { return Err("Invalid clinic synchronization configuration".into()); }
        let state=app.clone();
        Some(tokio::spawn(async move {loop {
            let _=crate::clinic::sync(&state,org).await;
            tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
        }}))
    } else {None};
    let diagnoses_worker=if !cfg!(feature="standalone") && env::var("M53_DIAGNOSES_SYNC_WORKER").as_deref()==Ok("1") {
        let seconds=env::var("M53_DIAGNOSES_SYNC_INTERVAL_SECONDS").ok().map(|v|v.parse::<u64>()).transpose()?.unwrap_or(86400);
        if !(3600..=604800).contains(&seconds) { return Err("Invalid diagnosis synchronization interval".into()); }
        let state=app.clone();Some(tokio::spawn(async move {loop {
            let _=crate::diagnoses::sync(&state).await;
            tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
        }}))
    }else{None};
    let result = transport.run(app).await.map_err(|_| "Transport listener failed");
    if let Some(worker)=diagnoses_worker {worker.abort();}
    if let Some(worker) = clinic_worker {worker.abort();}
    if let Some(worker) = result_worker {
        worker.abort();
    }
    if let Some(worker) = core_worker {
        worker.abort();
    }
    if let Some(worker) = doctor_worker {
        worker.abort();
    }
    result?;
    Ok(())
}
pub async fn seed_demo() -> Result<(), Box<dyn std::error::Error>> {
    let db = connect().await?;
    let password = env::var("M53_DEMO_PASSWORD")?;
    let workstation = env::var("M53_DEMO_WORKSTATION_KEY")?;
    let simple_demo =
        password == "123" && env::var("M53_DEV_ALLOW_SIMPLE_PASSWORD").as_deref() == Ok("1");
    if (password.len() < 12 && !simple_demo) || workstation.len() < 32 {
        return Err("Demo password must be >=12 characters; workstation key >=32".into());
    }
    let salt =
        SaltString::encode_b64(Uuid::new_v4().as_bytes()).map_err(|_| "Salt encoding failed")?;
    let password_hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| "Password hashing failed")?
        .to_string();
    let id = Uuid::from_u128(1);
    let mut tx = db.begin().await?;
    sqlx::query("SET LOCAL ROLE colposcopy_owner")
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO app.organizations(id,name) VALUES($1,'M53 SYNTHETIC DEMO') ON CONFLICT DO NOTHING").bind(id).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO app.users(id,organization_id,display_name) VALUES($1,$1,'Тестовый врач') ON CONFLICT DO NOTHING").bind(id).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO app.roles(code) VALUES('doctor') ON CONFLICT DO NOTHING")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO app.user_roles(user_id,role_code) VALUES($1,'doctor') ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO app.workstations(id,organization_id,name) VALUES($1,$1,'Demo workstation') ON CONFLICT DO NOTHING").bind(id).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO app.api_credentials(id,workstation_id,purpose,verifier,permissions) VALUES($1,$1,'client',$2,ARRAY['clinical']) ON CONFLICT(id) DO UPDATE SET verifier=excluded.verifier").bind(id).bind(hash(workstation)).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO app.user_auth(user_id,login,password_hash) VALUES($1,'demo',$2) ON CONFLICT(user_id) DO UPDATE SET password_hash=excluded.password_hash").bind(id).bind(password_hash).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO app.terminology_versions(id,code,source,clinically_approved) VALUES($1,'SYNTHETIC-DEMO-ONLY','Synthetic test dictionary, NOT IFCPC terminology',false) ON CONFLICT DO NOTHING").bind(id).execute(&mut *tx).await?;
    let already_approved: bool =
        sqlx::query_scalar("SELECT clinically_approved FROM app.terminology_versions WHERE id=$1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    if !already_approved {
        for (code, region, english) in [
            (
                "demo.cervix.observation",
                "cervix",
                "Synthetic cervical observation",
            ),
            (
                "demo.vagina.observation",
                "vagina",
                "Synthetic vaginal observation",
            ),
            (
                "demo.vulva.observation",
                "vulva",
                "Synthetic vulvar observation",
            ),
            (
                "demo.anus.observation",
                "anus",
                "Synthetic anal observation",
            ),
        ] {
            sqlx::query("INSERT INTO app.terminology_terms(version_id,code,region,english,russian,source_locator,kind) VALUES($1,$2,$3,$4,'Тестовый признак','Synthetic test fixture','finding') ON CONFLICT DO NOTHING").bind(id).bind(code).bind(region).bind(english).execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE app.terminology_versions SET clinically_approved=true WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    let layout = m53_contracts::workflow::ReportLayout {
        letterhead: None,
        title: "Протокол кольпоскопии — ТЕСТОВАЯ ВЕРСИЯ".into(),
        blocks: vec![
            m53_contracts::workflow::ReportBlock::Patient,
            m53_contracts::workflow::ReportBlock::Indications,
            m53_contracts::workflow::ReportBlock::Assessment,
            m53_contracts::workflow::ReportBlock::Findings,
            m53_contracts::workflow::ReportBlock::Photos,
            m53_contracts::workflow::ReportBlock::Conclusion,
            m53_contracts::workflow::ReportBlock::Recommendations,
            m53_contracts::workflow::ReportBlock::Signature,
        ],
        photo_columns: 2,
    };
    sqlx::query("INSERT INTO app.report_templates(id,name,organization_id) VALUES($1,'Тестовый шаблон',$1) ON CONFLICT DO NOTHING").bind(id).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO app.report_template_versions(id,template_id,version,schema_version,layout,styles,published_at) VALUES($1,$1,1,1,$2,'',now()) ON CONFLICT DO NOTHING").bind(id).bind(serde_json::to_value(layout)?).execute(&mut *tx).await?;
    tx.commit().await?;
    eprintln!("Synthetic demo initialized; login: demo (credentials were not printed)");
    // Verify server identity cannot be confused with a production connection.
    let _ = sqlx::query("SELECT current_database()")
        .fetch_one(&db)
        .await?
        .get::<String, _>(0);
    Ok(())
}
