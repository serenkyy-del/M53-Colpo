//! Geometry of the selected marker's property card, independent of medical fields.

/// UI field estimates supplied by the caller; long cards retain one scroll container.
pub fn content_height(field_heights: impl IntoIterator<Item = f32>) -> f32 {
    let heights: Vec<_> = field_heights.into_iter().collect();
    // Header, body padding (including a safe bottom inset), and NativeForm's gap_2.
    (76. + heights.iter().sum::<f32>() + heights.len().saturating_sub(1) as f32 * 8.).clamp(110., 300.)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoxRect { pub x: f32, pub y: f32, pub w: f32, pub h: f32 }
impl BoxRect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self { Self { x, y, w, h } }
    pub fn right(self) -> f32 { self.x + self.w }
    pub fn bottom(self) -> f32 { self.y + self.h }
    pub fn overlaps(self, other: Self) -> bool {
        self.x < other.right() && self.right() > other.x && self.y < other.bottom() && self.bottom() > other.y
    }
    pub fn contains(self, other: Self) -> bool {
        other.x >= self.x && other.y >= self.y && other.right() <= self.right() + 0.1 && other.bottom() <= self.bottom() + 0.1
    }
}
/// Prefer empty image margins, then the largest nearby free side. The caller scrolls card contents.
/// `marker` includes the badge and arrow tip so the card never masks either or their connecting line.
pub fn place(viewport: BoxRect, image: BoxRect, marker: BoxRect, desired_width: f32, desired_height: f32) -> Option<BoxRect> {
    let gap = 8.;
    let view = BoxRect::new(viewport.x + gap, viewport.y + gap, (viewport.w - gap * 2.).max(0.), (viewport.h - gap * 2.).max(0.));
    let regions = [
        BoxRect::new(image.right() + gap, view.y, view.right() - image.right() - gap, view.h),
        BoxRect::new(view.x, view.y, image.x - gap - view.x, view.h),
        BoxRect::new(view.x, view.y, view.w, image.y - gap - view.y),
        BoxRect::new(view.x, image.bottom() + gap, view.w, view.bottom() - image.bottom() - gap),
        BoxRect::new(marker.right() + gap, view.y, view.right() - marker.right() - gap, view.h),
        BoxRect::new(view.x, view.y, marker.x - gap - view.x, view.h),
        BoxRect::new(view.x, view.y, view.w, marker.y - gap - view.y),
        BoxRect::new(view.x, marker.bottom() + gap, view.w, view.bottom() - marker.bottom() - gap),
    ];
    let mut best: Option<(BoxRect, bool, f32, f32)> = None;
    for region in regions {
        let region = BoxRect::new(region.x.max(view.x), region.y.max(view.y), region.right().min(view.right()) - region.x.max(view.x), region.bottom().min(view.bottom()) - region.y.max(view.y));
        if region.w < 180. || region.h < 84. { continue; }
        let w = desired_width.min(region.w);
        let h = desired_height.min(region.h);
        let card = BoxRect::new((marker.x + marker.w + gap).clamp(region.x, region.right() - w), marker.y.clamp(region.y, region.bottom() - h), w, h);
        if card.overlaps(marker) || !view.contains(card) { continue; }
        let outside = !card.overlaps(image);
        let area = w * h;
        let distance = (card.x + w / 2. - marker.x - marker.w / 2.).hypot(card.y + h / 2. - marker.y - marker.h / 2.);
        let better = best.is_none_or(|(_, old_outside, old_area, old_distance)| {
            (outside && !old_outside) || (outside == old_outside && (area > old_area + 0.1 || ((area - old_area).abs() <= 0.1 && distance < old_distance)))
        });
        if better { best = Some((card, outside, area, distance)); }
    }
    best.map(|(rect, _, _, _)| rect)
}

fn segment_intersects(rect: BoxRect, start: (f32, f32), end: (f32, f32)) -> bool {
    let mut enter: f32 = 0.;
    let mut leave: f32 = 1.;
    for (origin, delta, low, high) in [(start.0, end.0 - start.0, rect.x, rect.right()), (start.1, end.1 - start.1, rect.y, rect.bottom())] {
        if delta.abs() < 0.001 { if origin < low || origin > high { return false; } } else {
            let a = (low - origin) / delta;
            let b = (high - origin) / delta;
            enter = enter.max(a.min(b));
            leave = leave.min(a.max(b));
            if enter > leave { return false; }
        }
    }
    true
}
/// A long diagonal arrow need not reserve its entire rectangular bounding box.
/// If no full card fits, find a smaller scrollable card that avoids its actual segment.
pub fn place_arrow(viewport: BoxRect, image: BoxRect, badge: BoxRect, tip: (f32, f32), width: f32, height: f32) -> Option<BoxRect> {
    let protected = BoxRect::new(badge.x.min(tip.0 - 8.), badge.y.min(tip.1 - 8.), badge.right().max(tip.0 + 8.) - badge.x.min(tip.0 - 8.), badge.bottom().max(tip.1 + 8.) - badge.y.min(tip.1 - 8.));
    if let Some(card) = place(viewport, image, protected, width, height) { return Some(card); }
    let view = BoxRect::new(viewport.x + 8., viewport.y + 8., (viewport.w - 16.).max(0.), (viewport.h - 16.).max(0.));
    let center = (badge.x + badge.w / 2., badge.y + badge.h / 2.);
    let tip_box = BoxRect::new(tip.0 - 8., tip.1 - 8., 16., 16.);
    let mut best: Option<(BoxRect, bool, f32, f32)> = None;
    for w in [width, 260., 220., 180.] {
        for h in [height, 220., 160., 96.] {
            let w = w.min(width).min(view.w);
            let h = h.min(height).min(view.h);
            if w < 180. || h < 84. { continue; }
            for x in [view.x, view.right() - w, badge.right() + 8., badge.x - 8. - w] {
                for y in [view.y, view.bottom() - h, badge.y, badge.bottom() + 8., badge.y - 8. - h] {
                    let card = BoxRect::new(x.clamp(view.x, view.right() - w), y.clamp(view.y, view.bottom() - h), w, h);
                    let padded = BoxRect::new(card.x - 4., card.y - 4., card.w + 8., card.h + 8.);
                    if padded.overlaps(badge) || padded.overlaps(tip_box) || segment_intersects(padded, center, tip) { continue; }
                    let outside = !card.overlaps(image);
                    let area = w * h;
                    let distance = (card.x + w / 2. - center.0).hypot(card.y + h / 2. - center.1);
                    if best.is_none_or(|(_, old_outside, old_area, old_distance)| (outside && !old_outside) || (outside == old_outside && (area > old_area + 0.1 || ((area - old_area).abs() <= 0.1 && distance < old_distance)))) {
                        best = Some((card, outside, area, distance));
                    }
                }
            }
        }
    }
    best.map(|(card, _, _, _)| card)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_height_follows_field_content_and_keeps_compact_bounds() {
        assert_eq!(content_height([]), 110.);
        assert_eq!(content_height([104.]), 180.);
        assert_eq!(content_height([60., 28.]), 172.);
        assert_eq!(content_height([28.; 3]) + 24., 200.); // three checks + read-only line
        assert_eq!(content_height([104.; 8]), 300.);
    }

    #[test]
    fn uses_image_gutter_when_it_has_room() {
        let viewport = BoxRect::new(0., 0., 1000., 600.);
        let image = BoxRect::new(0., 0., 620., 600.);
        let marker = BoxRect::new(270., 230., 60., 60.);
        let card = place(viewport, image, marker, 310., 280.).unwrap();
        assert!(card.x >= image.right());
        assert!(!card.overlaps(marker));
        assert!(viewport.contains(card));
    }

    #[test]
    fn compact_viewport_shrinks_scrollable_card_instead_of_covering_marker() {
        let viewport = BoxRect::new(0., 0., 540., 360.);
        let marker = BoxRect::new(245., 150., 50., 50.);
        let card = place(viewport, viewport, marker, 310., 280.).unwrap();
        assert!(!card.overlaps(marker));
        assert!(viewport.contains(card));
        assert!(card.w >= 180. && card.h >= 84.);
    }

    #[test]
    fn an_edge_marker_does_not_push_card_out_of_canvas() {
        let viewport = BoxRect::new(0., 0., 700., 420.);
        for marker in [BoxRect::new(0., 0., 50., 50.), BoxRect::new(650., 370., 50., 50.)] {
            let card = place(viewport, viewport, marker, 310., 280.).unwrap();
            assert!(viewport.contains(card));
            assert!(!card.overlaps(marker));
        }
    }

    #[test]
    fn a_long_diagonal_arrow_keeps_a_usable_card_without_covering_arrow_or_badge() {
        let viewport = BoxRect::new(0., 0., 700., 420.);
        let badge = BoxRect::new(605., 325., 50., 50.);
        let tip = (45., 45.);
        let card = place_arrow(viewport, viewport, badge, tip, 310., 300.).unwrap();
        assert!(viewport.contains(card));
        assert!(!card.overlaps(badge));
        assert!(!segment_intersects(card, (630., 350.), tip));
    }
}
