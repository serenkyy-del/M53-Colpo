//! Presentation-only decisions. No clinical defaults, validation rules or I/O.

pub fn compact_context(width: f32) -> bool {
    width.is_finite() && width > 0. && width < 448.
}

pub fn choice_name(field: &str, value: &str) -> String {
    format!("{field}: {value} — раскрыть список")
}

pub fn section_summary(missing: usize, entered: bool, optional: bool) -> String {
    if missing > 0 {
        let noun = match (missing % 10, missing % 100) {
            (1, n) if n != 11 => "поле",
            (2..=4, n) if !(12..=14).contains(&n) => "поля",
            _ => "полей",
        };
        format!("Осталось {missing} {noun}")
    } else if optional {
        if entered { "Введено · необязательно" } else { "Необязательно" }.into()
    } else if entered {
        "Введено".into()
    } else {
        String::new()
    }
}

pub fn phrase_matches(query: &str, title: &str, body: &str) -> bool {
    matches_with_content(query, || format!("{title}\n{body}").to_lowercase())
}

fn matches_with_content(query: &str, folded_content: impl FnOnce() -> String) -> bool {
    let query = query.trim();
    // The usual unfiltered render never allocates or scans the phrase body.
    if query.is_empty() { return true; }
    let content = folded_content();
    query.to_lowercase().split_whitespace().all(|word| content.contains(word))
}

pub fn phrase_preview(body: &str) -> String {
    preview_chars(body.chars())
}

fn preview_chars(mut chars: impl Iterator<Item = char>) -> String {
    const SOURCE_CHARS: usize = 160;
    let mut preview = String::new();
    let mut space = false;
    for ch in chars.by_ref().take(SOURCE_CHARS) {
        if ch.is_whitespace() {
            space = !preview.is_empty();
        } else {
            if space { preview.push(' '); space = false; }
            preview.push(ch);
        }
    }
    // Inspect at most one extra scalar, even for a large whitespace-only prefix.
    if chars.next().is_some() { preview.push('…'); }
    preview
}

pub fn editor_errors(title: &str, body: &str) -> (Option<&'static str>, Option<&'static str>) {
    (
        title.trim().is_empty().then_some("Введите наименование шаблона"),
        body.trim().is_empty().then_some("Введите текст шаблона"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_stacks_when_the_full_label_and_control_row_cannot_fit() {
        assert!(compact_context(300.));
        assert!(compact_context(447.));
        assert!(!compact_context(500.));
        assert!(!compact_context(f32::NAN));
    }

    #[test]
    fn identical_empty_values_keep_distinct_field_names() {
        let surface = choice_name("Поверхность", "Не заполнено");
        let color = choice_name("Цвет", "Не заполнено");
        assert_eq!(surface, "Поверхность: Не заполнено — раскрыть список");
        assert_ne!(surface, color);
    }

    #[test]
    fn missing_summary_never_calls_partial_input_complete() {
        assert_eq!(section_summary(2, true, false), "Осталось 2 поля");
        assert_eq!(section_summary(1, false, false), "Осталось 1 поле");
        assert_eq!(section_summary(5, false, false), "Осталось 5 полей");
        assert_eq!(section_summary(0, true, true), "Введено · необязательно");
        assert_eq!(section_summary(0, false, true), "Необязательно");
        assert_eq!(section_summary(0, true, false), "Введено");
        assert_eq!(section_summary(0, false, false), "");
    }

    #[test]
    fn search_matches_all_unicode_words_across_cached_title_and_body() {
        assert!(phrase_matches("  ВРАЧ  строка ", "Мой врач", "Вторая\nстрока"));
        assert!(!phrase_matches("врач отсутствует", "Мой врач", "Вторая строка"));
        assert!(phrase_matches(" \n ", "", ""));
    }

    #[test]
    fn preview_preserves_unicode_without_changing_the_inserted_body() {
        let body = "Первая  строка\r\nВторая\tстрока";
        assert_eq!(phrase_preview(body), "Первая строка Вторая строка");
        assert!(body.contains('\n'));
    }

    #[test]
    fn long_unicode_preview_has_a_fixed_bound_and_keeps_the_complete_body() {
        let body = "Шейка 🙂\n".repeat(2048);
        let preview = phrase_preview(&body);
        assert!(preview.chars().count() <= 161);
        assert!(preview.ends_with('…'));
        assert!(preview.starts_with("Шейка 🙂 Шейка"));
        assert_eq!(body.chars().count(), 16384);
    }

    #[test]
    fn empty_query_never_folds_the_cached_phrase_content() {
        assert!(matches_with_content(" \n\t", || panic!("Unfiltered render must not scan the body")));
        let folds = std::cell::Cell::new(0);
        assert!(matches_with_content("врач", || { folds.set(folds.get() + 1); "мой врач".into() }));
        assert_eq!(folds.get(), 1);
    }

    #[test]
    fn preview_scan_budget_does_not_depend_on_the_body_length_or_whitespace() {
        let consumed = std::cell::Cell::new(0);
        let chars = std::iter::repeat(' ').inspect(|_| consumed.set(consumed.get() + 1));
        assert_eq!(preview_chars(chars), "…");
        assert_eq!(consumed.get(), 161);
    }

    #[test]
    fn editor_identifies_each_missing_field_and_accepts_real_text() {
        assert_eq!(editor_errors(" \n", "Текст"), (Some("Введите наименование шаблона"), None));
        assert_eq!(editor_errors("Название", "\t"), (None, Some("Введите текст шаблона")));
        assert_eq!(editor_errors("Название", " Текст "), (None, None));
    }
}
