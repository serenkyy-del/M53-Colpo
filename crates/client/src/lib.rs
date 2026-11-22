//! Windows client: import, annotations and client-side reports.
pub mod connection;
#[cfg(not(feature = "standalone"))]
pub mod transport;
#[cfg(feature = "standalone")]
#[path = "standalone_transport.rs"]
pub mod transport;
#[cfg(windows)]
pub mod credentials;
#[cfg(all(target_os = "linux", feature = "standalone"))]
#[path = "linux_credentials.rs"]
pub mod credentials;
#[cfg(all(target_os = "linux", feature = "standalone"))]
pub mod linux_storage;
#[cfg(all(windows, feature = "legacy-ui"))]
pub mod forms;
#[cfg(all(windows, feature = "legacy-ui"))]
pub mod gui;
pub mod imaging;
pub mod model;
pub mod report;
pub mod report_identity;
pub mod report_assets;

pub mod cache;
pub mod care_path;
pub mod offline;
pub mod patient_demographics;
pub mod patient_edit;
pub mod pedal;
#[cfg(windows)]
pub mod preview;

#[cfg(any(windows, all(target_os = "linux", feature = "standalone")))]
pub mod offline_profile;

#[cfg(all(windows, feature = "legacy-ui"))]
pub mod web_panel;
pub mod workspace;

pub mod diagnostics;
pub mod panel_transport;

pub mod defects;

#[cfg(feature = "legacy-ui")]
pub mod editor_ui;

#[cfg(windows)]
pub mod video_probe;

pub mod video;

pub mod one_c_auth;

pub mod events;

pub mod delivery;

#[cfg(not(feature = "standalone"))]
pub mod updates;

pub mod activity;

pub mod phrases;

pub mod ui_guidance;

pub mod outbox;
pub mod diagnoses;
pub mod diagnosis_rows;
pub mod diagnosis_cache;
pub mod diagnosis_priority;

/// Windows storage namespace and program name for this edition.
pub const APP_DATA_DIR: &str = if cfg!(feature = "standalone") {
    "Colposcopy"
} else {
    "M53Colposcopy"
};
pub const APP_NAME: &str = if cfg!(feature = "standalone") {
    "Кольпоскопия"
} else {
    "М53 · Кольпоскопия"
};

/// The build may point to the release containing its corresponding source.
pub fn public_source_url() -> &'static str {
    option_env!("COLPOSCOPY_SOURCE_URL")
        .filter(|url| url.starts_with("https://"))
        .unwrap_or("https://github.com/serenkyy-del/M53-Colpo")
}
