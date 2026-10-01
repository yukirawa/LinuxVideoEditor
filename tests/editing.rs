use std::path::PathBuf;
use uuid::Uuid;
use yuki_video_editor::domain::{
    Clip, Layer, MediaAsset, MediaMetadata, Project, SequenceSettings, ShapeItem, ShapeKind,
    SpeechItem, TextItem, TextTemplate, TimelineItem, TimelineTime,
};
use yuki_video_editor::editing::{EditCommand, EditError, EditHistory};

fn asset(duration: f64) -> MediaAsset {
    MediaAsset {
        id: Uuid::new_v4(),
        name: "sample.mp4".into(),
        path: PathBuf::from("sample.mp4"),
        metadata: MediaMetadata {
            duration: TimelineTime::from_seconds(duration),
            width: 1920,
            height: 1080,
            frame_rate: 30.0,
            has_video: true,
            has_audio: true,
            video_codec: Some("h264".into()),
            audio_codec: Some("aac".into()),
            is_image: false,
        },
    }
}

fn clips_of(project: &Project, layer_index: usize) -> Vec<&TimelineItem> {
    project.sequence.layers[layer_index]
        .items
        .iter()
        .filter(|item| item.as_clip().is_some())
        .collect()
}

#[test]
fn split_is_non_destructive_and_undoable() {
    let mut project = Project::new("test");
    let layer_id = project.sequence.layers[0].id;
    let media = asset(10.0);
    let mut history = EditHistory::default();
    history
        .execute(
            &mut project,
            EditCommand::ImportAsset {
                asset: media,
                layer_id,
                timeline_start: TimelineTime::ZERO,
            },
        )
        .unwrap();
    let clip_id = clips_of(&project, 0)[0].id();
    history
        .execute(
            &mut project,
            EditCommand::SplitClip {
                clip_id,
                timeline_time: TimelineTime::from_seconds(4.0),
            },
        )
        .unwrap();

    assert_eq!(clips_of(&project, 0).len(), 2);
    let durations: Vec<f64> = clips_of(&project, 0)
        .iter()
        .map(|item| item.duration().as_seconds())
        .collect();
    assert_eq!(durations, vec![4.0, 6.0]);
    assert!(history.undo(&mut project));
    assert_eq!(clips_of(&project, 0).len(), 1);
}

#[test]
fn overlapping_move_is_rejected_transactionally() {
    let mut project = Project::new("test");
    let layer_id = project.sequence.layers[0].id;
    let mut history = EditHistory::default();
    history
        .execute(
            &mut project,
            EditCommand::ImportAsset {
                asset: asset(5.0),
                layer_id,
                timeline_start: TimelineTime::ZERO,
            },
        )
        .unwrap();
    history
        .execute(
            &mut project,
            EditCommand::ImportAsset {
                asset: asset(5.0),
                layer_id,
                timeline_start: TimelineTime::from_seconds(5.0),
            },
        )
        .unwrap();
    let second = clips_of(&project, 0)[1].id();
    let result = history.execute(
        &mut project,
        EditCommand::MoveClip {
            clip_id: second,
            timeline_start: TimelineTime::from_seconds(3.0),
        },
    );

    assert!(result.is_err());
    assert_eq!(clips_of(&project, 0)[1].timeline_start().as_seconds(), 5.0);
}

#[test]
fn sequence_settings_are_validated_and_undoable() {
    let mut project = Project::new("test");
    let original = project.sequence.settings.clone();
    assert_eq!(
        original,
        SequenceSettings {
            width: 1920,
            height: 1080,
            frame_rate: 30.0,
            sample_rate: 48_000,
        }
    );

    let changed = SequenceSettings {
        width: 3840,
        height: 2160,
        frame_rate: 60.0,
        sample_rate: 96_000,
    };
    let mut history = EditHistory::default();
    history
        .execute(
            &mut project,
            EditCommand::SetSequenceSettings {
                settings: changed.clone(),
            },
        )
        .unwrap();
    assert_eq!(project.sequence.settings, changed);
    assert!(history.undo(&mut project));
    assert_eq!(project.sequence.settings, original);

    let invalid_settings = [
        SequenceSettings {
            width: 15,
            ..original.clone()
        },
        SequenceSettings {
            frame_rate: 241.0,
            ..original.clone()
        },
        SequenceSettings {
            sample_rate: 0,
            ..original.clone()
        },
    ];
    for settings in invalid_settings {
        assert!(
            history
                .execute(&mut project, EditCommand::SetSequenceSettings { settings })
                .is_err()
        );
        assert_eq!(project.sequence.settings, original);
    }
}

#[test]
fn text_commands_are_validated_undoable_and_extend_duration() {
    let mut project = Project::new("test");
    let layer_id = project.sequence.layers[0].id;
    let mut history = EditHistory::default();
    let text = TextItem::new("Title", TimelineTime::from_seconds(12.0), (1920.0, 1080.0));
    let mut text = text;
    text.duration = TimelineTime::from_seconds(3.0);

    history
        .execute(
            &mut project,
            EditCommand::AddText {
                layer_id,
                text: text.clone(),
            },
        )
        .unwrap();
    assert_eq!(project.text(text.id).unwrap(), &text);
    assert_eq!(project.duration(), TimelineTime::from_seconds(15.0));

    let mut updated = text.clone();
    updated.text = "Updated title".into();
    updated.transform.x = 100.0;
    history
        .execute(
            &mut project,
            EditCommand::UpdateText {
                text: updated.clone(),
            },
        )
        .unwrap();
    assert_eq!(project.text(text.id).unwrap(), &updated);
    assert!(history.undo(&mut project));
    assert_eq!(project.text(text.id).unwrap(), &text);

    history
        .execute(&mut project, EditCommand::DeleteText { text_id: text.id })
        .unwrap();
    assert!(project.text(text.id).is_none());
    assert!(history.undo(&mut project));
    assert_eq!(project.text(text.id).unwrap(), &text);
}

#[test]
fn invalid_text_is_rejected_transactionally() {
    let mut project = Project::new("test");
    let layer_id = project.sequence.layers[0].id;
    let mut history = EditHistory::default();
    let mut invalid = TextItem::new("Invalid", TimelineTime::ZERO, (1920.0, 1080.0));
    invalid.duration = TimelineTime::ZERO;
    invalid.font_size = 0.0;

    assert!(
        history
            .execute(
                &mut project,
                EditCommand::AddText {
                    layer_id,
                    text: invalid,
                },
            )
            .is_err()
    );
    assert!(
        project
            .sequence
            .layers
            .iter()
            .all(|layer| layer.items.is_empty())
    );
}

#[test]
fn shape_commands_are_validated_undoable() {
    let mut project = Project::new("test");
    let layer_id = project.sequence.layers[0].id;
    let mut history = EditHistory::default();
    let shape = ShapeItem::new(
        ShapeKind::Rectangle,
        TimelineTime::from_seconds(2.0),
        (1920.0, 1080.0),
    );

    history
        .execute(
            &mut project,
            EditCommand::AddShape {
                layer_id,
                shape: shape.clone(),
            },
        )
        .unwrap();
    assert!(project.item(shape.id).unwrap().as_shape().is_some());

    let mut updated = shape.clone();
    updated.kind = ShapeKind::Ellipse;
    updated.opacity = 50.0;
    updated.width = 640.0;
    history
        .execute(
            &mut project,
            EditCommand::UpdateShape {
                shape: updated.clone(),
            },
        )
        .unwrap();
    assert_eq!(
        project.item(shape.id).unwrap().as_shape().unwrap().kind,
        ShapeKind::Ellipse
    );
    assert_eq!(
        project.item(shape.id).unwrap().as_shape().unwrap().opacity,
        50.0
    );

    assert!(history.undo(&mut project));
    assert_eq!(
        project.item(shape.id).unwrap().as_shape().unwrap().kind,
        ShapeKind::Rectangle
    );

    let mut invalid = shape.clone();
    invalid.opacity = 200.0;
    assert!(
        history
            .execute(&mut project, EditCommand::UpdateShape { shape: invalid },)
            .is_err()
    );
}

#[test]
fn text_template_stamps_style_and_content() {
    let mut template = TextTemplate::new("subtitle");
    template.font_size = 96.0;
    template.color_rgba = [255, 0, 0, 255];

    let text = template.build_text(
        "こんにちは",
        TimelineTime::ZERO,
        TimelineTime::from_seconds(2.0),
    );
    assert_eq!(text.text, "こんにちは");
    assert_eq!(text.font_size, 96.0);
    assert_eq!(text.color_rgba, [255, 0, 0, 255]);
    assert_eq!(text.duration, TimelineTime::from_seconds(2.0));
}

#[test]
fn voicevox_speech_is_one_item_with_audio_and_subtitle() {
    // The VOICEVOX flow produces a single speech item that carries both the synthesized audio
    // and its subtitle, so the two can no longer drift apart on the timeline.
    let mut project = Project::new("test");
    let mut history = EditHistory::default();
    let frame = project.sequence.settings.frame_size();
    let duration = TimelineTime::from_seconds(2.5);

    let audio = MediaAsset {
        id: Uuid::new_v4(),
        name: "voice".into(),
        path: PathBuf::from("voice.wav"),
        metadata: MediaMetadata {
            duration,
            width: 0,
            height: 0,
            frame_rate: 0.0,
            has_video: false,
            has_audio: true,
            video_codec: None,
            audio_codec: Some("pcm_s16le".into()),
            is_image: false,
        },
    };
    let layer_id = project.sequence.layers[0].id;

    let audio_start = TimelineTime::from_seconds(1.0);
    let clip = Clip::new(&audio, audio_start, frame);
    let mut subtitle = TextItem::new("こんにちは", audio_start, frame);
    subtitle.duration = duration;
    let speech = SpeechItem::new(clip, subtitle);
    let speech_id = speech.id();
    history
        .execute(
            &mut project,
            EditCommand::AddSpeech {
                asset: audio,
                layer_id,
                speech,
            },
        )
        .unwrap();

    // Exactly one item was added, and it is a speech.
    assert_eq!(project.sequence.layers[0].items.len(), 1);
    let stored = project.item(speech_id).expect("speech must exist");
    let stored_speech = stored.as_speech().expect("item must be a speech");
    assert_eq!(stored_speech.duration(), duration);
    assert_eq!(stored_speech.timeline_start(), audio_start);
    // The subtitle is still reachable as text (preview/export/font loading rely on this) ...
    assert_eq!(
        project.text(speech_id).map(|text| text.text.as_str()),
        Some("こんにちは")
    );
    // ... and the synthesized audio is reachable as a clip (export/audio preview rely on this).
    assert!(stored.audio_clip().is_some());

    // Moving the speech moves its subtitle with it.
    history
        .execute(
            &mut project,
            EditCommand::MoveItem {
                item_id: speech_id,
                target_layer_id: layer_id,
                timeline_start: TimelineTime::from_seconds(4.0),
            },
        )
        .unwrap();
    let moved = project
        .item(speech_id)
        .and_then(TimelineItem::as_speech)
        .expect("speech must still exist");
    assert_eq!(moved.timeline_start(), TimelineTime::from_seconds(4.0));
    assert_eq!(
        moved.subtitle.timeline_start,
        TimelineTime::from_seconds(4.0)
    );
    assert_eq!(moved.subtitle.duration, duration);
}

#[test]
fn speech_round_trips_through_json() {
    // A speech is an internally tagged `TimelineItem` variant that nests a clip and a text item;
    // make sure the project still serializes and parses back as one speech item.
    let mut project = Project::new("json");
    let frame = project.sequence.settings.frame_size();
    let duration = TimelineTime::from_seconds(1.5);
    let asset = MediaAsset {
        id: Uuid::new_v4(),
        name: "voice".into(),
        path: PathBuf::from("voice.wav"),
        metadata: MediaMetadata {
            duration,
            width: 0,
            height: 0,
            frame_rate: 0.0,
            has_video: false,
            has_audio: true,
            video_codec: None,
            audio_codec: Some("pcm_s16le".into()),
            is_image: false,
        },
    };
    let layer_id = project.sequence.layers[0].id;
    let clip = Clip::new(&asset, TimelineTime::ZERO, frame);
    let mut subtitle = TextItem::new("セリフ", TimelineTime::ZERO, frame);
    subtitle.duration = duration;
    let speech = SpeechItem::new(clip, subtitle);
    let speech_id = speech.id();
    let mut history = EditHistory::default();
    history
        .execute(
            &mut project,
            EditCommand::AddSpeech {
                asset,
                layer_id,
                speech,
            },
        )
        .unwrap();

    let json = serde_json::to_string(&project).unwrap();
    let restored: Project = serde_json::from_str(&json).unwrap();
    let restored_speech = restored
        .item(speech_id)
        .and_then(TimelineItem::as_speech)
        .expect("speech must survive a JSON round trip");
    assert_eq!(restored_speech.subtitle.text, "セリフ");
    assert_eq!(
        restored_speech.subtitle.timeline_start,
        restored_speech.timeline_start()
    );
    assert_eq!(restored_speech.subtitle.duration, duration);
    assert!(
        restored
            .item(speech_id)
            .is_some_and(|item| item.audio_clip().is_some())
    );
}

#[test]
fn speech_split_cuts_audio_and_subtitle_together() {
    let mut project = Project::new("test");
    let mut history = EditHistory::default();
    let frame = project.sequence.settings.frame_size();
    let audio = MediaAsset {
        id: Uuid::new_v4(),
        name: "voice".into(),
        path: PathBuf::from("voice.wav"),
        metadata: MediaMetadata {
            duration: TimelineTime::from_seconds(2.0),
            width: 0,
            height: 0,
            frame_rate: 0.0,
            has_video: false,
            has_audio: true,
            video_codec: None,
            audio_codec: Some("pcm_s16le".into()),
            is_image: false,
        },
    };
    let layer_id = project.sequence.layers[0].id;
    let start = TimelineTime::from_seconds(1.0);
    let clip = Clip::new(&audio, start, frame);
    let mut subtitle = TextItem::new("長いセリフ", start, frame);
    subtitle.duration = audio.metadata.duration;
    let speech = SpeechItem::new(clip, subtitle);
    let speech_id = speech.id();
    history
        .execute(
            &mut project,
            EditCommand::AddSpeech {
                asset: audio,
                layer_id,
                speech,
            },
        )
        .unwrap();

    let split_at = TimelineTime::from_seconds(1.8);
    history
        .execute(
            &mut project,
            EditCommand::SplitSpeech {
                speech_id,
                timeline_time: split_at,
            },
        )
        .unwrap();

    let speeches: Vec<&SpeechItem> = project.sequence.layers[0]
        .items
        .iter()
        .filter_map(TimelineItem::as_speech)
        .collect();
    assert_eq!(speeches.len(), 2, "a split must yield two speech items");
    let (left, right) = (speeches[0], speeches[1]);
    assert_eq!(left.timeline_start(), start);
    assert_eq!(left.duration(), TimelineTime::from_seconds(0.8));
    assert_eq!(left.subtitle.duration, TimelineTime::from_seconds(0.8));
    assert_eq!(right.timeline_start(), split_at);
    assert_eq!(right.duration(), TimelineTime::from_seconds(1.2));
    assert_eq!(right.subtitle.duration, TimelineTime::from_seconds(1.2));
    // Both halves carry the line's text and its own id.
    assert_eq!(left.subtitle.text, "長いセリフ");
    assert_eq!(right.subtitle.text, "長いセリフ");
    assert_ne!(left.id(), right.id());

    // The split is a normal undoable edit.
    assert!(history.undo(&mut project));
    assert_eq!(project.sequence.layers[0].items.len(), 1);
    assert!(project.item(speech_id).is_some());
}

#[test]
fn default_template_position_is_inside_the_frame() {
    let template = TextTemplate::new("t");
    let text = template.build_text("x", TimelineTime::ZERO, TimelineTime::from_seconds(1.0));
    // A default template must not sit on the canvas origin, where centered text would be
    // half outside the frame and look like it was never added.
    assert!(text.transform.x > 0.0 && text.transform.y > 0.0);
    assert!(text.transform.x < 1920.0 && text.transform.y < 1080.0);
}

#[test]
fn template_can_be_derived_from_text() {
    let text = TextItem::new("x", TimelineTime::ZERO, (1920.0, 1080.0));
    let template = TextTemplate::from_text("t", &text);
    let rebuilt = template.build_text(
        "y",
        TimelineTime::from_seconds(1.0),
        TimelineTime::from_seconds(4.0),
    );
    assert_eq!(rebuilt.font_size, text.font_size);
    assert_eq!(rebuilt.color_rgba, text.color_rgba);
    assert_eq!(rebuilt.transform, text.transform);
}

#[test]
fn text_style_fields_have_forward_compatible_defaults() {
    let mut project = Project::new("legacy");
    let layer_id = project.sequence.layers[0].id;
    let text = TextItem::new("Hello", TimelineTime::ZERO, (1920.0, 1080.0));
    let mut history = EditHistory::default();
    history
        .execute(
            &mut project,
            EditCommand::AddText {
                layer_id,
                text: text.clone(),
            },
        )
        .unwrap();

    let mut json = serde_json::to_value(&project).unwrap();
    // Simulate an older document that predates style fields with `#[serde(default)]`.
    let text_json = json["sequence"]["layers"][0]["items"][0]
        .as_object_mut()
        .unwrap();
    text_json.remove("bold");
    text_json.remove("outline_enabled");
    text_json.remove("shadow_enabled");
    text_json.remove("background_enabled");

    let loaded: Project = serde_json::from_value(json).unwrap();
    let loaded_text = loaded.text(text.id).unwrap();
    assert!(!loaded_text.bold);
    assert!(!loaded_text.outline_enabled);
    assert!(!loaded_text.shadow_enabled);
    assert!(!loaded_text.background_enabled);
}

fn first_clip_id(project: &Project) -> uuid::Uuid {
    clips_of(project, 0)[0].id()
}

#[test]
fn speed_changes_timeline_duration_and_source_mapping() {
    let mut project = Project::new("speed");
    let layer_id = project.sequence.layers[0].id;
    let mut history = EditHistory::default();
    history
        .execute(
            &mut project,
            EditCommand::ImportAsset {
                asset: asset(10.0),
                layer_id,
                timeline_start: TimelineTime::ZERO,
            },
        )
        .unwrap();
    let clip_id = first_clip_id(&project);
    history
        .execute(
            &mut project,
            EditCommand::SetClipSpeed {
                clip_id,
                speed: 2.0,
            },
        )
        .unwrap();

    let clip = project.clip(clip_id).unwrap();
    assert!((clip.duration().as_seconds() - 5.0).abs() < 1e-6);
    // 2 seconds on the timeline maps to 4 seconds in the source at 2× speed.
    let source = clip.source_time_at(TimelineTime::from_seconds(2.0));
    assert!((source.as_seconds() - 4.0).abs() < 1e-6);
}

#[test]
fn split_at_speed_keeps_source_contiguous_and_inherits_speed() {
    let mut project = Project::new("speed-split");
    let layer_id = project.sequence.layers[0].id;
    let mut history = EditHistory::default();
    history
        .execute(
            &mut project,
            EditCommand::ImportAsset {
                asset: asset(10.0),
                layer_id,
                timeline_start: TimelineTime::ZERO,
            },
        )
        .unwrap();
    let clip_id = first_clip_id(&project);
    history
        .execute(
            &mut project,
            EditCommand::SetClipSpeed {
                clip_id,
                speed: 2.0,
            },
        )
        .unwrap();
    // Timeline is 5s long at 2×; split at 2s → source halves 0-4 and 4-8.
    history
        .execute(
            &mut project,
            EditCommand::SplitClip {
                clip_id,
                timeline_time: TimelineTime::from_seconds(2.0),
            },
        )
        .unwrap();

    let items = clips_of(&project, 0);
    assert_eq!(items.len(), 2);
    let left = items[0].as_clip().unwrap();
    let right = items[1].as_clip().unwrap();
    assert!((left.duration().as_seconds() - 2.0).abs() < 1e-6);
    assert!((right.duration().as_seconds() - 3.0).abs() < 1e-6);
    assert!((left.source_out.as_seconds() - 4.0).abs() < 1e-6);
    assert!((right.source_in.as_seconds() - 4.0).abs() < 1e-6);
    assert!((left.speed() - 2.0).abs() < 1e-6);
    assert!((right.speed() - 2.0).abs() < 1e-6);
}

#[test]
fn resize_at_speed_maps_timeline_delta_to_source() {
    let mut project = Project::new("speed-resize");
    let layer_id = project.sequence.layers[0].id;
    let mut history = EditHistory::default();
    history
        .execute(
            &mut project,
            EditCommand::ImportAsset {
                asset: asset(10.0),
                layer_id,
                timeline_start: TimelineTime::ZERO,
            },
        )
        .unwrap();
    let clip_id = first_clip_id(&project);
    history
        .execute(
            &mut project,
            EditCommand::SetClipSpeed {
                clip_id,
                speed: 2.0,
            },
        )
        .unwrap();
    // Shrink the timeline end by 1s → source_out moves by 2s at 2× speed.
    history
        .execute(
            &mut project,
            EditCommand::ResizeItemEnd {
                item_id: clip_id,
                timeline_end: TimelineTime::from_seconds(4.0),
            },
        )
        .unwrap();
    let clip = project.clip(clip_id).unwrap();
    assert!((clip.duration().as_seconds() - 4.0).abs() < 1e-6);
    assert!((clip.source_out.as_seconds() - 8.0).abs() < 1e-6);
}

#[test]
fn invalid_speed_is_rejected() {
    let mut project = Project::new("speed-invalid");
    let layer_id = project.sequence.layers[0].id;
    let mut history = EditHistory::default();
    history
        .execute(
            &mut project,
            EditCommand::ImportAsset {
                asset: asset(4.0),
                layer_id,
                timeline_start: TimelineTime::ZERO,
            },
        )
        .unwrap();
    let clip_id = first_clip_id(&project);
    assert!(history
        .execute(
            &mut project,
            EditCommand::SetClipSpeed {
                clip_id,
                speed: 0.0,
            },
        )
        .is_err());
    assert!(history
        .execute(
            &mut project,
            EditCommand::SetClipSpeed {
                clip_id,
                speed: 100.0,
            },
        )
        .is_err());
}

#[test]
fn effects_and_rotation_round_trip_and_default_on_old_json() {
    use yuki_video_editor::domain::{AnimProperty, ClipEffects, Easing, Keyframe};

    let mut project = Project::new("effects");
    let layer_id = project.sequence.layers[0].id;
    let mut history = EditHistory::default();
    history
        .execute(
            &mut project,
            EditCommand::ImportAsset {
                asset: asset(4.0),
                layer_id,
                timeline_start: TimelineTime::ZERO,
            },
        )
        .unwrap();
    let clip_id = first_clip_id(&project);
    let mut transform = project.clip(clip_id).unwrap().transform;
    transform.rotation_degrees = 45.0;
    history
        .execute(
            &mut project,
            EditCommand::SetClipTransform {
                clip_id,
                transform,
            },
        )
        .unwrap();
    history
        .execute(
            &mut project,
            EditCommand::SetClipEffects {
                clip_id,
                effects: ClipEffects {
                    brightness: 0.2,
                    contrast: 1.1,
                    saturation: 0.8,
                    hue_degrees: 15.0,
                    blur_radius: 2.0,
                    grayscale: false,
                    invert: false,
                },
            },
        )
        .unwrap();
    history
        .execute(
            &mut project,
            EditCommand::SetKeyframe {
                clip_id,
                property: AnimProperty::X,
                keyframe: Keyframe {
                    offset: TimelineTime::ZERO,
                    value: 100.0,
                    easing: Easing::Linear,
                },
            },
        )
        .unwrap();
    history
        .execute(
            &mut project,
            EditCommand::SetKeyframe {
                clip_id,
                property: AnimProperty::X,
                keyframe: Keyframe {
                    offset: TimelineTime::from_seconds(2.0),
                    value: 300.0,
                    easing: Easing::Linear,
                },
            },
        )
        .unwrap();

    let json = serde_json::to_value(&project).unwrap();
    let loaded: Project = serde_json::from_value(json).unwrap();
    let clip = loaded.clip(clip_id).unwrap();
    assert!((clip.transform.rotation_degrees - 45.0).abs() < 1e-6);
    assert!((clip.effects.brightness - 0.2).abs() < 1e-6);
    assert!((clip.effects.contrast - 1.1).abs() < 1e-6);
    let x = clip
        .sample_anim(AnimProperty::X, TimelineTime::from_seconds(1.0))
        .unwrap();
    assert!((x - 200.0).abs() < 1e-3);

    // Older documents without the new fields must still load with defaults.
    let mut legacy = serde_json::to_value(&project).unwrap();
    let clip_json = legacy["sequence"]["layers"][0]["items"][0]
        .as_object_mut()
        .unwrap();
    clip_json.remove("speed");
    clip_json.remove("effects");
    clip_json.remove("anim");
    let transform_json = clip_json["transform"].as_object_mut().unwrap();
    transform_json.remove("rotation_degrees");
    let loaded: Project = serde_json::from_value(legacy).unwrap();
    let clip = loaded.clip(clip_id).unwrap();
    assert!((clip.speed() - 1.0).abs() < 1e-6);
    assert!(clip.effects.is_default());
    assert!(clip.anim.is_empty());
    assert!((clip.transform.rotation_degrees).abs() < 1e-6);
}

#[test]
fn keyframe_split_rebases_right_offsets() {
    use yuki_video_editor::domain::{AnimProperty, Easing, Keyframe};

    let mut project = Project::new("anim-split");
    let layer_id = project.sequence.layers[0].id;
    let mut history = EditHistory::default();
    history
        .execute(
            &mut project,
            EditCommand::ImportAsset {
                asset: asset(10.0),
                layer_id,
                timeline_start: TimelineTime::ZERO,
            },
        )
        .unwrap();
    let clip_id = first_clip_id(&project);
    for (offset, value) in [(0.0, 0.0), (2.0, 100.0), (6.0, 200.0)] {
        history
            .execute(
                &mut project,
                EditCommand::SetKeyframe {
                    clip_id,
                    property: AnimProperty::Opacity,
                    keyframe: Keyframe {
                        offset: TimelineTime::from_seconds(offset),
                        value,
                        easing: Easing::Linear,
                    },
                },
            )
            .unwrap();
    }
    history
        .execute(
            &mut project,
            EditCommand::SplitClip {
                clip_id,
                timeline_time: TimelineTime::from_seconds(4.0),
            },
        )
        .unwrap();

    let items = clips_of(&project, 0);
    let left = items[0].as_clip().unwrap();
    let right = items[1].as_clip().unwrap();
    let left_keys = &left.track(AnimProperty::Opacity).unwrap().keys;
    let right_keys = &right.track(AnimProperty::Opacity).unwrap().keys;
    assert_eq!(left_keys.len(), 2);
    assert_eq!(right_keys.len(), 1);
    assert!((right_keys[0].offset.as_seconds() - 2.0).abs() < 1e-6);
}

#[test]
fn paste_duplicates_clip_at_playhead_with_new_id() {
    let mut project = Project::new("test");
    let layer_id = project.sequence.layers[0].id;
    let mut history = EditHistory::default();
    history
        .execute(
            &mut project,
            EditCommand::ImportAsset {
                asset: asset(10.0),
                layer_id,
                timeline_start: TimelineTime::ZERO,
            },
        )
        .unwrap();
    let original = clips_of(&project, 0)[0].clone();
    let mut pasted = original.detached_copy();
    pasted.set_timeline_start(TimelineTime::from_seconds(5.0));
    assert_ne!(pasted.id(), original.id());

    // The copy spans 5..15 and would overlap the 0..10 original on one layer, so it lands on
    // a second layer (what `resolve_target_layer` picks in the app).
    let second = Layer::new("レイヤー 2");
    let paste_layer_id = second.id;
    history
        .execute(
            &mut project,
            EditCommand::AddLayer {
                layer: second,
                index: Some(1),
            },
        )
        .unwrap();
    history
        .execute(
            &mut project,
            EditCommand::PasteItem {
                item: pasted.clone(),
                layer_id: paste_layer_id,
            },
        )
        .unwrap();

    let mut clips: Vec<&TimelineItem> = project
        .sequence
        .layers
        .iter()
        .flat_map(|layer| &layer.items)
        .filter(|item| item.as_clip().is_some())
        .collect();
    clips.sort_by_key(|item| item.timeline_start());
    assert_eq!(clips.len(), 2);
    assert_ne!(clips[0].id(), clips[1].id());
    assert_eq!(clips[0].duration().as_seconds(), 10.0);
    assert_eq!(clips[1].duration().as_seconds(), 10.0);
    assert_eq!(clips[0].timeline_start().as_seconds(), 0.0);
    assert_eq!(clips[1].timeline_start().as_seconds(), 5.0);
}

#[test]
fn paste_rejects_duplicate_id() {
    let mut project = Project::new("test");
    let layer_id = project.sequence.layers[0].id;
    let mut history = EditHistory::default();
    history
        .execute(
            &mut project,
            EditCommand::ImportAsset {
                asset: asset(5.0),
                layer_id,
                timeline_start: TimelineTime::ZERO,
            },
        )
        .unwrap();
    // A copy that kept the original's id must be rejected instead of shadowing it.
    let mut duplicate = clips_of(&project, 0)[0].clone();
    duplicate.set_timeline_start(TimelineTime::from_seconds(5.0));
    let result = history.execute(
        &mut project,
        EditCommand::PasteItem {
            item: duplicate,
            layer_id,
        },
    );
    assert!(matches!(result, Err(EditError::DuplicateItemId(_))));
    assert_eq!(clips_of(&project, 0).len(), 1);
}

#[test]
fn undo_after_paste_restores_single_item() {
    let mut project = Project::new("test");
    let layer_id = project.sequence.layers[0].id;
    let mut history = EditHistory::default();
    history
        .execute(
            &mut project,
            EditCommand::ImportAsset {
                asset: asset(5.0),
                layer_id,
                timeline_start: TimelineTime::ZERO,
            },
        )
        .unwrap();
    let mut pasted = clips_of(&project, 0)[0].clone().detached_copy();
    pasted.set_timeline_start(TimelineTime::from_seconds(5.0));
    let pasted_id = pasted.id();
    history
        .execute(
            &mut project,
            EditCommand::PasteItem {
                item: pasted,
                layer_id,
            },
        )
        .unwrap();
    assert_eq!(clips_of(&project, 0).len(), 2);
    assert!(project.item(pasted_id).is_some());

    assert!(history.undo(&mut project));
    assert_eq!(clips_of(&project, 0).len(), 1);
    assert!(project.item(pasted_id).is_none());
}
