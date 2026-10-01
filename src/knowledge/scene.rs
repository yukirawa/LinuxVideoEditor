//! Scene and shot detection.
//!
//! FFmpeg's `select` filter exposes a per-frame scene-change score. Selecting frames whose
//! score exceeds a threshold and printing their timestamps yields the cut list, from which
//! shots and scenes are grouped with a deterministic, documented heuristic:
//!
//! - a **shot** is the interval between two detected cuts (a continuous visual segment);
//! - a **scene** groups consecutive shots; a cut stronger than `scene_threshold` starts a new
//!   scene, weaker cuts only start a new shot. A scene is also closed once it grows past
//!   `max_scene_seconds`, so a long static video does not become one endless scene.
//!
//! This is deliberately model-free: semantic scene grouping (same location/event) needs
//! vision embeddings and is left to a later phase.

use super::AnalysisError;
use crate::domain::TimelineTime;
use std::path::Path;
use std::process::Command;

/// Minimum duration of a scene. Shorter fragments are folded into the previous scene to avoid
/// noisy single-frame cuts producing unusable micro-scenes.
pub const MIN_SCENE_SECONDS: f64 = 0.6;

/// Cuts closer together than this are treated as one cut, keeping the stronger score.
const CUT_MERGE_SECONDS: f64 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cut {
    pub time: f64,
    pub score: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct SceneGrouping {
    /// A cut at or above this score starts a new scene; weaker cuts start a new shot.
    pub scene_threshold: f32,
    /// Shots shorter than this are absorbed into the previous shot.
    pub min_shot_seconds: f64,
    /// A scene is force-closed once joining another shot would exceed this length.
    pub max_scene_seconds: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShotSpan {
    pub start: TimelineTime,
    pub end: TimelineTime,
    /// Scene-change score of the cut that starts this shot (0.0 for the first shot).
    pub score: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SceneSpan {
    pub start: TimelineTime,
    pub end: TimelineTime,
    /// Strongest cut score inside the scene.
    pub score: f32,
    pub shots: Vec<ShotSpan>,
}

/// Runs FFmpeg scene detection and returns the detected cuts in ascending time order.
///
/// The metadata filter writes `<frame>/<pts_time>/<scene_score>` blocks to `metadata_file`;
/// the file is created by FFmpeg and may not exist at all when no cut passes the threshold.
pub fn detect_cuts(
    path: &Path,
    threshold: f32,
    threads: u8,
    metadata_file: &Path,
) -> Result<Vec<Cut>, AnalysisError> {
    if let Some(parent) = metadata_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(metadata_file);

    // The filename is wrapped in single quotes so that `:` and `,` inside a cache path are not
    // mistaken for filter/option separators.
    let filter = format!(
        "select='gt(scene,{:.4})',metadata=print:file='{}'",
        threshold.clamp(0.0, 1.0),
        metadata_file.display()
    );
    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .args(["-threads", &threads.max(1).to_string()])
        .arg("-i")
        .arg(path)
        .args(["-an", "-vf"])
        .arg(&filter)
        .args(["-f", "null", "-"])
        .output()?;
    if !output.status.success() {
        return Err(AnalysisError::Command {
            stage: "シーン検出",
            message: stderr(&output),
        });
    }

    let text = std::fs::read_to_string(metadata_file).unwrap_or_default();
    Ok(parse_scene_metadata(&text))
}

/// Parses the `metadata=print` output into cuts. The file looks like:
///
/// ```text
/// frame:0    pts:25600   pts_time:2
/// lavfi.scene_score=0.400000
/// ```
pub fn parse_scene_metadata(text: &str) -> Vec<Cut> {
    let mut cuts = Vec::new();
    let mut pending_time: Option<f64> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.split("pts_time:").nth(1) {
            pending_time = rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<f64>().ok());
            continue;
        }
        if let Some(rest) = line.strip_prefix("lavfi.scene_score=")
            && let (Some(time), Ok(score)) = (pending_time.take(), rest.trim().parse::<f32>())
        {
            cuts.push(Cut { time, score });
        }
    }
    cuts.sort_by(|left, right| left.time.total_cmp(&right.time));
    cuts
}

/// Groups cuts into shots and scenes. Always returns at least one scene for a positive
/// duration, so a cut-free (static) video still becomes exactly one scene.
pub fn group_scenes(
    cuts: &[Cut],
    duration: TimelineTime,
    grouping: &SceneGrouping,
) -> Vec<SceneSpan> {
    let total = duration.as_seconds();
    if total <= 0.0 {
        return Vec::new();
    }
    let epsilon = 1e-3;

    // 1. Keep cuts strictly inside the media, collapsing near-duplicates.
    let mut boundaries: Vec<Cut> = Vec::new();
    for cut in cuts {
        if cut.time <= epsilon || cut.time >= total - epsilon {
            continue;
        }
        match boundaries.last_mut() {
            Some(last) if (cut.time - last.time).abs() < CUT_MERGE_SECONDS => {
                if cut.score > last.score {
                    last.score = cut.score;
                }
            }
            _ => boundaries.push(*cut),
        }
    }

    // 2. Shots are the intervals between boundary times, short ones absorbed into the previous.
    let mut edges: Vec<Cut> = Vec::with_capacity(boundaries.len() + 2);
    edges.push(Cut {
        time: 0.0,
        score: 0.0,
    });
    edges.extend(boundaries);
    edges.push(Cut {
        time: total,
        score: 0.0,
    });

    let mut shots: Vec<ShotSpan> = Vec::new();
    for pair in edges.windows(2) {
        let (start, end, score) = (pair[0].time, pair[1].time, pair[0].score);
        if end - start <= 0.0 {
            continue;
        }
        if let Some(previous) = shots.last_mut()
            && (end - start) < grouping.min_shot_seconds
        {
            previous.end = TimelineTime::from_seconds(end);
            previous.score = previous.score.max(score);
            continue;
        }
        shots.push(ShotSpan {
            start: TimelineTime::from_seconds(start),
            end: TimelineTime::from_seconds(end),
            score,
        });
    }
    if shots.is_empty() {
        return Vec::new();
    }
    // A shot at the very start is too early for a "previous" to absorb it, so a short leading
    // shot (a cut within the first fraction of a second) is merged forward instead.
    while shots.len() > 1
        && (shots[0].end.as_seconds() - shots[0].start.as_seconds()) < grouping.min_shot_seconds
    {
        let next = shots.remove(1);
        shots[0].end = next.end;
    }

    // 3. Group shots into scenes.
    let mut scenes: Vec<SceneSpan> = Vec::new();
    for shot in shots {
        let joins_current = match scenes.last() {
            None => false,
            Some(current) => {
                let current_seconds = current.end.as_seconds() - current.start.as_seconds();
                let joined_seconds = shot.end.as_seconds() - current.start.as_seconds();
                shot.score < grouping.scene_threshold - 1e-6
                    && !(joined_seconds > grouping.max_scene_seconds
                        && current_seconds + 1e-9 >= MIN_SCENE_SECONDS)
            }
        };
        if joins_current {
            let current = scenes.last_mut().expect("scene exists");
            current.end = shot.end;
            current.score = current.score.max(shot.score);
            current.shots.push(shot);
        } else {
            scenes.push(SceneSpan {
                start: shot.start,
                end: shot.end,
                score: shot.score,
                shots: vec![shot],
            });
        }
    }

    // 4. Fold sub-minimum scenes into their neighbour.
    let mut merged: Vec<SceneSpan> = Vec::new();
    for scene in scenes {
        let too_short = scene.end.as_seconds() - scene.start.as_seconds() < MIN_SCENE_SECONDS;
        if too_short && let Some(previous) = merged.last_mut() {
            previous.end = scene.end;
            previous.score = previous.score.max(scene.score);
            previous.shots.extend(scene.shots);
        } else {
            merged.push(scene);
        }
    }
    merged
}

fn stderr(output: &std::process::Output) -> String {
    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if message.is_empty() {
        "外部ツールが失敗しました".to_owned()
    } else {
        message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grouping() -> SceneGrouping {
        SceneGrouping {
            scene_threshold: 0.40,
            min_shot_seconds: 0.35,
            max_scene_seconds: 30.0,
        }
    }

    fn seconds(value: f64) -> TimelineTime {
        TimelineTime::from_seconds(value)
    }

    #[test]
    fn parses_metadata_blocks() {
        let text = "\
frame:0    pts:25600   pts_time:2
lavfi.scene_score=0.400000
frame:1    pts:51200   pts_time:4.5
lavfi.scene_score=0.800000
";
        let cuts = parse_scene_metadata(text);
        assert_eq!(cuts.len(), 2);
        assert!((cuts[0].time - 2.0).abs() < 1e-6);
        assert!((cuts[0].score - 0.4).abs() < 1e-6);
        assert!((cuts[1].time - 4.5).abs() < 1e-6);
    }

    #[test]
    fn static_video_becomes_one_scene() {
        let scenes = group_scenes(&[], seconds(6.0), &grouping());
        assert_eq!(scenes.len(), 1);
        assert_eq!(scenes[0].start, TimelineTime::ZERO);
        assert_eq!(scenes[0].end, seconds(6.0));
        assert_eq!(scenes[0].shots.len(), 1);
    }

    #[test]
    fn strong_cuts_start_new_scenes() {
        let cuts = [
            Cut {
                time: 2.0,
                score: 0.6,
            },
            Cut {
                time: 4.0,
                score: 0.7,
            },
        ];
        let scenes = group_scenes(&cuts, seconds(6.0), &grouping());
        assert_eq!(scenes.len(), 3);
        assert_eq!(scenes[0].start, TimelineTime::ZERO);
        assert_eq!(scenes[2].end, seconds(6.0));
        assert!((scenes[2].score - 0.7).abs() < 1e-6);
        assert!(scenes.iter().all(|scene| scene.shots.len() == 1));
    }

    #[test]
    fn weak_cuts_only_split_shots() {
        let cuts = [
            Cut {
                time: 2.0,
                score: 0.30,
            },
            Cut {
                time: 4.0,
                score: 0.35,
            },
        ];
        let scenes = group_scenes(&cuts, seconds(6.0), &grouping());
        assert_eq!(scenes.len(), 1);
        assert_eq!(scenes[0].shots.len(), 3);
    }

    #[test]
    fn short_shots_are_absorbed() {
        let cuts = [Cut {
            time: 0.1,
            score: 0.9,
        }];
        let scenes = group_scenes(&cuts, seconds(3.0), &grouping());
        assert_eq!(scenes.len(), 1);
        assert_eq!(scenes[0].shots.len(), 1);
        assert_eq!(scenes[0].end, seconds(3.0));
    }

    #[test]
    fn long_scenes_are_force_closed() {
        let mut options = grouping();
        options.max_scene_seconds = 2.5;
        let cuts = [
            Cut {
                time: 2.0,
                score: 0.30,
            },
            Cut {
                time: 4.0,
                score: 0.30,
            },
        ];
        let scenes = group_scenes(&cuts, seconds(6.0), &options);
        assert_eq!(scenes.len(), 3);
    }
}
