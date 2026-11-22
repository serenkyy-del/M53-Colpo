#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_keeps_patient_name_date_and_visible_number() {
        assert_eq!(file_name(Some(("Иванова Анна Игоревна", "2026-11-10")), 2, true),
            "Иванова Анна Игоревна 2026-11-10 2 с разметкой.png");
        assert_eq!(file_name(None, 1, false), "Снимок 1 без разметки.png");
    }

    #[test]
    fn filename_is_a_bounded_windows_safe_component() {
        let name = file_name(Some((&format!("..\\Иванова/{}", "Анна".repeat(200)), "2026:11:10")), 12, false);
        assert!(!name.starts_with('.'));
        assert!(!name.contains(['\\', '/', ':']));
        assert!(name.encode_utf16().count() < 200);
        assert!(name.ends_with(" 2026 11 10 12 без разметки.png"));
        assert_eq!(file_name(Some(("CON.txt", "")), 1, false), "CON txt 1 без разметки.png");
    }
}
fn filename_part(value: &str, limit: usize) -> String {
    let cleaned: String = value.chars().map(|ch| {
        if ch.is_control() || r#"<>:"/\|?*"#.contains(ch) { ' ' } else { ch }
    }).collect();
    let cleaned = cleaned.trim_matches(|ch: char| ch.is_whitespace() || ch == '.');
    let compact = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut size = 0;
    let end = compact.char_indices().find_map(|(index, ch)| {
        size += ch.len_utf16();
        (size > limit).then_some(index)
    }).unwrap_or(compact.len());
    compact[..end].trim_end_matches(['.', ' ']).to_owned()
}

pub fn file_name(patient: Option<(&str, &str)>, number: usize, marked: bool) -> String {
    let (name, date) = patient.unwrap_or_default();
    // Keep Windows device names such as CON.txt from becoming the filename stem.
    let name = filename_part(&name.replace('.', " "), 120);
    let name = if name.is_empty() { "Снимок" } else { &name };
    let date = filename_part(date, 24);
    let date = if date.is_empty() { date } else { format!(" {date}") };
    let markup = if marked { "с разметкой" } else { "без разметки" };
    format!("{name}{date} {number} {markup}.png")
}

