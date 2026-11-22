//! Shared native editor chrome. Screen visibility never changes report metadata.
use egui::{self, Color32, Pos2, Response, Sense, Stroke, Vec2};
use m53_contracts::workflow::{Annotation, AnnotationLayer};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use uuid::Uuid;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LayerView {
    pub hidden: BTreeSet<Uuid>,
    pub show_legacy: bool,
}
impl Default for LayerView {
    fn default() -> Self {
        Self {
            hidden: BTreeSet::new(),
            show_legacy: true,
        }
    }
}
impl LayerView {
    pub fn legacy(a: &Annotation) -> bool {
        matches!(
            a.layer,
            AnnotationLayer::Mark | AnnotationLayer::Arrow { .. }
        )
    }
    pub fn visible(&self, a: &Annotation) -> bool {
        !self.hidden.contains(&a.id) && (self.show_legacy || !Self::legacy(a))
    }
    pub fn toggle(&mut self, id: Uuid) {
        if !self.hidden.remove(&id) {
            self.hidden.insert(id);
        }
    }
}
#[derive(Clone, Copy)]
pub enum Icon {
    Select,
    Hand,
    Fit,
    Plus,
    Minus,
    Target,
    Crop,
    Whole,
    Eye,
    Hidden,
    Edit,
    Delete,
    Lock,
}
pub fn icon(ui: &mut egui::Ui, which: Icon, selected: bool, label: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
    let visuals = ui.style().interact_selectable(&response, selected);
    if selected || response.hovered() || response.has_focus() {
        ui.painter().rect_filled(rect, 3.0, visuals.bg_fill);
    }
    let color = if selected {
        ui.visuals().selection.stroke.color
    } else {
        visuals.fg_stroke.color
    };
    paint_icon(ui.painter(), rect.center(), which, color);
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            selected,
            label,
        )
    });
    response.on_hover_text(label)
}
fn paint_icon(p: &egui::Painter, c: Pos2, icon: Icon, color: Color32) {
    let s = Stroke::new(1.5, color);
    let point = |x, y| c + Vec2::new(x, y);
    let line = |a: (f32, f32), b: (f32, f32)| {
        p.line_segment([point(a.0, a.1), point(b.0, b.1)], s);
    };
    match icon {
        Icon::Select => {
            p.add(egui::Shape::closed_line(
                vec![
                    point(-6.0, -8.0),
                    point(-4.0, 7.0),
                    point(0.0, 2.0),
                    point(6.0, 1.0),
                ],
                s,
            ));
            line((0.0, 2.0), (5.0, 8.0));
        }
        Icon::Hand => {
            p.add(egui::Shape::line(
                vec![
                    point(-6.0, 1.0),
                    point(-6.0, -4.0),
                    point(-3.0, -4.0),
                    point(-3.0, -8.0),
                    point(0.0, -8.0),
                    point(0.0, -5.0),
                    point(3.0, -5.0),
                    point(3.0, -2.0),
                    point(6.0, -2.0),
                    point(6.0, 5.0),
                    point(2.0, 8.0),
                    point(-3.0, 8.0),
                    point(-8.0, 2.0),
                    point(-6.0, 1.0),
                ],
                s,
            ));
        }
        Icon::Plus | Icon::Minus => {
            line((-6.0, 0.0), (6.0, 0.0));
            if matches!(icon, Icon::Plus) {
                line((0.0, -6.0), (0.0, 6.0));
            }
        }
        Icon::Fit => {
            for (x, y) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                line((x * 8.0, y * 3.0), (x * 8.0, y * 8.0));
                line((x * 8.0, y * 8.0), (x * 3.0, y * 8.0));
            }
        }
        Icon::Target => {
            p.circle_stroke(c, 7.0, s);
            p.circle_stroke(c, 3.0, s);
            line((-10.0, 0.0), (10.0, 0.0));
            line((0.0, -10.0), (0.0, 10.0));
        }
        Icon::Crop => {
            line((-4.0, -9.0), (-4.0, 5.0));
            line((-4.0, 5.0), (9.0, 5.0));
            line((-9.0, -5.0), (4.0, -5.0));
            line((4.0, -5.0), (4.0, 9.0));
        }
        Icon::Whole => {
            p.rect_stroke(
                egui::Rect::from_center_size(c, Vec2::new(18.0, 14.0)),
                1.0,
                s,
                egui::StrokeKind::Inside,
            );
            line((-6.0, 4.0), (-1.0, -2.0));
            line((-1.0, -2.0), (5.0, 4.0));
        }
        Icon::Eye | Icon::Hidden => {
            p.add(egui::Shape::closed_line(
                vec![
                    point(-9.0, 0.0),
                    point(-4.0, -5.0),
                    point(4.0, -5.0),
                    point(9.0, 0.0),
                    point(4.0, 5.0),
                    point(-4.0, 5.0),
                ],
                s,
            ));
            p.circle_stroke(c, 2.0, s);
            if matches!(icon, Icon::Hidden) {
                line((-9.0, -8.0), (9.0, 8.0));
            }
        }
        Icon::Edit => {
            p.add(egui::Shape::closed_line(
                vec![
                    point(-7.0, 7.0),
                    point(-5.0, 1.0),
                    point(4.0, -8.0),
                    point(8.0, -4.0),
                    point(-1.0, 5.0),
                ],
                s,
            ));
        }
        Icon::Delete => {
            line((-7.0, -5.0), (7.0, -5.0));
            line((-3.0, -8.0), (3.0, -8.0));
            p.add(egui::Shape::line(
                vec![
                    point(-5.0, -3.0),
                    point(-4.0, 8.0),
                    point(4.0, 8.0),
                    point(5.0, -3.0),
                ],
                s,
            ));
            line((-1.0, -1.0), (-1.0, 5.0));
            line((2.0, -1.0), (2.0, 5.0));
        }
        Icon::Lock => {
            p.rect_stroke(
                egui::Rect::from_center_size(point(0.0, 3.0), Vec2::new(12.0, 10.0)),
                2.0,
                s,
                egui::StrokeKind::Inside,
            );
            p.add(egui::Shape::line(
                vec![
                    point(-4.0, -2.0),
                    point(-4.0, -7.0),
                    point(4.0, -7.0),
                    point(4.0, -2.0),
                ],
                s,
            ));
        }
    }
}
#[derive(Clone, Copy)]
pub enum ToolAction {
    Select,
    Pan,
    Fit,
    ZoomIn,
    ZoomOut,
    Target,
    Crop,
    Whole,
}
pub fn tools(ui: &mut egui::Ui, tool: usize, target: bool) -> Option<ToolAction> {
    let mut action = None;
    for (i, which, label, a, active) in [
        (
            0,
            Icon::Select,
            "Выделение и перемещение",
            ToolAction::Select,
            tool == 5,
        ),
        (
            1,
            Icon::Hand,
            "Перемещение холста",
            ToolAction::Pan,
            tool == 0,
        ),
        (2, Icon::Fit, "Вписать весь снимок", ToolAction::Fit, false),
        (3, Icon::Plus, "Увеличить", ToolAction::ZoomIn, false),
        (4, Icon::Minus, "Уменьшить", ToolAction::ZoomOut, false),
        (
            5,
            Icon::Target,
            "Мишень: показать / скрыть",
            ToolAction::Target,
            target,
        ),
        (
            6,
            Icon::Crop,
            "Выбрать область отчета",
            ToolAction::Crop,
            tool == 3,
        ),
        (
            7,
            Icon::Whole,
            "Весь снимок в отчет",
            ToolAction::Whole,
            false,
        ),
    ] {
        if i == 2 || i == 5 || i == 6 {
            ui.add_space(8.0);
        }
        if icon(ui, which, active, label).clicked() {
            action = Some(a);
        }
    }
    action
}
#[derive(Clone)]
pub struct Stamp(pub usize);
pub fn palette(ui: &mut egui::Ui, choice: usize, editable: bool) -> Option<usize> {
    let mut result = None;
    egui::Grid::new("defect-grid")
        .spacing([4.0, 4.0])
        .show(ui, |ui| {
            for (i, p) in crate::defects::PRESETS.iter().enumerate() {
                ui.add_enabled_ui(editable, |ui| {
                    let response = ui
                        .add_sized(
                            [64.0, 32.0],
                            egui::Button::new(p.short)
                                .selected(choice == i)
                                .sense(Sense::click_and_drag()),
                        )
                        .on_hover_text(p.name);
                    response.dnd_set_drag_payload(Stamp(i));
                    if response.clicked() {
                        result = Some(i);
                    }
                });
                if i % 3 == 2 {
                    ui.end_row();
                }
            }
        });
    result
}
#[derive(Clone, Copy)]
pub enum LayerAction {
    Select(Uuid),
    Edit(Uuid),
    Delete(Uuid),
}
pub fn layers(
    ui: &mut egui::Ui,
    annotations: &[Annotation],
    selected: Option<Uuid>,
    view: &mut LayerView,
    editable: bool,
) -> Option<LayerAction> {
    let mut action = None;
    let count = annotations.iter().filter(|a| LayerView::legacy(a)).count();
    if count > 0 {
        ui.checkbox(&mut view.show_legacy, format!("Старая разметка · {count}"))
            .on_hover_text(
                "Показывает старые круги и стрелки только на экране; данные и отчет не изменяются.",
            );
    }
    let show_legacy = view.show_legacy;
    egui::ScrollArea::vertical()
        .id_salt("layers-list")
        .max_height(210.0)
        .show(ui, |ui| {
            for a in annotations
                .iter()
                .rev()
                .filter(|a| show_legacy || !LayerView::legacy(a))
            {
                let title = match &a.layer {
                    AnnotationLayer::Callout { full_name, .. } => {
                        format!("{} · {}", a.label, full_name)
                    }
                    AnnotationLayer::Target { .. } => "Мишень".into(),
                    AnnotationLayer::OutputRegion { .. } => "Область отчета".into(),
                    _ => {
                        if a.label.is_empty() {
                            "Старая метка".into()
                        } else {
                            a.label.clone()
                        }
                    }
                };
                ui.horizontal(|ui| {
                    if icon(
                        ui,
                        if view.visible(a) {
                            Icon::Eye
                        } else {
                            Icon::Hidden
                        },
                        false,
                        "Видимость на экране; отчет не изменяется",
                    )
                    .clicked()
                    {
                        view.toggle(a.id);
                    }
                    let active = selected == Some(a.id);
                    let can_edit =
                        active && editable && matches!(a.layer, AnnotationLayer::Callout { .. });
                    let can_delete = active && editable;
                    let reserved = if can_edit {
                        80.0
                    } else if can_delete {
                        40.0
                    } else {
                        0.0
                    };
                    if ui
                        .add_sized(
                            [(ui.available_width() - reserved).max(32.0), 32.0],
                            egui::Button::new(egui::RichText::new(&title).size(13.0))
                                .selected(active)
                                .truncate(),
                        )
                        .on_hover_text(&title)
                        .clicked()
                    {
                        action = Some(LayerAction::Select(a.id));
                    }
                    if can_edit && icon(ui, Icon::Edit, false, "Изменить обозначение").clicked()
                    {
                        action = Some(LayerAction::Edit(a.id));
                    }
                    if can_delete
                        && icon(ui, Icon::Delete, false, "Удалить выбранный объект").clicked()
                    {
                        action = Some(LayerAction::Delete(a.id));
                    }
                });
            }
        });
    ui.horizontal(|ui| {
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::hover());
        paint_icon(
            ui.painter(),
            rect.center(),
            Icon::Lock,
            ui.visuals().weak_text_color(),
        );
        response.on_hover_text("Оригинал не изменяется");
        ui.label(egui::RichText::new("Оригинал").small().weak());
    });
    action
}

/// Shared spacing/type scale for the native application and its offscreen preview.
pub fn configure(ctx: &egui::Context) {
    ctx.style_mut(|s| {
        s.spacing.button_padding = egui::vec2(8.0, 5.0);
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
        s.text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
        s.text_styles
            .insert(egui::TextStyle::Small, egui::FontId::proportional(12.0));
    });
}

pub fn paint_layer(
    painter: &egui::Painter,
    rect: egui::Rect,
    a: &Annotation,
    color: egui::Color32,
) {
    let center = egui::pos2(
        rect.left() + a.x * rect.width(),
        rect.top() + a.y * rect.height(),
    );
    let r = a.radius * rect.width().min(rect.height());
    let stroke = egui::Stroke::new(1.0, color);
    match a.layer {
        AnnotationLayer::Mark => {
            painter.circle_stroke(center, r, egui::Stroke::new(2.0, color));
            painter.text(
                center,
                egui::Align2::CENTER_CENTER,
                &a.label,
                egui::FontId::proportional(13.0),
                color,
            );
        }
        AnnotationLayer::Callout { end_x, end_y, .. } => {
            let tip = egui::pos2(
                rect.left() + end_x * rect.width(),
                rect.top() + end_y * rect.height(),
            );
            let delta = tip - center;
            let length = delta.length();
            let start = if length > r {
                center + delta / length * r
            } else {
                center
            };
            let ink = egui::Color32::from_rgb(220, 65, 40);
            painter.arrow(start, tip - start, egui::Stroke::new(2.0, ink));
            painter.circle_filled(center, r, egui::Color32::WHITE);
            painter.circle_stroke(center, r, egui::Stroke::new(2.0, ink));
            let font = (r * 2.5 / (a.label.chars().count().max(2) as f32)).clamp(8.0, 30.0);
            painter.text(
                center,
                egui::Align2::CENTER_CENTER,
                &a.label,
                egui::FontId::proportional(font),
                egui::Color32::BLACK,
            );
        }
        AnnotationLayer::Target {
            rotation_degrees, ..
        } => {
            for fraction in [1.0, 0.67, 0.33] {
                painter.circle_stroke(center, r * fraction, stroke);
            }
            for hour in 0..12 {
                let angle = hour as f32 * std::f32::consts::TAU / 12.0
                    + rotation_degrees.to_radians()
                    - std::f32::consts::FRAC_PI_2;
                let direction = egui::vec2(angle.cos(), angle.sin());
                painter.line_segment([center, center + direction * r], stroke);
                if hour % 3 == 0 {
                    painter.text(
                        center + direction * (r + 12.0),
                        egui::Align2::CENTER_CENTER,
                        if hour == 0 {
                            "12".into()
                        } else {
                            hour.to_string()
                        },
                        egui::FontId::proportional(12.0),
                        color,
                    );
                }
            }
        }
        AnnotationLayer::Arrow { end_x, end_y } => {
            let end = egui::pos2(
                rect.left() + end_x * rect.width(),
                rect.top() + end_y * rect.height(),
            );
            painter.arrow(center, end - center, egui::Stroke::new(2.0, color));
            painter.text(
                center,
                egui::Align2::LEFT_BOTTOM,
                &a.label,
                egui::FontId::proportional(13.0),
                color,
            );
        }
        AnnotationLayer::OutputRegion { width, height } => {
            let end = center + egui::vec2(width * rect.width(), height * rect.height());
            let region = egui::Rect::from_two_pos(center, end);
            let points = [
                region.left_top(),
                region.right_top(),
                region.right_bottom(),
                region.left_bottom(),
                region.left_top(),
            ];
            for pair in points.windows(2) {
                painter.line_segment([pair[0], pair[1]], egui::Stroke::new(2.0, color));
            }
            painter.text(
                region.left_top() + egui::vec2(5.0, 5.0),
                egui::Align2::LEFT_TOP,
                "В отчет",
                egui::FontId::proportional(12.0),
                color,
            );
        }
    }
}

pub fn thumbnail(
    ui: &mut egui::Ui,
    texture: Option<&egui::TextureHandle>,
    number: usize,
    selected: bool,
    included: &mut bool,
) -> Response {
    ui.allocate_ui_with_layout(
        egui::vec2(116.0, 82.0),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            let button = if let Some(texture) = texture {
                egui::Button::image(
                    egui::Image::new((texture.id(), texture.size_vec2()))
                        .fit_to_exact_size(egui::vec2(96.0, 40.0)),
                )
            } else {
                egui::Button::new(format!("Снимок {number}"))
            };
            let response = ui.add_sized([108.0, 52.0], button.selected(selected));
            ui.horizontal(|ui| {
                ui.small(format!("Снимок {number}"));
                ui.checkbox(included, "")
                    .on_hover_text("Включить снимок в отчет");
            });
            response
        },
    )
    .inner
}
