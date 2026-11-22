//! Safe presentation states for bounded registry pages. No network operations.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DirectoryState {
    #[default]
    Loading,
    OfflineUncached,
    Cached,
    NoMatches,
    Empty,
    Ready,
}
impl DirectoryState {
    pub fn classify(online: bool, first_missing: bool, last_missing: bool, has_rows: bool, has_query: bool) -> Self {
        if !online {
            return if first_missing && !has_rows { Self::OfflineUncached } else { Self::Cached };
        }
        if last_missing { Self::Loading } else if has_rows { Self::Ready } else if has_query { Self::NoMatches } else { Self::Empty }
    }
    pub fn key(self) -> &'static str {
        match self {
            Self::Loading => "loading", Self::OfflineUncached => "offline_uncached", Self::Cached => "cached",
            Self::NoMatches => "no_matches", Self::Empty => "empty", Self::Ready => "ready",
        }
    }
    pub fn patient_message(self, has_rows: bool, has_query: bool) -> &'static str {
        match self {
            Self::Loading if has_rows => "Загружаем следующую страницу пациентов…",
            Self::Loading => "Загружаем пациентов…",
            Self::OfflineUncached => "Для этого запроса нет сохранённых данных. Сервер недоступен",
            Self::Cached if has_rows => "Показаны сохранённые данные. Сервер недоступен",
            Self::Cached if has_query => "Совпадений в сохранённых данных нет. Сервер недоступен",
            Self::Cached => "В сохранённых данных пациентов пока нет. Сервер недоступен",
            Self::NoMatches => "Пациенты по этому запросу не найдены",
            Self::Empty => "Пациентов пока нет",
            Self::Ready => "",
        }
    }
    pub fn task_message(self, has_rows: bool) -> &'static str {
        match self {
            Self::Loading if has_rows => "Загружаем ещё задания…",
            Self::Loading => "Загружаем задания…",
            Self::OfflineUncached => "Нет сохранённых заданий на эту дату. Сервер недоступен",
            Self::Cached if has_rows => "Показаны сохранённые задания. Сервер недоступен",
            Self::Cached => "Сохранённых заданий на эту дату нет. Сервер недоступен",
            Self::NoMatches | Self::Empty => "Невыполненных заданий на эту дату нет",
            Self::Ready => "",
        }
    }
    pub fn study_message(self, has_rows: bool) -> &'static str {
        match self {
            Self::Loading if has_rows => "Загружаем историю обследований… Показаны локальные обследования",
            Self::Loading => "Загружаем историю обследований…",
            Self::OfflineUncached if has_rows => "Показаны локальные обследования. Полная история не загружена. Сервер недоступен",
            Self::OfflineUncached => "История обследований не сохранена на этом компьютере. Сервер недоступен",
            Self::Cached if has_rows => "Показаны сохранённые обследования. Сервер недоступен",
            Self::Cached => "В сохранённых данных обследований нет. Сервер недоступен",
            Self::NoMatches | Self::Empty => "Обследований пока нет",
            Self::Ready => "",
        }
    }
}

/// Preserve all four columns and their identity/date minima, putting spare width into the name.
pub fn patient_column_widths(available: f32) -> [f32; 4] {
    let available = if available.is_finite() { available.max(538.) } else { 840. };
    let extra = available - 538.;
    [180. + extra * 0.58, 118. + extra * 0.12, 70. + extra * 0.08, 170. + extra * 0.22]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_offline_page_is_not_loading_or_an_authoritative_empty_result() {
        assert_eq!(DirectoryState::classify(false, true, true, false, false), DirectoryState::OfflineUncached);
        assert_eq!(DirectoryState::classify(false, false, false, false, true), DirectoryState::Cached);
        assert_eq!(DirectoryState::classify(false, true, true, true, true), DirectoryState::Cached);
    }

    #[test]
    fn online_loading_and_search_empty_are_distinct_and_keep_loaded_rows() {
        assert_eq!(DirectoryState::classify(true, true, true, false, true), DirectoryState::Loading);
        assert_eq!(DirectoryState::classify(true, false, true, true, false), DirectoryState::Loading);
        assert_eq!(DirectoryState::classify(true, false, false, false, true), DirectoryState::NoMatches);
        assert_eq!(DirectoryState::classify(true, false, false, false, false), DirectoryState::Empty);
        assert_eq!(DirectoryState::classify(true, false, false, true, true), DirectoryState::Ready);
    }

    #[test]
    fn adaptive_columns_fit_compact_table_without_hiding_identity() {
        for available in [590., 700., 840., 1100.] {
            let columns = patient_column_widths(available);
            assert!((columns.iter().sum::<f32>() - available).abs() < 0.1);
            assert!(columns[0] >= 180. && columns[1] >= 118. && columns[2] >= 70. && columns[3] >= 170.);
        }
    }
}
