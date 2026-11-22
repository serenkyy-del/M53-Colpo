//! Application calendar conventions shared by every date field.
pub fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        use gpui_kit::component as gpui_component;
        rust_i18n::extend!(gpui_component);
    });
    gpui_kit::component::set_locale("ru");
}

pub fn date_state(
    window: &mut gpui_kit::Window,
    cx: &mut gpui_kit::Context<gpui_kit::component::date_picker::DatePickerState>,
) -> gpui_kit::component::date_picker::DatePickerState {
    gpui_kit::component::date_picker::DatePickerState::new(window, cx)
        .date_format("%d.%m.%Y")
        .first_day_of_week(chrono::Weekday::Mon)
}

#[cfg(test)]
mod tests {
    #[test]
    fn all_calendar_labels_are_russian_and_initialization_is_repeatable() {
        super::init();
        super::init();
        let translate = gpui_kit::component::_rust_i18n_translate;
        for (key, expected) in [
            ("Calendar.week.0", "Вс"), ("Calendar.week.1", "Пн"),
            ("Calendar.week.2", "Вт"), ("Calendar.week.3", "Ср"),
            ("Calendar.week.4", "Чт"), ("Calendar.week.5", "Пт"),
            ("Calendar.week.6", "Сб"),
            ("Calendar.month.January", "Январь"), ("Calendar.month.February", "Февраль"),
            ("Calendar.month.March", "Март"), ("Calendar.month.April", "Апрель"),
            ("Calendar.month.May", "Май"), ("Calendar.month.June", "Июнь"),
            ("Calendar.month.July", "Июль"), ("Calendar.month.August", "Август"),
            ("Calendar.month.September", "Сентябрь"), ("Calendar.month.October", "Октябрь"),
            ("Calendar.month.November", "Ноябрь"), ("Calendar.month.December", "Декабрь"),
            ("DatePicker.placeholder", "Выберите дату"), ("DatePicker.time", "Время"),
        ] {
            assert_eq!(translate("ru", key), expected, "{key}");
        }
    }
}
