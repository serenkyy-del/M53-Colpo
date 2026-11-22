//! Linux backend: API, PostgreSQL, files and exchange. No report rendering.
pub mod api;
pub mod external;
pub mod setup;
pub mod clinic;
pub mod storage;
pub mod terminology;


pub mod network;

pub mod administration;
pub mod auth_access;
pub mod colposcopy_results;
pub mod colposcopy_tasks;
pub mod core_sync;
pub mod odata_core;
pub mod patient_refresh;
pub mod result_payloads;
pub mod rotation;

pub mod continuity;

pub mod catalog;

pub mod deployment;

pub(crate) mod database;

pub mod login_profiles;

pub mod one_c_web_auth;

pub mod one_c_login;

pub mod one_c_odata_auth;

pub mod client_events;

pub mod client_updates;

pub mod activity;
pub mod protocol_templates;

pub mod phrase_templates;

pub(crate) mod asset_download;

pub(crate) mod thumbnails;

pub(crate) mod worklists;

pub(crate) mod transport;
pub(crate) mod tls_files;

pub(crate) mod public_downloads;

pub(crate) mod anamnesis;

pub(crate) mod workflow_revision;

pub mod diagnoses;

pub mod diagnosis_priority;

pub mod patient_edit;

pub mod study_history;

pub mod body_access;

pub mod rgb;

mod one_c_endpoint;

mod result_diagnoses;

#[cfg(all(test, feature="standalone"))]
mod standalone_tests {
 #[test] fn standalone_rejects_integration_routes() {
  assert!(crate::standalone::integration_path("/api/v1/auth/1c/login"));
  assert!(crate::standalone::integration_path("/api/colposcopy/tasks"));
  assert!(!crate::standalone::integration_path("/api/v1/login"));
  assert!(!crate::standalone::integration_path("/api/v1/studies/123/reports"));
 }
}

#[cfg(feature="standalone")]
pub mod standalone;

#[cfg(all(test,feature="standalone"))]
mod standalone_http_tests {
 #[tokio::test] async fn integration_http_routes_are_absent() {
 let db=sqlx::postgres::PgPoolOptions::new().connect_lazy("postgres://unused:unused@127.0.0.1:1/unused_test").unwrap();
 let app=crate::api::router(crate::api::App{db,storage:Default::default()});
 let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
 let server=tokio::spawn(async move {axum::serve(listener,app).await.unwrap()});
 let client=reqwest::Client::new();
 for path in ["/api/v1/auth/1c/login","/api/colposcopy/tasks","/api/v1/tasks","/api/v1/tasks/page","/api/v1/patients/external","/api/v1/studies/00000000-0000-0000-0000-000000000001/results/1c"] {
 assert_eq!(client.get(format!("http://{address}{path}")).send().await.unwrap().status(),reqwest::StatusCode::NOT_FOUND,"{path}");
 }

 server.abort();
 }
}
