use std::fs;
use std::process::Command;
use std::thread;
use std::time::Duration;
use uuid::Uuid;
use yuki_video_editor::domain::{
    Clip, Layer, Project, ShapeItem, ShapeKind, SpeechItem, TextItem, TimelineTime,
};
use yuki_video_editor::editing::{EditCommand, EditHistory};
use yuki_video_editor::media::{
    AudioCodec, EncoderPreset, ExportOptions, IMAGE_DEFAULT_DURATION_SECONDS, PowerProfile,
    PreviewRequest, PreviewWorker, VideoEncoder, detect_hardware_capabilities,
    ensure_decodable_image, export_project_with_options, probe_media,
};

#[test]
fn image_preview_succeeds_even_when_qsv_is_requested() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("FFmpeg is unavailable; skipping image preview test");
        return;
    }
    let directory = std::env::temp_dir().join(format!("yuki-preview-image-{}", Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let png = directory.join("still.png");
    let generated = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x180:rate=30",
            "-frames:v",
            "1",
        ])
        .arg(&png)
        .status()
        .unwrap();
    assert!(generated.success());

    let heic = directory.join("still.heic");
    let have_heic = Command::new("heif-enc")
        .args(["-q", "80", "-o"])
        .arg(&heic)
        .arg(&png)
        .status()
        .is_ok_and(|status| status.success());

    let mut sources = vec![png];
    if have_heic {
        sources.push(heic);
    }

    // The app reports QSV as available on machines it can use, and used to pass that straight to
    // the decoder. For a still image the QSV filter graph fails (and hangs for HEIC), which left
    // the preview stuck on “映像をデコードできません”. Asking for QSV here
    // reproduces that regression.
    for source in &sources {
        for use_qsv in [false, true] {
            let preview = PreviewWorker::spawn(|| {});
            preview.request_batch(vec![PreviewRequest {
                generation: 1,
                clip_id: Uuid::new_v4(),
                path: source.clone(),
                source_time: TimelineTime::from_seconds(3.0),
                profile: PowerProfile::Eco,
                use_qsv,
                is_image: true,
            }]);
            let frame = (0..300).find_map(|_| {
                let frame = preview.try_frames().into_iter().next();
                if frame.is_none() {
                    thread::sleep(Duration::from_millis(10));
                }
                frame
            });
            assert!(
                frame.is_some_and(|frame| frame.error.is_none() && !frame.rgb.is_empty()),
                "{} with use_qsv={use_qsv} should produce a preview frame",
                source.display()
            );
        }
    }

    let _ = fs::remove_dir_all(directory);
}

#[test]
fn ffmpeg_probe_and_export_round_trip() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("FFmpeg is unavailable; skipping media integration test");
        return;
    }

    let directory = std::env::temp_dir().join(format!("yuki-video-test-{}", Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("source.mp4");
    let destination = directory.join("result.mp4");

    let generated = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x180:rate=30",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "1",
            "-c:v",
            "mpeg4",
            "-c:a",
            "aac",
        ])
        .arg(&source)
        .status()
        .unwrap();
    assert!(generated.success());

    let asset = probe_media(&source).unwrap();
    assert!(asset.metadata.has_video);
    assert!(asset.metadata.has_audio);
    assert_eq!((asset.metadata.width, asset.metadata.height), (320, 180));

    let preview = PreviewWorker::spawn(|| {});
    preview.request_batch(vec![PreviewRequest {
        generation: 1,
        clip_id: Uuid::new_v4(),
        path: source.clone(),
        source_time: TimelineTime::ZERO,
        profile: PowerProfile::Eco,
        use_qsv: false,
        is_image: false,
    }]);
    let first_frame = (0..100).find_map(|_| {
        let frame = preview.try_frames().into_iter().next();
        if frame.is_none() {
            thread::sleep(Duration::from_millis(10));
        }
        frame
    });
    assert!(first_frame.is_some_and(|frame| frame.error.is_none() && !frame.rgb.is_empty()));

    let mut project = Project::new("integration");
    project.sequence.settings.width = 320;
    project.sequence.settings.height = 180;
    let layer_id = project.sequence.layers[0].id;
    let mut history = EditHistory::default();
    history
        .execute(
            &mut project,
            EditCommand::ImportAsset {
                asset,
                layer_id,
                timeline_start: TimelineTime::ZERO,
            },
        )
        .unwrap();

    let text_layer = Layer::new("テキスト");
    let text_layer_id = text_layer.id;
    history
        .execute(
            &mut project,
            EditCommand::AddLayer {
                layer: text_layer,
                index: Some(0),
            },
        )
        .unwrap();

    let mut text = TextItem::new("テスト", TimelineTime::ZERO, (320.0, 180.0));
    text.duration = TimelineTime::from_seconds(0.8);
    text.font_size = 32.0;
    history
        .execute(
            &mut project,
            EditCommand::AddText {
                layer_id: text_layer_id,
                text,
            },
        )
        .unwrap();

    export_project_with_options(
        &project,
        &destination,
        ExportOptions {
            threads: 1,
            use_qsv: true,
            ..ExportOptions::default()
        },
    )
    .unwrap();
    let rendered = probe_media(&destination).unwrap();
    assert!(rendered.metadata.duration.as_seconds() > 0.9);
    assert_eq!(
        (rendered.metadata.width, rendered.metadata.height),
        (320, 180)
    );

    let _ = fs::remove_dir_all(directory);
}

#[test]
fn audio_only_media_imports_and_exports() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("FFmpeg is unavailable; skipping media integration test");
        return;
    }

    let directory = std::env::temp_dir().join(format!("yuki-video-audio-test-{}", Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("voice.wav");
    let destination = directory.join("result.mp4");

    let generated = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "1",
        ])
        .arg(&source)
        .status()
        .unwrap();
    assert!(generated.success());

    let asset = probe_media(&source).unwrap();
    assert!(!asset.metadata.has_video);
    assert!(asset.metadata.has_audio);

    let mut project = Project::new("audio-only");
    let layer_id = project.sequence.layers[0].id;
    let mut history = EditHistory::default();
    history
        .execute(
            &mut project,
            EditCommand::ImportAsset {
                asset,
                layer_id,
                timeline_start: TimelineTime::ZERO,
            },
        )
        .unwrap();

    export_project_with_options(&project, &destination, ExportOptions::default()).unwrap();

    let rendered = probe_media(&destination).unwrap();
    assert!(rendered.metadata.has_video);
    assert!(rendered.metadata.has_audio);

    let _ = fs::remove_dir_all(directory);
}

#[test]
fn high_efficiency_codecs_export_when_available() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("FFmpeg is unavailable; skipping media integration test");
        return;
    }

    let hardware = detect_hardware_capabilities();
    let directory = std::env::temp_dir().join(format!("yuki-video-codec-test-{}", Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();

    let mut project = Project::new("codecs");
    project.sequence.settings.width = 160;
    project.sequence.settings.height = 90;
    project.sequence.settings.frame_rate = 24.0;
    let layer_id = project.sequence.layers[0].id;
    let mut history = EditHistory::default();
    let mut shape = ShapeItem::new(ShapeKind::Rectangle, TimelineTime::ZERO, (160.0, 90.0));
    shape.width = 100.0;
    shape.height = 60.0;
    shape.duration = TimelineTime::from_seconds(0.4);
    history
        .execute(&mut project, EditCommand::AddShape { layer_id, shape })
        .unwrap();

    let mut tested = 0;
    for encoder in [
        VideoEncoder::Libx265,
        VideoEncoder::Av1Svt,
        VideoEncoder::Av1Aom,
        VideoEncoder::Vp9,
    ] {
        if !encoder.is_available(&hardware) {
            eprintln!("{} is unavailable; skipping", encoder.label());
            continue;
        }
        let destination = directory.join(format!("{}.mp4", encoder.ffmpeg_name()));
        export_project_with_options(
            &project,
            &destination,
            ExportOptions {
                threads: 2,
                video_encoder: encoder,
                crf: 30,
                preset: EncoderPreset::Veryfast,
                audio_codec: AudioCodec::None,
                ..ExportOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("{} export failed: {error}", encoder.label()));

        let rendered = probe_media(&destination).unwrap();
        assert!(
            rendered.metadata.has_video,
            "{} produced no video",
            encoder.label()
        );
        assert_eq!(
            (rendered.metadata.width, rendered.metadata.height),
            (160, 90),
            "{} produced the wrong frame size",
            encoder.label()
        );
        tested += 1;
    }
    eprintln!("verified {tested} high-efficiency encoders");

    let _ = fs::remove_dir_all(directory);
}

#[test]
fn still_image_imports_previews_and_exports() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("FFmpeg is unavailable; skipping media integration test");
        return;
    }

    let directory = std::env::temp_dir().join(format!("yuki-video-image-test-{}", Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("still.png");
    let destination = directory.join("result.mp4");

    let generated = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x180:rate=30",
            "-frames:v",
            "1",
        ])
        .arg(&source)
        .status()
        .unwrap();
    assert!(generated.success());

    // A still image has no intrinsic duration; the probe must substitute the default and mark it
    // as an image rather than as a ~0.04s video.
    let asset = probe_media(&source).unwrap();
    assert!(asset.metadata.is_image);
    assert!(asset.metadata.has_video);
    assert!(!asset.metadata.has_audio);
    assert_eq!(
        asset.metadata.duration,
        TimelineTime::from_seconds(IMAGE_DEFAULT_DURATION_SECONDS)
    );
    assert_eq!((asset.metadata.width, asset.metadata.height), (320, 180));

    // The still frame can be previewed without seeking (the decoder loops the image).
    let preview = PreviewWorker::spawn(|| {});
    preview.request_batch(vec![PreviewRequest {
        generation: 1,
        clip_id: Uuid::new_v4(),
        path: source.clone(),
        source_time: TimelineTime::from_seconds(2.5),
        profile: PowerProfile::Eco,
        use_qsv: false,
        is_image: true,
    }]);
    let first_frame = (0..100).find_map(|_| {
        let frame = preview.try_frames().into_iter().next();
        if frame.is_none() {
            thread::sleep(Duration::from_millis(10));
        }
        frame
    });
    assert!(first_frame.is_some_and(|frame| frame.error.is_none() && !frame.rgb.is_empty()));

    let mut project = Project::new("image");
    project.sequence.settings.width = 320;
    project.sequence.settings.height = 180;
    let layer_id = project.sequence.layers[0].id;
    let mut history = EditHistory::default();
    history
        .execute(
            &mut project,
            EditCommand::ImportAsset {
                asset,
                layer_id,
                timeline_start: TimelineTime::ZERO,
            },
        )
        .unwrap();

    // Images loop, so the source range may run past the asset's default length.
    let clip_id = project.sequence.layers[0].items[0].id();
    history
        .execute(
            &mut project,
            EditCommand::TrimClip {
                clip_id,
                source_in: TimelineTime::ZERO,
                source_out: TimelineTime::from_seconds(7.0),
            },
        )
        .unwrap();
    assert_eq!(project.duration(), TimelineTime::from_seconds(7.0));

    export_project_with_options(&project, &destination, ExportOptions::default()).unwrap();

    let rendered = probe_media(&destination).unwrap();
    assert!(rendered.metadata.has_video);
    assert!(
        rendered.metadata.duration.as_seconds() > 6.5,
        "image clip should export at its stretched length, got {}",
        rendered.metadata.duration
    );
    assert_eq!(
        (rendered.metadata.width, rendered.metadata.height),
        (320, 180)
    );

    let _ = fs::remove_dir_all(directory);
}

#[test]
fn heic_image_imports_and_exports_when_a_converter_is_available() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("FFmpeg is unavailable; skipping HEIC test");
        return;
    }
    // HEIC cannot be decoded by every FFmpeg build; when libheif's `heif-convert` is missing the
    // environment cannot create or verify a sample, so the test is skipped.
    if Command::new("heif-enc").arg("-h").output().is_err() {
        eprintln!("heif-enc is unavailable; skipping HEIC test");
        return;
    }

    let directory = std::env::temp_dir().join(format!("yuki-video-heic-test-{}", Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let png = directory.join("source.png");
    let heic = directory.join("photo.heic");
    let destination = directory.join("result.mp4");

    let generated = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x180:rate=30",
            "-frames:v",
            "1",
        ])
        .arg(&png)
        .status()
        .unwrap();
    assert!(generated.success());
    let encoded = Command::new("heif-enc")
        .args(["-q", "80", "-o"])
        .arg(&heic)
        .arg(&png)
        .status()
        .unwrap();
    assert!(encoded.success());

    // The key regression: HEIC has no duration at all, which used to surface as
    // "media duration is missing or invalid".
    let asset = probe_media(&heic).unwrap();
    assert!(asset.metadata.is_image);
    assert!(asset.metadata.has_video);
    assert!(!asset.metadata.has_audio);
    assert_eq!(
        asset.metadata.duration,
        TimelineTime::from_seconds(IMAGE_DEFAULT_DURATION_SECONDS)
    );
    // This FFmpeg build decodes the sample, so the resolver returns the original file.
    assert_eq!(ensure_decodable_image(&heic), heic);

    let preview = PreviewWorker::spawn(|| {});
    preview.request_batch(vec![PreviewRequest {
        generation: 1,
        clip_id: Uuid::new_v4(),
        path: heic.clone(),
        source_time: TimelineTime::ZERO,
        profile: PowerProfile::Eco,
        use_qsv: false,
        is_image: true,
    }]);
    let first_frame = (0..100).find_map(|_| {
        let frame = preview.try_frames().into_iter().next();
        if frame.is_none() {
            thread::sleep(Duration::from_millis(10));
        }
        frame
    });
    assert!(first_frame.is_some_and(|frame| frame.error.is_none() && !frame.rgb.is_empty()));

    let mut project = Project::new("heic");
    project.sequence.settings.width = 320;
    project.sequence.settings.height = 180;
    let layer_id = project.sequence.layers[0].id;
    let mut history = EditHistory::default();
    history
        .execute(
            &mut project,
            EditCommand::ImportAsset {
                asset,
                layer_id,
                timeline_start: TimelineTime::ZERO,
            },
        )
        .unwrap();

    export_project_with_options(&project, &destination, ExportOptions::default()).unwrap();
    let rendered = probe_media(&destination).unwrap();
    assert!(rendered.metadata.has_video);
    assert!(rendered.metadata.duration.as_seconds() > 4.5);

    let _ = fs::remove_dir_all(directory);
}

#[test]
fn tiled_heic_still_is_flattened_before_decoding() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("FFmpeg is unavailable; skipping tiled HEIC test");
        return;
    }
    // Creating the grid needs `heif-enc --cut-tiles`, flattening it needs `heif-convert`.
    if Command::new("heif-enc").arg("-h").output().is_err()
        || Command::new("heif-convert").arg("-h").output().is_err()
    {
        eprintln!("libheif tools are unavailable; skipping tiled HEIC test");
        return;
    }

    let directory = std::env::temp_dir().join(format!("yuki-video-tiled-heic-{}", Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let png = directory.join("big.png");
    let heic = directory.join("tiled.heic");

    let generated = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=1600x1200:rate=30",
            "-frames:v",
            "1",
        ])
        .arg(&png)
        .status()
        .unwrap();
    assert!(generated.success());

    // `--cut-tiles` stores the image as a 4x3 grid of tiles, which FFmpeg exposes as a *stream
    // group*. A plain decode auto-assembles the grid in a complex filtergraph, but the preview's
    // `-vf` simple filtergraph cannot be layered on top of that, so it produced no frame at all.
    let encoded = Command::new("heif-enc")
        .args(["--cut-tiles", "512", "-q", "80", "-o"])
        .arg(&heic)
        .arg(&png)
        .status()
        .unwrap();
    assert!(encoded.success());

    let asset = probe_media(&heic).unwrap();
    assert!(asset.metadata.is_image);
    assert!(asset.metadata.has_video);

    // The regression: this used to hand back the tiled HEIC unchanged, and the preview then failed
    // with “映像をデコードできません（FFmpegが映像ストリームを返しませんでした）”.
    let decodable = ensure_decodable_image(&heic);
    assert_ne!(decodable, heic, "a tiled HEIC must be flattened first");
    // Flattening must assemble the full grid, not hand back one 512x512 tile.
    let flattened = probe_media(&decodable).unwrap();
    assert_eq!(
        (flattened.metadata.width, flattened.metadata.height),
        (1600, 1200)
    );

    // The app asks the preview worker for the original asset path; the worker is what resolves the
    // tiled file to its flattened PNG, so exercise that exact path here.
    let preview = PreviewWorker::spawn(|| {});
    preview.request_batch(vec![PreviewRequest {
        generation: 1,
        clip_id: Uuid::new_v4(),
        path: heic.clone(),
        source_time: TimelineTime::ZERO,
        profile: PowerProfile::Eco,
        use_qsv: false,
        is_image: true,
    }]);
    let first_frame = (0..300).find_map(|_| {
        let frame = preview.try_frames().into_iter().next();
        if frame.is_none() {
            thread::sleep(Duration::from_millis(10));
        }
        frame
    });
    assert!(first_frame.is_some_and(|frame| frame.error.is_none() && !frame.rgb.is_empty()));

    let _ = fs::remove_dir_all(directory);
    let _ = fs::remove_file(decodable);
}

#[test]
fn reused_still_image_exports_at_every_placement() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("FFmpeg is unavailable; skipping media integration test");
        return;
    }

    let directory = std::env::temp_dir().join(format!("yuki-video-reuse-{}", Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("logo.png");
    let destination = directory.join("result.mp4");
    Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x180:rate=30",
            "-frames:v",
            "1",
        ])
        .arg(&source)
        .status()
        .unwrap();

    let asset = probe_media(&source).unwrap();
    let mut project = Project::new("reuse");
    project.sequence.settings.width = 320;
    project.sequence.settings.height = 180;
    let layer_id = project.sequence.layers[0].id;
    let mut history = EditHistory::default();
    // The same asset dropped twice must not be duplicated in `assets`, and both placements should
    // still render (the exporter opens one FFmpeg input per distinct asset).
    for start in [TimelineTime::ZERO, TimelineTime::from_seconds(6.0)] {
        history
            .execute(
                &mut project,
                EditCommand::ImportAsset {
                    asset: asset.clone(),
                    layer_id,
                    timeline_start: start,
                },
            )
            .unwrap();
    }
    assert_eq!(project.assets.len(), 1);
    assert_eq!(project.duration(), TimelineTime::from_seconds(11.0));

    export_project_with_options(&project, &destination, ExportOptions::default()).unwrap();
    let rendered = probe_media(&destination).unwrap();
    assert!(rendered.metadata.duration.as_seconds() > 10.5);
    assert_eq!(
        (rendered.metadata.width, rendered.metadata.height),
        (320, 180)
    );

    let _ = fs::remove_dir_all(directory);
}

#[test]
fn shape_only_project_exports_rect_and_ellipse() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("FFmpeg is unavailable; skipping media integration test");
        return;
    }

    let directory = std::env::temp_dir().join(format!("yuki-video-shape-test-{}", Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let destination = directory.join("result.mp4");

    let mut project = Project::new("shapes");
    project.sequence.settings.width = 320;
    project.sequence.settings.height = 180;
    let mut history = EditHistory::default();

    let mut rect = ShapeItem::new(ShapeKind::Rectangle, TimelineTime::ZERO, (320.0, 180.0));
    rect.width = 200.0;
    rect.height = 120.0;
    rect.transform.x = 160.0;
    rect.transform.y = 60.0;
    rect.color_rgba = [255, 0, 0, 255];
    rect.duration = TimelineTime::from_seconds(1.0);
    let rect_layer = project.sequence.layers[0].id;
    history
        .execute(
            &mut project,
            EditCommand::AddShape {
                layer_id: rect_layer,
                shape: rect,
            },
        )
        .unwrap();

    let mut ellipse = ShapeItem::new(ShapeKind::Ellipse, TimelineTime::ZERO, (320.0, 180.0));
    ellipse.width = 100.0;
    ellipse.height = 100.0;
    ellipse.transform.x = 240.0;
    ellipse.transform.y = 140.0;
    ellipse.color_rgba = [0, 0, 255, 255];
    ellipse.duration = TimelineTime::from_seconds(1.0);
    let ellipse_layer = Layer::new("レイヤー 2");
    let ellipse_layer_id = ellipse_layer.id;
    history
        .execute(
            &mut project,
            EditCommand::AddLayer {
                layer: ellipse_layer,
                index: Some(0),
            },
        )
        .unwrap();
    history
        .execute(
            &mut project,
            EditCommand::AddShape {
                layer_id: ellipse_layer_id,
                shape: ellipse,
            },
        )
        .unwrap();

    export_project_with_options(&project, &destination, ExportOptions::default()).unwrap();

    let rendered = probe_media(&destination).unwrap();
    assert!(rendered.metadata.has_video);
    assert!(rendered.metadata.duration.as_seconds() > 0.9);
    assert_eq!(
        (rendered.metadata.width, rendered.metadata.height),
        (320, 180)
    );

    let frame_path = directory.join("frame.rgb");
    let status = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-ss",
            "0.5",
            "-i",
        ])
        .arg(&destination)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24"])
        .arg(&frame_path)
        .status()
        .unwrap();
    assert!(status.success());
    let rgb = fs::read(&frame_path).unwrap();
    let has_red = rgb
        .chunks_exact(3)
        .any(|pixel| pixel[0] > 200 && pixel[1] < 80 && pixel[2] < 80);
    let has_blue = rgb
        .chunks_exact(3)
        .any(|pixel| pixel[2] > 200 && pixel[0] < 80 && pixel[1] < 80);
    assert!(has_red, "rectangle should render red");
    assert!(has_blue, "ellipse should render blue");

    let _ = fs::remove_dir_all(directory);
}

#[test]
fn speech_item_exports_audio_and_subtitle_together() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("FFmpeg is unavailable; skipping media integration test");
        return;
    }

    let directory = std::env::temp_dir().join(format!("yuki-video-speech-test-{}", Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("voice.wav");
    let destination = directory.join("result.mp4");

    let generated = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "1",
        ])
        .arg(&source)
        .status()
        .unwrap();
    assert!(generated.success());

    let asset = probe_media(&source).unwrap();
    assert!(!asset.metadata.has_video);
    assert!(asset.metadata.has_audio);

    let mut project = Project::new("speech");
    project.sequence.settings.width = 320;
    project.sequence.settings.height = 180;
    let layer_id = project.sequence.layers[0].id;
    let frame = project.sequence.settings.frame_size();
    let clip = Clip::new(&asset, TimelineTime::ZERO, frame);
    let mut subtitle = TextItem::new("セリフ", TimelineTime::ZERO, frame);
    subtitle.duration = asset.metadata.duration;
    subtitle.font_size = 28.0;
    let speech = SpeechItem::new(clip, subtitle);
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

    export_project_with_options(&project, &destination, ExportOptions::default()).unwrap();

    let rendered = probe_media(&destination).unwrap();
    assert!(
        rendered.metadata.has_video,
        "speech export must carry video"
    );
    assert!(
        rendered.metadata.has_audio,
        "speech export must carry audio"
    );
    assert!(rendered.metadata.duration.as_seconds() > 0.9);

    let _ = fs::remove_dir_all(directory);
}

#[test]
fn speed_rotation_and_effects_export_successfully() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("FFmpeg is unavailable; skipping media integration test");
        return;
    }

    let directory = std::env::temp_dir().join(format!("yuki-video-fx-test-{}", Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("source.mp4");
    let destination = directory.join("result.mp4");

    let generated = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x180:rate=30",
            "-t",
            "2",
            "-c:v",
            "mpeg4",
        ])
        .arg(&source)
        .status()
        .unwrap();
    assert!(generated.success());

    let asset = probe_media(&source).unwrap();
    let mut project = Project::new("fx");
    project.sequence.settings.width = 320;
    project.sequence.settings.height = 180;
    let layer_id = project.sequence.layers[0].id;
    let mut history = EditHistory::default();
    history
        .execute(
            &mut project,
            EditCommand::ImportAsset {
                asset,
                layer_id,
                timeline_start: TimelineTime::ZERO,
            },
        )
        .unwrap();
    let clip_id = project
        .sequence
        .layers
        .iter()
        .flat_map(|layer| &layer.items)
        .find_map(|item| item.as_clip().map(|clip| clip.id))
        .unwrap();
    history
        .execute(
            &mut project,
            EditCommand::SetClipSpeed {
                clip_id,
                speed: 2.0,
            },
        )
        .unwrap();
    let mut transform = project.clip(clip_id).unwrap().transform;
    transform.rotation_degrees = 30.0;
    transform.scale = 60.0;
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
                effects: yuki_video_editor::domain::ClipEffects {
                    brightness: 0.1,
                    contrast: 1.2,
                    saturation: 0.7,
                    hue_degrees: 10.0,
                    blur_radius: 1.5,
                    grayscale: false,
                    invert: false,
                },
            },
        )
        .unwrap();

    // 2s source at 2× becomes 1s on the timeline.
    assert!((project.duration().as_seconds() - 1.0).abs() < 1e-3);

    export_project_with_options(
        &project,
        &destination,
        ExportOptions {
            threads: 1,
            use_qsv: false,
            ..ExportOptions::default()
        },
    )
    .unwrap();

    let rendered = probe_media(&destination).unwrap();
    let duration = rendered.metadata.duration.as_seconds();
    assert!(
        duration > 0.7 && duration < 1.4,
        "speed-affected export should be about 1s, got {duration}"
    );

    let _ = fs::remove_dir_all(directory);
}
