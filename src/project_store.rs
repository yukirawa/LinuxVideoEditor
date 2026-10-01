use crate::domain::{
    Clip, ItemId, PROJECT_SCHEMA_VERSION, Project, SpeechItem, TextItem, TimelineItem,
};
use crate::editing::validate_project;
use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn save(project: &Project, path: &Path) -> Result<()> {
    validate_project(project).context("project validation failed")?;
    let bytes = serde_json::to_vec_pretty(project)?;
    let temporary = temporary_path(path);
    let mut file = fs::File::create(&temporary)
        .with_context(|| format!("could not create {}", temporary.display()))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)
        .with_context(|| format!("could not replace {}", path.display()))?;
    Ok(())
}

pub fn load(path: &Path) -> Result<Project> {
    let bytes = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    let mut project: Project = match serde_json::from_slice(&bytes) {
        Ok(project) => project,
        Err(strict_error) => {
            // Schema 0.1/0.2 files written before the `shape_kind` fix contain a duplicate
            // `kind` key and cannot be parsed as-is. Repair that known pattern and retry.
            match repair_duplicate_shape_kind(&String::from_utf8_lossy(&bytes)) {
                Some(repaired) => serde_json::from_str(&repaired)
                    .with_context(|| format!("could not parse {}", path.display()))?,
                None => {
                    return Err(strict_error)
                        .with_context(|| format!("could not parse {}", path.display()));
                }
            }
        }
    };
    if project.schema_version.split('.').next() != Some("0") {
        bail!("unsupported project schema {}", project.schema_version);
    }
    if project.schema_version != PROJECT_SCHEMA_VERSION {
        // Only the minor version moved; the loader above already migrated the data it can.
        project.schema_version = PROJECT_SCHEMA_VERSION.to_owned();
    }
    migrate_voicevox_speeches(&mut project);
    validate_project(&project).context("project is invalid")?;
    Ok(project)
}

/// Rewrites the legacy duplicate shape key: an old shape item was serialized as
/// `{"kind": "shape", ..., "kind": "ellipse"}` (the `TimelineItem` tag plus `ShapeItem::kind`).
/// `serde_json` rejects the duplicate, so the inner key becomes `shape_kind`. The shape values
/// (`rectangle`/`ellipse`) never collide with item tags (`clip`/`text`/`shape`), and quotes
/// inside string contents are escaped by the serializer, so this replacement is unambiguous.
/// Returns `None` when nothing had to be changed.
fn repair_duplicate_shape_kind(text: &str) -> Option<String> {
    let mut repaired = text.to_owned();
    for kind in ["rectangle", "ellipse"] {
        for pattern in [
            format!("\"kind\": \"{kind}\""),
            format!("\"kind\":\"{kind}\""),
        ] {
            repaired = repaired.replace(&pattern, &format!("\"shape_kind\": \"{kind}\""));
        }
    }
    (repaired != text).then_some(repaired)
}

/// Merges legacy VOICEVOX pairs into single speech items.
///
/// Before speech items existed, generating a line added an audio-only clip and, on a separate
/// layer, a subtitle text item of the same span. Opening such a project would still show two
/// timeline items, so this migrates recognizable pairs into one [`SpeechItem`]. It is
/// deliberately conservative: only audio clips whose file was produced by the VOICEVOX bar
/// (`voicevox-*.wav`) and exactly one subtitle with the same span and the synthesized line's text
/// are merged, so a genuine audio + caption pair is left untouched. Idempotent.
pub fn migrate_voicevox_speeches(project: &mut Project) {
    let pairs = collect_voicevox_pairs(project);
    if pairs.is_empty() {
        return;
    }
    for (clip_id, text_id) in pairs {
        // Keep the subtitle's layer: it is usually the front-most one, which is where the
        // subtitle's z-order must stay for preview and export.
        let Some(target_layer_id) = project
            .sequence
            .layers
            .iter()
            .find(|layer| layer.items.iter().any(|item| item.id() == text_id))
            .map(|layer| layer.id)
        else {
            continue;
        };
        let Some(subtitle) = take_text(project, text_id) else {
            continue;
        };
        let Some(clip) = take_clip(project, clip_id) else {
            // Never lose data: put the subtitle back if the clip vanished mid-way.
            if let Some(layer) = project
                .sequence
                .layers
                .iter_mut()
                .find(|layer| layer.id == target_layer_id)
            {
                layer.items.push(TimelineItem::Text(subtitle));
            }
            continue;
        };
        let speech = SpeechItem::new(clip, subtitle);
        if let Some(layer) = project
            .sequence
            .layers
            .iter_mut()
            .find(|layer| layer.id == target_layer_id)
        {
            layer.items.push(TimelineItem::Speech(speech));
        }
    }
    // The merged speech is appended to its layer, and validation relies on items being ordered.
    for layer in &mut project.sequence.layers {
        layer.items.sort_by_key(TimelineItem::timeline_start);
    }
}

/// Finds unambiguously VOICEVOX audio + subtitle pairs, one entry per pair (clip id, text id).
fn collect_voicevox_pairs(project: &Project) -> Vec<(ItemId, ItemId)> {
    let mut pairs = Vec::new();
    let mut claimed_texts: HashSet<ItemId> = HashSet::new();
    for layer in &project.sequence.layers {
        for item in &layer.items {
            let Some(clip) = item.as_clip() else {
                continue;
            };
            let Some(asset) = project.asset(clip.asset_id) else {
                continue;
            };
            if asset.metadata.has_video
                || !asset.metadata.has_audio
                || !is_voicevox_asset(&asset.path)
            {
                continue;
            }
            // The asset name is the line's first 30 characters (plus an ellipsis when longer).
            let label = asset.name.trim_end_matches('…');
            if label.is_empty() {
                continue;
            }
            let candidates: Vec<ItemId> = project
                .sequence
                .layers
                .iter()
                .flat_map(|layer| layer.items.iter())
                .filter_map(TimelineItem::as_text)
                .filter(|text| {
                    text.timeline_start == clip.timeline_start
                        && text.duration == clip.duration()
                        && text.text.starts_with(label)
                        && !claimed_texts.contains(&text.id)
                })
                .map(|text| text.id)
                .collect();
            // Only merge when exactly one subtitle matches; otherwise the intent is unclear.
            if let [text_id] = candidates.as_slice() {
                claimed_texts.insert(*text_id);
                pairs.push((clip.id, *text_id));
            }
        }
    }
    pairs
}

/// True for a WAV produced by the VOICEVOX bar (`<cache>/voicevox/voicevox-<uuid>.wav`).
fn is_voicevox_asset(path: &Path) -> bool {
    let in_voicevox_dir = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "voicevox");
    let named_like_voicevox = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|name| name.starts_with("voicevox-") && name.ends_with(".wav"));
    in_voicevox_dir || named_like_voicevox
}

fn take_text(project: &mut Project, id: ItemId) -> Option<TextItem> {
    for layer in &mut project.sequence.layers {
        if let Some(index) = layer.items.iter().position(|item| item.id() == id) {
            if let TimelineItem::Text(text) = layer.items.remove(index) {
                return Some(text);
            }
        }
    }
    None
}

fn take_clip(project: &mut Project, id: ItemId) -> Option<Clip> {
    for layer in &mut project.sequence.layers {
        if let Some(index) = layer.items.iter().position(|item| item.id() == id) {
            if let TimelineItem::Clip(clip) = layer.items.remove(index) {
                return Some(clip);
            }
        }
    }
    None
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(".part");
    PathBuf::from(value)
}
