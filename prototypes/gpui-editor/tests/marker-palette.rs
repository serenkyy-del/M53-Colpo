use m53_contracts::workflow::AnnotationLayer;
use m53_domain::colpo::{PRESETS, marker_color};
use m53_gpui_editor::{Photo, Session};

#[test]
fn two_point_geometry_roundtrips_and_tip_moves_independently() {
    let mut photo = Photo::new(1600, 900);
    let id = photo.place_callout(7, (0.32, 0.63), (0.81, 0.14));
    let a = photo.annotations.iter().find(|a| a.id == id).unwrap();
    assert_eq!((a.x, a.y), (0.81, 0.14));
    assert!(
        matches!(a.layer, AnnotationLayer::Callout{end_x,end_y,..} if (end_x,end_y)==(0.32,0.63))
    );
    let json = serde_json::to_vec(&Session {
        photos: vec![photo],
        selected: 0,
    })
    .unwrap();
    let mut restored: Session = serde_json::from_slice(&json).unwrap();
    restored.photos[0].edit(id, m53_client::defects::Handle::Body, 0.7, 0.2);
    let a = &restored.photos[0].annotations[0];
    assert!(
        matches!(a.layer, AnnotationLayer::Callout{end_x,end_y,..} if (end_x,end_y)==(0.32,0.63))
    );
}

#[test]
fn exported_marker_uses_same_catalog_color_as_palette_and_native_canvas() {
    for (preset, expected) in [
        (0, 0x7e22ce),
        (2, 0x15803d),
        (7, 0xdc2626),
        (12, 0xdc2626),
        (14, 0xc66a06),
    ] {
        let p = &PRESETS[preset];
        assert_eq!(p.marker_color(), expected);
        assert_eq!(marker_color(p.short, p.name, p.code), expected);
        let mut photo = Photo::new(400, 300);
        photo.place_callout(preset, (0.3, 0.6), (0.8, 0.2));
        let original = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            400,
            300,
            image::Rgb([233, 233, 233]),
        ));
        let mut encoded = std::io::Cursor::new(Vec::new());
        original
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        let decoded = m53_client::imaging::decode(&encoded.into_inner()).unwrap();
        let exported = m53_client::imaging::annotated(&decoded, &photo.annotations).to_rgb8();
        let color = [
            (expected >> 16) as u8,
            (expected >> 8) as u8,
            expected as u8,
        ];
        // The mid-shaft is outside the badge and arrowhead, proving arrow ink
        // follows the group color as well as the ring.
        assert_eq!(exported.get_pixel(220, 120).0, color);
        assert_eq!(original.to_rgb8().get_pixel(220, 120).0, [233, 233, 233]);
    }
}

#[test]
fn legend_covers_all_presets_and_distinguishes_same_short_label() {
    for p in PRESETS {
        assert!(p.marker_group() < m53_domain::colpo::MARKER_GROUPS.len());
        assert_eq!(marker_color(p.short, p.name, p.code), p.marker_color());
    }
    assert_eq!(PRESETS[6].marker_color(), 0x15803d);
    assert_eq!(PRESETS[11].marker_color(), 0xdc2626);
}
