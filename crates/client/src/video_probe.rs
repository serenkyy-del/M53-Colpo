//! Windows capture diagnostics. Enumeration alone does not start streaming; optional single-frame check is explicit.
use windows::{
    Win32::{Media::MediaFoundation::*, System::Com::*},
    core::Result,
};
pub fn inspect() -> Result<Vec<String>> {
    inspect_impl(None)
}
pub fn inspect_frame(path: &std::path::Path) -> Result<Vec<String>> {
    inspect_impl(Some(path))
}
fn inspect_impl(output: Option<&std::path::Path>) -> Result<Vec<String>> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        struct Com;
        impl Drop for Com {
            fn drop(&mut self) {
                unsafe {
                    CoUninitialize();
                }
            }
        }
        let _com = Com;
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
        struct Mf;
        impl Drop for Mf {
            fn drop(&mut self) {
                let _ = unsafe { MFShutdown() };
            }
        }
        let _mf = Mf;
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.unwrap();
        attrs.SetGUID(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        )?;
        let mut ptr = std::ptr::null_mut();
        let mut count = 0;
        MFEnumDeviceSources(&attrs, &mut ptr, &mut count)?;
        struct Devices(*mut Option<IMFActivate>, usize);
        impl Drop for Devices {
            fn drop(&mut self) {
                unsafe {
                    if !self.0.is_null() {
                        for i in 0..self.1 {
                            std::ptr::drop_in_place(self.0.add(i));
                        }
                        CoTaskMemFree(Some(self.0.cast()));
                    }
                }
            }
        }
        let list = Devices(ptr, count as usize);
        let mut lines = vec![];
        for i in 0..list.1 {
            let Some(device) = (&*list.0.add(i)).as_ref() else {
                continue;
            };
            let n = device.GetStringLength(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME)?;
            let mut name = vec![0u16; n as usize + 1];
            device.GetString(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, &mut name, None)?;
            lines.push(format!(
                "DEVICE: {}",
                String::from_utf16_lossy(&name[..n as usize])
            ));
            let source: IMFMediaSource = match device.ActivateObject() {
                Ok(s) => s,
                Err(e) => {
                    lines.push(format!("OPEN ERROR: {e}"));
                    continue;
                }
            };
            struct Source(IMFMediaSource);
            impl Drop for Source {
                fn drop(&mut self) {
                    let _ = unsafe { self.0.Shutdown() };
                }
            }
            let source = Source(source);
            let descriptor = source.0.CreatePresentationDescriptor()?;
            for stream in 0..descriptor.GetStreamDescriptorCount()? {
                let mut selected = Default::default();
                let mut info = None;
                descriptor.GetStreamDescriptorByIndex(stream, &mut selected, &mut info)?;
                let handler = info.unwrap().GetMediaTypeHandler()?;
                if handler.GetMajorType()? != MFMediaType_Video {
                    continue;
                }
                for index in 0..handler.GetMediaTypeCount()? {
                    let media = handler.GetMediaTypeByIndex(index)?;
                    let size = media.GetUINT64(&MF_MT_FRAME_SIZE)?;
                    let rate = media.GetUINT64(&MF_MT_FRAME_RATE)?;
                    let subtype = media.GetGUID(&MF_MT_SUBTYPE)?;
                    let code = subtype.data1.to_le_bytes();
                    let format = String::from_utf8_lossy(&code);
                    lines.push(format!(
                        "{}x{} {} {}/{} fps",
                        size >> 32,
                        size as u32,
                        format,
                        rate >> 32,
                        rate as u32
                    ));
                }
            }
            if let Some(path) = output {
                if String::from_utf16_lossy(&name[..n as usize]) == "USB Video" {
                    match frame(&source.0, path) {
                        Ok(info) => lines.push(info),
                        Err(e) => lines.push(format!("FRAME ERROR: {e}")),
                    }
                }
            }
        }
        Ok(lines)
    }
}

fn frame(source: &IMFMediaSource, path: &std::path::Path) -> std::result::Result<String, String> {
    let get = || -> Result<(Vec<u8>, u32, u32, i32)> {
        unsafe {
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 1)?;
            let attrs = attrs.unwrap();
            attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
            let reader = MFCreateSourceReaderFromMediaSource(source, &attrs)?;
            let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
            reader.SetStreamSelection(stream, true)?;
            let mut native = None;
            for index in 0..512 {
                let Ok(media) = reader.GetNativeMediaType(stream, index) else {
                    break;
                };
                let size = media.GetUINT64(&MF_MT_FRAME_SIZE)?;
                let rate = media.GetUINT64(&MF_MT_FRAME_RATE)?;
                if media.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_MJPG
                    && size >> 32 == 1920
                    && size as u32 == 1080
                    && rate >> 32 == 60 * (rate as u32 as u64)
                {
                    native = Some(media);
                    break;
                }
            }
            let native = native.ok_or_else(|| {
                windows::core::Error::from_hresult(windows::core::HRESULT(0x80004005u32 as i32))
            })?;
            reader.SetCurrentMediaType(stream, None, &native)?;
            let warmup = std::time::Instant::now();
            for _ in 0..300 {
                let mut sample = None;
                let mut flags = 0;
                reader.ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample))?;
                if flags & (MF_SOURCE_READERF_ERROR.0 | MF_SOURCE_READERF_ENDOFSTREAM.0) as u32 != 0
                {
                    break;
                }
                if let Some(sample) = sample {
                    if warmup.elapsed() < std::time::Duration::from_secs(2) {
                        continue;
                    }
                    let media = reader.GetCurrentMediaType(stream)?;
                    let size = media.GetUINT64(&MF_MT_FRAME_SIZE)?;
                    let (width, height) = ((size >> 32) as u32, size as u32);
                    let subtype = media.GetGUID(&MF_MT_SUBTYPE)?;
                    let stride = media
                        .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                        .map(|v| v as i32)
                        .unwrap_or((width * 4) as i32);
                    let buffer = sample.ConvertToContiguousBuffer()?;
                    let mut ptr = std::ptr::null_mut();
                    let mut len = 0;
                    buffer.Lock(&mut ptr, None, Some(&mut len))?;
                    if ptr.is_null() || len == 0 || len > 256 * 1024 * 1024 {
                        buffer.Unlock()?;
                        return Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                            0x80004005u32 as i32,
                        )));
                    }
                    let bytes = std::slice::from_raw_parts(ptr, len as usize).to_vec();
                    buffer.Unlock()?;
                    if subtype == MFVideoFormat_MJPG {
                        return Ok((bytes, width, height, 0));
                    }
                    return Ok((bytes, width, height, stride));
                }
            }
            Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                0x80004005u32 as i32,
            )))
        }
    };
    let (bytes, width, height, stride) = get().map_err(|e| e.to_string())?;
    if stride == 0 {
        let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)
            .map_err(|e| e.to_string())?;
        decoded.save(path).map_err(|e| e.to_string())?;
        return Ok(format!(
            "FRAME: {}x{} MJPEG 60 fps requested -> {}",
            decoded.width(),
            decoded.height(),
            path.display()
        ));
    }
    let pitch = stride.unsigned_abs() as usize;
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || pitch < width as usize * 4
        || pitch
            .checked_mul(height as usize)
            .is_none_or(|n| n > bytes.len())
    {
        return Err("Некорректный размер буфера кадра".into());
    }
    let mut image = image::RgbaImage::new(width, height);
    for y in 0..height {
        let row = if stride < 0 { height - 1 - y } else { y };
        for x in 0..width {
            let pos = row as usize * pitch + x as usize * 4;
            image.put_pixel(
                x,
                y,
                image::Rgba([bytes[pos + 2], bytes[pos + 1], bytes[pos], 255]),
            );
        }
    }
    image.save(path).map_err(|e| e.to_string())?;
    Ok(format!(
        "FRAME: {width}x{height} RGB32 -> {}",
        path.display()
    ))
}
