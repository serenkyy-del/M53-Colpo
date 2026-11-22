//! Bundled, offline-only Chromium + PDF.js. No installed browser or WinRT PDF.
//! The browser is reused; disposable contexts release native image/canvas memory.
//! Preview pixels come from the returned PDF bytes.
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    fs,
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
    os::windows::{io::AsRawHandle, process::CommandExt},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket, protocol::WebSocketConfig};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    },
};

#[path = "print_runtime.rs"]
mod bundled;
use bundled::{hash, runtime};

const LIMIT: usize = 128 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(45);
static GENERATION: AtomicU64 = AtomicU64::new(0);
const PDF_JS: &str = include_str!("../../../../assets/print-runtime/render.js");
const CSP: &str = "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data: blob:; style-src 'unsafe-inline'; font-src data: blob:; script-src 'none'; connect-src 'none'; object-src 'none'; base-uri 'none'\">";

fn check_generation(generation: u64) -> Result<(), String> {
    if generation == GENERATION.load(Ordering::Relaxed) {
        Ok(())
    } else {
        Err("Предпросмотр отменен".into())
    }
}

struct Job(HANDLE);
unsafe impl Send for Job {}
impl Drop for Job {
    fn drop(&mut self) {
        self.close();
    }
}
impl Job {
    fn close(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CloseHandle(std::mem::replace(&mut self.0, std::ptr::null_mut()));
            }
        }
    }
    fn own(child: &Child) -> Result<Self, String> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err("Не удалось изолировать процесс печати".into());
        }
        let job = Self(handle);
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as _,
                std::mem::size_of_val(&info) as u32,
            )
        } == 0
            || unsafe { AssignProcessToJobObject(handle, child.as_raw_handle() as _) } == 0
        {
            return Err("Не удалось изолировать процесс печати".into());
        }
        Ok(job)
    }
}
struct Browser {
    child: Child,
    socket: WebSocket<TcpStream>,
    id: u64,
    context: String,
    print: String,
    raster: String,
    raster_script: String,
    profile: PathBuf,
    _job: Job,
    generation: u64,
    cancellable: bool,
}
impl Drop for Browser {
    fn drop(&mut self) {
        // Close the kill-on-close job before removing its still-open profile files.
        self._job.close();
        let _ = self.child.kill();
        let _ = self.child.wait();
        let deadline = Instant::now() + Duration::from_millis(250);
        while fs::remove_dir_all(&self.profile).is_err()
            && self.profile.exists()
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Browser {
    fn start(generation: Option<u64>) -> Result<Self, String> {
        let check_cancelled = || generation.map_or(Ok(()), check_generation);
        let root = runtime()?;
        let module =
            fs::read(root.join("pdfjs/pdf.mjs")).map_err(|_| "Нет модуля просмотра PDF")?;
        let worker =
            fs::read(root.join("pdfjs/pdf.worker.mjs")).map_err(|_| "Нет модуля просмотра PDF")?;
        check_cancelled()?;
        let profile = std::env::temp_dir().join(format!("m53-print-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&profile).map_err(|_| "Не удалось подготовить процесс печати")?;
        let spawned = (|| {
            check_cancelled()?;
            Command::new(root.join("chrome/chrome-headless-shell.exe"))
                .args([
                    "--headless",
                    "--disable-gpu",
                    "--no-first-run",
                    "--no-default-browser-check",
                    "--disable-extensions",
                    "--disable-background-networking",
                    "--disable-sync",
                    "--disable-component-update",
                    "--disk-cache-size=1",
                    "--media-cache-size=1",
                    "--remote-debugging-address=127.0.0.1",
                    "--remote-debugging-port=0",
                    "--host-resolver-rules=MAP * ~NOTFOUND",
                    "--renderer-process-limit=2",
                    "about:blank",
                ])
                .arg(format!("--user-data-dir={}", profile.display()))
                .creation_flags(0x08000000)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|_| "Не удалось запустить встроенную печать".to_string())
        })();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                let _ = fs::remove_dir_all(&profile);
                return Err(error);
            }
        };
        let job = match Job::own(&child) {
            Ok(job) => job,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_dir_all(&profile);
                return Err(e);
            }
        };
        let connected = (|| {
            let started = Instant::now();
            let info = loop {
                check_cancelled()?;
                if let Ok(info) = fs::read_to_string(profile.join("DevToolsActivePort")) {
                    break info;
                }
                if started.elapsed() > Duration::from_secs(15)
                    || child
                        .try_wait()
                        .map_err(|_| "Ошибка встроенной печати")?
                        .is_some()
                {
                    return Err("Встроенная печать не запустилась".into());
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            let mut lines = info.lines();
            let port = lines
                .next()
                .and_then(|s| s.parse::<u16>().ok())
                .filter(|p| *p != 0)
                .ok_or("Нет локального порта печати")?;
            let path = lines
                .next()
                .filter(|s| s.starts_with("/devtools/browser/") && !s.contains(['?', '#']))
                .ok_or("Нет канала печати")?;
            let stream = TcpStream::connect_timeout(
                &SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into(),
                Duration::from_secs(5),
            )
            .map_err(|_| "Нет локального канала печати")?;
            stream
                .set_read_timeout(Some(Duration::from_millis(250)))
                .map_err(|_| "Ошибка локального канала")?;
            stream
                .set_write_timeout(Some(TIMEOUT))
                .map_err(|_| "Ошибка локального канала")?;
            let config = WebSocketConfig::default()
                .max_message_size(Some(256 * 1024 * 1024))
                .max_frame_size(Some(256 * 1024 * 1024));
            tungstenite::client::client_with_config(
                format!("ws://127.0.0.1:{port}{path}"),
                stream,
                Some(config),
            )
            .map(|v| v.0)
            .map_err(|_| "Нет локального канала печати".into())
        })();
        let socket = match connected {
            Ok(s) => s,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_dir_all(&profile);
                return Err(e);
            }
        };
        let b = Self {
            child,
            socket,
            id: 0,
            context: String::new(),
            print: String::new(),
            raster: String::new(),
            raster_script: PDF_JS
                .replace("__PDF_MODULE__", &STANDARD.encode(module))
                .replace("__PDF_WORKER__", &STANDARD.encode(worker)),
            profile,
            _job: job,
            generation: GENERATION.load(Ordering::Relaxed),
            cancellable: true,
        };
        Ok(b)
    }
    fn call(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        self.id += 1;
        let id = self.id;
        let mut message = json!({"id":id,"method":method,"params":params});
        if let Some(session) = session {
            message["sessionId"] = session.into();
        }
        let message = Message::Text(message.to_string().into());
        if self.cancellable {
            check_generation(self.generation)?;
        }
        self.socket
            .send(message)
            .map_err(|_| "Прерван процесс печати")?;
        let start = Instant::now();
        loop {
            if self.cancellable {
                check_generation(self.generation)?;
            }
            if start.elapsed() > TIMEOUT {
                return Err("Превышено время встроенной печати".into());
            }
            let message = match self.socket.read() {
                Ok(message) => message,
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(_) => return Err("Прерван процесс печати".into()),
            };
            if let Message::Text(text) = message {
                let data: Value =
                    serde_json::from_str(&text).map_err(|_| "Некорректный ответ печати")?;
                if data["id"].as_u64() != Some(id) {
                    continue;
                }
                if data.get("error").is_some() {
                    return Err("Встроенная печать отклонила команду".into());
                }
                return Ok(data["result"].clone());
            }
        }
    }
    fn begin_context(&mut self) -> Result<(), String> {
        let result = self.call(None, "Target.createBrowserContext", json!({}))?;
        self.context = result["browserContextId"]
            .as_str()
            .ok_or("Нет изолированной страницы печати")?
            .to_owned();
        Ok(())
    }
    fn end_context(&mut self) -> Result<(), String> {
        // PDF.js destroys its document/worker, but Chromium retains large native
        // image/canvas pools. Disposing the context also terminates its renderer.
        self.call(
            None,
            "Target.disposeBrowserContext",
            json!({"browserContextId":self.context}),
        )?;
        self.context.clear();
        self.print.clear();
        self.raster.clear();
        Ok(())
    }
    fn target(&mut self) -> Result<String, String> {
        let target = self.call(
            None,
            "Target.createTarget",
            json!({"url":"about:blank","browserContextId":self.context}),
        )?;
        let result = self.call(
            None,
            "Target.attachToTarget",
            json!({"targetId":target["targetId"],"flatten":true}),
        )?;
        let session = result["sessionId"]
            .as_str()
            .map(str::to_owned)
            .ok_or("Нет страницы печати")?;
        self.call(Some(&session), "Network.enable", json!({}))?;
        self.call(
            Some(&session),
            "Network.setBlockedURLs",
            json!({"urls":["http://*","https://*","ftp://*","ws://*","wss://*","file://*"]}),
        )?;
        self.call(
            Some(&session),
            "Network.setCacheDisabled",
            json!({"cacheDisabled":true}),
        )?;
        Ok(session)
    }
    fn raster_session(&mut self) -> Result<String, String> {
        if self.raster.is_empty() {
            self.raster = self.target()?;
            self.eval(&self.raster.clone(), &self.raster_script.clone())?;
        }
        Ok(self.raster.clone())
    }
    fn eval(&mut self, session: &str, expression: &str) -> Result<Value, String> {
        let result = self.call(Some(session), "Runtime.evaluate", json!({"expression":expression,"awaitPromise":true,"returnByValue":true,"timeout":40000}))?;
        if result["exceptionDetails"]["exception"]["description"]
            .as_str()
            .is_some_and(|v| v.contains("M53_PREVIEW_LIMIT"))
        {
            return Err(
                "Предпросмотр ограничен 20 страницами; PDF можно сохранить и напечатать полностью"
                    .into(),
            );
        }
        if result.get("exceptionDetails").is_some() {
            return Err("Не удалось обработать документ встроенным модулем".into());
        }
        Ok(result["result"]["value"].clone())
    }
    fn pdf(&mut self, html: &[u8]) -> Result<Vec<u8>, String> {
        let html = std::str::from_utf8(html).map_err(|_| "Некорректный HTML отчета")?;
        if self.print.is_empty() {
            self.print = self.target()?;
        }
        let session = self.print.clone();
        let frame = self.call(Some(&session), "Page.getFrameTree", json!({}))?;
        // CSP is parsed before any supplied content; report data cannot fetch LAN/Internet resources.
        let head = html
            .find("<head>")
            .ok_or("В отчете отсутствует заголовок HTML")?
            + "<head>".len();
        let secured = format!("{}{CSP}{}", &html[..head], &html[head..]);
        self.call(
            Some(&session),
            "Page.setDocumentContent",
            json!({"frameId":frame["frameTree"]["frame"]["id"],"html":secured}),
        )?;
        self.eval(&session, "document.fonts.ready.then(() => Promise.all(Array.from(document.images, i => i.decode().catch(() => {}))))")?;
        let footer=super::letterhead::print_footer(html);
        let result = self.call(
            Some(&session),
            "Page.printToPDF",
            json!({"printBackground":true,"preferCSSPageSize":true,"displayHeaderFooter":footer.is_some(),"headerTemplate":"<span></span>","footerTemplate":footer.unwrap_or("<span></span>"),"marginBottom":0.94}),
        )?;
        self.call(
            Some(&session),
            "Page.setDocumentContent",
            json!({"frameId":frame["frameTree"]["frame"]["id"],"html":"<html></html>"}),
        )?;
        let bytes = STANDARD
            .decode(result["data"].as_str().ok_or("PDF не создан")?)
            .map_err(|_| "Некорректный PDF")?;
        if !bytes.starts_with(b"%PDF-") || bytes.len() > LIMIT {
            return Err("Некорректный размер PDF".into());
        }
        Ok(bytes)
    }
    fn pages(&mut self, pdf: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        let session = self.raster_session()?;
        let result = self.eval(
            &session,
            &format!("m53RenderPdf('{}')", STANDARD.encode(pdf)),
        )?;
        let values = result.as_array().ok_or("Не удалось отобразить PDF")?;
        let mut pages = Vec::with_capacity(values.len());
        let mut size = 0;
        for value in values {
            let bytes = STANDARD
                .decode(value.as_str().ok_or("Некорректная страница PDF")?)
                .map_err(|_| "Некорректная страница PDF")?;
            size += bytes.len();
            if size > LIMIT {
                return Err("Предпросмотр слишком большой".into());
            }
            pages.push(bytes);
        }
        Ok(pages)
    }
}
#[derive(Default)]
struct Renderer {
    browser: Option<Browser>,
    pdf: Option<(String, Vec<u8>)>,
    pages: Option<(String, Vec<Vec<u8>>)>,
}
fn renderer() -> &'static Mutex<Renderer> {
    static R: OnceLock<Mutex<Renderer>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Renderer::default()))
}
impl Renderer {
    fn with_browser<T>(
        &mut self,
        generation: Option<u64>,
        work: impl FnOnce(&mut Browser) -> Result<T, String>,
    ) -> Result<T, String> {
        if let Some(generation) = generation {
            check_generation(generation)?;
        }
        if self.browser.is_none() {
            self.browser = Some(Browser::start(generation)?);
        }
        let browser = self.browser.as_mut().unwrap();
        browser.generation = generation.unwrap_or_else(|| GENERATION.load(Ordering::Relaxed));
        browser.cancellable = generation.is_some();
        let result = (|| {
            browser.begin_context()?;
            let value = work(browser)?;
            browser.end_context()?;
            Ok(value)
        })();
        browser.cancellable = true;
        if result.is_err() {
            self.browser = None;
        } // Kill timed out/crashed process; next request gets a clean engine.
        result
    }
}
/// Cancel an obsolete preview without waiting for the renderer mutex.
/// The active child stops at the next CDP read (at most 250 ms).
pub fn cancel_pending() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}
/// Normal app-exit hook. Only this process's owned UUID profile is removed.
/// Cancellation makes an active preview yield; shutdown never waits indefinitely.
pub fn shutdown() {
    cancel_pending();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match renderer().try_lock() {
            Ok(mut renderer) => {
                *renderer = Renderer::default();
                return;
            }
            Err(std::sync::TryLockError::Poisoned(error)) => {
                *error.into_inner() = Renderer::default();
                return;
            }
            Err(std::sync::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => return,
        }
    }
}
/// Warm only bundled code and a fixed synthetic sheet, before any patient is opened.
/// Call on a separate startup thread; this never starts a printer job or uses the network.
pub fn warm_up() -> Result<(), String> {
    let generation = GENERATION.load(Ordering::Relaxed);
    renderer().lock().map_err(|_| "Занят модуль печати")?.with_browser(Some(generation), |browser| {
        let pdf = browser.pdf(b"<!doctype html><html><head><meta charset='utf-8'><style>@page{size:A4}</style></head><body>M53</body></html>")?;
        browser.pages(&pdf).map(|_| ())
    })
}
pub fn pdf(html: &[u8]) -> Result<Vec<u8>, String> {
    let generation = GENERATION.load(Ordering::Relaxed);
    if html.len() > LIMIT {
        return Err("Отчет слишком большой для печати".into());
    }
    let key = hash(html);
    let mut r = renderer().lock().map_err(|_| "Занят модуль печати")?;
    check_generation(generation)?;
    if let Some((old, bytes)) = &r.pdf {
        if *old == key {
            return Ok(bytes.clone());
        }
    }
    let bytes = r.with_browser(Some(generation), |b| b.pdf(html))?;
    r.pdf = Some((key, bytes.clone()));
    r.pages = None;
    Ok(bytes)
}
pub fn pages(pdf: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let generation = GENERATION.load(Ordering::Relaxed);
    if !pdf.starts_with(b"%PDF-") || pdf.len() > LIMIT {
        return Err("Некорректный PDF для предпросмотра".into());
    }
    let key = hash(pdf);
    let mut r = renderer().lock().map_err(|_| "Занят модуль печати")?;
    check_generation(generation)?;
    if let Some((old, pages)) = &r.pages {
        if *old == key {
            return Ok(pages.clone());
        }
    }
    let pages = r.with_browser(Some(generation), |b| b.pages(pdf))?;
    r.pages = Some((key, pages.clone()));
    Ok(pages)
}

fn with_print_document<T>(
    pdf: &[u8],
    cancellable: bool,
    work: impl FnOnce(&mut Browser, usize) -> Result<T, String>,
) -> Result<T, String> {
    let generation = cancellable.then(|| GENERATION.load(Ordering::Relaxed));
    if !pdf.starts_with(b"%PDF-") || pdf.len() > LIMIT {
        return Err("Некорректный PDF для печати".into());
    }
    renderer()
        .lock()
        .map_err(|_| "Занят модуль печати")?
        .with_browser(generation, |browser| {
            let session = browser.raster_session()?;
            let count = browser
                .eval(
                    &session,
                    &format!("m53OpenPrint('{}')", STANDARD.encode(pdf)),
                )?
                .as_u64()
                .filter(|n| *n > 0 && *n <= 100)
                .ok_or("Некорректное число страниц PDF")? as usize;
            let result = work(browser, count);
            let close = browser.eval(&session, "m53ClosePrint()");
            result.and_then(|value| close.map(|_| value))
        })
}
pub fn page_count(pdf: &[u8]) -> Result<usize, String> {
    with_print_document(pdf, true, |_, count| Ok(count))
}
/// Raster one page at a time from the exact PDF, without retaining print bitmaps.
/// This does not interact with printers; the native print dialog owns that decision.
pub fn print_pages(
    pdf: &[u8],
    from: usize,
    to: usize,
    mut consume: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    with_print_document(pdf, false, |browser, count| {
        if from == 0 || from > to || to > count {
            return Err("Некорректный диапазон страниц".into());
        }
        for number in from..=to {
            let encoded = browser.eval(
                &browser.raster.clone(),
                &format!("m53PrintPage({number}, 300)"),
            )?;
            let png = STANDARD
                .decode(
                    encoded
                        .as_str()
                        .ok_or("Не удалось подготовить страницу печати")?,
                )
                .map_err(|_| "Некорректная страница печати")?;
            if png.len() > LIMIT {
                return Err("Страница печати слишком большая".into());
            }
            consume(&png)?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_pdf_never_starts_browser() {
        assert!(pages(b"not a PDF").is_err());
    }
    #[test]
    fn shutdown_removes_this_process_profile() {
        let locked = renderer().lock().unwrap();
        let generation = GENERATION.load(Ordering::Relaxed);
        let (ready, waiting) = std::sync::mpsc::channel();
        let queued = std::thread::spawn(move || {
            ready.send(()).unwrap();
            renderer()
                .lock()
                .unwrap()
                .with_browser(Some(generation), |_| Ok(()))
        });
        waiting.recv().unwrap();
        cancel_pending();
        drop(locked);
        assert!(queued.join().unwrap().unwrap_err().contains("отменен"));
        assert!(
            renderer().lock().unwrap().browser.is_none(),
            "An obsolete queued request must not start Chromium"
        );
        warm_up().unwrap();
        let (profile, pid) = {
            let mut renderer = renderer().lock().unwrap();
            let browser = renderer.browser.as_mut().unwrap();
            assert!(browser.context.is_empty());
            assert!(browser.print.is_empty() && browser.raster.is_empty());
            assert_eq!(
                browser
                    .call(None, "Target.getBrowserContexts", json!({}))
                    .unwrap()["browserContextIds"],
                json!([])
            );
            (browser.profile.clone(), browser.child.id())
        };
        assert!(profile.is_dir());
        warm_up().unwrap();
        {
            let mut renderer = renderer().lock().unwrap();
            assert_eq!(
                renderer.browser.as_ref().unwrap().child.id(),
                pid,
                "The browser stays warm while document contexts are disposed"
            );
            let cancelled =
                renderer.with_browser(Some(GENERATION.load(Ordering::Relaxed)), |browser| {
                    let session = browser.raster_session()?;
                    cancel_pending();
                    browser.eval(&session, "0")
                });
            assert!(cancelled.unwrap_err().contains("отменен"));
            assert!(
                renderer.browser.is_none(),
                "An error/cancel must kill the owned browser and all its contexts"
            );
        }
        assert!(!profile.exists());
        let pdf = pdf(b"<!doctype html><html><head></head><body>print error cleanup</body></html>")
            .unwrap();
        let profile = renderer()
            .lock()
            .unwrap()
            .browser
            .as_ref()
            .unwrap()
            .profile
            .clone();
        assert!(print_pages(&pdf, 0, 1, |_| Ok(())).is_err());
        assert!(renderer().lock().unwrap().browser.is_none());
        assert!(
            !profile.exists(),
            "Print errors must not retain an image renderer"
        );
        warm_up().unwrap();
        let profile = renderer()
            .lock()
            .unwrap()
            .browser
            .as_ref()
            .unwrap()
            .profile
            .clone();
        shutdown();
        assert!(
            !profile.exists(),
            "Only this renderer's owned profile must be removed"
        );
    }
}
