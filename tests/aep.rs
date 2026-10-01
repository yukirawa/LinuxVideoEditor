use uuid::Uuid;
use yuki_video_editor::domain::Project;
use yuki_video_editor::protocol::{AepDocument, AepOperation, validate_aep};

#[test]
fn duplicate_aep_operations_are_rejected() {
    let project = Project::new("test");
    let operation_id = Uuid::new_v4();
    let missing_clip = Uuid::new_v4();
    let document = AepDocument {
        protocol: "AEP".into(),
        version: "1.0".into(),
        project_id: project.id,
        operations: vec![
            AepOperation::DeleteClip {
                operation_id,
                clip_id: missing_clip,
                reason: None,
                confidence: Some(0.8),
            },
            AepOperation::DeleteClip {
                operation_id,
                clip_id: missing_clip,
                reason: None,
                confidence: Some(0.8),
            },
        ],
    };

    assert!(validate_aep(&project, &document).is_err());
}
