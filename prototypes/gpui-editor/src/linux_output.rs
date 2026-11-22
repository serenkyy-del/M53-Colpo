//! Preview and system printing of the PDF produced by the Linux client.
use std::{
    fs::{self, DirBuilder},
    io::Write,
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    process::{Command, Stdio},
};

struct PreviewDirectory(PathBuf);

impl Drop for PreviewDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn pages(bytes: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let count = m53_client::report::linux_pdf_pages(bytes)?;
    let path = std::env::temp_dir().join(format!("colposcopy-preview-{}", uuid::Uuid::new_v4()));
    DirBuilder::new().mode(0o700).create(&path)
        .map_err(|_| "Не удалось создать каталог предпросмотра")?;
    let preview = PreviewDirectory(path);
    let input = preview.0.join("report.pdf");
    fs::write(&input, bytes).map_err(|_| "Не удалось подготовить PDF для предпросмотра")?;
    let status = Command::new("pdftoppm")
        .args(["-png", "-scale-to", "1500"])
        .arg(&input)
        .arg(preview.0.join("page"))
        .stdout(Stdio::null()).stderr(Stdio::null()).status()
        .map_err(|_| "Для предпросмотра установите poppler-utils")?;
    if !status.success() {
        return Err("Не удалось прочитать страницы PDF".into());
    }
    let files = fs::read_dir(&preview.0).map_err(|_| "Не удалось прочитать предпросмотр")?;
    let mut ordered = files.filter_map(Result::ok).filter_map(|file| {
        let name = file.file_name();
        let number = name.to_str()?.strip_prefix("page-")?.strip_suffix(".png")?.parse::<usize>().ok()?;
        Some((number, file.path()))
    }).collect::<Vec<_>>();
    ordered.sort_unstable_by_key(|(number, _)| *number);
    if ordered.len() != count || ordered.iter().enumerate().any(|(index, item)| item.0 != index + 1) {
        return Err("Предпросмотр содержит неполный набор страниц".into());
    }
    ordered.into_iter().map(|(_, path)| fs::read(path).map_err(|_| "Не удалось прочитать страницу PDF".into())).collect()
}

pub fn print(bytes: &[u8]) -> Result<bool, String> {
    let mut child = Command::new("lp")
        .args(["-t", "Протокол кольпоскопии", "-"])
        .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
        .map_err(|_| "Для печати установите cups-client")?;
    let written = child.stdin.take().ok_or("Не удалось открыть поток печати")?.write_all(bytes);
    let status = child.wait().map_err(|_| "Не удалось отправить PDF на печать")?;
    if written.is_err() || !status.success() {
        return Err("Проверьте очередь CUPS и принтер по умолчанию".into());
    }
    Ok(true)
}
