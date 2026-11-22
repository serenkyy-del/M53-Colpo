#[path="../src/verification.rs"] mod fixture;
use m53_gpui_editor::{Photo,Session,integration};use serde_json::json;use uuid::Uuid;
#[test]fn selection_and_tombstones_survive_save_and_reopen(){
 let photos=(0..3).map(|_|Photo::new(100,80)).collect::<Vec<_>>();let ids=photos.iter().map(|p|p.id).collect::<Vec<_>>();let mut session=Session{photos,selected:1};let original=json!(session);
 assert_eq!(session.remove_photos(&[ids[1]]),1);assert_eq!(session.visible_count(),2);assert_eq!(session.selected,2);assert!(!session.photos[1].in_report);
 let mut restored:Session=serde_json::from_value(json!(session)).unwrap();restored.ensure_visible_selection();assert_eq!(restored.visible_count(),2);assert!(restored.photos[1].removed);
 assert_eq!(restored.remove_photos(&ids),2);assert_eq!(restored.visible_count(),0);assert_eq!(restored.selected,0);assert_eq!(restored.remove_photos(&[Uuid::new_v4()]),0);
 let mut old=original;for photo in old["photos"].as_array_mut().unwrap(){photo.as_object_mut().unwrap().remove("removed");}let old:Session=serde_json::from_value(old).unwrap();assert_eq!(old.visible_count(),3);
}
#[test]fn removal_clears_linked_marks_and_filters_report_without_erasing_original(){
 let(detail,dictionary,asset)=fixture::fixture();let mut photo=Photo::new(1000,800);photo.id=asset;photo.add_callout(7,0.2,0.3);let mut session=Session{photos:vec![photo],selected:0};
 let marked=integration::apply(&detail,&dictionary,&session).unwrap();assert!(!marked.annotations.is_empty());let before=json!(marked);session.remove_photos(&[asset]);
 let removed=integration::apply(&marked,&dictionary,&session).unwrap();assert!(removed.annotations.is_empty());assert_eq!(removed.assets.len(),detail.assets.len());assert_eq!(json!(marked),before);
 let journal=json!({"native_editor":session});let visible=integration::visible_detail(&removed,&journal);assert!(visible.assets.iter().all(|a|a.id!=asset));assert!(visible.annotations.is_empty());
 let restored:Session=serde_json::from_value(journal["native_editor"].clone()).unwrap();assert_eq!(restored.visible_count(),0);
}
#[test]fn approved_study_rejects_removal_but_keeps_previously_removed_photos_hidden(){
 let(mut detail,dictionary,asset)=fixture::fixture();detail.study.status="approved".into();let mut photo=Photo::new(1000,800);photo.id=asset;let mut session=Session{photos:vec![photo],selected:0};session.remove_photos(&[asset]);
 assert!(integration::apply(&detail,&dictionary,&session).is_err());
 let mut workspace=m53_client::workspace::Workspace::default();workspace.detail=Some(detail.clone());workspace.dictionary=Some(dictionary);workspace.journal=json!({"detail":detail});
 assert!(integration::checkpoint(&mut workspace,detail.study.id,&session).is_err());
 workspace.journal["native_editor"]=json!(session);assert!(integration::checkpoint(&mut workspace,detail.study.id,&session).is_ok());
}
#[test]fn removal_invalidates_completion_even_when_photo_was_not_in_report(){
 let(mut detail,_,asset)=fixture::fixture();detail.study.status="draft".into();let mut photo=Photo::new(100,80);photo.id=asset;photo.in_report=false;let mut session=Session{photos:vec![photo],selected:0};let mut journal=json!({"selected_photos":[],"native_editor":session});
 m53_client::care_path::complete(&detail,&mut journal,m53_client::care_path::Action::FinishExamination).unwrap();
 assert!(m53_client::care_path::status(&detail,&journal).examination_complete);session.remove_photos(&[asset]);journal["native_editor"]=json!(session);assert!(!m53_client::care_path::status(&detail,&journal).examination_complete);
}
