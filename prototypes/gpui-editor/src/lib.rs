//! Portable editor state shared with the existing Rust annotation and image engine.
//! No API credentials, patient cache, or production endpoint are used by this slice.
rust_i18n::i18n!("locales", fallback = "ru");
pub mod calendar_locale;
use m53_client::defects::{self, Handle, PRESETS};
use m53_contracts::workflow::{Annotation, AnnotationLayer, AnnotationSpace};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
pub mod registry;

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub photos: Vec<Photo>,
    pub selected: usize,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Photo {
    pub id: Uuid,
    pub width: u32,
    pub height: u32,
    pub in_report: bool,
    #[serde(default)]
    pub removed: bool,
    pub annotations: Vec<Annotation>,
    pub hidden: Vec<Uuid>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}
impl Rect {
    pub fn fit(view: Self, width: u32, height: u32, zoom: f32, pan: (f32, f32)) -> Self {
        let scale = (view.w / width as f32).min(view.h / height as f32) * zoom;
        let w = width as f32 * scale;
        let h = height as f32 * scale;
        Self {
            x: view.x + (view.w - w) * 0.5 + pan.0,
            y: view.y + (view.h - h) * 0.5 + pan.1,
            w,
            h,
        }
    }
    pub fn normalized(self, x: f32, y: f32) -> (f32, f32) {
        ((x - self.x) / self.w, (y - self.y) / self.h)
    }
    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x <= self.x + self.w && y <= self.y + self.h
    }
}
impl Session {
    pub fn visible_count(&self)->usize {self.photos.iter().filter(|p|!p.removed).count()}
    pub fn remove_photos(&mut self,ids:&[Uuid])->usize {
        let mut count=0;for photo in &mut self.photos {if !photo.removed && ids.contains(&photo.id){photo.removed=true;photo.in_report=false;count+=1;}}
        self.ensure_visible_selection();count
    }
    pub fn ensure_visible_selection(&mut self){
        if self.photos.get(self.selected).is_none_or(|p|p.removed){self.selected=self.photos.iter().enumerate().skip(self.selected).find(|(_,p)|!p.removed).map(|(i,_)|i).or_else(||self.photos.iter().rposition(|p|!p.removed)).unwrap_or(0);}
    }
}
impl Photo {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            id: Uuid::new_v4(),
            width,
            height,
            in_report: true,
            removed: false,
            annotations: vec![],
            hidden: vec![],
        }
    }
    pub fn add_callout(&mut self, preset: usize, x: f32, y: f32) -> Uuid {
        let p = &PRESETS[preset];
        if matches!(preset, 6 | 11) {
            self.annotations.retain(|a| {
                !matches!(&a.layer,AnnotationLayer::Callout{full_name,..}
                if (full_name==PRESETS[6].name || full_name==PRESETS[11].name) && a.label!=p.short)
            });
        }
        let mut a = self.annotation(
            x,
            y,
            AnnotationLayer::Callout {
                end_x: (x + 0.15).min(0.98),
                end_y: (y + 0.12).min(0.98),
                full_name: p.name.into(),
                term_code: p.code.map(str::to_owned),
            },
        );
        a.label = p.short.into();
        let id = a.id;
        self.annotations.push(a);
        id
    }
    pub fn place_callout(&mut self, preset: usize, tip: (f32, f32), center: (f32, f32)) -> Uuid {
        let id = self.add_callout(preset, center.0, center.1);
        if let Some(a) = self.annotations.iter_mut().find(|a| a.id == id)
            && let AnnotationLayer::Callout { end_x, end_y, .. } = &mut a.layer
        {
            *end_x = tip.0;
            *end_y = tip.1;
        }
        id
    }
    pub fn annotation(&self, x: f32, y: f32, layer: AnnotationLayer) -> Annotation {
        Annotation {
            id: Uuid::new_v4(),
            asset_id: self.id,
            lesion_id: None,
            finding_id: None,
            coordinate_system: AnnotationSpace::OriginalOrientedNormalizedV1,
            x,
            y,
            radius: 0.045,
            label: String::new(),
            layer,
        }
    }
    pub fn hit(&self, rect: Rect, x: f32, y: f32) -> Option<(Uuid, Handle)> {
        for a in self
            .annotations
            .iter()
            .rev()
            .filter(|a| !self.hidden.contains(&a.id))
        {
            let AnnotationLayer::Callout { end_x, end_y, .. } = a.layer else {
                continue;
            };
            let cx = rect.x + a.x * rect.w;
            let cy = rect.y + a.y * rect.h;
            let radius = defects::CALLOUT_RADIUS * rect.w.min(rect.h);
            if (x - (rect.x + end_x * rect.w)).hypot(y - (rect.y + end_y * rect.h)) <= 10.0 {
                return Some((a.id, Handle::Tip));
            }
            let d = (x - cx).hypot(y - cy);
            if d <= radius + 3. {
                return Some((a.id, Handle::Body));
            }
        }
        None
    }
    pub fn remove_annotations(&mut self, ids: &[Uuid]) {
        self.annotations.retain(|a| !ids.contains(&a.id));
        self.hidden.retain(|id| !ids.contains(id));
    }
    pub fn edit(&mut self, id: Uuid, handle: Handle, x: f32, y: f32) {
        if handle == Handle::Radius {return;}
        if let Some(a) = self.annotations.iter_mut().find(|a| a.id == id) {
            defects::edit(a, handle, x, y, self.width as f32 / self.height as f32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fit_and_hit_use_original_aspect_at_different_zoom_and_pan() {
        let mut p = Photo::new(800, 1200);
        let id = p.add_callout(7, 0.3, 0.4);
        for zoom in [0.25, 0.5, 1.0, 2.0] {
            let r = Rect::fit(
                Rect {
                    x: 52.,
                    y: 80.,
                    w: 900.,
                    h: 600.,
                },
                p.width,
                p.height,
                zoom,
                (17., -23.),
            );
            assert!((r.w / r.h - 2. / 3.).abs() < 0.00001);
            assert_eq!(
                p.hit(r, r.x + 0.3 * r.w, r.y + 0.4 * r.h),
                Some((id, Handle::Body))
            );
            let a = &p.annotations[0];
            assert_eq!(
                p.hit(
                    r,
                    r.x + a.x * r.w + a.radius * r.w.min(r.h),
                    r.y + a.y * r.h
                ),
                Some((id, Handle::Body))
            );
        }
    }
    #[test]
    fn drag_label_preserves_tip_and_serialization_preserves_full_name() {
        let mut p = Photo::new(800, 1200);
        let id = p.add_callout(7, 0.3, 0.4);
        let layer = p.annotations[0].layer.clone();
        p.edit(id, Handle::Body, 0.5, 0.6);
        assert_eq!(p.annotations[0].layer, layer);
        let restored: Photo = serde_json::from_slice(&serde_json::to_vec(&p).unwrap()).unwrap();
        assert_eq!(restored.annotations[0].label, "АБЭ I");
        assert!(
            matches!(&restored.annotations[0].layer,AnnotationLayer::Callout{full_name,..} if full_name.contains("Ацетобелый"))
        );
        p.hidden.push(id);
        assert_eq!(
            p.hit(
                Rect {
                    x: 0.,
                    y: 0.,
                    w: 800.,
                    h: 1200.
                },
                400.,
                720.
            ),
            None
        );
    }
}

pub mod integration;
pub mod schema;

pub mod state;

#[cfg(test)]
mod marker_typography_tests {
    #[test]
    fn common_labels_have_same_size_and_scale_with_photo() {
        use m53_client::defects::callout_font_size as size;
        for label in ["Р", "РО", "МПЭ", "АБЭ II", "Йод(-)"] {
            assert_eq!(size(1000.,45.,label),26.);
            assert_eq!(size(2000.,90.,label),52.);
        }
        assert!(size(1000.,10.,"Йод(-)")<10.);
    }
}

#[cfg(test)]
mod marker_region_tests {
    use super::*;
    #[test]
    fn region_is_per_photo_hidden_counts_and_removal_unlocks() {
        let cervix=0;
        let vulva=PRESETS.iter().position(|p| p.region==m53_domain::protocol::Region::Vulva).unwrap();
        let mut p=Photo::new(1000,800);
        assert!(defects::allows_marker(p.annotations.iter(),cervix));
        assert!(defects::allows_marker(p.annotations.iter(),vulva));
        let id=p.add_callout(cervix,0.4,0.4);
        p.hidden.push(id);
        assert!(defects::allows_marker(p.annotations.iter(),cervix));
        assert!(!defects::allows_marker(p.annotations.iter(),vulva));
        let mut other=Photo::new(1000,800);
        other.add_callout(vulva,0.4,0.4);
        assert!(!defects::allows_marker(other.annotations.iter(),cervix));
        assert!(defects::allows_marker(other.annotations.iter(),vulva));
        p.annotations.clear();
        assert!(defects::allows_marker(p.annotations.iter(),vulva));
    }
    #[test]
    fn preexisting_mixed_photo_is_preserved_and_blocks_both_additions() {
        let vulva=PRESETS.iter().position(|p| p.region==m53_domain::protocol::Region::Vulva).unwrap();
        let mut p=Photo::new(1000,800);
        p.add_callout(0,0.2,0.2);p.add_callout(vulva,0.8,0.8);
        assert!(!defects::allows_marker(p.annotations.iter(),0));
        assert!(!defects::allows_marker(p.annotations.iter(),vulva));
        assert_eq!(p.annotations.len(),2);
    }
}

#[cfg(test)]
mod fixed_badge_and_bulk_tests {
    use super::*;
    #[test]
    fn legacy_radius_cannot_resize_and_edge_moves_badge() {
        let mut p=Photo::new(1000,1000);let id=p.add_callout(0,0.5,0.5);
        p.annotations[0].radius=0.2;
        let r=Rect{x:0.,y:0.,w:1000.,h:1000.};
        assert_eq!(p.hit(r,545.,500.),Some((id,Handle::Body)));
        assert_eq!(p.hit(r,700.,500.),None);
        let before=p.annotations[0].clone();p.edit(id,Handle::Radius,0.8,0.8);
        assert_eq!(serde_json::to_value(&p.annotations[0]).unwrap(),serde_json::to_value(&before).unwrap());
        p.edit(id,Handle::Body,0.6,0.6);assert_eq!(p.annotations[0].x,0.6);
    }
    #[test]
    fn batch_delete_cleans_hidden_only_for_selected_and_snapshot_restores() {
        let mut p=Photo::new(1000,800);let a=p.add_callout(0,0.2,0.2);let b=p.add_callout(1,0.6,0.6);
        p.hidden=vec![a,b];let before=p.clone();
        p.remove_annotations(&[a]);assert_eq!(p.annotations.len(),1);assert_eq!(p.annotations[0].id,b);assert_eq!(p.hidden,vec![b]);
        p=before;assert_eq!(p.annotations.len(),2);assert_eq!(p.hidden,vec![a,b]);
        p.remove_annotations(&[a,b]);assert!(p.annotations.is_empty());assert!(p.hidden.is_empty());
    }
}

#[cfg(test)]
mod palette_compatibility_tests {
    use super::*;
    #[test]
    fn every_vulva_palette_item_is_compatible_including_anal_junction() {
        let vulva=PRESETS.iter().position(|p| p.region==m53_domain::protocol::Region::Vulva).unwrap();
        let anal=PRESETS.iter().position(|p| p.id=="vulva.magnification.analjunction").unwrap();
        let mut photo=Photo::new(1000,800);photo.add_callout(vulva,0.2,0.2);
        for (ix,p) in PRESETS.iter().enumerate().filter(|(_,p)| p.region!=m53_domain::protocol::Region::Cervix) {
            assert!(defects::allows_marker(photo.annotations.iter(),ix),"{}",p.id);
        }
        photo.add_callout(anal,0.6,0.6);
        assert!(defects::allows_marker(photo.annotations.iter(),vulva));
        assert!(!defects::allows_marker(photo.annotations.iter(),0));
        let mut cervical=Photo::new(1000,800);cervical.add_callout(0,0.3,0.3);
        assert!(!defects::allows_marker(cervical.annotations.iter(),anal));
        assert_eq!(PRESETS[anal].region,m53_domain::protocol::Region::Anus);
    }
}
