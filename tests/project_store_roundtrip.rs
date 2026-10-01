use std::fs;
use std::path::PathBuf;
use uuid::Uuid;
use yuki_video_editor::domain::{
    Layer, MediaAsset, MediaMetadata, PROJECT_SCHEMA_VERSION, Project, ShapeItem, ShapeKind,
    TextItem, TimelineItem, TimelineTime,
};
use yuki_video_editor::editing::{EditCommand, EditHistory};
use yuki_video_editor::project_store;

fn temp_directory(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("yuki-{name}-{}", Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    directory
}

#[test]
fn project_round_trips_through_disk() {
    let mut project = Project::new("round-trip");
    project.sequence.settings.width = 640;
    project.sequence.settings.height = 360;
    project.sequence.settings.frame_rate = 30.0;

    let asset = MediaAsset {
        id: Uuid::new_v4(),
        name: "clip.mp4".into(),
        path: PathBuf::from("/tmp/clip.mp4"),
        metadata: MediaMetadata {
            duration: TimelineTime::from_seconds(10.0),
            width: 640,
            height: 360,
            frame_rate: 30.0,
            has_video: true,
            has_audio: true,
            video_codec: Some("h264".into()),
            audio_codec: Some("aac".into()),
            is_image: false,
        },
    };

    let mut history = EditHistory::default();
    let video_layer = project.sequence.layers[0].id;
    history
        .execute(
            &mut project,
            EditCommand::ImportAsset {
                asset,
                layer_id: video_layer,
                timeline_start: TimelineTime::ZERO,
            },
        )
        .expect("import asset");

    let mut text_layer = Layer::new("テキスト");
    text_layer.id = Uuid::new_v4();
    let text_layer_id = text_layer.id;
    history
        .execute(
            &mut project,
            EditCommand::AddLayer {
                layer: text_layer,
                index: Some(0),
            },
        )
        .expect("add layer");

    let mut text = TextItem::new(
        "こんにちは",
        TimelineTime::from_seconds(2.0),
        (640.0, 360.0),
    );
    text.transform.anchor = yuki_video_editor::domain::Anchor::BottomRight;
    text.transform.x = 600.0;
    history
        .execute(
            &mut project,
            EditCommand::AddText {
                layer_id: text_layer_id,
                text,
            },
        )
        .expect("add text");

    history
        .execute(
            &mut project,
            EditCommand::AddShape {
                layer_id: text_layer_id,
                shape: ShapeItem::new(
                    ShapeKind::Ellipse,
                    TimelineTime::from_seconds(5.0),
                    (640.0, 360.0),
                ),
            },
        )
        .expect("add shape");

    let directory = temp_directory("proj-roundtrip");
    let path = directory.join("project.yukivproj");

    project_store::save(&project, &path).expect("save");
    let loaded = project_store::load(&path).expect("load saved project");

    assert_eq!(loaded.title, project.title);
    assert_eq!(loaded.schema_version, PROJECT_SCHEMA_VERSION);
    assert_eq!(loaded.sequence.settings, project.sequence.settings);
    assert_eq!(loaded.sequence.layers.len(), project.sequence.layers.len());
    assert_eq!(loaded.assets.len(), 1);
    let shape = loaded
        .sequence
        .layers
        .iter()
        .flat_map(|layer| &layer.items)
        .find_map(|item| item.as_shape())
        .expect("shape should survive the round trip");
    assert_eq!(shape.kind, ShapeKind::Ellipse);

    let _ = fs::remove_dir_all(directory);
}

/// Files written before schema 0.3 put a duplicate `kind` key on shape items
/// (`"kind": "shape"` from the item tag plus `"kind": "ellipse"` from the shape itself).
/// The loader must repair and open them instead of failing.
#[test]
fn legacy_duplicate_shape_kind_is_repaired() {
    const LEGACY: &str = r#"{
  "schema_version": "0.2",
  "id": "11111111-1111-4111-8111-111111111111",
  "title": "legacy",
  "assets": [],
  "sequence": {
    "id": "22222222-2222-4222-8222-222222222222",
    "settings": { "width": 1920, "height": 1080, "frame_rate": 30.0, "sample_rate": 48000 },
    "layers": [
      {
        "id": "33333333-3333-4333-8333-333333333333",
        "name": "レイヤー 1",
        "locked": false,
        "hidden": false,
        "muted": false,
        "items": [
          {
            "kind": "shape",
            "id": "44444444-4444-4444-8444-444444444444",
            "kind": "ellipse",
            "timeline_start": 0,
            "duration": 3000000,
            "transform": { "x": 960.0, "y": 540.0, "anchor": "center", "scale": 100.0 },
            "width": 320.0,
            "height": 180.0,
            "color_rgba": [255, 255, 255, 255],
            "opacity": 100.0
          }
        ]
      }
    ]
  }
}"#;

    let directory = temp_directory("proj-legacy");
    let path = directory.join("legacy.yukivproj");
    fs::write(&path, LEGACY).unwrap();

    let loaded = project_store::load(&path).expect("legacy project should be repaired and load");
    assert_eq!(loaded.schema_version, PROJECT_SCHEMA_VERSION);
    let shape = loaded.sequence.layers[0].items[0]
        .as_shape()
        .expect("item should be a shape");
    assert_eq!(shape.kind, ShapeKind::Ellipse);
    assert_eq!(shape.width, 320.0);

    let _ = fs::remove_dir_all(directory);
}

fn count_items(project: &Project) -> usize {
    project
        .sequence
        .layers
        .iter()
        .map(|layer| layer.items.len())
        .sum()
}

/// A voice line written before speech items existed is an audio-only VOICEVOX clip plus a
/// same-span subtitle on another layer. Loading it must merge the pair into one speech item.
#[test]
fn legacy_voicevox_audio_and_subtitle_merge_into_one_speech() {
    let mut project = Project::new("legacy-voice");
    let duration = TimelineTime::from_seconds(2.0);
    let asset = MediaAsset {
        id: Uuid::new_v4(),
        name: "こんにちは".into(),
        path: PathBuf::from("/home/user/.cache/yuki-video-editor/voicevox/voicevox-0000.wav"),
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
    let mut history = EditHistory::default();
    let audio_layer = project.sequence.layers[0].id;
    history
        .execute(
            &mut project,
            EditCommand::ImportAsset {
                asset,
                layer_id: audio_layer,
                timeline_start: TimelineTime::ZERO,
            },
        )
        .expect("import audio");

    let text_layer = Layer::new("字幕");
    let text_layer_id = text_layer.id;
    history
        .execute(
            &mut project,
            EditCommand::AddLayer {
                layer: text_layer,
                index: Some(0),
            },
        )
        .expect("add subtitle layer");
    let mut text = TextItem::new("こんにちは", TimelineTime::ZERO, (1920.0, 1080.0));
    text.duration = duration;
    let text_id = text.id;
    history
        .execute(
            &mut project,
            EditCommand::AddText {
                layer_id: text_layer_id,
                text,
            },
        )
        .expect("add subtitle");

    // The legacy layout is two separate items.
    assert_eq!(count_items(&project), 2);

    let directory = temp_directory("proj-voicevox-migrate");
    let path = directory.join("legacy.yukivproj");
    project_store::save(&project, &path).expect("save");
    let loaded = project_store::load(&path).expect("load");

    let speeches: Vec<_> = loaded
        .sequence
        .layers
        .iter()
        .flat_map(|layer| &layer.items)
        .filter_map(TimelineItem::as_speech)
        .collect();
    assert_eq!(speeches.len(), 1, "the pair must become one speech item");
    let speech = speeches[0];
    assert_eq!(speech.subtitle.text, "こんにちは");
    assert_eq!(speech.duration(), duration);
    // The old audio clip and subtitle text item are gone, leaving just the speech.
    assert_eq!(count_items(&loaded), 1);
    assert!(loaded.item(text_id).is_none());

    let _ = fs::remove_dir_all(directory);
}

/// The merge is deliberately conservative: an ordinary audio clip whose text happens to match
/// and share its span must be left as two items.
#[test]
fn non_voicevox_audio_with_matching_text_is_not_merged() {
    let mut project = Project::new("legacy-music");
    let duration = TimelineTime::from_seconds(2.0);
    let asset = MediaAsset {
        id: Uuid::new_v4(),
        name: "こんにちは".into(),
        path: PathBuf::from("/tmp/music.wav"),
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
    let mut history = EditHistory::default();
    let audio_layer = project.sequence.layers[0].id;
    history
        .execute(
            &mut project,
            EditCommand::ImportAsset {
                asset,
                layer_id: audio_layer,
                timeline_start: TimelineTime::ZERO,
            },
        )
        .expect("import audio");
    let text_layer = Layer::new("字幕");
    let text_layer_id = text_layer.id;
    history
        .execute(
            &mut project,
            EditCommand::AddLayer {
                layer: text_layer,
                index: Some(0),
            },
        )
        .expect("add subtitle layer");
    let mut text = TextItem::new("こんにちは", TimelineTime::ZERO, (1920.0, 1080.0));
    text.duration = duration;
    history
        .execute(
            &mut project,
            EditCommand::AddText {
                layer_id: text_layer_id,
                text,
            },
        )
        .expect("add subtitle");

    let directory = temp_directory("proj-non-voicevox");
    let path = directory.join("legacy.yukivproj");
    project_store::save(&project, &path).expect("save");
    let loaded = project_store::load(&path).expect("load");

    assert_eq!(
        count_items(&loaded),
        2,
        "a plain audio + text pair stays separate"
    );

    let _ = fs::remove_dir_all(directory);
}
