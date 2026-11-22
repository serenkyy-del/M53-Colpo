//! GPUI paints vectors directly; no annotated raster is regenerated during a gesture.
use gpui_kit::*;
use m53_contracts::workflow::AnnotationLayer;
use m53_gpui_editor::{Photo, Rect};
use uuid::Uuid;
fn at(r: Rect, x: f32, y: f32) -> Point<Pixels> {
    point(px(r.x + x * r.w), px(r.y + y * r.h))
}
fn path(points: &[Point<Pixels>], color: Hsla, width: f32, window: &mut Window) {
    let mut p = PathBuilder::stroke(px(width));
    if let Some(first) = points.first() {
        p.move_to(*first);
        for point in &points[1..] {
            p.line_to(*point);
        }
    }
    if let Ok(p) = p.build() {
        window.paint_path(p, color);
    }
}
fn circle(center: Point<Pixels>, radius: f32, color: Hsla, width: f32, window: &mut Window) {
    let points: Vec<_> = (0..=96)
        .map(|i| {
            let a = i as f32 * std::f32::consts::TAU / 96.;
            point(
                center.x + px(a.cos() * radius),
                center.y + px(a.sin() * radius),
            )
        })
        .collect();
    path(&points, color, width, window);
}
fn handle(p: Point<Pixels>, window: &mut Window) {
    window.paint_quad(fill(
        Bounds::new(point(p.x - px(3.), p.y - px(3.)), size(px(6.), px(6.))),
        rgb(0x1686d9),
    ));
}
fn text(
    label: &str,
    position: Point<Pixels>,
    font_size: f32,
    color: Hsla,
    center: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let style = window.text_style();
    let line = window.text_system().shape_line(
        label.to_string().into(),
        px(font_size),
        &[TextRun {
            len: label.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        }],
        None,
    );
    let origin = if center {
        point(
            position.x - line.width / 2.,
            position.y - px(font_size * 0.65),
        )
    } else {
        position
    };
    let _ = line.paint(
        origin,
        px(font_size * 1.3),
        TextAlign::Left,
        None,
        window,
        cx,
    );
}
pub fn paint(photo: &Photo, r: Rect, selected: Option<Uuid>, window: &mut Window, cx: &mut App) {
    let target_color: Hsla = rgb(0x77b8e8).into();
    // Screen-only crop veil. Original/export pixels are never modified.
    if let Some(a) = photo.annotations.iter().find(|a| {
        !photo.hidden.contains(&a.id) && matches!(a.layer, AnnotationLayer::OutputRegion { .. })
    }) && let AnnotationLayer::OutputRegion { width, height } = a.layer
    {
        let (l, t, rr, b) = (
            a.x.clamp(0., 1.),
            a.y.clamp(0., 1.),
            (a.x + width).clamp(0., 1.),
            (a.y + height).clamp(0., 1.),
        );
        for (x, y, w, h) in [
            (0., 0., 1., t),
            (0., b, 1., 1. - b),
            (0., t, l, b - t),
            (rr, t, 1. - rr, b - t),
        ] {
            if w > 0. && h > 0. {
                window.paint_quad(fill(
                    Bounds::new(at(r, x, y), size(px(w * r.w), px(h * r.h))),
                    rgba(0x00000088),
                ));
            }
        }
    }
    // Crop and target first; defect callouts always remain legible above them.
    let mut ordered: Vec<_> = photo
        .annotations
        .iter()
        .filter(|a| !photo.hidden.contains(&a.id))
        .collect();
    ordered.sort_by_key(|a| matches!(a.layer, AnnotationLayer::Callout { .. }));
    for a in ordered {
        let c = at(r, a.x, a.y);
        let radius = if matches!(a.layer,AnnotationLayer::Callout{..}) {m53_client::defects::CALLOUT_RADIUS} else {a.radius} * r.w.min(r.h);
        match &a.layer {
            AnnotationLayer::Callout {
                end_x,
                end_y,
                full_name,
                term_code,
            } => {
                let ink: Hsla = rgb(m53_domain::colpo::marker_color(
                    &a.label,
                    full_name,
                    term_code.as_deref(),
                ))
                .into();
                let end = at(r, *end_x, *end_y);
                let (dx, dy) = (end.x.as_f32() - c.x.as_f32(), end.y.as_f32() - c.y.as_f32());
                let d = dx.hypot(dy).max(0.001);
                let (ux, uy) = (dx / d, dy / d);
                if d > radius {
                    path(
                        &[point(c.x + px(ux * radius), c.y + px(uy * radius)), end],
                        ink,
                        2.,
                        window,
                    );
                    let k = 10.;
                    path(
                        &[
                            point(
                                end.x - px(ux * k) + px(uy * k * 0.5),
                                end.y - px(uy * k) - px(ux * k * 0.5),
                            ),
                            end,
                            point(
                                end.x - px(ux * k) - px(uy * k * 0.5),
                                end.y - px(uy * k) + px(ux * k * 0.5),
                            ),
                        ],
                        ink,
                        2.,
                        window,
                    );
                }
                let mut quad = fill(
                    Bounds::new(
                        point(c.x - px(radius), c.y - px(radius)),
                        size(px(radius * 2.), px(radius * 2.)),
                    ),
                    white(),
                );
                quad.corner_radii = Corners::all(px(radius));
                window.paint_quad(quad);
                circle(c, radius, ink, 2., window);
                let font = m53_client::defects::callout_font_size(r.w.min(r.h), radius, &a.label);
                text(&a.label, c, font, black(), true, window, cx);
                if selected == Some(a.id) {
                    handle(end, window);

                }
            }
            AnnotationLayer::Target {
                rotation_degrees, ..
            } => {
                for k in [1. / 3., 2. / 3., 1.] {
                    circle(c, radius * k, target_color, 1., window);
                }
                let rot = rotation_degrees.to_radians() - std::f32::consts::FRAC_PI_2;
                for i in 0..12 {
                    let angle = rot + i as f32 * std::f32::consts::TAU / 12.;
                    path(
                        &[
                            c,
                            point(
                                c.x + px(angle.cos() * radius),
                                c.y + px(angle.sin() * radius),
                            ),
                        ],
                        target_color,
                        1.,
                        window,
                    );
                }
                for (label, dx, dy) in [
                    ("12", 0., -1.),
                    ("3", 1., 0.),
                    ("6", 0., 1.),
                    ("9", -1., 0.),
                ] {
                    text(
                        label,
                        point(c.x + px(dx * (radius + 13.)), c.y + px(dy * (radius + 13.))),
                        12.,
                        target_color,
                        true,
                        window,
                        cx,
                    );
                }
                if selected == Some(a.id) {
                    handle(c, window);
                    for (dx, dy) in [(radius, 0.), (-radius, 0.), (0., radius), (0., -radius)] {
                        handle(point(c.x + px(dx), c.y + px(dy)), window);
                    }
                }
            }
            AnnotationLayer::OutputRegion { width, height } => {
                let end = at(r, a.x + width, a.y + height);
                path(
                    &[c, point(end.x, c.y), end, point(c.x, end.y), c],
                    target_color,
                    1.5,
                    window,
                );
                text(
                    "В отчет",
                    point(c.x + px(6.), c.y + px(5.)),
                    12.,
                    target_color,
                    false,
                    window,
                    cx,
                );
                for corner in [c, point(end.x, c.y), end, point(c.x, end.y)] {
                    handle(corner, window);
                }
            }
            AnnotationLayer::Mark => {
                circle(c, radius, target_color, 1.5, window);
                if !a.label.is_empty() {
                    text(
                        &a.label,
                        point(c.x, c.y - px(radius + 10.)),
                        12.,
                        target_color,
                        true,
                        window,
                        cx,
                    );
                }
                if selected == Some(a.id) {
                    handle(c, window);
                }
            }
            AnnotationLayer::Arrow { end_x, end_y } => {
                let end = at(r, *end_x, *end_y);
                let dx = end.x.as_f32() - c.x.as_f32();
                let dy = end.y.as_f32() - c.y.as_f32();
                let d = dx.hypot(dy).max(0.001);
                let (ux, uy) = (dx / d, dy / d);
                path(&[c, end], target_color, 1.5, window);
                path(
                    &[
                        point(
                            end.x - px(ux * 10.) + px(uy * 5.),
                            end.y - px(uy * 10.) - px(ux * 5.),
                        ),
                        end,
                        point(
                            end.x - px(ux * 10.) - px(uy * 5.),
                            end.y - px(uy * 10.) + px(ux * 5.),
                        ),
                    ],
                    target_color,
                    1.5,
                    window,
                );
                if !a.label.is_empty() {
                    text(
                        &a.label,
                        point(c.x, c.y - px(12.)),
                        12.,
                        target_color,
                        true,
                        window,
                        cx,
                    );
                }
                if selected == Some(a.id) {
                    handle(c, window);
                    handle(end, window);
                }
            }
        }
    }
}
