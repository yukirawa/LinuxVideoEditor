use crate::domain::{
    AnimProperty, AnimTrack, AssetId, Clip, ClipEffects, ClipId, ItemId, Keyframe, Layer, LayerId,
    MediaAsset, Project, SequenceSettings, ShapeId, ShapeItem, SpeechItem, TextId, TextItem,
    TimelineItem, TimelineTime, Transform,
};
use thiserror::Error;
use uuid::Uuid;

const MIN_ITEM_DURATION: TimelineTime = TimelineTime::from_micros(1_000);

#[derive(Clone, Debug)]
pub enum EditCommand {
    ImportAsset {
        asset: MediaAsset,
        layer_id: LayerId,
        timeline_start: TimelineTime,
    },
    SplitClip {
        clip_id: ClipId,
        timeline_time: TimelineTime,
    },
    /// Splits a speech (audio + subtitle) at `timeline_time` into two speech items, keeping the
    /// cut non-destructive. The audio is split by source range and each half's subtitle is
    /// re-glued to it. Used for "cut the line short" gags; the unwanted half is deleted after.
    SplitSpeech {
        speech_id: ItemId,
        timeline_time: TimelineTime,
    },
    TrimClip {
        clip_id: ClipId,
        source_in: TimelineTime,
        source_out: TimelineTime,
    },
    MoveClip {
        clip_id: ClipId,
        timeline_start: TimelineTime,
    },
    /// Generic drag-and-drop move used by the timeline UI: moves any item (clip or text)
    /// to a new position, optionally onto a different layer.
    MoveItem {
        item_id: ItemId,
        target_layer_id: LayerId,
        timeline_start: TimelineTime,
    },
    /// Drags the left edge of an item: keeps the end fixed, adjusts start (and, for
    /// clips, the in-point) in lockstep.
    ResizeItemStart {
        item_id: ItemId,
        timeline_start: TimelineTime,
    },
    /// Drags the right edge of an item: keeps the start fixed, adjusts the end.
    ResizeItemEnd {
        item_id: ItemId,
        timeline_end: TimelineTime,
    },
    DeleteItem {
        item_id: ItemId,
    },
    DeleteClip {
        clip_id: ClipId,
    },
    SetClipGain {
        clip_id: ClipId,
        gain_db: f32,
    },
    SetClipMuted {
        clip_id: ClipId,
        muted: bool,
    },
    SetClipTransform {
        clip_id: ClipId,
        transform: Transform,
    },
    SetClipOpacity {
        clip_id: ClipId,
        opacity: f32,
    },
    SetClipSpeed {
        clip_id: ClipId,
        speed: f64,
    },
    SetClipEffects {
        clip_id: ClipId,
        effects: ClipEffects,
    },
    /// Replaces (or inserts) the keyframe track for one animated property of a clip.
    SetAnimTrack {
        clip_id: ClipId,
        track: AnimTrack,
    },
    RemoveAnimTrack {
        clip_id: ClipId,
        property: AnimProperty,
    },
    /// Upserts a single keyframe on a property's track (creates the track if needed).
    SetKeyframe {
        clip_id: ClipId,
        property: AnimProperty,
        keyframe: Keyframe,
    },
    RemoveKeyframe {
        clip_id: ClipId,
        property: AnimProperty,
        offset: TimelineTime,
    },
    SetSequenceSettings {
        settings: SequenceSettings,
    },
    /// Adds a VOICEVOX line (audio + subtitle) as one item. The asset is registered in the
    /// same transaction so the speech never references a missing asset.
    AddSpeech {
        asset: MediaAsset,
        layer_id: LayerId,
        speech: SpeechItem,
    },
    AddText {
        layer_id: LayerId,
        text: TextItem,
    },
    UpdateText {
        text: TextItem,
    },
    DeleteText {
        text_id: TextId,
    },
    AddShape {
        layer_id: LayerId,
        shape: ShapeItem,
    },
    UpdateShape {
        shape: ShapeItem,
    },
    DeleteShape {
        shape_id: ShapeId,
    },
    /// Inserts a copied item (with its own already-assigned id) onto a layer. Takes a fully
    /// built item so trims, effects and keyframes survive a paste unchanged.
    PasteItem {
        item: TimelineItem,
        layer_id: LayerId,
    },
    AddLayer {
        layer: Layer,
        index: Option<usize>,
    },
    RemoveLayer {
        layer_id: LayerId,
    },
    RenameLayer {
        layer_id: LayerId,
        name: String,
    },
    SetLayerLocked {
        layer_id: LayerId,
        locked: bool,
    },
    SetLayerHidden {
        layer_id: LayerId,
        hidden: bool,
    },
    SetLayerMuted {
        layer_id: LayerId,
        muted: bool,
    },
    ReorderLayer {
        layer_id: LayerId,
        new_index: usize,
    },
}

#[derive(Debug, Error)]
pub enum EditError {
    #[error("asset not found: {0}")]
    AssetNotFound(AssetId),
    #[error("clip not found: {0}")]
    ClipNotFound(ClipId),
    #[error("item not found: {0}")]
    ItemNotFound(ItemId),
    #[error("layer not found: {0}")]
    LayerNotFound(LayerId),
    #[error("layer is locked")]
    LayerLocked,
    #[error("invalid source range")]
    InvalidSourceRange,
    #[error("split point must be inside the clip")]
    InvalidSplitPoint,
    #[error("items overlap on layer '{0}'")]
    ItemOverlap(String),
    #[error("media has no video stream")]
    NoVideoStream,
    #[error("audio gain must be between -60 dB and +12 dB")]
    InvalidGain,
    #[error("opacity must be between 0 and 100")]
    InvalidOpacity,
    #[error("transform values must be finite and scale must be positive")]
    InvalidTransform,
    #[error("speed must be finite and between 0.1 and 8.0")]
    InvalidSpeed,
    #[error("effect values are out of range")]
    InvalidEffects,
    #[error("animation track is invalid")]
    InvalidAnimTrack,
    #[error("sequence resolution must be between 16x16 and 7680x4320")]
    InvalidResolution,
    #[error("sequence frame rate must be finite and between 1 and 240 fps")]
    InvalidFrameRate,
    #[error("sequence sample rate must be between 8000 and 192000 Hz")]
    InvalidSampleRate,
    #[error("text not found: {0}")]
    TextNotFound(TextId),
    #[error("duplicate text id: {0}")]
    DuplicateTextId(TextId),
    #[error("text duration must be greater than zero")]
    InvalidTextDuration,
    #[error("text font size must be finite and between 1 and 1000")]
    InvalidFontSize,
    #[error("text style values must be finite and non-negative")]
    InvalidTextStyle,
    #[error("shape not found: {0}")]
    ShapeNotFound(ShapeId),
    #[error("duplicate shape id: {0}")]
    DuplicateShapeId(ShapeId),
    #[error("duplicate item id: {0}")]
    DuplicateItemId(ItemId),
    #[error("shape duration must be greater than zero")]
    InvalidShapeDuration,
    #[error("shape opacity must be between 0 and 100")]
    InvalidShapeOpacity,
    #[error("shape size must be finite and positive")]
    InvalidShapeSize,
    #[error("layer name must not be empty")]
    InvalidLayerName,
    #[error("cannot remove the last layer")]
    CannotRemoveLastLayer,
    #[error("unsupported project schema: {0}")]
    UnsupportedSchema(String),
}

pub fn apply_command(project: &mut Project, command: EditCommand) -> Result<(), EditError> {
    match command {
        EditCommand::ImportAsset {
            asset,
            layer_id,
            timeline_start,
        } => {
            if !asset.metadata.has_video && !asset.metadata.has_audio {
                return Err(EditError::NoVideoStream);
            }
            let frame = project.sequence.settings.frame_size();
            let clip = Clip::new(&asset, timeline_start, frame);
            let layer = layer_mut(project, layer_id)?;
            if layer.locked {
                return Err(EditError::LayerLocked);
            }
            layer.items.push(TimelineItem::Clip(clip));
            if project
                .assets
                .iter()
                .all(|existing| existing.id != asset.id)
            {
                project.assets.push(asset);
            }
        }
        EditCommand::SplitClip {
            clip_id,
            timeline_time,
        } => {
            let layer = layer_containing_item_mut(project, clip_id)?;
            if layer.locked {
                return Err(EditError::LayerLocked);
            }
            let index = layer
                .items
                .iter()
                .position(|item| item.id() == clip_id)
                .ok_or(EditError::ClipNotFound(clip_id))?;
            let TimelineItem::Clip(left) = &mut layer.items[index] else {
                return Err(EditError::ClipNotFound(clip_id));
            };
            if timeline_time <= left.timeline_start || timeline_time >= left.timeline_end() {
                return Err(EditError::InvalidSplitPoint);
            }
            let split_source_time = left.source_time_at(timeline_time);
            let split_offset = left.anim_offset_at(timeline_time);
            let mut right = left.clone();
            right.id = Uuid::new_v4();
            right.timeline_start = timeline_time;
            right.source_in = split_source_time;
            left.source_out = split_source_time;
            right.anim = left.split_anim_at(split_offset);
            layer.items.push(TimelineItem::Clip(right));
        }
        EditCommand::SplitSpeech {
            speech_id,
            timeline_time,
        } => {
            let layer = layer_containing_item_mut(project, speech_id)?;
            if layer.locked {
                return Err(EditError::LayerLocked);
            }
            let index = layer
                .items
                .iter()
                .position(|item| item.id() == speech_id)
                .ok_or(EditError::ItemNotFound(speech_id))?;
            // Build the right half first (clone + re-point the audio at the cut), then trim the
            // left half, so both halves' subtitles end up glued to their own audio.
            let right = {
                let TimelineItem::Speech(left) = &mut layer.items[index] else {
                    return Err(EditError::ItemNotFound(speech_id));
                };
                if timeline_time <= left.timeline_start() || timeline_time >= left.timeline_end() {
                    return Err(EditError::InvalidSplitPoint);
                }
                let split_source_time = left.audio.source_time_at(timeline_time);
                let split_offset = left.audio.anim_offset_at(timeline_time);
                let mut right = left.clone();
                right.audio.id = Uuid::new_v4();
                right.audio.timeline_start = timeline_time;
                right.audio.source_in = split_source_time;
                left.audio.source_out = split_source_time;
                right.audio.anim = left.audio.split_anim_at(split_offset);
                left.sync_subtitle();
                right.sync_subtitle();
                right
            };
            layer.items.push(TimelineItem::Speech(right));
        }
        EditCommand::TrimClip {
            clip_id,
            source_in,
            source_out,
        } => {
            let asset_id = project
                .clip(clip_id)
                .ok_or(EditError::ClipNotFound(clip_id))?
                .asset_id;
            let asset = project
                .asset(asset_id)
                .ok_or(EditError::AssetNotFound(asset_id))?;
            // A still image is looped, so its source range is not limited by the asset's
            // nominal length; only a real video's source has a fixed end.
            let out_of_range = !asset.metadata.is_image && source_out > asset.metadata.duration;
            let source_span = source_out - source_in;
            if source_in >= source_out || out_of_range || source_span < MIN_ITEM_DURATION
            {
                return Err(EditError::InvalidSourceRange);
            }
            let clip = clip_mut(project, clip_id)?;
            // Timeline span must stay above the minimum even when the clip is slowed down.
            let timeline_span = TimelineTime::from_micros(
                (source_span.as_micros() as f64 / clip.speed()).round() as i64,
            );
            if timeline_span < MIN_ITEM_DURATION {
                return Err(EditError::InvalidSourceRange);
            }
            clip.source_in = source_in;
            clip.source_out = source_out;
        }
        EditCommand::MoveClip {
            clip_id,
            timeline_start,
        } => {
            clip_mut(project, clip_id)?.timeline_start = timeline_start;
        }
        EditCommand::MoveItem {
            item_id,
            target_layer_id,
            timeline_start,
        } => {
            if timeline_start < TimelineTime::ZERO {
                return Err(EditError::InvalidSourceRange);
            }
            let source_layer_id = layer_containing_item_mut(project, item_id)?.id;
            if layer_ref(project, source_layer_id)?.locked
                || layer_ref(project, target_layer_id)?.locked
            {
                return Err(EditError::LayerLocked);
            }
            let source_layer = layer_mut(project, source_layer_id)?;
            let index = source_layer
                .items
                .iter()
                .position(|item| item.id() == item_id)
                .ok_or(EditError::ItemNotFound(item_id))?;
            let mut item = source_layer.items.remove(index);
            item.set_timeline_start(timeline_start);
            let target_layer = layer_mut(project, target_layer_id)?;
            target_layer.items.push(item);
        }
        EditCommand::ResizeItemStart {
            item_id,
            timeline_start,
        } => {
            if timeline_start < TimelineTime::ZERO {
                return Err(EditError::InvalidSourceRange);
            }
            let layer = layer_containing_item_mut(project, item_id)?;
            if layer.locked {
                return Err(EditError::LayerLocked);
            }
            let item = layer
                .items
                .iter_mut()
                .find(|item| item.id() == item_id)
                .ok_or(EditError::ItemNotFound(item_id))?;
            match item {
                TimelineItem::Clip(clip) => {
                    let delta = timeline_start.as_micros() - clip.timeline_start.as_micros();
                    let source_delta = (delta as f64 * clip.speed()).round() as i64;
                    let new_source_in =
                        TimelineTime::from_micros(clip.source_in.as_micros() + source_delta);
                    if new_source_in < TimelineTime::ZERO
                        || clip.source_out - new_source_in < MIN_ITEM_DURATION
                    {
                        return Err(EditError::InvalidSourceRange);
                    }
                    clip.timeline_start = timeline_start;
                    clip.source_in = new_source_in;
                }
                TimelineItem::Text(text) => {
                    if text.timeline_end() - timeline_start < MIN_ITEM_DURATION {
                        return Err(EditError::InvalidTextDuration);
                    }
                    text.duration = text.timeline_end() - timeline_start;
                    text.timeline_start = timeline_start;
                }
                TimelineItem::Shape(shape) => {
                    if shape.timeline_end() - timeline_start < MIN_ITEM_DURATION {
                        return Err(EditError::InvalidShapeDuration);
                    }
                    shape.duration = shape.timeline_end() - timeline_start;
                    shape.timeline_start = timeline_start;
                }
                TimelineItem::Speech(speech) => {
                    let delta =
                        timeline_start.as_micros() - speech.audio.timeline_start.as_micros();
                    let source_delta = (delta as f64 * speech.audio.speed()).round() as i64;
                    let new_source_in =
                        TimelineTime::from_micros(speech.audio.source_in.as_micros() + source_delta);
                    if new_source_in < TimelineTime::ZERO
                        || speech.audio.source_out - new_source_in < MIN_ITEM_DURATION
                    {
                        return Err(EditError::InvalidSourceRange);
                    }
                    speech.audio.timeline_start = timeline_start;
                    speech.audio.source_in = new_source_in;
                    speech.sync_subtitle();
                }
            }
        }
        EditCommand::ResizeItemEnd {
            item_id,
            timeline_end,
        } => {
            let layer = layer_containing_item_mut(project, item_id)?;
            if layer.locked {
                return Err(EditError::LayerLocked);
            }
            let item = layer
                .items
                .iter_mut()
                .find(|item| item.id() == item_id)
                .ok_or(EditError::ItemNotFound(item_id))?;
            match item {
                TimelineItem::Clip(clip) => {
                    let delta = timeline_end.as_micros() - clip.timeline_end().as_micros();
                    let source_delta = (delta as f64 * clip.speed()).round() as i64;
                    let new_source_out =
                        TimelineTime::from_micros(clip.source_out.as_micros() + source_delta);
                    if new_source_out - clip.source_in < MIN_ITEM_DURATION {
                        return Err(EditError::InvalidSourceRange);
                    }
                    clip.source_out = new_source_out;
                }
                TimelineItem::Text(text) => {
                    if timeline_end - text.timeline_start < MIN_ITEM_DURATION {
                        return Err(EditError::InvalidTextDuration);
                    }
                    text.duration = timeline_end - text.timeline_start;
                }
                TimelineItem::Shape(shape) => {
                    if timeline_end - shape.timeline_start < MIN_ITEM_DURATION {
                        return Err(EditError::InvalidShapeDuration);
                    }
                    shape.duration = timeline_end - shape.timeline_start;
                }
                TimelineItem::Speech(speech) => {
                    let delta = timeline_end.as_micros() - speech.audio.timeline_end().as_micros();
                    let source_delta = (delta as f64 * speech.audio.speed()).round() as i64;
                    let new_source_out =
                        TimelineTime::from_micros(speech.audio.source_out.as_micros() + source_delta);
                    if new_source_out - speech.audio.source_in < MIN_ITEM_DURATION {
                        return Err(EditError::InvalidSourceRange);
                    }
                    speech.audio.source_out = new_source_out;
                    speech.sync_subtitle();
                }
            }
        }
        EditCommand::DeleteItem { item_id } => {
            let layer = layer_containing_item_mut(project, item_id)?;
            if layer.locked {
                return Err(EditError::LayerLocked);
            }
            layer.items.retain(|item| item.id() != item_id);
        }
        EditCommand::DeleteClip { clip_id } => {
            let layer = layer_containing_item_mut(project, clip_id)?;
            if layer.locked {
                return Err(EditError::LayerLocked);
            }
            layer.items.retain(|item| item.id() != clip_id);
        }
        EditCommand::SetClipGain { clip_id, gain_db } => {
            if !gain_db.is_finite() || !(-60.0..=12.0).contains(&gain_db) {
                return Err(EditError::InvalidGain);
            }
            audio_clip_mut(project, clip_id)?.audio.gain_db = gain_db;
        }
        EditCommand::SetClipMuted { clip_id, muted } => {
            audio_clip_mut(project, clip_id)?.audio.muted = muted;
        }
        EditCommand::SetClipTransform { clip_id, transform } => {
            if !transform.is_finite() {
                return Err(EditError::InvalidTransform);
            }
            clip_mut(project, clip_id)?.transform = transform;
        }
        EditCommand::SetClipOpacity { clip_id, opacity } => {
            if !opacity.is_finite() || !(0.0..=100.0).contains(&opacity) {
                return Err(EditError::InvalidOpacity);
            }
            clip_mut(project, clip_id)?.opacity = opacity;
        }
        EditCommand::SetClipSpeed { clip_id, speed } => {
            if !speed.is_finite() || !(0.1..=8.0).contains(&speed) {
                return Err(EditError::InvalidSpeed);
            }
            let clip = clip_mut(project, clip_id)?;
            clip.speed = speed;
        }
        EditCommand::SetClipEffects { clip_id, effects } => {
            if !effects.is_valid() {
                return Err(EditError::InvalidEffects);
            }
            clip_mut(project, clip_id)?.effects = effects;
        }
        EditCommand::SetAnimTrack { clip_id, track } => {
            validate_anim_track(&track)?;
            let clip = clip_mut(project, clip_id)?;
            clip.anim.retain(|existing| existing.property != track.property);
            clip.anim.push(track);
        }
        EditCommand::RemoveAnimTrack { clip_id, property } => {
            let clip = clip_mut(project, clip_id)?;
            clip.anim.retain(|track| track.property != property);
        }
        EditCommand::SetKeyframe {
            clip_id,
            property,
            keyframe,
        } => {
            if !keyframe.value.is_finite() || keyframe.offset < TimelineTime::ZERO {
                return Err(EditError::InvalidAnimTrack);
            }
            let clip = clip_mut(project, clip_id)?;
            match clip
                .anim
                .iter_mut()
                .find(|track| track.property == property)
            {
                Some(track) => {
                    if let Some(existing) = track
                        .keys
                        .iter_mut()
                        .find(|key| key.offset == keyframe.offset)
                    {
                        *existing = keyframe;
                    } else {
                        track.keys.push(keyframe);
                        track
                            .keys
                            .sort_by_key(|key| key.offset);
                    }
                }
                None => {
                    clip.anim.push(AnimTrack {
                        property,
                        keys: vec![keyframe],
                    });
                }
            }
        }
        EditCommand::RemoveKeyframe {
            clip_id,
            property,
            offset,
        } => {
            let clip = clip_mut(project, clip_id)?;
            if let Some(track) = clip
                .anim
                .iter_mut()
                .find(|track| track.property == property)
            {
                track.keys.retain(|key| key.offset != offset);
            }
            clip.anim.retain(|track| !track.keys.is_empty());
        }
        EditCommand::SetSequenceSettings { settings } => {
            validate_sequence_settings(&settings)?;
            project.sequence.settings = settings;
        }
        EditCommand::AddSpeech {
            asset,
            layer_id,
            speech,
        } => {
            if !asset.metadata.has_video && !asset.metadata.has_audio {
                return Err(EditError::NoVideoStream);
            }
            validate_text(&speech.subtitle)?;
            if speech.audio.source_in >= speech.audio.source_out
                || (!asset.metadata.is_image && speech.audio.source_out > asset.metadata.duration)
            {
                return Err(EditError::InvalidSourceRange);
            }
            if project.item(speech.id()).is_some() {
                return Err(EditError::DuplicateTextId(speech.id()));
            }
            let layer = layer_mut(project, layer_id)?;
            if layer.locked {
                return Err(EditError::LayerLocked);
            }
            layer.items.push(TimelineItem::Speech(speech));
            if project
                .assets
                .iter()
                .all(|existing| existing.id != asset.id)
            {
                project.assets.push(asset);
            }
        }
        EditCommand::AddText { layer_id, text } => {
            validate_text(&text)?;
            if project.item(text.id).is_some() {
                return Err(EditError::DuplicateTextId(text.id));
            }
            let layer = layer_mut(project, layer_id)?;
            if layer.locked {
                return Err(EditError::LayerLocked);
            }
            layer.items.push(TimelineItem::Text(text));
        }
        EditCommand::UpdateText { text } => {
            validate_text(&text)?;
            let text_id = text.id;
            // A speech subtitle may stretch its synthesized audio, so the asset's limits are read
            // before the layer is borrowed mutably.
            let audio_limit = match project.item(text_id) {
                Some(TimelineItem::Speech(speech)) => project
                    .asset(speech.audio.asset_id)
                    .map(|asset| (asset.metadata.is_image, asset.metadata.duration)),
                _ => None,
            };
            let layer = layer_containing_item_mut(project, text_id)?;
            if layer.locked {
                return Err(EditError::LayerLocked);
            }
            let existing = layer
                .items
                .iter_mut()
                .find(|item| item.id() == text_id)
                .ok_or(EditError::TextNotFound(text_id))?;
            match existing {
                TimelineItem::Text(slot) => *slot = text,
                TimelineItem::Speech(speech) => {
                    // The subtitle drives the line's span; the audio's source range follows so the
                    // speech stays one item whose audio and subtitle match. With playback speed
                    // the source span is the timeline span scaled by speed.
                    if let Some((is_image, asset_duration)) = audio_limit {
                        let source_in = speech.audio.source_in;
                        let source_delta =
                            (text.duration.as_micros() as f64 * speech.audio.speed()).round() as i64;
                        let mut source_out = TimelineTime::from_micros(
                            source_in.as_micros() + source_delta,
                        );
                        if !is_image && source_out > asset_duration {
                            source_out = asset_duration;
                        }
                        if source_out - source_in < MIN_ITEM_DURATION {
                            return Err(EditError::InvalidSourceRange);
                        }
                        speech.audio.source_out = source_out;
                    }
                    speech.audio.timeline_start = text.timeline_start;
                    speech.subtitle = text;
                    speech.sync_subtitle();
                }
                _ => return Err(EditError::TextNotFound(text_id)),
            }
        }
        EditCommand::DeleteText { text_id } => {
            let layer = layer_containing_item_mut(project, text_id)?;
            if layer.locked {
                return Err(EditError::LayerLocked);
            }
            let before = layer.items.len();
            layer.items.retain(|item| item.id() != text_id);
            if layer.items.len() == before {
                return Err(EditError::TextNotFound(text_id));
            }
        }
        EditCommand::AddShape { layer_id, shape } => {
            validate_shape(&shape)?;
            if project.item(shape.id).is_some() {
                return Err(EditError::DuplicateShapeId(shape.id));
            }
            let layer = layer_mut(project, layer_id)?;
            if layer.locked {
                return Err(EditError::LayerLocked);
            }
            layer.items.push(TimelineItem::Shape(shape));
        }
        EditCommand::UpdateShape { shape } => {
            validate_shape(&shape)?;
            let shape_id = shape.id;
            let layer = layer_containing_item_mut(project, shape_id)?;
            if layer.locked {
                return Err(EditError::LayerLocked);
            }
            let existing = layer
                .items
                .iter_mut()
                .find(|item| item.id() == shape_id)
                .ok_or(EditError::ShapeNotFound(shape_id))?;
            *existing = TimelineItem::Shape(shape);
        }
        EditCommand::DeleteShape { shape_id } => {
            let layer = layer_containing_item_mut(project, shape_id)?;
            if layer.locked {
                return Err(EditError::LayerLocked);
            }
            let before = layer.items.len();
            layer.items.retain(|item| item.id() != shape_id);
            if layer.items.len() == before {
                return Err(EditError::ShapeNotFound(shape_id));
            }
        }
        EditCommand::PasteItem { item, layer_id } => {
            match &item {
                TimelineItem::Clip(clip) => validate_asset_clip(project, clip)?,
                TimelineItem::Text(text) => validate_text(text)?,
                TimelineItem::Shape(shape) => validate_shape(shape)?,
                TimelineItem::Speech(speech) => {
                    validate_text(&speech.subtitle)?;
                    validate_asset_clip(project, &speech.audio)?;
                }
            }
            if project.item(item.id()).is_some() {
                return Err(EditError::DuplicateItemId(item.id()));
            }
            let layer = layer_mut(project, layer_id)?;
            if layer.locked {
                return Err(EditError::LayerLocked);
            }
            layer.items.push(item);
        }
        EditCommand::AddLayer { layer, index } => {
            if layer.name.trim().is_empty() {
                return Err(EditError::InvalidLayerName);
            }
            let index = index.unwrap_or(0).min(project.sequence.layers.len());
            project.sequence.layers.insert(index, layer);
        }
        EditCommand::RemoveLayer { layer_id } => {
            if project.sequence.layers.len() <= 1 {
                return Err(EditError::CannotRemoveLastLayer);
            }
            let before = project.sequence.layers.len();
            project.sequence.layers.retain(|layer| layer.id != layer_id);
            if project.sequence.layers.len() == before {
                return Err(EditError::LayerNotFound(layer_id));
            }
        }
        EditCommand::RenameLayer { layer_id, name } => {
            if name.trim().is_empty() {
                return Err(EditError::InvalidLayerName);
            }
            layer_mut(project, layer_id)?.name = name;
        }
        EditCommand::SetLayerLocked { layer_id, locked } => {
            layer_mut(project, layer_id)?.locked = locked;
        }
        EditCommand::SetLayerHidden { layer_id, hidden } => {
            layer_mut(project, layer_id)?.hidden = hidden;
        }
        EditCommand::SetLayerMuted { layer_id, muted } => {
            layer_mut(project, layer_id)?.muted = muted;
        }
        EditCommand::ReorderLayer {
            layer_id,
            new_index,
        } => {
            let current_index = project
                .sequence
                .layers
                .iter()
                .position(|layer| layer.id == layer_id)
                .ok_or(EditError::LayerNotFound(layer_id))?;
            let layer = project.sequence.layers.remove(current_index);
            let new_index = new_index.min(project.sequence.layers.len());
            project.sequence.layers.insert(new_index, layer);
        }
    }

    for layer in &mut project.sequence.layers {
        layer.items.sort_by_key(TimelineItem::timeline_start);
    }
    validate_project(project)
}

pub fn validate_project(project: &Project) -> Result<(), EditError> {
    if project.schema_version.split('.').next() != Some("0") {
        return Err(EditError::UnsupportedSchema(project.schema_version.clone()));
    }

    validate_sequence_settings(&project.sequence.settings)?;

    let mut seen_items = std::collections::HashSet::new();
    for layer in &project.sequence.layers {
        for item in &layer.items {
            if let TimelineItem::Text(text) = item {
                validate_text(text)?;
            }
            if let TimelineItem::Shape(shape) = item {
                validate_shape(shape)?;
            }
            if let TimelineItem::Speech(speech) = item {
                validate_text(&speech.subtitle)?;
            }
            if !seen_items.insert(item.id()) {
                return Err(EditError::DuplicateTextId(item.id()));
            }
        }

        for pair in layer.items.windows(2) {
            if pair[0].timeline_end() > pair[1].timeline_start() {
                return Err(EditError::ItemOverlap(layer.name.clone()));
            }
        }

        for item in &layer.items {
            if let TimelineItem::Clip(clip) = item {
                validate_asset_clip(project, clip)?;
            }
            if let TimelineItem::Speech(speech) = item {
                validate_asset_clip(project, &speech.audio)?;
            }
        }
    }
    Ok(())
}

/// Validates a clip that references a media asset. Shared by plain clips and the audio half of a
/// speech item.
fn validate_asset_clip(project: &Project, clip: &Clip) -> Result<(), EditError> {
    let asset = project
        .asset(clip.asset_id)
        .ok_or(EditError::AssetNotFound(clip.asset_id))?;
    // Still images loop, so only real footage is bounded by the asset's length.
    let out_of_range = !asset.metadata.is_image && clip.source_out > asset.metadata.duration;
    if clip.source_in >= clip.source_out || out_of_range {
        return Err(EditError::InvalidSourceRange);
    }
    if !clip.transform.is_finite() {
        return Err(EditError::InvalidTransform);
    }
    if !clip.opacity.is_finite() || !(0.0..=100.0).contains(&clip.opacity) {
        return Err(EditError::InvalidOpacity);
    }
    if !clip.speed.is_finite() || !(0.1..=8.0).contains(&clip.speed) {
        return Err(EditError::InvalidSpeed);
    }
    if !clip.effects.is_valid() {
        return Err(EditError::InvalidEffects);
    }
    for track in &clip.anim {
        validate_anim_track(track)?;
    }
    Ok(())
}

fn validate_anim_track(track: &AnimTrack) -> Result<(), EditError> {
    if track.keys.len() > 10_000 {
        return Err(EditError::InvalidAnimTrack);
    }
    for key in &track.keys {
        if !key.value.is_finite() || key.offset < TimelineTime::ZERO {
            return Err(EditError::InvalidAnimTrack);
        }
    }
    Ok(())
}

fn validate_sequence_settings(settings: &SequenceSettings) -> Result<(), EditError> {
    if !(16..=7_680).contains(&settings.width) || !(16..=4_320).contains(&settings.height) {
        return Err(EditError::InvalidResolution);
    }
    if !settings.frame_rate.is_finite() || !(1.0..=240.0).contains(&settings.frame_rate) {
        return Err(EditError::InvalidFrameRate);
    }
    if !(8_000..=192_000).contains(&settings.sample_rate) {
        return Err(EditError::InvalidSampleRate);
    }
    Ok(())
}

fn validate_text(text: &TextItem) -> Result<(), EditError> {
    if text.duration <= TimelineTime::ZERO {
        return Err(EditError::InvalidTextDuration);
    }
    if !text.transform.is_finite() {
        return Err(EditError::InvalidTransform);
    }
    if !text.font_size.is_finite() || !(1.0..=1000.0).contains(&text.font_size) {
        return Err(EditError::InvalidFontSize);
    }
    if !text.line_spacing.is_finite()
        || !text.outline_width.is_finite()
        || text.outline_width < 0.0
        || !text.shadow_offset_x.is_finite()
        || !text.shadow_offset_y.is_finite()
        || !text.background_padding.is_finite()
        || text.background_padding < 0.0
    {
        return Err(EditError::InvalidTextStyle);
    }
    Ok(())
}

fn validate_shape(shape: &ShapeItem) -> Result<(), EditError> {
    if shape.duration <= TimelineTime::ZERO {
        return Err(EditError::InvalidShapeDuration);
    }
    if !shape.transform.is_finite() {
        return Err(EditError::InvalidTransform);
    }
    if !shape.opacity.is_finite() || !(0.0..=100.0).contains(&shape.opacity) {
        return Err(EditError::InvalidShapeOpacity);
    }
    if !shape.width.is_finite()
        || !shape.height.is_finite()
        || shape.width <= 0.0
        || shape.height <= 0.0
    {
        return Err(EditError::InvalidShapeSize);
    }
    Ok(())
}

fn layer_ref(project: &Project, layer_id: LayerId) -> Result<&crate::domain::Layer, EditError> {
    project
        .layer(layer_id)
        .ok_or(EditError::LayerNotFound(layer_id))
}

fn layer_mut(
    project: &mut Project,
    layer_id: LayerId,
) -> Result<&mut crate::domain::Layer, EditError> {
    project
        .sequence
        .layers
        .iter_mut()
        .find(|layer| layer.id == layer_id)
        .ok_or(EditError::LayerNotFound(layer_id))
}

fn layer_containing_item_mut(
    project: &mut Project,
    item_id: ItemId,
) -> Result<&mut crate::domain::Layer, EditError> {
    project
        .sequence
        .layers
        .iter_mut()
        .find(|layer| layer.items.iter().any(|item| item.id() == item_id))
        .ok_or(EditError::ItemNotFound(item_id))
}

fn clip_mut(project: &mut Project, clip_id: ClipId) -> Result<&mut Clip, EditError> {
    layer_containing_item_mut(project, clip_id)?
        .items
        .iter_mut()
        .find_map(|item| item.as_clip_mut().filter(|clip| clip.id == clip_id))
        .ok_or(EditError::ClipNotFound(clip_id))
}

/// Like [`clip_mut`] but also reaches the synthesized audio of a speech item, so volume and mute
/// apply to a VOICEVOX line just as they do to a plain clip.
fn audio_clip_mut(project: &mut Project, clip_id: ClipId) -> Result<&mut Clip, EditError> {
    layer_containing_item_mut(project, clip_id)?
        .items
        .iter_mut()
        .find_map(|item| item.audio_clip_mut().filter(|clip| clip.id == clip_id))
        .ok_or(EditError::ClipNotFound(clip_id))
}
