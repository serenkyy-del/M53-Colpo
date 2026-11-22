//! Isolated WebView2 host. The parent owns data, cache, API and report processing.
use crate::workspace::Workspace;
use m53_contracts::workflow::*;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    sync::mpsc,
    time::{Duration, Instant},
};
use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};
use wry::{WebViewBuilder, http::Response};
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: u64,
    command: String,
    #[serde(default)]
    body: Value,
}
#[derive(Debug)]
enum UiEvent {
    Response(Value),
    Close(Value),
    HostGone,
    CloseRequested,
}
enum Job {
    Request(Request),
    Finished(u64, Result<Value, String>),
    HostGone,
    Close,
}
pub struct Panel {
    pub result: mpsc::Receiver<Result<Value, String>>,
    close: mpsc::Sender<()>,
    bounds: mpsc::Sender<Value>,
}
impl Panel {
    pub fn set_bounds(&self, bounds: [i32; 4]) {
        let _ = self.bounds.send(json!({"event":"bounds","bounds":bounds}));
    }
    pub fn navigate(&self, report: bool) {
        let _ = self
            .bounds
            .send(json!({"event":"navigate","screen":if report {"report"} else {"protocol"}}));
    }
    pub fn request_close(&self) {
        let _ = self.close.send(());
    }
}
pub fn open(
    workspace: Workspace,
    report: bool,
    context: eframe::egui::Context,
    parent: isize,
) -> Panel {
    let (result_tx, result) = mpsc::channel();
    let (close, close_rx) = mpsc::channel();
    let (bounds, bounds_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_parent(workspace, report, close_rx, bounds_rx, context, parent)
        }))
        .unwrap_or_else(|_| Err("Не удалось открыть форму; локальный журнал сохранен".into()));
        let _ = result_tx.send(result);
    });
    Panel {
        result,
        close,
        bounds,
    }
}
fn run_parent(
    mut workspace: Workspace,
    report: bool,
    close_rx: mpsc::Receiver<()>,
    bounds_rx: mpsc::Receiver<Value>,
    context: eframe::egui::Context,
    parent: isize,
) -> Result<Value, String> {
    use std::io::BufReader;
    use std::process::{Command, Stdio};
    if let Some(journal) = workspace.journal.as_object_mut() {
        journal.remove("panel_error");
    }
    if parent == 0 {
        return Err("Не найдено главное окно приложения".into());
    }
    // The OpenGL parent must not paint over the embedded child surface.
    // SAFETY: parent is the live HWND supplied by eframe; only a clipping style is added.
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GWL_STYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_CLIPCHILDREN,
        };
        let hwnd = parent as windows_sys::Win32::Foundation::HWND;
        SetWindowLongPtrW(
            hwnd,
            GWL_STYLE,
            GetWindowLongPtrW(hwnd, GWL_STYLE) | WS_CLIPCHILDREN as isize,
        );
    }
    let mut child = Command::new(std::env::current_exe().map_err(|_| "Не найден EXE приложения")?)
        .arg("--web-panel")
        .arg(parent.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "Не удалось запустить встроенную форму")?;
    crate::diagnostics::event("panel-child-start");
    let mut input = child.stdin.take().ok_or("Не удалось открыть канал формы")?;
    let output = child
        .stdout
        .take()
        .ok_or("Не удалось открыть канал формы")?;
    let (tx, rx) = mpsc::channel();
    let (events, event_rx) = mpsc::channel();
    let reader_jobs = tx.clone();
    let reader_events = events.clone();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(output);
        while let Ok(Some(value)) = crate::panel_transport::read(&mut reader) {
            if value.get("host_close_requested").is_some() {
                let _ = reader_events.send(UiEvent::CloseRequested);
                continue;
            }
            if value.get("host_error").is_some() {
                crate::diagnostics::event("webview-host-initialization-failed");
                break;
            }
            if let Ok(request) = serde_json::from_value::<Request>(value) {
                let _ = reader_jobs.send(Job::Request(request));
            }
        }
        let _ = reader_events.send(UiEvent::HostGone);
    });
    let worker_tx = tx.clone();
    let worker_events = events.clone();
    std::thread::spawn(move || {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            worker(workspace, report, rx, worker_tx, worker_events, context)
        }))
        .is_err()
        {
            let _ = events.send(UiEvent::Response(json!({"event":"worker_failed"})));
        }
    });
    let mut gone = false;
    let mut close_requested = false;
    let mut close_started: Option<Instant> = None;
    let mut last_bounds = None;
    let result = loop {
        for command in bounds_rx.try_iter() {
            if gone {
                break;
            }
            if command["event"] == "bounds" {
                if last_bounds.as_ref() == Some(&command) {
                    continue;
                }
                last_bounds = Some(command.clone());
            }
            let _ = crate::panel_transport::write(&mut input, &command);
        }
        if close_rx.try_recv().is_ok() && !close_requested && !gone {
            close_requested = true;
            close_started = Some(Instant::now());
            if crate::panel_transport::write(&mut input, &json!({"event":"request_close"})).is_err()
            {
                gone = true;
                let _ = tx.send(Job::HostGone);
            }
        }
        if close_started.is_some_and(|t| t.elapsed() > Duration::from_secs(3)) && !gone {
            gone = true;
            crate::diagnostics::event("panel-close-timeout");
            let _ = tx.send(Job::HostGone);
        }
        match event_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(UiEvent::Response(value)) => {
                if value["event"] == "worker_failed" {
                    break Err("Ошибка подготовки формы; локальная запись сохранена. Подробности в client.log".into());
                }
                if !gone && crate::panel_transport::write(&mut input, &value).is_err() {
                    gone = true;
                    crate::diagnostics::event("panel-pipe-closed");
                    let _ = tx.send(Job::HostGone);
                }
            }
            Ok(UiEvent::Close(journal)) => {
                if !gone {
                    let _ = crate::panel_transport::write(&mut input, &json!({"event":"exit"}));
                }
                break Ok(journal);
            }
            Ok(UiEvent::CloseRequested) => {
                close_started.get_or_insert_with(Instant::now);
            }
            Ok(UiEvent::HostGone) => {
                if !gone {
                    gone = true;
                    crate::diagnostics::event("panel-child-exited");
                    let _ = tx.send(Job::HostGone);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => (),
            Err(_) => break Err("Канал формы закрыт; восстановите локальную работу".into()),
        }
    };
    let _ = tx.send(Job::Close);
    drop(input);
    let deadline = Instant::now() + Duration::from_secs(2);
    while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
    crate::diagnostics::event("panel-child-finished");
    result
}
/// Called only by --web-panel. This process has no Connection, Workspace, cache key or assets.
pub fn run_child() -> Result<(), String> {
    let mut event_loop = EventLoopBuilder::<Value>::with_user_event().build();
    use tao::platform::windows::WindowBuilderExtWindows;
    let parent: isize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .filter(|h| *h != 0)
        .ok_or("missing-parent-window")?;
    let window = WindowBuilder::new()
        .with_parent_window(parent)
        .with_decorations(false)
        .with_resizable(false)
        .with_visible(false)
        .with_title("М53 — протокол и отчет")
        .with_inner_size(tao::dpi::LogicalSize::new(1360.0, 900.0))
        .build(&event_loop)
        .map_err(|_| "window-init")?;
    let proxy = event_loop.create_proxy();
    std::thread::spawn(move || {
        let mut input = std::io::BufReader::new(std::io::stdin());
        while let Ok(Some(value)) = crate::panel_transport::read(&mut input) {
            let _ = proxy.send_event(value);
        }
        let _ = proxy.send_event(json!({"event":"exit"}));
    });
    let webview=WebViewBuilder::new()
        .with_custom_protocol("m53".into(),move|_,request|{
            let(mime,bytes):(&str,&'static[u8])=match request.uri().path(){
                "/"=>("text/html; charset=utf-8",include_bytes!("../web/index.html")),
                "/app.css"=>("text/css; charset=utf-8",include_bytes!("../web/app.css")),
                "/app.js"=>("text/javascript; charset=utf-8",include_bytes!("../web/app.js")),
                "/form-schema.json"=>("application/json",include_bytes!("../../../docs/schemas/save-draft.schema.json")),
                _=>return Response::builder().status(404).body(Cow::Borrowed(&b"Not found"[..])).unwrap(),
            };
            Response::builder().header("Content-Type",mime).header("Cache-Control","no-store").header("X-Content-Type-Options","nosniff").header("Content-Security-Policy","default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src data:; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-src 'self' about:; object-src 'none'").body(Cow::Borrowed(bytes)).unwrap()
        })
        .with_initialization_script("window.__m53Embedded=true;")
        .with_url("m53://localhost/").with_incognito(true).with_devtools(false)
        .with_navigation_handler(|url|matches!(url.as_str(),"m53://localhost/"|"http://m53.localhost/"|"https://m53.localhost/"))
        .with_new_window_req_handler(|_,_|wry::NewWindowResponse::Deny)
        .with_download_started_handler(|_,_|false)
        .with_ipc_handler(move|request|{
            if !matches!(request.uri().host(),Some("m53.localhost"|"localhost"))||request.body().len()>2*1024*1024{return;}
            if let Ok(value)=serde_json::from_str::<Value>(request.body()){let _=crate::panel_transport::write(&mut std::io::stdout().lock(),&value);}
        }).build(&window).map_err(|error|{crate::diagnostics::event(&format!("webview-init {error}"));"webview-init"})?;
    crate::diagnostics::event("webview-ready");
    use tao::platform::run_return::EventLoopExtRunReturn;
    let mut close_requested = false;
    event_loop.run_return(move |event, _, control| {
        *control = ControlFlow::Wait;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                if !close_requested {
                    close_requested = true;
                    let _ = crate::panel_transport::write(
                        &mut std::io::stdout().lock(),
                        &json!({"host_close_requested":true}),
                    );
                    let _ = webview
                        .evaluate_script("window.__m53PendingClose=true; window.m53Close?.()");
                }
            }
            Event::UserEvent(value) => {
                if value["event"] == "bounds" {
                    if let Ok([x, y, w, h]) =
                        serde_json::from_value::<[i32; 4]>(value["bounds"].clone())
                    {
                        window.set_outer_position(tao::dpi::PhysicalPosition::new(x, y));
                        window.set_inner_size(tao::dpi::PhysicalSize::new(
                            w.max(1) as u32,
                            h.max(1) as u32,
                        ));
                        if !window.is_visible() {
                            window.set_visible(true);
                            window.set_focus();
                        }
                    }
                } else if value["event"] == "navigate" {
                    let screen = if value["screen"] == "report" {
                        "report"
                    } else {
                        "protocol"
                    };
                    let _ = webview.evaluate_script(&format!(
                        "window.__m53RequestedScreen='{screen}'; window.m53Navigate?.('{screen}')"
                    ));
                } else if value["event"] == "exit" {
                    *control = ControlFlow::Exit;
                } else if value["event"] == "request_close" {
                    close_requested = true;
                    let _ = webview
                        .evaluate_script("window.__m53PendingClose=true; window.m53Close?.()");
                } else {
                    let _ = webview.evaluate_script(&format!("window.m53Receive?.({value})"));
                }
            }
            _ => (),
        }
    });
    Ok(())
}
fn worker(
    mut workspace: Workspace,
    report: bool,
    receiver: mpsc::Receiver<Job>,
    sender: mpsc::Sender<Job>,
    proxy: mpsc::Sender<UiEvent>,
    context: eframe::egui::Context,
) {
    if let Some(offline) = workspace
        .connection
        .as_ref()
        .and_then(|c| c.offline.as_ref())
    {
        offline.fetch("/api/v1/templates");
        offline.fetch("/api/v1/terminology/versions");
    }
    let mut busy = 0usize;
    let mut closing = false;
    let mut last = Instant::now();
    loop {
        let request = match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(Job::Request(request)) => Some(request),
            Ok(Job::Finished(id, result)) => {
                busy = busy.saturating_sub(1);
                let _ = proxy.send(UiEvent::Response(reply(id, result, &workspace)));
                None
            }
            Ok(Job::HostGone) => {
                closing = true;
                workspace.journal["panel_error"] = json!(
                    "Окно формы завершилось с ошибкой. Принятые изменения сохранены; снимки остаются доступны. Журнал: %LOCALAPPDATA%\\M53Colposcopy\\logs\\client.log"
                );
                workspace.persist();
                None
            }
            Ok(Job::Close) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => None,
        };
        if let Some(request) = request {
            let id = request.id;
            let result = (|| -> Result<Value, String> {
                match request.command.as_str() {
                    "ui_error" => {
                        crate::diagnostics::event("html-javascript-error");
                        Ok(json!({}))
                    }
                    "ready" => Ok(
                        json!({"screen":if report{"report"}else{"protocol"},"state":workspace.snapshot()}),
                    ),
                    "checkpoint" => {
                        workspace.checkpoint(request.body)?;
                        Ok(json!({}))
                    }
                    "save" => {
                        workspace.checkpoint(request.body)?;
                        workspace.save()?;
                        Ok(json!({}))
                    }
                    "close" => {
                        workspace.checkpoint(request.body)?;
                        workspace.persist();
                        closing = true;
                        Ok(json!({}))
                    }
                    "approve" => {
                        workspace.approve()?;
                        Ok(json!({}))
                    }
                    "reopen" => {
                        workspace.reopen()?;
                        Ok(json!({}))
                    }
                    "resolve" => {
                        workspace.checkpoint(request.body)?;
                        workspace.resolve()?;
                        Ok(json!({}))
                    }
                    "revise_operation" => {
                        workspace
                            .connection()?
                            .offline
                            .as_ref()
                            .ok_or("Нет журнала")?
                            .revise_failed(request.body)?;
                        Ok(json!({}))
                    }
                    "save_template" => {
                        let layout: ReportLayout =
                            serde_json::from_value(request.body).map_err(|_| "Неверный шаблон")?;
                        workspace.connection()?.post("/api/v1/templates", &layout)
                    }
                    "report" | "archive" | "preview" => {
                        if busy > 0 {
                            return Err("Дождитесь завершения текущей подготовки PDF".into());
                        }
                        let connection = workspace.connection()?.clone();
                        let detail = workspace.detail.clone().ok_or("Нет исследования")?;
                        let preview = request.command == "preview";
                        let task = if request.command == "report" || preview {
                            crate::workspace::check_unfinished_input(&workspace.journal)?;
                            if workspace.dirty && !preview {
                                workspace.save()?;
                            }
                            let detail = workspace.detail.clone().unwrap();
                            let template: Template =
                                serde_json::from_value(request.body["template"].clone())
                                    .map_err(|_| "Выберите сохраненный шаблон")?;
                            let selected: Vec<uuid::Uuid> =
                                serde_json::from_value(request.body["selected"].clone())
                                    .map_err(|_| "Выберите снимки")?;
                            let dictionary = workspace.dictionary.clone().ok_or("Нет словаря")?;
                            Box::new(move || {
                                if preview {
                                    let mut photos = vec![];
                                    for (index, asset) in detail
                                        .assets
                                        .iter()
                                        .filter(|a| a.kind == "original")
                                        .enumerate()
                                        .filter(|(_, a)| selected.contains(&a.id))
                                    {
                                        photos.push(crate::report::Photo {
                                            id: asset.id,
                                            bytes: connection.download(asset.id)?,
                                            caption: format!("Снимок {}", index + 1),
                                        });
                                    }
                                    let (context,_)=crate::report_identity::load(&connection,&detail)?;
                                    let (report_detail,performer)=crate::report_identity::bind(&detail,&context)?;
                                    Ok(crate::report::preview_with_terms(
                                        &report_detail,
                                        &template.layout,
                                        &photos,
                                        &performer,
                                        &dictionary.terms,
                                    )?
                                    .html)
                                } else {
                                    crate::workspace::render_report(
                                        &connection,
                                        &detail,
                                        &template,
                                        &dictionary,
                                        &selected,
                                    )
                                }
                            })
                                as Box<dyn FnOnce() -> Result<Vec<u8>, String> + Send>
                        } else {
                            let asset: uuid::Uuid =
                                serde_json::from_value(request.body["asset"].clone())
                                    .map_err(|_| "Неверный файл")?;
                            if !detail
                                .assets
                                .iter()
                                .any(|a| a.id == asset && a.kind == "pdf")
                            {
                                return Err("Выберите PDF текущего исследования".into());
                            }
                            Box::new(move || connection.download(asset))
                                as Box<dyn FnOnce() -> Result<Vec<u8>, String> + Send>
                        };
                        busy += 1;
                        let sender = sender.clone();
                        std::thread::spawn(move || {
                            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                let bytes = task()?;
                                if preview {
                                    return Ok(
                                        json!({"html":String::from_utf8(bytes).map_err(|_|"Неверная кодировка отчета")?}),
                                    );
                                }
                                if let Some(path) = rfd::FileDialog::new()
                                    .add_filter("PDF", &["pdf"])
                                    .set_file_name("report.pdf")
                                    .save_file()
                                {
                                    std::fs::write(&path, bytes)
                                        .map_err(|_| "Не удалось сохранить PDF")?;
                                    crate::preview::open_pdf(&path)?;
                                }
                                Ok(
                                    json!({"message":"PDF подготовлен; отчет сохранен в локальной очереди"}),
                                )
                            })).unwrap_or_else(|_| Err("Ошибка подготовки отчета. Исходные данные сохранены; подробности в client.log".into()));
                            let _ = sender.send(Job::Finished(id, result));
                        });
                        Ok(json!({"background":true}))
                    }
                    _ => Err("Команда не поддерживается HTML-формой".into()),
                }
            })();
            let _ = proxy.send(UiEvent::Response(reply(id, result, &workspace)));
        }
        workspace.tick();
        if closing
            && busy == 0
            && workspace
                .connection
                .as_ref()
                .and_then(|c| c.offline.as_ref())
                .is_none_or(|o| o.information().2)
        {
            workspace.persist();
            if workspace
                .connection
                .as_ref()
                .and_then(|c| c.offline.as_ref())
                .is_none_or(|o| o.information().2)
            {
                let _ = proxy.send(UiEvent::Close(workspace.journal.clone()));
                context.request_repaint();
                break;
            }
        }
        if last.elapsed() > Duration::from_millis(700) {
            let _ = proxy.send(UiEvent::Response(
                json!({"event":"state","state":workspace.snapshot(),"busy":busy,"closing":closing}),
            ));
            last = Instant::now();
        }
    }
}
fn reply(id: u64, result: Result<Value, String>, workspace: &Workspace) -> Value {
    match result {
        Ok(data) => json!({"id":id,"ok":true,"data":data,"state":workspace.snapshot()}),
        Err(error) => json!({"id":id,"ok":false,"error":error,"state":workspace.snapshot()}),
    }
}
