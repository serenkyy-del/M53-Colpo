//! Explicit PDF export/open and native print dialog. No PDF association is used for printing.
use std::path::Path;

/// Resolve the Windows Known Folder, including a redirected/localized Documents directory.
pub fn documents_directory() -> std::path::PathBuf {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::UI::Shell::{FOLDERID_Documents, SHGetKnownFolderPath};
    let mut value = std::ptr::null_mut();
    let result = unsafe { SHGetKnownFolderPath(&FOLDERID_Documents, 0, std::ptr::null_mut(), &mut value) };
    if !value.is_null() {
        let path = if result >= 0 {
            let mut length = 0;
            unsafe { while *value.add(length) != 0 { length += 1; } }
            Some(std::path::PathBuf::from(std::ffi::OsString::from_wide(unsafe { std::slice::from_raw_parts(value, length) })))
        } else { None };
        unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(value.cast())); }
        if let Some(path) = path { return path; }
    }
    std::env::var_os("USERPROFILE").map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir).join("Documents")
}

/// Legacy explicit "open file" only. Printing does not use a PDF association.
pub fn open_pdf(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let verb: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            path.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    };
    if result as isize <= 32 {
        return Err("PDF сохранен; не удалось открыть выбранную программу просмотра".into());
    }
    Ok(())
}

/// Show the native printer/range dialog, then print 300-DPI pages from this exact PDF.
/// Cancel is successful with `false`; no spooler job is created until explicit OK.
pub fn print_pdf(path: &Path) -> Result<bool, String> {
    let size = std::fs::metadata(path)
        .map_err(|_| "Не удалось открыть PDF для печати")?
        .len();
    if size > 128 * 1024 * 1024 {
        return Err("PDF слишком большой для печати".into());
    }
    let bytes = std::fs::read(path).map_err(|_| "Не удалось прочитать PDF для печати")?;
    print_pdf_bytes(&bytes)
}

/// Print the already prepared PDF without asking for a file or saving a temporary report.
pub fn print_pdf_bytes(bytes: &[u8]) -> Result<bool, String> {
    use windows_sys::Win32::{
        Foundation::GlobalFree, Graphics::Gdi::DeleteDC, Storage::Xps::*, UI::Controls::Dialogs::*,
    };
    if !bytes.starts_with(b"%PDF-") || bytes.len() > 128 * 1024 * 1024 {
        return Err("Некорректный PDF для печати".into());
    }
    let count = crate::report::chromium::page_count(bytes)?;
    struct Dialog(PRINTDLGW);
    impl Drop for Dialog {
        fn drop(&mut self) {
            unsafe {
                if !self.0.hDC.is_null() {
                    DeleteDC(self.0.hDC);
                }
                if !self.0.hDevMode.is_null() {
                    GlobalFree(self.0.hDevMode);
                }
                if !self.0.hDevNames.is_null() {
                    GlobalFree(self.0.hDevNames);
                }
            }
        }
    }
    let mut dialog = Dialog(unsafe { std::mem::zeroed() });
    dialog.0.lStructSize = std::mem::size_of::<PRINTDLGW>() as u32;
    dialog.0.Flags =
        PD_RETURNDC | PD_USEDEVMODECOPIESANDCOLLATE | PD_NOSELECTION | PD_HIDEPRINTTOFILE;
    dialog.0.nMinPage = 1;
    dialog.0.nMaxPage = count as u16;
    dialog.0.nFromPage = 1;
    dialog.0.nToPage = count as u16;
    dialog.0.nCopies = 1;
    if unsafe { PrintDlgW(&mut dialog.0) } == 0 {
        return if unsafe { CommDlgExtendedError() } == 0 {
            Ok(false)
        } else {
            Err("Не удалось открыть диалог печати".into())
        };
    }
    let (from, to) = if dialog.0.Flags & PD_PAGENUMS != 0 {
        (dialog.0.nFromPage as usize, dialog.0.nToPage as usize)
    } else {
        (1, count)
    };
    if from == 0 || from > to || to > count {
        return Err("Некорректный диапазон страниц печати".into());
    }
    let title: Vec<u16> = crate::APP_NAME.encode_utf16().chain(Some(0)).collect();
    let mut info: DOCINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<DOCINFOW>() as i32;
    info.lpszDocName = title.as_ptr();
    let dc = dialog.0.hDC;
    if unsafe { StartDocW(dc, &info) } <= 0 {
        return Err("Не удалось создать задание печати".into());
    }
    let result = crate::report::chromium::print_pages(bytes, from, to, |png| {
        if unsafe { StartPage(dc) } <= 0 {
            return Err("Принтер не начал страницу".into());
        }
        draw_page(dc, png)?;
        if unsafe { EndPage(dc) } <= 0 {
            return Err("Принтер не принял страницу".into());
        }
        Ok(())
    });
    if let Err(e) = result {
        unsafe {
            AbortDoc(dc);
        }
        return Err(e);
    }
    if unsafe { EndDoc(dc) } <= 0 {
        unsafe {
            AbortDoc(dc);
        }
        return Err("Не удалось завершить задание печати".into());
    }
    Ok(true)
}

fn print_rect(
    width: u32,
    height: u32,
    physical: (i32, i32),
    offset: (i32, i32),
    dpi: (i32, i32),
) -> Result<(i32, i32, i32, i32), String> {
    if width == 0 || height == 0 || physical.0 <= 0 || physical.1 <= 0 || dpi.0 <= 0 || dpi.1 <= 0 {
        return Err("Некорректные размеры страницы принтера".into());
    }
    let native_width = width as f64 * dpi.0 as f64 / 300.;
    let native_height = height as f64 * dpi.1 as f64 / 300.;
    let scale = (physical.0 as f64 / native_width).min(physical.1 as f64 / native_height);
    let w = (native_width * scale).round() as i32;
    let h = (native_height * scale).round() as i32;
    Ok((
        (physical.0 - w) / 2 - offset.0,
        (physical.1 - h) / 2 - offset.1,
        w,
        h,
    ))
}
fn draw_page(dc: windows_sys::Win32::Graphics::Gdi::HDC, png: &[u8]) -> Result<(), String> {
    use windows_sys::Win32::Graphics::Gdi::*;
    let mut image = image::load_from_memory(png)
        .map_err(|_| "Не удалось подготовить растровую страницу печати")?
        .to_rgba8();
    let (width, height) = image.dimensions();
    let cap = |index| unsafe { GetDeviceCaps(dc, index as i32) };
    let physical = (
        cap(PHYSICALWIDTH).max(cap(HORZRES)),
        cap(PHYSICALHEIGHT).max(cap(VERTRES)),
    );
    let (x, y, w, h) = print_rect(
        width,
        height,
        physical,
        (cap(PHYSICALOFFSETX), cap(PHYSICALOFFSETY)),
        (cap(LOGPIXELSX), cap(LOGPIXELSY)),
    )?;
    blit_bitmap(dc, &mut image, (x, y, w, h))
}
fn blit_bitmap(
    dc: windows_sys::Win32::Graphics::Gdi::HDC,
    image: &mut image::RgbaImage,
    (x, y, w, h): (i32, i32, i32, i32),
) -> Result<(), String> {
    use windows_sys::Win32::Graphics::Gdi::*;
    let (width, height) = image.dimensions();
    for pixel in image.pixels_mut() {
        pixel.0.swap(0, 2);
        pixel.0[3] = 255;
    }
    let mut info: BITMAPINFO = unsafe { std::mem::zeroed() };
    info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    info.bmiHeader.biWidth = width as i32;
    info.bmiHeader.biHeight = -(height as i32);
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB;
    unsafe {
        SetStretchBltMode(dc, HALFTONE);
        SetBrushOrgEx(dc, 0, 0, std::ptr::null_mut());
    }
    let lines = unsafe {
        StretchDIBits(
            dc,
            x,
            y,
            w,
            h,
            0,
            0,
            width as i32,
            height as i32,
            image.as_ptr() as _,
            &info,
            DIB_RGB_COLORS,
            SRCCOPY,
        )
    };
    if lines == 0 || lines == -1 {
        return Err("Драйвер принтера не принял изображение страницы".into());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raster_print_keeps_aspect_and_physical_offsets() {
        assert_eq!(
            print_rect(2480, 3508, (4960, 7016), (50, 60), (600, 600)).unwrap(),
            (-50, -60, 4960, 7016)
        );
        assert!(print_rect(0, 100, (1, 1), (0, 0), (300, 300)).is_err());
        let (_, _, w, h) = print_rect(3508, 2480, (4960, 7016), (0, 0), (600, 600)).unwrap();
        assert_eq!(w, 4960);
        assert!(h < 7016);
    }
    #[test]
    fn gdi_memory_surface_preserves_colors_and_top_down_orientation() {
        use windows_sys::Win32::Graphics::Gdi::*;
        // A memory DIB only: no print dialog, printer DC or spooler calls in this test.
        unsafe {
            let dc = CreateCompatibleDC(std::ptr::null_mut());
            assert!(!dc.is_null());
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = 80;
            info.bmiHeader.biHeight = -160;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(
                dc,
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                std::ptr::null_mut(),
                0,
            );
            assert!(!bitmap.is_null());
            assert!(!bits.is_null());
            let previous = SelectObject(dc, bitmap);
            let mut source = image::RgbaImage::from_fn(8, 16, |_, y| {
                image::Rgba(if y < 8 {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 255, 255]
                })
            });
            let result = blit_bitmap(dc, &mut source, (0, 0, 80, 160));
            GdiFlush();
            let surface = std::slice::from_raw_parts(bits as *const u8, 80 * 160 * 4);
            let top = surface[(20 * 80 + 40) * 4..][..3].to_vec();
            let bottom = surface[(140 * 80 + 40) * 4..][..3].to_vec();
            SelectObject(dc, previous);
            DeleteObject(bitmap);
            DeleteDC(dc);
            result.unwrap();
            assert_eq!(top, [0, 0, 255]);
            assert_eq!(bottom, [255, 0, 0]);
        }
    }
}
