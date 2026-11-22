#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(all(windows, feature = "legacy-ui"))]
fn main() {
    m53_client::diagnostics::install();
    if std::env::args().any(|arg| arg == "--web-panel") {
        let result = std::panic::catch_unwind(m53_client::web_panel::run_child);
        if !matches!(result, Ok(Ok(()))) {
            let _ = m53_client::panel_transport::write(
                &mut std::io::stdout().lock(),
                &serde_json::json!({"host_error":"initialization-or-panic"}),
            );
            std::process::exit(2);
        }
        return;
    }
    if !matches!(
        std::panic::catch_unwind(m53_client::gui::launch),
        Ok(Ok(()))
    ) {
        let message: Vec<u16> =
            "Не удалось запустить интерфейс М53. Проверьте драйвер видеокарты и поддержку OpenGL."
                .encode_utf16()
                .chain(Some(0))
                .collect();
        let title: Vec<u16> = "М53 — запуск".encode_utf16().chain(Some(0)).collect();
        // SAFETY: immutable NUL-terminated UTF-16 buffers live for the modal call.
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                std::ptr::null_mut(),
                message.as_ptr(),
                title.as_ptr(),
                0x10,
            );
        }
    }
}
#[cfg(not(windows))]
fn main() {
    eprintln!("M53 desktop UI runs on Windows. Rendering and API components are tested on Linux.");
}

#[cfg(all(windows, not(feature = "legacy-ui")))]
fn main() {
    eprintln!("Use the Rust/GPUI application executable.");
}
