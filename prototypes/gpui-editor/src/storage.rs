//! Blocking I/O is confined to this worker. Originals are encrypted and never rewritten.
use gpui_kit::RenderImage;
use m53_client::{cache::Store, imaging};
use m53_gpui_editor::{Photo, Session};
use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
};
use uuid::Uuid;

pub enum Request {
    Restore,
    Import(Vec<PathBuf>),
    Save(u64, Session),
    Export(Photo, PathBuf),
}
pub enum Reply {
    Restored(Session, Vec<(Uuid, Arc<RenderImage>, Arc<RenderImage>)>),
    Imported(Photo, Arc<RenderImage>, Arc<RenderImage>),
    Saved(u64),
    Exported,
    ImportFinished,
    Error(String, bool),
}
pub fn start(store: Store) -> (mpsc::Sender<Request>, mpsc::Receiver<Reply>) {
    let (tx, rx) = mpsc::channel();
    let (out, incoming) = mpsc::channel();
    std::thread::spawn(move || {
        for request in rx {
            let io_complete = matches!(&request, Request::Import(..) | Request::Export(..));
            let result = (|| -> Result<Reply, String> {
                Ok(match request {
                    Request::Restore => {
                        let state: Session = match store.read("session")? {
                            Some(bytes) => serde_json::from_slice(&bytes)
                                .map_err(|_| "Поврежден локальный проект; перезапись отключена")?,
                            None => Session::default(),
                        };
                        let mut images = vec![];
                        for photo in &state.photos {
                            let bytes = store
                                .read(&format!("image-{}", photo.id))?
                                .ok_or("Не найден оригинал снимка")?;
                            let (_, _, full, small) = previews(&bytes)?;
                            images.push((photo.id, full, small));
                        }
                        Reply::Restored(state, images)
                    }
                    Request::Import(paths) => {
                        for path in paths {
                            use std::io::Read;
                            let mut bytes = vec![];
                            std::fs::File::open(path)
                                .map_err(|_| "Не удалось открыть снимок")?
                                .take(imaging::MAX_FILE_BYTES as u64 + 1)
                                .read_to_end(&mut bytes)
                                .map_err(|_| "Не удалось прочитать снимок")?;
                            let (w, h, image, thumbnail) = previews(&bytes)?;
                            let photo = Photo::new(w, h);
                            store.write(&format!("image-{}", photo.id), &bytes)?;
                            let _ = out.send(Reply::Imported(photo, image, thumbnail));
                        }
                        Reply::ImportFinished
                    }
                    Request::Save(version, state) => {
                        let bytes = serde_json::to_vec(&state)
                            .map_err(|_| "Не удалось подготовить разметку")?;
                        store.write("session", &bytes)?;
                        Reply::Saved(version)
                    }
                    Request::Export(photo, path) => {
                        let bytes = store
                            .read(&format!("image-{}", photo.id))?
                            .ok_or("Не найден оригинал")?;
                        let image = imaging::decode(&bytes)?;
                        let png = imaging::png(&imaging::annotated(&image, &photo.annotations))?;
                        // Create-new protects existing user files from accidental replacement.
                        use std::io::Write;
                        let mut file = std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(path)
                            .map_err(|_| "Не удалось создать PNG; выберите новое имя файла")?;
                        file.write_all(&png)
                            .and_then(|_| file.sync_all())
                            .map_err(|_| "Ошибка записи PNG")?;
                        Reply::Exported
                    }
                })
            })();
            let _ = out.send(result.unwrap_or_else(|e| Reply::Error(e, io_complete)));
        }
    });
    (tx, incoming)
}
pub fn texture(bytes: &[u8]) -> Result<(u32, u32, Arc<RenderImage>), String> {
    texture_bound(bytes, 2400)
}
pub fn thumbnail(bytes: &[u8]) -> Result<Arc<RenderImage>, String> {
    Ok(texture_bound(bytes, 256)?.2)
}
fn render_pixels(width: u32, height: u32, mut raw: Vec<u8>) -> Result<Arc<RenderImage>, String> {
    for p in raw.chunks_exact_mut(4) { p.swap(0, 2); }
    let rgba = image::RgbaImage::from_raw(width, height, raw).ok_or("Не удалось создать текстуру")?;
    Ok(Arc::new(RenderImage::new(vec![image::Frame::new(rgba)])))
}
fn texture_bound(bytes: &[u8], side: u32) -> Result<(u32, u32, Arc<RenderImage>), String> {
    let decoded = imaging::decode(bytes)?;
    let (w, h) = imaging::dimensions(&decoded);
    // The GPU preview is bounded; normalization and export always use original dimensions.
    let preview = decoded.thumbnail(side, side).to_rgba8();
    let (pw, ph) = preview.dimensions();
    Ok((w, h, render_pixels(pw, ph, preview.into_raw())?))
}
/// One original decode produces independent, bounded canvas and filmstrip textures.
pub fn previews(bytes: &[u8]) -> Result<(u32, u32, Arc<RenderImage>, Arc<RenderImage>), String> {
    let decoded = imaging::decode(bytes)?;
    let (width, height) = imaging::dimensions(&decoded);
    let render = |side| {
        let rgba = decoded.thumbnail(side, side).to_rgba8();
        render_pixels(rgba.width(), rgba.height(), rgba.into_raw())
    };
    Ok((width, height, render(2400)?, render(256)?))
}
pub fn open_store() -> Result<Store, String> {
    let root = std::env::var_os("M53_GPUI_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            #[cfg(windows)]
            let base = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir);
            #[cfg(not(windows))]
            let base = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir)
                .join(".local/share");
            base.join("M53Colposcopy-GPUI-Pilot")
        });
    #[cfg(windows)]
    let cipher: Arc<dyn m53_client::cache::Protector> = Arc::new(m53_client::cache::Dpapi);
    #[cfg(not(windows))]
    let cipher: Arc<dyn m53_client::cache::Protector> = {
        use std::{
            io::Write,
            os::unix::fs::{OpenOptionsExt, PermissionsExt},
        };
        std::fs::create_dir_all(&root).map_err(|_| "Не удалось создать каталог проекта")?;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "Не удалось защитить каталог")?;
        let key_path = root.join("local.key");
        if !key_path.exists() {
            let mut key = Vec::from(*Uuid::new_v4().as_bytes());
            key.extend(Uuid::new_v4().as_bytes());
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&key_path)
                .map_err(|_| "Не удалось создать ключ проекта")?;
            file.write_all(&key)
                .and_then(|_| file.sync_all())
                .map_err(|_| "Не удалось сохранить ключ")?;
        }
        let key: [u8; 32] = std::fs::read(key_path)
            .map_err(|_| "Не удалось прочитать ключ")?
            .try_into()
            .map_err(|_| "Поврежден ключ проекта")?;
        Arc::new(m53_client::cache::AesProtector(key))
    };
    Store::new(root, cipher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use m53_client::cache::AesProtector;
    use m53_contracts::workflow::AnnotationLayer;
    #[test]
    fn filmstrip_has_its_own_small_texture_and_keeps_source_geometry() {
        let source=image::RgbImage::from_pixel(1000,800,image::Rgb([20,50,80]));
        let mut encoded=std::io::Cursor::new(vec![]);
        source.write_to(&mut encoded,image::ImageFormat::Png).unwrap();
        let bytes=encoded.into_inner(); let before=bytes.clone();
        let (width,height,full,small)=previews(&bytes).unwrap();
        assert_eq!((width,height),(1000,800));
        assert_ne!(full.id,small.id);
        assert!(small.size(0).width<=gpui_kit::DevicePixels(256));
        assert!(small.size(0).height<=gpui_kit::DevicePixels(256));
        assert!(small.as_bytes(0).unwrap().len()<=256*256*4);
        assert_eq!(&small.as_bytes(0).unwrap()[..4],&[80,50,20,255]);
        assert_eq!(bytes,before);
    }
    #[test]
    fn large_jpeg_preview_keeps_original_dimensions_and_bounds_gpu_texture() {
        let image = image::RgbImage::from_fn(4000, 3000, |x, y| {
            image::Rgb([(x % 251) as u8, (y % 251) as u8, ((x + y) % 251) as u8])
        });
        let mut encoded = std::io::Cursor::new(vec![]);
        image
            .write_to(&mut encoded, image::ImageFormat::Jpeg)
            .unwrap();
        let bytes = encoded.into_inner();
        let before = bytes.clone();
        let started = std::time::Instant::now();
        let (width, height, preview) = texture(&bytes).unwrap();
        eprintln!("12 MP JPEG decode/preview: {:?}", started.elapsed());
        assert_eq!((width, height), (4000, 3000));
        assert!(preview.size(0).width <= gpui_kit::DevicePixels(2400));
        assert!(preview.size(0).height <= gpui_kit::DevicePixels(2400));
        assert_eq!(bytes, before);
    }
    #[test]
    fn worker_roundtrip_and_crop_leave_original_untouched() {
        let root = std::env::temp_dir().join(format!("m53-gpui-test-{}", Uuid::new_v4()));
        let store = Store::new(root.clone(), Arc::new(AesProtector([73; 32]))).unwrap();
        let image = image::RgbaImage::from_pixel(200, 100, image::Rgba([180, 190, 200, 255]));
        let mut encoded = std::io::Cursor::new(vec![]);
        image
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        let original = encoded.into_inner();
        let mut photo = Photo::new(200, 100);
        photo.add_callout(1, 0.3, 0.4);
        photo.annotations.push(photo.annotation(
            0.2,
            0.1,
            AnnotationLayer::OutputRegion {
                width: 0.5,
                height: 0.6,
            },
        ));
        let key = format!("image-{}", photo.id);
        store.write(&key, &original).unwrap();
        let (tx, rx) = start(store.clone());
        let receive = || rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        tx.send(Request::Save(
            9,
            Session {
                photos: vec![photo.clone()],
                selected: 0,
            },
        ))
        .unwrap();
        assert!(matches!(receive(), Reply::Saved(9)));
        tx.send(Request::Restore).unwrap();
        let Reply::Restored(session, images) = receive() else {
            panic!("restore failed")
        };
        assert_eq!(session.photos[0].annotations[0].label, "РО");
        assert_eq!(images.len(), 1);
        let output = root.join("export.png");
        tx.send(Request::Export(photo, output.clone())).unwrap();
        assert!(matches!(receive(), Reply::Exported));
        let exported = image::open(output).unwrap();
        assert_eq!((exported.width(), exported.height()), (100, 60));
        assert_eq!(store.read(&key).unwrap().unwrap(), original);
        drop(tx);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}
