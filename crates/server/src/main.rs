use m53_contracts::{ApiError, SaveProtocolRequest};
use m53_domain::validation::{Dictionary, ValidationMode, validate};
use std::{env, fs, process::ExitCode};

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<_> = env::args().skip(1).collect();
    #[cfg(feature="standalone")]
    {
        if args.first().is_some_and(|s| s == "init-standalone") {
            return match m53_server::standalone::init().await { Ok(())=>ExitCode::SUCCESS, Err(_)=>{eprintln!("Standalone provisioning failed; secret details suppressed");ExitCode::from(2)} };
        }
        if args.first().is_some_and(|s| !matches!(s.as_str(), "serve"|"check-database"|"generate-offline-key"|"validate"|"import-terminology"|"approve-terminology"|"reconcile")) {
            eprintln!("Command unavailable in standalone edition"); return ExitCode::from(2);
        }
    }
    if args.first().is_some_and(|s| s == "sync-core-diagnoses") {
        let result=async {
            let db=m53_server::deployment::connect(m53_server::deployment::Purpose::Runtime).await.map_err(|_|"database_connection_failed")?;
            m53_server::diagnoses::sync(&m53_server::api::App{db,storage:Default::default()}).await
        }.await;
        return match result {Ok(())=>ExitCode::SUCCESS,Err(code)=>{eprintln!("Diagnosis catalogue synchronization failed: {code}");ExitCode::from(2)}};
    }
    if args.first().is_some_and(|s| s == "bind-clinic-branch") {
        return if m53_server::clinic::bind().await.is_ok() {
            ExitCode::SUCCESS
        } else {
            eprintln!(
                "Workstation binding is obsolete; sync-core-clinics sets the organization default"
            );
            ExitCode::from(2)
        };
    }
    if args
        .first()
        .is_some_and(|s| s == "publish-report-letterhead")
    {
        return if m53_server::clinic::publish().await.is_ok() {
            ExitCode::SUCCESS
        } else {
            eprintln!("Report letterhead publication failed");
            ExitCode::from(2)
        };
    }
    if args.first().is_some_and(|s| s == "sync-core-clinics") {
        let result = async {
            let db =
                m53_server::deployment::connect(m53_server::deployment::Purpose::Runtime).await?;
            let org = env::var("M53_ORGANIZATION_ID")?.parse()?;
            m53_server::clinic::sync(
                &m53_server::api::App {
                    db,
                    storage: Default::default(),
                },
                org,
            )
            .await
            .map_err(|e| e as Box<dyn std::error::Error>)
        }
        .await;
        return if result.is_ok() {
            ExitCode::SUCCESS
        } else {
            eprintln!("Clinic synchronization failed; old snapshot retained");
            ExitCode::from(2)
        };
    }
    if args.first().is_some_and(|s| s == "check-database") {
        return if m53_server::deployment::connect(m53_server::deployment::Purpose::Runtime)
            .await
            .is_ok()
        {
            ExitCode::SUCCESS
        } else {
            eprintln!("Database identity, schema or runtime role check failed");
            ExitCode::from(2)
        };
    }

    if args.first().is_some_and(|s| {
        matches!(
            s.as_str(),
            "configure-task-source"
                | "provision-doctor-user"
                | "provision-workstation"
                | "generate-offline-key"
        )
    }) {
        let result = if args[0] == "configure-task-source" {
            m53_server::administration::source().await
        } else if args[0] == "provision-workstation" {
            m53_server::administration::workstation().await
        } else if args[0] == "generate-offline-key" {
            m53_server::administration::offline_key()
        } else {
            m53_server::administration::user().await
        };
        return if result.is_ok() {
            ExitCode::SUCCESS
        } else {
            eprintln!("Administrative configuration failed; secret details suppressed");
            ExitCode::from(2)
        };
    }
    if args.first().is_some_and(|s| {
        matches!(
            s.as_str(),
            "sync-1c-doctors" | "enrich-1c-once" | "sync-core-doctors" | "enrich-core-once"
        )
    }) {
        let result = async {
            let db = m53_server::deployment::connect(m53_server::deployment::Purpose::Runtime)
                .await
                .map_err(|_| "database_connection_failed")?;
            let source = env::var("M53_ODATA_CORE_SOURCE_ID")
                .ok()
                .and_then(|s| uuid::Uuid::parse_str(&s).ok())
                .ok_or("source_id_required")?;
            let app = m53_server::api::App {
                db,
                storage: Default::default(),
            };
            if matches!(args[0].as_str(), "sync-1c-doctors" | "sync-core-doctors") {
                m53_server::core_sync::sync_doctors(&app, source)
                    .await
                    .map_err(|_| "doctor_sync_failed")?;
            } else {
                m53_server::core_sync::run_once(&app)
                    .await
                    .map_err(|_| "patient_enrichment_failed")?;
            }
            Ok::<(), &str>(())
        }
        .await;
        return if result.is_ok() {
            ExitCode::SUCCESS
        } else {
            eprintln!("Core synchronization failed; previous data retained");
            ExitCode::from(2)
        };
    }
    if args.first().is_some_and(|s| s == "configure-1c-pdf") {
        return if m53_server::colposcopy_results::configure_from_env()
            .await
            .is_ok()
        {
            ExitCode::SUCCESS
        } else {
            eprintln!("1C PDF configuration failed; secret details suppressed");
            ExitCode::from(2)
        };
    }
    if args.first().is_some_and(|s| s == "send-1c-results") {
        let result = async {
            let db = m53_server::deployment::connect(m53_server::deployment::Purpose::Runtime)
                .await
                .map_err(|_| "database_connection_failed")?;
            let storage = env::var("M53_STORAGE_DIR")
                .map_err(|_| "storage_configuration_required")?
                .into();
            let app = m53_server::api::App { db, storage };
            for _ in 0..100 {
                if !m53_server::colposcopy_results::run_once(&app)
                    .await
                    .map_err(|_| "result_delivery_failed")?
                {
                    break;
                }
            }
            Ok::<(), &str>(())
        }
        .await;
        return if result.is_ok() {
            ExitCode::SUCCESS
        } else {
            eprintln!("1C result delivery failed; safe job state retained");
            ExitCode::from(2)
        };
    }
    if args.first().is_some_and(|s| s == "rotate-key") {
        return if m53_server::rotation::rotate().await.is_ok() {
            ExitCode::SUCCESS
        } else {
            eprintln!("Key rotation failed; transaction rolled back");
            ExitCode::from(2)
        };
    }
    if args.first().is_some_and(|s|s=="approve-terminology") {
        return if args.len()==2 && m53_server::terminology::approve_review(&args[1]).await.is_ok() {ExitCode::SUCCESS}else{eprintln!("Terminology review authorization failed");ExitCode::from(2)};
    }
    if args.first().is_some_and(|s| s == "import-terminology") {
        return if args.len() == 2 && m53_server::terminology::import(&args[1]).await.is_ok() {
            ExitCode::SUCCESS
        } else {
            eprintln!("Terminology import failed");
            ExitCode::from(2)
        };
    }
    if args.first().is_some_and(|s| s == "reconcile") {
        return if m53_server::storage::reconcile(args.iter().any(|s| s == "--cleanup"))
            .await
            .is_ok()
        {
            ExitCode::SUCCESS
        } else {
            eprintln!("Storage reconciliation failed");
            ExitCode::from(2)
        };
    }
    if args
        .first()
        .is_some_and(|s| s == "serve" || s == "seed-demo")
    {
        let result = if args[0] == "serve" {
            m53_server::setup::serve().await
        } else {
            m53_server::setup::seed_demo().await
        };
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => {
                eprintln!("Service initialization failed; check development configuration");
                ExitCode::from(2)
            }
        };
    }
    if args.len() != 3 && args.len() != 4 {
        eprintln!("Usage: m53-server validate REQUEST.json DICTIONARY.json [--approval]");
        return ExitCode::from(2);
    }
    if args[0] != "validate" || (args.len() == 4 && args[3] != "--approval") {
        eprintln!("Unsupported command or option");
        return ExitCode::from(2);
    }
    let request = fs::read(&args[1])
        .ok()
        .and_then(|data| serde_json::from_slice::<SaveProtocolRequest>(&data).ok());
    let dictionary = fs::read(&args[2])
        .ok()
        .and_then(|data| serde_json::from_slice::<Dictionary>(&data).ok());
    let (Some(request), Some(dictionary)) = (request, dictionary) else {
        // Do not echo input values or file contents in errors.
        eprintln!("Cannot read input or invalid JSON structure");
        return ExitCode::from(2);
    };
    let mut errors = request.validate(&dictionary);
    if args.len() == 4 {
        for error in validate(&request.protocol, &dictionary, ValidationMode::Approval) {
            if !errors.contains(&error) {
                errors.push(error);
            }
        }
    }
    if errors.is_empty() {
        println!("{{\"valid\":true}}");
        ExitCode::SUCCESS
    } else {
        println!(
            "{}",
            serde_json::to_string(&ApiError::Validation { errors })
                .expect("serializable validation errors")
        );
        ExitCode::from(1)
    }
}
