//! Capture configuration and latest-frame transport. No clinical persistence or UI work here.
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub numerator: u32,
    pub denominator: u32,
    pub format: String,
}
impl Mode {
    pub fn label(&self) -> String {
        format!(
            "{}×{} · {:.2} кадр/с · {}",
            self.width,
            self.height,
            self.numerator as f64 / self.denominator.max(1) as f64,
            self.format
        )
    }
    pub fn valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.width <= 4096
            && self.height <= 2160
            && self.denominator > 0
            && self.numerator > 0
            && self.numerator as u64 <= 120 * self.denominator as u64
            && ["MJPG", "YUY2", "NV12", "UYVY", "RGB32", "RGB24", "ARGB32"]
                .contains(&self.format.as_str())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub device_id: String,
    pub mode: Mode,
}
impl Config {
    pub fn validate(self) -> Result<Self, String> {
        if self.device_id.is_empty() || self.device_id.len() > 8192 || !self.mode.valid() {
            Err("Некорректные настройки видеозахвата".into())
        } else {
            Ok(self)
        }
    }
}
#[derive(Clone, Debug)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub modes: Vec<Mode>,
}
impl Device {
    pub fn preferred(&self) -> Option<Mode> {
        self.modes
            .iter()
            .min_by_key(|m| {
                (
                    if m.width == 1920 && m.height == 1080 {
                        0
                    } else {
                        1
                    },
                    if m.format == "MJPG" { 0 } else { 1 },
                    std::cmp::Reverse(m.numerator as u64 * 1000 / m.denominator.max(1) as u64),
                )
            })
            .cloned()
    }
}
pub struct Frame {
    pub epoch: u64,
    pub generation: u64,
    pub received: Instant,
    pub jpeg: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}
pub type Latest = Arc<Mutex<Option<Arc<Frame>>>>;
#[cfg(any(windows, test))]
#[derive(Default)]
struct FrameCadence {
    next: Option<Instant>,
}
#[cfg(any(windows, test))]
impl FrameCadence {
    // Admit before copying/decoding the full sample. A late frame is processed once;
    // there is no catch-up queue after a slow codec or a paused device.
    fn admit(&mut self, now: Instant) -> bool {
        if self.next.is_some_and(|next| now < next) {
            return false;
        }
        self.next = Some(now + Duration::from_millis(66));
        true
    }
}
#[cfg(any(windows, test))]
fn preview_frame(
    decoded: image::DynamicImage,
    jpeg: Vec<u8>,
    generation: u64,
    received: Instant,
) -> Frame {
    let preview = decoded.thumbnail(1280, 720);
    let (width, height) = (preview.width(), preview.height());
    let mut bgra = preview.to_rgba8().into_raw();
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Frame {
        epoch: 0,
        generation,
        received,
        jpeg: Arc::new(jpeg),
        width,
        height,
        bgra,
    }
}
/// Capture identity is JPEG metadata; compressed pixels are preserved exactly.
/// Repeated deliberate photos of a still scene must remain distinct assets.
pub fn snapshot_bytes(jpeg: &[u8]) -> Result<Vec<u8>, String> {
    if !jpeg.starts_with(&[0xff, 0xd8]) {
        return Err("Некорректный JPEG видеокадра".into());
    }
    let comment = format!("M53 capture {}", uuid::Uuid::new_v4());
    let mut result = Vec::with_capacity(jpeg.len() + comment.len() + 4);
    result.extend_from_slice(&jpeg[..2]);
    result.extend_from_slice(&[0xff, 0xfe]);
    result.extend_from_slice(&((comment.len() + 2) as u16).to_be_bytes());
    result.extend_from_slice(comment.as_bytes());
    result.extend_from_slice(&jpeg[2..]);
    Ok(result)
}
pub enum Request {
    Enumerate,
    Start(u64, Config),
    Stop(u64),
}
pub enum Event {
    Devices(Result<Vec<Device>, String>),
    Status {
        epoch: u64,
        active: bool,
        message: String,
    },
}
pub struct Worker {
    pub tx: mpsc::Sender<Request>,
    pub events: mpsc::Receiver<Event>,
    pub latest: Latest,
}
pub fn start() -> Worker {
    let (tx, rx) = mpsc::channel();
    let (events, out) = mpsc::channel();
    let latest = Arc::new(Mutex::new(None));
    let shared = latest.clone();
    std::thread::spawn(move || {
        #[cfg(windows)]
        native::run(rx, events, shared);
        #[cfg(not(windows))]
        {
            let _ = (rx, shared);
            let epoch = 0;
            let _ = events.send(Event::Status {
                epoch,
                active: false,
                message: "Видеозахват доступен в Windows".into(),
            });
        }
    });
    Worker {
        tx,
        events: out,
        latest,
    }
}
#[cfg(windows)]
mod native {
    use super::*;
    use windows::{
        Win32::{Media::MediaFoundation::*, System::Com::*},
        core::{HRESULT, Interface, Ref, implement},
    };
    struct Platform;
    impl Platform {
        fn new() -> windows::core::Result<Self> {
            unsafe {
                CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
                if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                    CoUninitialize();
                    return Err(e);
                }
                Ok(Self)
            }
        }
    }
    impl Drop for Platform {
        fn drop(&mut self) {
            unsafe {
                let _ = MFShutdown();
                CoUninitialize();
            }
        }
    }
    struct Devices(*mut Option<IMFActivate>, usize);
    impl Drop for Devices {
        fn drop(&mut self) {
            unsafe {
                if !self.0.is_null() {
                    for n in 0..self.1 {
                        std::ptr::drop_in_place(self.0.add(n));
                    }
                    CoTaskMemFree(Some(self.0.cast()));
                }
            }
        }
    }
    unsafe fn devices() -> windows::core::Result<Devices> {
        unsafe {
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 1)?;
            let attrs = attrs.unwrap();
            attrs.SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            )?;
            let mut ptr = std::ptr::null_mut();
            let mut n = 0;
            MFEnumDeviceSources(&attrs, &mut ptr, &mut n)?;
            Ok(Devices(ptr, n as usize))
        }
    }
    unsafe fn text(
        attrs: &IMFAttributes,
        key: &windows::core::GUID,
    ) -> windows::core::Result<String> {
        unsafe {
            let n = attrs.GetStringLength(key)?;
            let mut out = vec![0; n as usize + 1];
            attrs.GetString(key, &mut out, None)?;
            Ok(String::from_utf16_lossy(&out[..n as usize]))
        }
    }
    fn mode(media: &IMFMediaType) -> windows::core::Result<Mode> {
        unsafe {
            let size = media.GetUINT64(&MF_MT_FRAME_SIZE)?;
            let rate = media.GetUINT64(&MF_MT_FRAME_RATE)?;
            let guid = media.GetGUID(&MF_MT_SUBTYPE)?;
            let format = if guid == MFVideoFormat_MJPG {
                "MJPG"
            } else if guid == MFVideoFormat_NV12 {
                "NV12"
            } else if guid == MFVideoFormat_UYVY {
                "UYVY"
            } else if guid == MFVideoFormat_RGB32 {
                "RGB32"
            } else if guid == MFVideoFormat_RGB24 {
                "RGB24"
            } else if guid == MFVideoFormat_ARGB32 {
                "ARGB32"
            } else if guid == MFVideoFormat_YUY2 {
                "YUY2"
            } else {
                "unsupported"
            };
            Ok(Mode {
                width: (size >> 32) as u32,
                height: size as u32,
                numerator: (rate >> 32) as u32,
                denominator: rate as u32,
                format: format.into(),
            })
        }
    }
    struct Source(IMFMediaSource);
    impl Drop for Source {
        fn drop(&mut self) {
            let _ = unsafe { self.0.Shutdown() };
        }
    }
    fn enumerate() -> windows::core::Result<Vec<Device>> {
        unsafe {
            let list = devices()?;
            let mut result = vec![];
            for n in 0..list.1 {
                let Some(device) = (&*list.0.add(n)).as_ref() else {
                    continue;
                };
                let attrs: IMFAttributes = device.cast()?;
                let id = text(
                    &attrs,
                    &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
                )?;
                let name = text(&attrs, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME)?;
                let mut modes = vec![];
                if let Ok(source) = device.ActivateObject::<IMFMediaSource>() {
                    let source = Source(source);
                    let desc = source.0.CreatePresentationDescriptor()?;
                    for stream in 0..desc.GetStreamDescriptorCount()? {
                        let mut selected = Default::default();
                        let mut info = None;
                        desc.GetStreamDescriptorByIndex(stream, &mut selected, &mut info)?;
                        let handler = info.unwrap().GetMediaTypeHandler()?;
                        if handler.GetMajorType()? != MFMediaType_Video {
                            continue;
                        }
                        for index in 0..handler.GetMediaTypeCount()? {
                            if let Ok(m) = mode(&handler.GetMediaTypeByIndex(index)?) {
                                if m.valid() && !modes.contains(&m) {
                                    modes.push(m);
                                }
                            }
                        }
                    }
                }
                result.push(Device { id, name, modes });
            }
            Ok(result)
        }
    }
    struct Sample {
        status: HRESULT,
        flags: u32,
        sample: Option<IMFSample>,
        received: Instant,
    }
    #[implement(IMFSourceReaderCallback)]
    struct Callback {
        tx: mpsc::SyncSender<Sample>,
    }
    impl IMFSourceReaderCallback_Impl for Callback_Impl {
        fn OnReadSample(
            &self,
            status: HRESULT,
            _: u32,
            flags: u32,
            _: i64,
            sample: Ref<IMFSample>,
        ) -> windows::core::Result<()> {
            let _ = self.tx.try_send(Sample {
                status,
                flags,
                sample: sample.as_ref().cloned(),
                received: Instant::now(),
            });
            Ok(())
        }
        fn OnFlush(&self, _: u32) -> windows::core::Result<()> {
            Ok(())
        }
        fn OnEvent(&self, _: u32, _: Ref<IMFMediaEvent>) -> windows::core::Result<()> {
            Ok(())
        }
    }
    struct Session {
        reader: IMFSourceReader,
        _source: Source,
        rx: mpsc::Receiver<Sample>,
        native_jpeg: bool,
        mode: Mode,
        last: Instant,
        started: Instant,
        cadence: FrameCadence,
    }
    impl Drop for Session {
        fn drop(&mut self) {
            let _ = unsafe { self.reader.Flush(MF_SOURCE_READER_ALL_STREAMS.0 as u32) };
        }
    }
    impl Session {
        fn open(config: Config) -> windows::core::Result<Self> {
            unsafe {
                let list = devices()?;
                let mut source = None;
                for n in 0..list.1 {
                    if let Some(device) = (&*list.0.add(n)).as_ref() {
                        let attrs: IMFAttributes = device.cast()?;
                        if text(
                            &attrs,
                            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
                        )? == config.device_id
                        {
                            source = Some(Source(device.ActivateObject::<IMFMediaSource>()?));
                            break;
                        }
                    }
                }
                let source = source.ok_or_else(|| {
                    windows::core::Error::from_hresult(HRESULT(0x80070490u32 as i32))
                })?;
                let (tx, rx) = mpsc::sync_channel(2);
                let cb: IMFSourceReaderCallback = Callback { tx }.into();
                let mut attrs = None;
                MFCreateAttributes(&mut attrs, 2)?;
                let attrs = attrs.unwrap();
                attrs.SetUnknown(&MF_SOURCE_READER_ASYNC_CALLBACK, &cb)?;
                attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
                let reader = MFCreateSourceReaderFromMediaSource(&source.0, &attrs)?;
                let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
                reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
                reader.SetStreamSelection(stream, true)?;
                let mut found = false;
                for index in 0..512 {
                    let Ok(media) = reader.GetNativeMediaType(stream, index) else {
                        break;
                    };
                    if mode(&media)? == config.mode {
                        reader.SetCurrentMediaType(stream, None, &media)?;
                        found = true;
                        break;
                    }
                }
                if !found {
                    return Err(windows::core::Error::from_hresult(HRESULT(
                        0xC00D36B4u32 as i32,
                    )));
                }
                let native_jpeg = config.mode.format == "MJPG";
                if !native_jpeg {
                    let rgb = MFCreateMediaType()?;
                    rgb.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                    rgb.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
                    reader.SetCurrentMediaType(stream, None, &rgb)?;
                }
                reader.ReadSample(stream, 0, None, None, None, None)?;
                Ok(Self {
                    reader,
                    _source: source,
                    rx,
                    native_jpeg,
                    mode: config.mode,
                    last: Instant::now(),
                    started: Instant::now(),
                    cadence: FrameCadence::default(),
                })
            }
        }
        fn read(&mut self, generation: u64) -> Result<Option<Frame>, String> {
            let item = match self.rx.recv_timeout(Duration::from_millis(50)) {
                Ok(s) => s,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if self.last.elapsed() > Duration::from_secs(3) {
                        return Err("Нет новых кадров. Проверьте плату и HDMI-источник".into());
                    }
                    return Ok(None);
                }
                Err(_) => return Err("Захват остановлен драйвером".into()),
            };
            item.status.ok().map_err(|e| e.to_string())?;
            if self.started.elapsed() < Duration::from_millis(1500) {
                unsafe {
                    self.reader
                        .ReadSample(
                            MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                            0,
                            None,
                            None,
                            None,
                            None,
                        )
                        .map_err(|e| e.to_string())?;
                }
                return Ok(None);
            }
            if item.flags
                & (MF_SOURCE_READERF_ERROR.0
                    | MF_SOURCE_READERF_ENDOFSTREAM.0
                    | MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0) as u32
                != 0
            {
                return Err(
                    "Видеосигнал изменился или устройство отключено. Запустите камеру повторно"
                        .into(),
                );
            }
            let result = (|| -> Result<Option<Frame>, String> {
                let Some(sample) = item.sample else {
                    return Ok(None);
                };
                if !self.cadence.admit(Instant::now()) {
                    return Ok(None);
                }
                unsafe {
                    let buffer = sample
                        .ConvertToContiguousBuffer()
                        .map_err(|e| e.to_string())?;
                    let mut ptr = std::ptr::null_mut();
                    let mut len = 0;
                    buffer
                        .Lock(&mut ptr, None, Some(&mut len))
                        .map_err(|e| e.to_string())?;
                    if ptr.is_null() || len == 0 || len > 64 * 1024 * 1024 {
                        let _ = buffer.Unlock();
                        return Err("Некорректный буфер видеокадра".into());
                    }
                    let bytes = std::slice::from_raw_parts(ptr, len as usize).to_vec();
                    buffer.Unlock().map_err(|e| e.to_string())?;
                    let (decoded, jpeg) = if self.native_jpeg {
                        let decoded = match image::load_from_memory_with_format(
                            &bytes,
                            image::ImageFormat::Jpeg,
                        ) {
                            Ok(image) => image,
                            Err(_) if self.last.elapsed() < Duration::from_secs(3) => {
                                return Ok(None);
                            }
                            Err(error) => {
                                return Err(format!(
                                    "Плата передает поврежденные JPEG-кадры: {error}"
                                ));
                            }
                        };
                        (decoded, bytes)
                    } else {
                        let media = self
                            .reader
                            .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
                            .map_err(|e| e.to_string())?;
                        let size = media
                            .GetUINT64(&MF_MT_FRAME_SIZE)
                            .map_err(|e| e.to_string())?;
                        let (w, h) = ((size >> 32) as u32, size as u32);
                        if w != self.mode.width || h != self.mode.height {
                            return Err("Изменилось разрешение видеосигнала".into());
                        }
                        let stride = media
                            .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                            .map(|v| v as i32)
                            .unwrap_or((w * 4) as i32);
                        let pitch = stride.unsigned_abs() as usize;
                        if pitch < w as usize * 4
                            || pitch
                                .checked_mul(h as usize)
                                .is_none_or(|n| n > bytes.len())
                        {
                            return Err("Некорректная длина видеокадра".into());
                        }
                        let mut rgba = image::RgbaImage::new(w, h);
                        for y in 0..h {
                            let row = if stride < 0 { h - 1 - y } else { y };
                            for x in 0..w {
                                let p = row as usize * pitch + x as usize * 4;
                                rgba.put_pixel(
                                    x,
                                    y,
                                    image::Rgba([bytes[p + 2], bytes[p + 1], bytes[p], 255]),
                                );
                            }
                        }
                        let decoded = image::DynamicImage::ImageRgba8(rgba);
                        let jpeg = crate::imaging::jpeg(&decoded)?;
                        (decoded, jpeg)
                    };
                    if decoded.width() != self.mode.width || decoded.height() != self.mode.height {
                        return Err("Размер кадра не совпадает с выбранным режимом".into());
                    }
                    self.last = Instant::now();
                    Ok(Some(preview_frame(decoded, jpeg, generation, item.received)))
                }
            })();
            unsafe {
                self.reader
                    .ReadSample(
                        MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                        0,
                        None,
                        None,
                        None,
                        None,
                    )
                    .map_err(|e| e.to_string())?;
            }
            result
        }
    }
    pub(super) fn run(rx: mpsc::Receiver<Request>, events: mpsc::Sender<Event>, latest: Latest) {
        let mut epoch = 0;
        let _platform = match Platform::new() {
            Ok(p) => p,
            Err(e) => {
                let _ = events.send(Event::Status {
                    epoch,
                    active: false,
                    message: format!("Media Foundation: {e}"),
                });
                return;
            }
        };
        let mut session: Option<Session> = None;
        let mut generation = 0;
        loop {
            let request = if session.is_some() {
                match rx.try_recv() {
                    Ok(r) => Some(r),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(_) => break,
                }
            } else {
                match rx.recv() {
                    Ok(r) => Some(r),
                    Err(_) => break,
                }
            };
            if let Some(request) = request {
                match request {
                    Request::Enumerate => {
                        if session.is_some() {
                            let _ = events.send(Event::Status {
                                epoch,
                                active: true,
                                message:
                                    "Сначала остановите камеру для обновления списка устройств"
                                        .into(),
                            });
                        } else {
                            let _ =
                                events.send(Event::Devices(enumerate().map_err(|e| e.to_string())));
                        }
                    }
                    Request::Stop(id) => {
                        epoch = id;
                        session = None;
                        *latest.lock().unwrap() = None;
                        let _ = events.send(Event::Status {
                            epoch,
                            active: false,
                            message: "Камера остановлена".into(),
                        });
                    }
                    Request::Start(id, config) => {
                        epoch = id;
                        session = None;
                        *latest.lock().unwrap() = None;
                        match config.validate().and_then(|c|Session::open(c).map_err(|e|format!("Не удалось открыть устройство. Возможно, его использует другая программа: {e}"))){Ok(s)=>{session=Some(s);let _=events.send(Event::Status{epoch,active:true,message:"Ожидание видеосигнала…".into()});},Err(e)=>{let _=events.send(Event::Status{epoch,active:false,message:e});}}
                    }
                }
            }
            if let Some(s) = session.as_mut() {
                generation += 1;
                match s.read(generation) {
                    Ok(Some(mut frame)) => {
                        frame.epoch = epoch;
                        *latest.lock().unwrap() = Some(Arc::new(frame));
                    }
                    Ok(None) => (),
                    Err(error) => {
                        session = None;
                        *latest.lock().unwrap() = None;
                        let _ = events.send(Event::Status {
                            epoch,
                            active: false,
                            message: error,
                        });
                    }
                }
            }
        }
        drop(session);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cadence_skips_conversion_without_queuing_or_delaying_first_frame() {
        let start = Instant::now();
        let mut cadence = FrameCadence::default();
        let admitted: Vec<_> = (0..60)
            .filter(|index| {
                cadence.admit(start + Duration::from_nanos(*index * 16_666_667))
            })
            .collect();
        assert_eq!(admitted, (0..60).step_by(4).collect::<Vec<_>>());
        assert!(cadence.admit(start + Duration::from_secs(5)));
        assert!(!cadence.admit(start + Duration::from_secs(5)));
        // Restarting the capture gets its first frame immediately.
        assert!(FrameCadence::default().admit(start));
        let mut slow_camera = FrameCadence::default();
        for index in 0..5 {
            assert!(slow_camera.admit(start + Duration::from_millis(index * 200)));
        }
    }
    #[test]
    fn preview_preserves_full_resolution_capture_and_acquisition_age() {
        let original = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            1920, 1080, image::Rgb([35, 110, 205]),
        ));
        let mut encoded = std::io::Cursor::new(vec![]);
        original.write_to(&mut encoded, image::ImageOutputFormat::Jpeg(95)).unwrap();
        let jpeg = encoded.into_inner();
        let received = Instant::now() - Duration::from_secs(2);
        let frame = preview_frame(original, jpeg.clone(), 37, received);
        assert_eq!((frame.width, frame.height), (1280, 720));
        assert_eq!(frame.bgra.len(), 1280 * 720 * 4);
        assert_eq!(frame.generation, 37);
        assert_eq!(frame.received, received);
        assert!(frame.received.elapsed() >= Duration::from_secs(2));
        assert_eq!(*frame.jpeg, jpeg);
        let saved = image::load_from_memory(&snapshot_bytes(frame.jpeg.as_ref()).unwrap()).unwrap();
        assert_eq!((saved.width(), saved.height()), (1920, 1080));
    }
    #[test]
    #[ignore = "synthetic CPU timing; no camera, patient files or network"]
    fn synthetic_mjpg_conversion_timing() {
        let original = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(
            1920, 1080, |x, y| image::Rgb([
                (x % 251) as u8, (y % 251) as u8, ((x + y) % 251) as u8,
            ]),
        ));
        let mut encoded = std::io::Cursor::new(vec![]);
        original.write_to(&mut encoded, image::ImageOutputFormat::Jpeg(95)).unwrap();
        let jpeg = encoded.into_inner();
        let mut results = vec![];
        for early in [false, true] {
            let timeline = Instant::now();
            let started = Instant::now();
            let mut cadence = FrameCadence::default();
            let mut conversions = 0;
            let mut published = 0;
            for index in 0..12 {
                let received = timeline + Duration::from_nanos(index * 16_666_667);
                if early && !cadence.admit(received) { continue; }
                let decoded = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg).unwrap();
                let frame = preview_frame(decoded, jpeg.clone(), index, received);
                conversions += 1;
                if early || cadence.admit(received) {
                    assert_eq!(*frame.jpeg, jpeg);
                    assert_eq!(frame.received, received);
                    published += 1;
                }
            }
            let elapsed = started.elapsed();
            eprintln!("MJPG 1920x1080, 12 source frames at 60 fps, early_gate={early}: conversions={conversions}, published={published}, CPU wall={elapsed:?}");
            results.push((conversions, published));
        }
        assert_eq!(results, vec![(12, 3), (3, 3)]);
    }
    #[test]
    fn deliberate_identical_photos_are_distinct_without_pixel_changes() {
        let original = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            16,
            12,
            image::Rgb([50, 100, 150]),
        ));
        let mut encoded = std::io::Cursor::new(vec![]);
        original
            .write_to(&mut encoded, image::ImageOutputFormat::Jpeg(95))
            .unwrap();
        let jpeg = encoded.into_inner();
        let first = snapshot_bytes(&jpeg).unwrap();
        let second = snapshot_bytes(&jpeg).unwrap();
        assert_ne!(first, second);
        let decoded = image::load_from_memory(&jpeg).unwrap().to_rgb8();
        assert_eq!(image::load_from_memory(&first).unwrap().to_rgb8(), decoded);
        assert_eq!(image::load_from_memory(&second).unwrap().to_rgb8(), decoded);
        assert!(snapshot_bytes(b"not a jpeg").is_err());
    }
    #[test]
    fn invalid_modes_rejected() {
        let c = Config {
            device_id: "pci-device".into(),
            mode: Mode {
                width: 1920,
                height: 1080,
                numerator: 60,
                denominator: 1,
                format: "MJPG".into(),
            },
        };
        assert!(c.clone().validate().is_ok());
        let mut invalid = c.clone();
        invalid.mode.denominator = 0;
        assert!(invalid.validate().is_err());
        let mut invalid = c;
        invalid.mode.width = u32::MAX;
        assert!(invalid.validate().is_err());
    }
}
