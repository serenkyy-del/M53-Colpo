//! Bounded, immutable thumbnail derivatives. Call only after study authorization.
use crate::api::{Error, hash};
use axum::{
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use image::{DynamicImage, ImageDecoder};
use std::{
    collections::HashMap,
    io::{Cursor, Read, Write},
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, Weak},
};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
const MAX_INPUT: u64 = 64 * 1024 * 1024;
const MAX_OUTPUT: u64 = 1024 * 1024;
const VERSION: &str = "jpeg-256-v1";
type Flights = Mutex<HashMap<String, Weak<AsyncMutex<()>>>>;
static FLIGHTS: OnceLock<Flights> = OnceLock::new();
static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
fn error(code: &'static str) -> Error {
    Error(StatusCode::UNPROCESSABLE_ENTITY, code)
}
fn limits(w: u32, h: u32) -> Result<(), Error> {
    if w == 0 || h == 0 || w > 32768 || h > 32768 || u64::from(w) * u64::from(h) > 40_000_000 {
        return Err(error("thumbnail_image_limits"));
    }
    Ok(())
}
fn render(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let image = if bytes.starts_with(b"M53RGB01") {
        if bytes.len() < 16 {
            return Err(error("thumbnail_decode_failed"));
        }
        let w = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let h = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        limits(w, h)?;
        let pixels = image::RgbImage::from_raw(w, h, bytes[16..].to_vec())
            .ok_or_else(|| error("thumbnail_decode_failed"))?;
        DynamicImage::ImageRgb8(pixels)
    } else if bytes.starts_with(&[255, 216]) {
        let mut decoder = image::codecs::jpeg::JpegDecoder::new(Cursor::new(bytes))
            .map_err(|_| error("thumbnail_decode_failed"))?;
        let (w, h) = decoder.dimensions();
        limits(w, h)?;
        decoder
            .scale(256, 256)
            .map_err(|_| error("thumbnail_decode_failed"))?;
        DynamicImage::from_decoder(decoder).map_err(|_| error("thumbnail_decode_failed"))?
    } else {
        let reader = image::io::Reader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|_| error("thumbnail_decode_failed"))?;
        let (w, h) = reader
            .into_dimensions()
            .map_err(|_| error("thumbnail_decode_failed"))?;
        limits(w, h)?;
        let mut reader = image::io::Reader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|_| error("thumbnail_decode_failed"))?;
        let mut bound = image::io::Limits::default();
        bound.max_alloc = Some(256 * 1024 * 1024);
        bound.max_image_width = Some(32768);
        bound.max_image_height = Some(32768);
        reader.limits(bound);
        reader
            .decode()
            .map_err(|_| error("thumbnail_decode_failed"))?
    };
    let small = image.thumbnail(256, 256).to_rgb8();
    let mut output = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, 80)
        .encode_image(&small)
        .map_err(|_| error("thumbnail_decode_failed"))?;
    if output.len() as u64 > MAX_OUTPUT {
        return Err(error("thumbnail_image_limits"));
    }
    Ok(output)
}
fn cached(path: &std::path::Path) -> Option<Vec<u8>> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_OUTPUT {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_OUTPUT + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_OUTPUT {
        return None;
    }
    let decoder = image::codecs::jpeg::JpegDecoder::new(Cursor::new(&bytes)).ok()?;
    let (w, h) = decoder.dimensions();
    (w > 0 && h > 0 && w <= 256 && h <= 256).then_some(bytes)
}
pub(crate) async fn response(
    root: PathBuf,
    key: String,
    sha: String,
    size: i64,
    headers: &HeaderMap,
) -> Result<Response, Error> {
    if size < 0 || size as u64 > MAX_INPUT {
        return Err(error("thumbnail_image_limits"));
    }
    if sha.len() != 64 || !sha.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "asset_integrity_error",
        ));
    }
    let flight = {
        let mut map = FLIGHTS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        map.retain(|_, v| v.strong_count() > 0);
        if let Some(lock) = map.get(&sha).and_then(Weak::upgrade) {
            lock
        } else {
            let lock = Arc::new(AsyncMutex::new(()));
            map.insert(sha.clone(), Arc::downgrade(&lock));
            lock
        }
    };
    let single = flight.lock_owned().await;
    let cache = root
        .join(".thumbnails")
        .join(VERSION)
        .join(format!("{sha}.jpg"));
    let probe = cache.clone();
    let mut bytes = tokio::task::spawn_blocking(move || cached(&probe))
        .await
        .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "thumbnail_worker_error"))?;
    if bytes.is_none() {
        let permit = WORKERS
            .get_or_init(|| Arc::new(Semaphore::new(2)))
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "thumbnail_unavailable"))?;
        bytes = Some(
            tokio::task::spawn_blocking(move || -> Result<Vec<u8>, Error> {
                // Blocking work outlives a disconnected HTTP request. Keep both
                // guards inside it so cancellation cannot bypass either limit.
                let _permit = permit;
                let _single = single;
                let original = root.join(key);
                let meta = std::fs::metadata(&original)?;
                if !meta.is_file() || meta.len() != size as u64 {
                    return Err(Error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "asset_integrity_error",
                    ));
                }
                let mut input = Vec::new();
                std::fs::File::open(original)?
                    .take(MAX_INPUT + 1)
                    .read_to_end(&mut input)?;
                if input.len() as i64 != size || hash(&input) != sha {
                    return Err(Error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "asset_integrity_error",
                    ));
                }
                let output = render(&input)?;
                let directory = cache.parent().unwrap();
                std::fs::create_dir_all(directory)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
                }
                let temp = directory.join(format!("{}.part", uuid::Uuid::new_v4().simple()));
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options.open(&temp)?;
                let result = (|| -> std::io::Result<()> {
                    file.write_all(&output)?;
                    file.sync_all()?;
                    std::fs::rename(&temp, &cache)?;
                    std::fs::File::open(directory)?.sync_all()?;
                    Ok(())
                })();
                if result.is_err() {
                    let _ = std::fs::remove_file(&temp);
                }
                result?;
                Ok(output)
            })
            .await
            .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "thumbnail_worker_error"))??,
        );
    }
    let bytes = bytes.unwrap();
    let sha = hash(&bytes);
    let etag = format!("\"{sha}\"");
    let mut response = if crate::asset_download::unchanged(headers, &etag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        bytes.into_response()
    };
    let h = response.headers_mut();
    h.insert(header::CONTENT_TYPE, "image/jpeg".parse().unwrap());
    h.insert("x-content-sha256", sha.parse().unwrap());
    h.insert(header::ETAG, etag.parse().unwrap());
    h.insert(header::CACHE_CONTROL, "private, no-cache".parse().unwrap());
    h.insert(
        header::VARY,
        "Authorization, X-Workstation-Key".parse().unwrap(),
    );
    Ok(response)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_rgb_thumbnail_decoder_keeps_wire_bytes() {
        let mut bytes=crate::rgb::RGB_HEADER.to_vec();
        bytes.extend(2u32.to_le_bytes());bytes.extend(1u32.to_le_bytes());
        bytes.extend([0,1,255,128,64,32]);let before=bytes.clone();
        let thumbnail=render(&bytes).unwrap();
        assert!(thumbnail.starts_with(&[255,216,255]));
        assert_eq!(bytes,before);
    }
    #[test]
    fn derivative_is_bounded_and_original_unchanged() {
        let rgb = image::RgbImage::from_pixel(640, 480, image::Rgb([130, 90, 40]));
        let mut input = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut input, 95)
            .encode_image(&rgb)
            .unwrap();
        let original = input.clone();
        let output = render(&input).unwrap();
        let decoder = image::codecs::jpeg::JpegDecoder::new(Cursor::new(output)).unwrap();
        assert_eq!(decoder.dimensions(), (256, 192));
        assert_eq!(input, original);
    }
    #[test]
    fn malformed_and_oversized_inputs_rejected() {
        assert!(render(b"invalid").is_err());
        assert!(limits(32768, 32768).is_err());
    }
}
