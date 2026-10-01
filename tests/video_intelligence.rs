//! Phase 2 (Video Intelligence) integration tests: real FFmpeg analysis producing a VIR,
//! cache reuse, and VIR schema compatibility.
//!
//! Tests that need FFmpeg skip themselves when it is unavailable, matching the other media
//! integration tests in this repository.

use std::fs;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use uuid::Uuid;
use yuki_video_editor::domain::ProjectId;
use yuki_video_editor::knowledge::{AnalysisOptions, AnalysisRequest, media_key, run_analysis};
use yuki_video_editor::media::probe_media;
use yuki_video_editor::protocol::{VIR_ANALYSIS_VERSION, VIR_SCHEMA_VERSION, VirDocument};

/// Builds a short clip with two hard cuts (red → blue → green) so scene detection has
/// unambiguous boundaries.
fn make_cut_video(directory: &std::path::Path) -> std::path::PathBuf {
    let path = directory.join("cuts.mp4");
    let status = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=320x180:d=2",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=320x180:d=2",
            "-f",
            "lavfi",
            "-i",
            "color=c=green:s=320x180:d=2",
            "-filter_complex",
            "[0:v][1:v][2:v]concat=n=3:v=1:a=0[out]",
            "-map",
            "[out]",
            "-r",
            "25",
            "-c:v",
            "mpeg4",
        ])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success(), "failed to generate the test clip");
    path
}

fn request(
    asset: yuki_video_editor::domain::MediaAsset,
    cache_root: &std::path::Path,
    force: bool,
    options: AnalysisOptions,
) -> AnalysisRequest {
    AnalysisRequest {
        asset,
        project_id: ProjectId::new_v4(),
        options,
        cache_root: cache_root.to_path_buf(),
        force,
    }
}

#[test]
fn analysis_produces_scenes_shots_and_thumbnails() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("FFmpeg is unavailable; skipping video intelligence test");
        return;
    }

    let directory = std::env::temp_dir().join(format!("yuki-vir-{}", Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let source = make_cut_video(&directory);
    let asset = probe_media(&source).unwrap();
    assert!(asset.metadata.has_video);

    let cache_root = directory.join("cache");
    let cancel = AtomicBool::new(false);
    let mut progress_calls = 0usize;
    let result = run_analysis(
        &request(asset.clone(), &cache_root, true, AnalysisOptions::default()),
        &cancel,
        &mut |_progress| progress_calls += 1,
    )
    .expect("analysis should succeed");

    assert!(!result.from_cache);
    assert!(progress_calls > 0, "analysis should report progress");

    let media = &result.media;
    assert_eq!(media.fingerprint, media_key(&source));
    assert_eq!(media.media_id, asset.id);
    // Two hard cuts => three scenes, or at least clearly more than one.
    assert!(
        media.scenes.len() >= 2,
        "expected multiple scenes, found {}",
        media.scenes.len()
    );
    let shots: usize = media.scenes.iter().map(|scene| scene.shots.len()).sum();
    assert!(shots >= 2, "expected multiple shots, found {shots}");

    // Every scene should carry a keyframe whose thumbnail actually exists on disk.
    for scene in &media.scenes {
        let frame = scene
            .keyframes
            .first()
            .expect("each scene should have a keyframe");
        assert!(scene.end > scene.start);
        assert!(
            scene.start <= frame.timestamp && frame.timestamp < scene.end,
            "keyframe must lie inside its scene"
        );
        let file = frame.thumbnail.as_ref().expect("keyframe has a thumbnail");
        assert!(
            result.cache_dir.join(file).exists(),
            "thumbnail {file} should exist on disk"
        );
        assert!(scene.importance.is_some());
    }

    // The document must be self-describing and round-trip through JSON.
    assert_eq!(result.vir.schema_version, VIR_SCHEMA_VERSION);
    assert_eq!(result.vir.analysis_version, VIR_ANALYSIS_VERSION);
    assert_eq!(
        result.vir.provenance.scene_detector.as_deref(),
        Some("ffmpeg:scene")
    );
    let encoded = serde_json::to_string_pretty(&result.vir).unwrap();
    let decoded: VirDocument = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.media.len(), 1);
    assert_eq!(decoded.media[0].scenes.len(), media.scenes.len());

    // Semantic IDs must be deterministic: a re-analysis yields the same scene ids.
    let again = run_analysis(
        &request(asset, &cache_root, true, AnalysisOptions::default()),
        &cancel,
        &mut |_progress| {},
    )
    .unwrap();
    let first_ids: Vec<Uuid> = media.scenes.iter().map(|scene| scene.scene_id).collect();
    let second_ids: Vec<Uuid> = again
        .media
        .scenes
        .iter()
        .map(|scene| scene.scene_id)
        .collect();
    assert_eq!(first_ids, second_ids);

    let _ = fs::remove_dir_all(directory);
}

#[test]
fn second_run_uses_the_cache() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("FFmpeg is unavailable; skipping video intelligence test");
        return;
    }

    let directory = std::env::temp_dir().join(format!("yuki-vir-cache-{}", Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let source = make_cut_video(&directory);
    let asset = probe_media(&source).unwrap();
    let cache_root = directory.join("cache");
    let cancel = AtomicBool::new(false);

    let first = run_analysis(
        &request(
            asset.clone(),
            &cache_root,
            false,
            AnalysisOptions::default(),
        ),
        &cancel,
        &mut |_progress| {},
    )
    .unwrap();
    assert!(!first.from_cache);

    let second = run_analysis(
        &request(
            asset.clone(),
            &cache_root,
            false,
            AnalysisOptions::default(),
        ),
        &cancel,
        &mut |_progress| {},
    )
    .unwrap();
    assert!(second.from_cache, "the second run should hit the cache");
    assert_eq!(first.media.scenes.len(), second.media.scenes.len());

    // Changing an option that affects the result must invalidate the cache.
    let changed_options = AnalysisOptions {
        thumbnail_width: 320,
        ..AnalysisOptions::default()
    };
    let third = run_analysis(
        &request(asset, &cache_root, false, changed_options),
        &cancel,
        &mut |_progress| {},
    )
    .unwrap();
    assert!(!third.from_cache, "an option change must force re-analysis");

    let _ = fs::remove_dir_all(directory);
}

/// VIR 0.1 documents (the Phase 1 draft) must still load: removed fields are ignored and added
/// fields default.
#[test]
fn legacy_vir_documents_still_load() {
    let legacy = format!(
        r#"{{
            "schema_version": "0.1",
            "analysis_version": "0.1",
            "project_id": "{}",
            "media": [{{
                "media_id": "{}",
                "scenes": [{{
                    "scene_id": "{}",
                    "start": 0,
                    "end": 1000000,
                    "summary": "legacy",
                    "transcript": null,
                    "ocr": [],
                    "objects": [],
                    "keyframe_timestamps": [500000],
                    "importance": 0.5
                }}]
            }}]
        }}"#,
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4()
    );
    let document: VirDocument = serde_json::from_str(&legacy).unwrap();
    assert_eq!(document.schema_version, "0.1");
    assert_eq!(document.media.len(), 1);
    assert_eq!(document.media[0].scenes.len(), 1);
    // New optional fields fall back to their defaults.
    assert!(document.media[0].scenes[0].shots.is_empty());
    assert!(document.media[0].scenes[0].keyframes.is_empty());
    assert!(document.provenance.scene_detector.is_none());
}
