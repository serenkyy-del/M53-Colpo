use image::{DynamicImage, Rgb, RgbImage};
use m53_client::imaging::*;
use m53_contracts::workflow::Annotation;
use uuid::Uuid;
#[test]
fn original_is_unchanged_by_annotation_and_rgb_matches() {
    let original = DynamicImage::ImageRgb8(RgbImage::from_pixel(20, 10, Rgb([1, 2, 3])));
    let before = png(&original).unwrap();
    let a = Annotation {
        layer: Default::default(),
        coordinate_system: Default::default(),
        id: Uuid::from_u128(1),
        asset_id: Uuid::from_u128(2),
        lesion_id: None,
        finding_id: None,
        x: 0.5,
        y: 0.5,
        radius: 0.2,
        label: "Synthetic".into(),
    };
    let edited = annotated(&original, &[a]);
    assert_ne!(png(&edited).unwrap(), before);
    assert_eq!(png(&original).unwrap(), before);
    let rgb = raw_rgb_pixels(&original);
    assert_eq!(&rgb[..8], b"M53RGB01");
    assert_eq!(rgb.len(), 16 + 20 * 10 * 3);
    assert_eq!(&rgb[16..19], &[1, 2, 3]);
}
#[test]
fn rejects_corrupt_image() {
    assert!(decode(b"\x89PNG\r\n\x1a\ncorrupt").is_err());
}
#[test]
fn target_and_output_region_are_metadata_and_only_crop_a_new_render_buffer() {
    use m53_contracts::workflow::AnnotationLayer;
    let original = DynamicImage::ImageRgb8(RgbImage::from_pixel(100, 80, Rgb([10, 20, 30])));
    let bytes = png(&original).unwrap();
    let target = Annotation {
        layer: AnnotationLayer::Target {
            rotation_degrees: 30.0,
            include_in_report: true,
        },
        coordinate_system: Default::default(),
        id: Uuid::new_v4(),
        asset_id: Uuid::new_v4(),
        lesion_id: None,
        finding_id: None,
        x: 0.5,
        y: 0.5,
        radius: 0.4,
        label: "Мишень".into(),
    };
    let crop = Annotation {
        layer: AnnotationLayer::OutputRegion {
            width: 0.5,
            height: 0.5,
        },
        x: 0.25,
        y: 0.25,
        id: Uuid::new_v4(),
        ..target.clone()
    };
    let layers = vec![target, crop];
    let persisted = serde_json::to_vec(&layers).unwrap();
    let restored: Vec<Annotation> = serde_json::from_slice(&persisted).unwrap();
    let output = annotated(&original, &restored);
    assert_eq!(dimensions(&output), (50, 40));
    assert_eq!(png(&original).unwrap(), bytes);
    assert!(output.to_rgb8().pixels().any(|p| p.0 == [255, 210, 0]));
    let mut invalid = restored[1].clone();
    invalid.x = 0.9;
    assert!(!invalid.valid_layer());
    let mut hidden = restored[0].clone();
    hidden.layer = AnnotationLayer::Target {
        rotation_degrees: 30.0,
        include_in_report: false,
    };
    assert_eq!(png(&annotated(&original, &[hidden])).unwrap(), bytes);
}
#[test]
fn applies_exif_orientation_before_display() {
    let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(3, 2, Rgb([10, 20, 30])));
    let mut encoded = std::io::Cursor::new(vec![]);
    image
        .write_to(&mut encoded, image::ImageOutputFormat::Jpeg(90))
        .unwrap();
    let jpeg = encoded.into_inner();
    let mut exif =
        b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0".to_vec();
    let mut oriented = vec![0xff, 0xd8, 0xff, 0xe1];
    oriented.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
    oriented.append(&mut exif);
    oriented.extend_from_slice(&jpeg[2..]);
    assert_eq!(dimensions(&decode(&oriented).unwrap()), (2, 3));
}

#[test]
fn single_report_photo_uses_full_row_and_the_selected_crop() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use m53_contracts::workflow::*;
    let id = Uuid::new_v4();
    let source = DynamicImage::ImageRgb8(RgbImage::from_pixel(100, 80, Rgb([10, 20, 30])));
    let bytes = png(&source).unwrap();
    let mut detail = StudyDetail {
        mis_task_bound: false,
        study: Study {
            id,
            patient_id: id,
            status: "draft".into(),
            row_version: 1,
            study_instance_uid: "2.25.1".into(),
        },
        patient: Patient { card_number: None,
            sex: None,
            id,
            external_id: None,
            family_name: "Тест".into(),
            given_name: "Тест".into(),
            patronymic: None,
            birth_date: None,
        },
        revision_id: Some(id),
        protocol: Some(m53_client::model::blank_protocol(id)),
        assets: vec![Asset {
            id,
            study_id: id,
            kind: "original".into(),
            sha256: {
                use sha2::Digest;
                hex::encode(sha2::Sha256::digest(&bytes))
            },
            byte_size: bytes.len() as i64,
            mime_type: "image/png".into(),
        }],
        annotations: vec![Annotation {
            id,
            asset_id: id,
            lesion_id: None,
            finding_id: None,
            coordinate_system: Default::default(),
            layer: AnnotationLayer::OutputRegion {
                width: 0.5,
                height: 0.5,
            },
            x: 0.25,
            y: 0.25,
            radius: 0.01,
            label: String::new(),
        }],
    };
    let layout = ReportLayout {
        title: "Тест".into(),
        photo_columns: 3, letterhead: None,
        blocks: vec![ReportBlock::Photos],
    };
    let rendered = m53_client::report::render(
        &detail,
        &layout,
        &[m53_client::report::Photo {
            id,
            bytes: bytes.clone(),
            caption: "Тест".into(),
        }],
        "Тест",
    )
    .unwrap();
    let html = std::str::from_utf8(&rendered.html).unwrap();
    assert!(html.contains("grid-template-columns:repeat(1,1fr)"));
    let encoded = html
        .split("base64,")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let embedded = decode(&STANDARD.decode(encoded).unwrap()).unwrap();
    assert_eq!(dimensions(&embedded), (50, 40));
    assert_eq!(png(&source).unwrap(), bytes);
    assert!(rendered.pdf.starts_with(b"%PDF-"));
    // Selected photos must be registered originals, as in the real study contract.
    let second = Uuid::new_v4();
    detail.assets.push(Asset {
        id: second,
        ..detail.assets[0].clone()
    });
    let photos = vec![
        m53_client::report::Photo {
            id,
            bytes: bytes.clone(),
            caption: "Снимок 1".into(),
        },
        m53_client::report::Photo {
            id: second,
            bytes: bytes.clone(),
            caption: "Снимок 2".into(),
        },
    ];
    let pages = m53_client::report::render(&detail, &layout, &photos, "Тест").unwrap();
    assert_eq!(
        printpdf::lopdf::Document::load_mem(&pages.pdf)
            .unwrap()
            .get_pages()
            .len(),
        3
    );
    assert_eq!(
        std::str::from_utf8(&pages.html)
            .unwrap()
            .matches("<figure class=\"photo-page\">")
            .count(),
        2
    );
    let preview =
        m53_client::report::preview_with_terms(&detail, &layout, &photos, "Тест", &[]).unwrap();
    assert!(preview.pdf.is_empty());
    assert!(
        std::str::from_utf8(&preview.html)
            .unwrap()
            .contains("data:image/jpeg;base64,")
    );
}

#[test]
fn arrow_metadata_moves_without_distortion_and_renders_without_changing_original() {
    use m53_contracts::workflow::AnnotationLayer;
    let original = DynamicImage::ImageRgb8(RgbImage::from_pixel(100, 80, Rgb([10, 20, 30])));
    let bytes = png(&original).unwrap();
    let mut a = Annotation {
        id: Uuid::new_v4(),
        asset_id: Uuid::new_v4(),
        lesion_id: None,
        finding_id: None,
        coordinate_system: Default::default(),
        layer: AnnotationLayer::Arrow {
            end_x: 0.8,
            end_y: 0.7,
        },
        x: 0.3,
        y: 0.2,
        radius: 0.05,
        label: "Тест".into(),
    };
    move_layer(&mut a, 1.0, 1.0);
    if let AnnotationLayer::Arrow { end_x, end_y } = a.layer {
        assert!((end_x - a.x - 0.5).abs() < 0.0001);
        assert!((end_y - a.y - 0.5).abs() < 0.0001);
        assert!(end_x <= 1.0 && end_y <= 1.0);
    } else {
        panic!("expected arrow")
    }
    assert!(a.valid_layer());
    let restored: Annotation = serde_json::from_slice(&serde_json::to_vec(&a).unwrap()).unwrap();
    assert_ne!(png(&annotated(&original, &[restored])).unwrap(), bytes);
    assert_eq!(png(&original).unwrap(), bytes);
    a.layer = AnnotationLayer::Arrow {
        end_x: f32::NAN,
        end_y: 0.5,
    };
    assert!(!a.valid_layer());
}
