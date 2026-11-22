use gpui_kit::component::{
    ActiveTheme, StyledExt,
    table::{Column, ColumnSort, TableDelegate, TableState},
};
use gpui_kit::{prelude::*, *};
use m53_gpui_editor::registry::{PatientRow, display_date, sort_patients};
use uuid::Uuid;

#[derive(Default)]
pub struct PatientsTable {
    pub rows: Vec<PatientRow>,
    pub selected: Option<Uuid>,
    column: usize,
    descending: bool,
    pub worker: Option<std::sync::mpsc::Sender<crate::clinic::Command>>,
    pub more: bool,
    pub loading: bool,
    empty_message: String,
    available_width: f32,
}
impl PatientsTable {
    pub fn with_worker(worker: std::sync::mpsc::Sender<crate::clinic::Command>) -> Self {
        Self {
            worker: Some(worker),
            ..Default::default()
        }
    }
    pub fn replace(&mut self, rows: Vec<PatientRow>) -> bool {
        let mut rows = rows;
        sort_patients(&mut rows, self.column, self.descending);
        if self.rows == rows {
            return false;
        }
        self.rows = rows;
        true
    }
    /// The worker supplies safe display text; response bodies/clinical values never become errors here.
    pub fn set_registry_state(&mut self, registry: &serde_json::Value) -> bool {
        let message = registry["patients_status"].as_str().filter(|s| !s.is_empty()).unwrap_or("Пациентов пока нет");
        let changed = self.empty_message != message;
        self.empty_message = message.to_string();
        changed
    }
    /// Caller passes the available table width after its own panel/padding layout.
    pub fn set_available_width(&mut self, width: f32) -> bool {
        let width = if width.is_finite() { width.max(538.) } else { 840. };
        if (self.available_width - width).abs() < 2. { return false; }
        self.available_width = width;
        true
    }
}
impl TableDelegate for PatientsTable {
    fn has_more(&self, _: &App) -> bool {
        self.more && !self.loading
    }
    fn load_more_threshold(&self) -> usize {
        10
    }
    fn load_more(&mut self, _: &mut Window, _: &mut Context<TableState<Self>>) {
        if self.more && !self.loading {
            self.loading = true;
            if let Some(worker) = &self.worker {
                let _ = worker.send(crate::clinic::Command::Registry(
                    crate::registry_browser::Request::MorePatients,
                ));
            }
        }
    }
    fn columns_count(&self, _: &App) -> usize {
        4
    }
    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }
    fn column(&self, index: usize, _: &App) -> Column {
        let widths = crate::registry_view::patient_column_widths(if self.available_width > 0. { self.available_width } else { 840. });
        let (key, title, minimum) = match index {
            1 => ("birth-date", "Дата рождения", 118.),
            2 => ("sex", "Пол", 70.),
            3 => ("last-exam", "Последнее обследование", 170.),
            _ => ("patient-name", "ФИО пациента", 180.),
        };
        Column::new(key, title)
            .width(px(widths[index.min(3)]))
            .min_width(px(minimum))
            .movable(false)
            .sort(if self.column == index {
                if self.descending {
                    ColumnSort::Descending
                } else {
                    ColumnSort::Ascending
                }
            } else {
                ColumnSort::Default
            })
    }
    fn render_th(
        &mut self,
        index: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let title = self.column(index, cx).name;
        div()
            .id(("patient-sort-header", index))
            .size_full()
            .cursor_pointer()
            .font_semibold()
            .text_color(cx.theme().foreground)
            .child(title)
            .on_click(cx.listener(move |table, _, _, cx| {
                cx.stop_propagation();
                let selected = {
                    let data = table.delegate_mut();
                    data.descending = data.column == index && !data.descending;
                    data.column = index;
                    if let Some(worker) = &data.worker {
                        data.loading = true;
                        let _ = worker.send(crate::clinic::Command::Registry(
                            crate::registry_browser::Request::Sort(index, data.descending),
                        ));
                    }
                    sort_patients(&mut data.rows, index, data.descending);
                    data.selected
                        .and_then(|id| data.rows.iter().position(|r| r.id == id))
                };
                table.refresh(cx);
                if let Some(row) = selected {
                    table.set_selected_row(row, cx);
                } else {
                    table.clear_selection(cx);
                }
                cx.notify();
            }))
    }
    fn perform_sort(
        &mut self,
        index: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        self.column = index;
        self.descending = sort == ColumnSort::Descending;
        if let Some(worker) = &self.worker {
            self.loading = true;
            let _ = worker.send(crate::clinic::Command::Registry(
                crate::registry_browser::Request::Sort(index, self.descending),
            ));
        }
        sort_patients(&mut self.rows, index, self.descending);
        let selected = self
            .selected
            .and_then(|id| self.rows.iter().position(|r| r.id == id));
        let entity = cx.entity();
        cx.defer(move |cx| {
            entity.update(cx, |state, cx| {
                state.refresh(cx);
                if let Some(index) = selected {
                    state.set_selected_row(index, cx);
                } else {
                    state.clear_selection(cx);
                }
                cx.notify();
            })
        });
    }
    fn render_td(
        &mut self,
        row: usize,
        column: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let patient = &self.rows[row];
        let value = match column {
            1 => display_date(patient.birth_date.as_deref()),
            2 => patient.sex_label().to_owned(),
            3 => display_date(patient.last_exam_date.as_deref()),
            _ => patient.name.clone(),
        };
        let tooltip = if column == 0 && !patient.external_id.is_empty() {
            format!("{}\nВнешний ID: {}", patient.name, patient.external_id)
        } else {
            value.clone()
        };
        div()
            .id(("patient-cell", row * 4 + column))
            .w_full()
            .overflow_hidden()
            .text_ellipsis()
            .text_color(
                if column != 0
                    && (column == 1 && patient.birth_date.is_none()
                        || column == 2 && patient.sex_label()=="Не указан"
                        || column == 3 && patient.last_exam_date.is_none())
                {
                    cx.theme().muted_foreground
                } else {
                    cx.theme().foreground
                },
            )
            .child(value)
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
            })
    }
    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .p_4()
            .text_color(cx.theme().muted_foreground)
            .child(if self.empty_message.is_empty() { "Загружаем пациентов…".to_string() } else { self.empty_message.clone() })
    }
}
