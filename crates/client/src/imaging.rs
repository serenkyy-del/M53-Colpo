use image::{DynamicImage, GenericImageView, ImageFormat};
use m53_contracts::workflow::{Annotation, AnnotationLayer};
use std::io::Cursor;
pub const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
pub fn decode(bytes: &[u8]) -> Result<DynamicImage, String> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err("Снимок превышает 64 МиБ".into());
    }
    let mut reader = image::io::Reader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "Неизвестный формат")?;
    if !matches!(
        reader.format(),
        Some(ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::Bmp | ImageFormat::Tiff)
    ) {
        return Err("Поддерживаются JPEG, PNG, TIFF и BMP".into());
    }
    if reader.format() == Some(ImageFormat::Tiff)
        && tiff::decoder::Decoder::new(Cursor::new(bytes))
            .map_err(|_| "Поврежденный TIFF")?
            .more_images()
    {
        return Err("Многостраничный TIFF: экспортируйте каждый кадр отдельным файлом".into());
    }
    if reader.format() == Some(ImageFormat::Png) {
        let mut offset = 8;
        while offset + 12 <= bytes.len() {
            let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            if &bytes[offset + 4..offset + 8] == b"acTL" {
                return Err("Анимированный PNG: импортируйте отдельные кадры".into());
            }
            let Some(next) = offset.checked_add(length).and_then(|n| n.checked_add(12)) else {
                return Err("Поврежденный PNG".into());
            };
            if next > bytes.len() {
                break;
            }
            offset = next;
        }
    }
    let mut limits = image::io::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|_| "Снимок поврежден или превышает ограничения")?;
    if image.color().bits_per_pixel() > image.color().channel_count() as u16 * 8 {
        return Err("Для первого выпуска нужны 8-битные снимки; исходник более высокой разрядности не преобразуется автоматически".into());
    }
    if image.color().has_alpha() && image.pixels().any(|(_, _, p)| p.0[3] != 255) {
        return Err(
            "Снимки с прозрачностью не поддерживаются; используйте непрозрачный оригинал".into(),
        );
    }
    let orientation = orientation(bytes);
    Ok(match orientation {
        Some(2) => image.fliph(),
        Some(3) => image.rotate180(),
        Some(4) => image.flipv(),
        Some(5) => image.rotate90().fliph(),
        Some(6) => image.rotate90(),
        Some(7) => image.rotate270().fliph(),
        Some(8) => image.rotate270(),
        _ => image,
    })
}
fn orientation(bytes: &[u8]) -> Option<u32> {
    exif::Reader::new()
        .read_from_container(&mut Cursor::new(bytes))
        .ok()
        .and_then(|exif| {
            exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
                .and_then(|field| field.value.get_uint(0))
        })
}
/// Read dimensions of an already validated cached original without decoding its pixels.
pub fn dimensions_from_bytes(bytes: &[u8]) -> Result<(u32, u32), String> {
    let (width, height) = image::io::Reader::new(Cursor::new(bytes))
        .with_guessed_format().map_err(|_| "Неизвестный формат снимка")?
        .into_dimensions().map_err(|_| "Не удалось прочитать размеры снимка")?;
    Ok(if matches!(orientation(bytes), Some(5..=8)) { (height, width) } else { (width, height) })
}

/// Normalize newly imported/captured originals once, before computing their asset hash.
/// Already compressed JPEGs retain their exact bytes (including capture identity and EXIF).
/// Historic assets and queued immutable uploads must never be passed through this conversion.
pub fn storage_jpeg(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    let decoded = decode(&bytes)?;
    if bytes.starts_with(&[0xff, 0xd8]) {
        return Ok(bytes);
    }
    jpeg(&decoded)
}

/// The full-resolution JPEG format used by imported originals and raw capture frames.
pub fn jpeg(image: &DynamicImage) -> Result<Vec<u8>, String> {
    let rgb = image.to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95)
        .encode(&rgb, rgb.width(), rgb.height(), image::ColorType::Rgb8)
        .map_err(|_| "Ошибка кодирования JPEG снимка")?;
    if out.len() > MAX_FILE_BYTES {
        return Err("JPEG снимок превышает 64 МиБ".into());
    }
    Ok(out)
}

#[cfg(test)]
mod storage_tests {
    use super::*;

    #[test]
    fn valid_jpeg_is_not_reencoded_and_capture_identity_is_preserved() {
        let source = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            64, 48, image::Rgb([40, 130, 220]),
        ));
        let original = crate::video::snapshot_bytes(&jpeg(&source).unwrap()).unwrap();
        let allocation = original.as_ptr();
        let expected = original.clone();
        let normalized = storage_jpeg(original).unwrap();
        assert_eq!(normalized, expected);
        assert_eq!(normalized.as_ptr(), allocation);
    }

    #[test]
    fn supported_uncompressed_imports_become_full_resolution_jpeg() {
        let source = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            96, 64, image::Rgb([40, 130, 220]),
        ));
        for format in [ImageFormat::Png, ImageFormat::Bmp, ImageFormat::Tiff] {
            let mut input = Cursor::new(Vec::new());
            source.write_to(&mut input, format).unwrap();
            let normalized = storage_jpeg(input.into_inner()).unwrap();
            assert_eq!(image::guess_format(&normalized).unwrap(), ImageFormat::Jpeg);
            let decoded = decode(&normalized).unwrap();
            assert_eq!(decoded.dimensions(), source.dimensions());
            assert_eq!(decoded.color(), image::ColorType::Rgb8);
        }
    }

    #[test]
    fn jpeg_orientation_metadata_remains_intact_for_consistent_display() {
        let source = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            3, 2, image::Rgb([40, 130, 220]),
        ));
        let original = jpeg(&source).unwrap();
        let exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
        let mut oriented = vec![0xff, 0xd8, 0xff, 0xe1];
        oriented.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        oriented.extend_from_slice(exif);
        oriented.extend_from_slice(&original[2..]);
        let expected = oriented.clone();
        let normalized = storage_jpeg(oriented).unwrap();
        assert_eq!(normalized, expected);
        assert_eq!(decode(&normalized).unwrap().dimensions(), (2, 3));
        assert_eq!(dimensions_from_bytes(&normalized).unwrap(), (2, 3));
    }

    #[test]
    fn tiff_orientation_is_applied_once_before_jpeg_storage() {
        let source = image::RgbImage::from_pixel(3, 2, image::Rgb([40, 130, 220]));
        let mut input = Cursor::new(Vec::new());
        {
            let mut encoder = tiff::encoder::TiffEncoder::new(&mut input).unwrap();
            let mut image = encoder.new_image::<tiff::encoder::colortype::RGB8>(3, 2).unwrap();
            image.encoder().write_tag(tiff::tags::Tag::Orientation, 6u16).unwrap();
            image.write_data(source.as_raw()).unwrap();
        }
        let normalized = storage_jpeg(input.into_inner()).unwrap();
        assert_eq!(image::guess_format(&normalized).unwrap(), ImageFormat::Jpeg);
        assert_eq!(decode(&normalized).unwrap().dimensions(), (2, 3));
    }

    #[test]
    fn invalid_and_transparent_sources_are_not_silently_converted() {
        assert!(storage_jpeg(b"invalid".to_vec()).is_err());
        let source = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2, 2, image::Rgba([30, 40, 50, 0]),
        ));
        assert!(storage_jpeg(png(&source).unwrap()).is_err());
    }
}

pub fn annotated(image: &DynamicImage, annotations: &[Annotation]) -> DynamicImage {
    let mut out = image.to_rgb8();
    let (width, height) = out.dimensions();
    fn dot(out: &mut image::RgbImage, x: f32, y: f32, color: [u8; 3]) {
        let (x, y) = (x.round() as i32, y.round() as i32);
        for dx in -1..=1 {
            for dy in -1..=1 {
                let (px, py) = (x + dx, y + dy);
                if px >= 0 && py >= 0 && px < out.width() as i32 && py < out.height() as i32 {
                    out.put_pixel(px as u32, py as u32, image::Rgb(color));
                }
            }
        }
    }
    fn line(out: &mut image::RgbImage, start: (f32, f32), end: (f32, f32), color: [u8; 3]) {
        let steps =
            ((end.0 - start.0).abs().max((end.1 - start.1).abs()).ceil() as usize).clamp(1, 65536);
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            dot(
                out,
                start.0 + (end.0 - start.0) * t,
                start.1 + (end.1 - start.1) * t,
                color,
            );
        }
    }
    fn ring(out: &mut image::RgbImage, cx: f32, cy: f32, r: f32, color: [u8; 3]) {
        let steps = (r * std::f32::consts::TAU).ceil().clamp(360.0, 65536.0) as usize;
        for step in 0..steps {
            let angle = step as f32 * std::f32::consts::TAU / steps as f32;
            dot(out, cx + r * angle.cos(), cy + r * angle.sin(), color);
        }
    }
    use ab_glyph::{Font, ScaleFont};
    let font = ab_glyph::FontRef::try_from_slice(include_bytes!("../assets/DejaVuSans.ttf")).ok();
    let mut ordered: Vec<_> = annotations.iter().collect();
    ordered.sort_by_key(|a| matches!(a.layer, AnnotationLayer::Callout { .. }));
    for a in ordered {
        if !a.valid_layer() || !a.x.is_finite() || !a.y.is_finite() || !a.radius.is_finite() {
            continue;
        }
        let cx = a.x * width as f32;
        let cy = a.y * height as f32;
        let r = if matches!(a.layer, AnnotationLayer::Callout {..}) {crate::defects::CALLOUT_RADIUS} else {a.radius} * width.min(height) as f32;
        if matches!(
            a.layer,
            AnnotationLayer::Mark | AnnotationLayer::Arrow { .. }
        ) && !a.label.is_empty()
            && let Some(font) = &font
        {
            let scale = (width.min(height) as f32 * 0.025).clamp(12.0, 36.0);
            let scaled = font.as_scaled(scale);
            let mut x = cx.clamp(0.0, width.saturating_sub(1) as f32);
            let baseline = (cy - r - 3.0).max(scale);
            for ch in a.label.chars().take(160) {
                let id = scaled.glyph_id(ch);
                let advance = scaled.h_advance(id);
                if x + advance > width as f32 {
                    break;
                }
                if let Some(glyph) = font
                    .outline_glyph(id.with_scale_and_position(scale, ab_glyph::point(x, baseline)))
                {
                    let bounds = glyph.px_bounds();
                    glyph.draw(|gx, gy, alpha| {
                        let px = bounds.min.x as i32 + gx as i32;
                        let py = bounds.min.y as i32 + gy as i32;
                        if px >= 0 && py >= 0 && px < width as i32 && py < height as i32 {
                            let p = out.get_pixel_mut(px as u32, py as u32);
                            for (c, v) in [240.0, 70.0, 40.0].iter().enumerate() {
                                p.0[c] = (p.0[c] as f32 * (1.0 - alpha) + v * alpha).round() as u8;
                            }
                        }
                    });
                }
                x += advance;
            }
        }
        match a.layer {
            AnnotationLayer::Callout {
                end_x,
                end_y,
                ref full_name,
                ref term_code,
            } => {
                let rgb =
                    m53_domain::colpo::marker_color(&a.label, full_name, term_code.as_deref());
                let color = [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8];
                let tip = (end_x * width as f32, end_y * height as f32);
                let dx = tip.0 - cx;
                let dy = tip.1 - cy;
                let length = dx.hypot(dy);
                let start = if length > r {
                    (cx + dx / length * r, cy + dy / length * r)
                } else {
                    (cx, cy)
                };
                line(&mut out, start, tip, color);
                let angle = dy.atan2(dx);
                let head = (r * 0.4).clamp(4.0, 20.0).min(length * 0.4);
                for delta in [-0.5, 0.5] {
                    line(
                        &mut out,
                        tip,
                        (
                            tip.0 - head * (angle + delta).cos(),
                            tip.1 - head * (angle + delta).sin(),
                        ),
                        [240, 70, 40],
                    );
                }
                // An opaque badge remains readable regardless of the source photograph.
                let minx = (cx - r).floor().max(0.0) as u32;
                let maxx = (cx + r).ceil().min(width.saturating_sub(1) as f32) as u32;
                let miny = (cy - r).floor().max(0.0) as u32;
                let maxy = (cy + r).ceil().min(height.saturating_sub(1) as f32) as u32;
                for y in miny..=maxy {
                    for x in minx..=maxx {
                        if (x as f32 - cx).hypot(y as f32 - cy) <= r {
                            out.put_pixel(x, y, image::Rgb([255, 255, 255]));
                        }
                    }
                }
                ring(&mut out, cx, cy, r, color);
                if let Some(font) = &font {
                    let size = crate::defects::callout_font_size(width.min(height) as f32, r, &a.label);
                    let scaled = font.as_scaled(size);
                    let total = a
                        .label
                        .chars()
                        .map(|c| scaled.h_advance(scaled.glyph_id(c)))
                        .sum::<f32>();
                    let mut x = cx - total / 2.0;
                    let y = cy + (scaled.ascent() + scaled.descent()) / 2.0;
                    for ch in a.label.chars().take(32) {
                        let gid = scaled.glyph_id(ch);
                        if let Some(g) = font
                            .outline_glyph(gid.with_scale_and_position(size, ab_glyph::point(x, y)))
                        {
                            let b = g.px_bounds();
                            g.draw(|gx, gy, alpha| {
                                let px = b.min.x as i32 + gx as i32;
                                let py = b.min.y as i32 + gy as i32;
                                if px >= 0 && py >= 0 && px < width as i32 && py < height as i32 {
                                    let p = out.get_pixel_mut(px as u32, py as u32);
                                    for v in &mut p.0 {
                                        *v = (*v as f32 * (1.0 - alpha)).round() as u8;
                                    }
                                }
                            });
                        }
                        x += scaled.h_advance(gid);
                    }
                }
            }
            AnnotationLayer::Arrow { end_x, end_y } => {
                let tip = (end_x * width as f32, end_y * height as f32);
                line(&mut out, (cx, cy), tip, [240, 70, 40]);
                let angle = (tip.1 - cy).atan2(tip.0 - cx);
                let head = r.max(5.0).min((tip.0 - cx).hypot(tip.1 - cy) * 0.4);
                for delta in [-0.5, 0.5] {
                    line(
                        &mut out,
                        tip,
                        (
                            tip.0 - head * (angle + delta).cos(),
                            tip.1 - head * (angle + delta).sin(),
                        ),
                        [240, 70, 40],
                    );
                }
            }
            AnnotationLayer::Mark => ring(&mut out, cx, cy, r, [240, 70, 40]),
            AnnotationLayer::Target {
                rotation_degrees,
                include_in_report: true,
            } => {
                for fraction in [1.0, 0.67, 0.33] {
                    ring(&mut out, cx, cy, r * fraction, [255, 210, 0]);
                }
                for hour in 0..12 {
                    let angle = hour as f32 * std::f32::consts::TAU / 12.0
                        + rotation_degrees.to_radians()
                        - std::f32::consts::FRAC_PI_2;
                    for step in 0..=r.ceil().clamp(0.0, 65536.0) as usize {
                        let d = step as f32;
                        dot(
                            &mut out,
                            cx + d * angle.cos(),
                            cy + d * angle.sin(),
                            [255, 210, 0],
                        );
                    }
                }
            }
            _ => (),
        }
    }
    // Crop only the new render buffer. Original bytes and normalized layer coordinates are unchanged.
    if let Some(region) = annotations
        .iter()
        .find(|a| matches!(a.layer, AnnotationLayer::OutputRegion { .. }) && a.valid_layer())
        && let AnnotationLayer::OutputRegion {
            width: rw,
            height: rh,
        } = region.layer
    {
        let x = (region.x * width as f32)
            .floor()
            .clamp(0.0, (width - 1) as f32) as u32;
        let y = (region.y * height as f32)
            .floor()
            .clamp(0.0, (height - 1) as f32) as u32;
        let w = (rw * width as f32).round().clamp(1.0, (width - x) as f32) as u32;
        let h = (rh * height as f32).round().clamp(1.0, (height - y) as f32) as u32;
        return DynamicImage::ImageRgb8(image::imageops::crop_imm(&out, x, y, w, h).to_image());
    }
    DynamicImage::ImageRgb8(out)
}

pub fn png(image: &DynamicImage) -> Result<Vec<u8>, String> {
    let mut out = Cursor::new(Vec::new());
    image
        .write_to(&mut out, ImageFormat::Png)
        .map_err(|_| "Ошибка кодирования снимка")?;
    Ok(out.into_inner())
}
pub fn dimensions(image: &DynamicImage) -> (u32, u32) {
    image.dimensions()
}

/// A lossless decoded RGB representation for legacy derived assets.
pub fn raw_rgb_pixels(image: &DynamicImage) -> Vec<u8> {
    let rgb = image.to_rgb8();
    let mut out = b"M53RGB01".to_vec();
    out.extend_from_slice(&rgb.width().to_le_bytes());
    out.extend_from_slice(&rgb.height().to_le_bytes());
    out.extend_from_slice(rgb.as_raw());
    out
}

/// Move graphic metadata while keeping both ends of an arrow inside the original.
pub fn move_layer(a: &mut Annotation, x: f32, y: f32) {
    if !x.is_finite() || !y.is_finite() {
        return;
    }
    let mut dx = x.clamp(0.0, 1.0) - a.x;
    let mut dy = y.clamp(0.0, 1.0) - a.y;
    if let AnnotationLayer::Arrow { end_x, end_y } = &mut a.layer {
        dx = dx.clamp(-a.x.min(*end_x), 1.0 - a.x.max(*end_x));
        dy = dy.clamp(-a.y.min(*end_y), 1.0 - a.y.max(*end_y));
        *end_x += dx;
        *end_y += dy;
    }
    a.x += dx;
    a.y += dy;
}
