//! Presentation geometry for the existing PDF preview; never renders or mutates a report.

pub const PAGE_WIDTH: f32 = 740.;
pub const PAGE_HEIGHT: f32 = 1047.;

pub fn default_template_changed<T,K:PartialEq>(old:&[T],new:&[T],key:impl Fn(&T)->K)->bool {
    old.first().map(&key)!=new.first().map(key)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Zoom {
    #[default]
    FitWidth,
    FitPage,
    ActualSize,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelWidths {
    pub navigation: f32,
    pub delivery: f32,
}

/// Both report tools remain visible; the central paper owns any horizontal scrolling.
pub fn panel_widths(viewport_width: f32) -> PanelWidths {
    if viewport_width < 1440. {
        PanelWidths {
            navigation: 176.,
            delivery: 232.,
        }
    } else {
        PanelWidths {
            navigation: 176.,
            delivery: 248.,
        }
    }
}
/// Uses an existing page image at a smaller display size; no new PDF/image is generated.
pub fn thumbnail_dimensions(width: f32, height: f32) -> PageSize {
    Zoom::FitPage.dimensions(width, height)
}
impl Zoom {
    pub fn label(self) -> &'static str {
        match self {
            Self::FitWidth => "По ширине",
            Self::FitPage => "Страница целиком",
            Self::ActualSize => "100%",
        }
    }
    /// Available dimensions exclude the preview's padding and toolbars.
    pub fn dimensions(self, available_width: f32, available_height: f32) -> PageSize {
        let width = if available_width.is_finite() {
            available_width.max(1.)
        } else {
            PAGE_WIDTH
        };
        let height = if available_height.is_finite() {
            available_height.max(1.)
        } else {
            PAGE_HEIGHT
        };
        let scale = match self {
            Self::FitWidth => width / PAGE_WIDTH,
            Self::FitPage => (width / PAGE_WIDTH).min(height / PAGE_HEIGHT),
            Self::ActualSize => 1.,
        };
        PageSize {
            width: PAGE_WIDTH * scale,
            height: PAGE_HEIGHT * scale,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReportView {
    pub zoom: Zoom,
    /// Zero-based page. Re-clamp against the actual page count after a new preview arrives.
    pub page: usize,
}
impl ReportView {
    pub fn set_page(&mut self, page: usize, count: usize) {
        self.page = page.min(count.saturating_sub(1));
    }
    pub fn previous(&mut self) {
        self.page = self.page.saturating_sub(1);
    }
    pub fn next(&mut self, count: usize) {
        self.set_page(self.page.saturating_add(1), count);
    }
    pub fn page_label(self, count: usize) -> String {
        if count == 0 {
            "Страниц пока нет".into()
        } else {
            format!("Страница {} из {count}", self.page.min(count - 1) + 1)
        }
    }
    /// Offset within a uniformly sized page stack, excluding outer padding.
    pub fn page_offset(self, size: PageSize, gap: f32) -> f32 {
        self.page as f32 * (size.height + gap.max(0.))
    }
}

/// A seek to the final page may be clamped before its top when the viewport is tall.
pub fn page_at_offset(
    top: f32,
    viewport_height: f32,
    size: PageSize,
    gap: f32,
    padding: f32,
    count: usize,
    current: usize,
) -> usize {
    if count == 0 {
        return 0;
    }
    let stride = size.height + gap;
    let max_scroll = (count as f32 * stride - gap + 2. * padding - viewport_height).max(0.);
    if max_scroll <= 1. {
        return current.min(count - 1);
    }
    if top >= max_scroll - 1. {
        return count - 1;
    }
    ((top.max(0.) / stride).floor() as usize).min(count - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitting_preserves_page_ratio_and_respects_the_available_space() {
        let width = Zoom::FitWidth.dimensions(500., 600.);
        assert!((width.width - 500.).abs() < 0.01);
        assert!((width.height / width.width - PAGE_HEIGHT / PAGE_WIDTH).abs() < 0.001);
        let page = Zoom::FitPage.dimensions(500., 600.);
        assert!(page.width <= 500. && page.height <= 600.);
        assert_eq!(
            Zoom::ActualSize.dimensions(500., 600.),
            PageSize {
                width: 740.,
                height: 1047.
            }
        );
    }

    #[test]
    fn page_navigation_clamps_after_report_rebuild_and_never_underflows() {
        let mut view = ReportView::default();
        view.previous();
        assert_eq!(view.page, 0);
        view.next(4);
        view.next(4);
        view.next(4);
        view.next(4);
        assert_eq!(view.page, 3);
        view.set_page(99, 2);
        assert_eq!(view.page, 1);
        view.set_page(99, 0);
        assert_eq!(view.page, 0);
        assert_eq!(view.page_label(0), "Страниц пока нет");
    }

    #[test]
    fn navigation_and_zoom_share_the_same_scroll_geometry() {
        let view = ReportView {
            zoom: Zoom::FitWidth,
            page: 2,
        };
        let page = view.zoom.dimensions(370., 600.);
        assert_eq!(view.page_offset(page, 20.), 1087.);
        assert_eq!(view.page_label(4), "Страница 3 из 4");
        assert!(Zoom::FitPage.dimensions(0., -1.).height.is_finite());
    }

    #[test]
    fn last_page_stays_selected_when_tall_viewport_clamps_its_top() {
        let page = PageSize {
            width: 740.,
            height: 1047.,
        };
        assert_eq!(page_at_offset(0., 2100., page, 20., 20., 2, 0), 0);
        assert_eq!(page_at_offset(54., 2100., page, 20., 20., 2, 1), 1);
        assert_eq!(page_at_offset(0., 3000., page, 20., 20., 2, 1), 1);
        assert_eq!(page_at_offset(0., 3000., page, 20., 20., 0, 99), 0);
    }

    #[test]
    fn permanent_report_panels_leave_paper_space_at_compact_and_large_widths() {
        for width in [1024., 1280., 1920.] {
            let panels = panel_widths(width);
            assert!(panels.navigation >= 152. && panels.delivery >= 208.);
            assert!(width - panels.navigation - panels.delivery >= 600.);
        }
    }

    #[test]
    fn thumbnails_fit_a_fixed_navigation_row_without_distorting_the_paper() {
        let thumb = thumbnail_dimensions(48., 68.);
        assert!(thumb.width <= 48. && thumb.height <= 68.);
        assert!((thumb.height / thumb.width - PAGE_HEIGHT / PAGE_WIDTH).abs() < 0.001);
    }
}

#[cfg(test)]
#[test]
fn only_a_change_of_the_first_template_invalidates_the_preview() {
    assert!(!default_template_changed(&[1,2], &[1,3], |v|*v));
    assert!(default_template_changed(&[1,2], &[2,1], |v|*v));
    assert!(default_template_changed(&[1], &[], |v|*v));
    assert!(default_template_changed(&[], &[1], |v|*v));
}
