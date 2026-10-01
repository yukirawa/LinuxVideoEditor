use super::TimelineTime;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

pub type ProjectId = Uuid;
pub type AssetId = Uuid;
pub type LayerId = Uuid;
/// A single addressable object on the timeline (clip or text). Kept generic so future
/// item kinds (images, shapes, voice lines, ...) can be added without reshaping layers.
pub type ItemId = Uuid;
pub type ClipId = ItemId;
pub type TextId = ItemId;
pub type ShapeId = ItemId;

pub const PROJECT_SCHEMA_VERSION: &str = "0.3";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Project {
    pub schema_version: String,
    pub id: ProjectId,
    pub title: String,
    pub assets: Vec<MediaAsset>,
    pub sequence: Sequence,
}

impl Project {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            schema_version: PROJECT_SCHEMA_VERSION.to_owned(),
            id: Uuid::new_v4(),
            title: title.into(),
            assets: Vec::new(),
            sequence: Sequence::default(),
        }
    }

    pub fn asset(&self, id: AssetId) -> Option<&MediaAsset> {
        self.assets.iter().find(|asset| asset.id == id)
    }

    pub fn layer(&self, id: LayerId) -> Option<&Layer> {
        self.sequence.layers.iter().find(|layer| layer.id == id)
    }

    pub fn item(&self, id: ItemId) -> Option<&TimelineItem> {
        self.sequence
            .layers
            .iter()
            .flat_map(|layer| &layer.items)
            .find(|item| item.id() == id)
    }

    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.item(id).and_then(TimelineItem::as_clip)
    }

    pub fn text(&self, id: TextId) -> Option<&TextItem> {
        self.item(id).and_then(TimelineItem::as_text)
    }

    /// Finds which layer currently owns the given item, if any.
    pub fn layer_of_item(&self, id: ItemId) -> Option<&Layer> {
        self.sequence
            .layers
            .iter()
            .find(|layer| layer.items.iter().any(|item| item.id() == id))
    }

    pub fn duration(&self) -> TimelineTime {
        self.sequence
            .layers
            .iter()
            .flat_map(|layer| &layer.items)
            .map(TimelineItem::timeline_end)
            .max()
            .unwrap_or(TimelineTime::ZERO)
    }

    /// Every visual clip (video or still image) visible at `time`, in back-to-front paint
    /// order (front-most layer's clip last, so it draws on top). Mirrors export's composite
    /// loop, so the live preview stack matches what export renders.
    pub fn visible_visual_clips_at(&self, time: TimelineTime) -> Vec<&Clip> {
        self.sequence
            .layers
            .iter()
            .rev()
            .filter(|layer| !layer.hidden)
            .flat_map(|layer| layer.items.iter())
            .filter_map(|item| item.as_clip())
            .filter(|clip| {
                self.asset(clip.asset_id)
                    .is_some_and(|asset| asset.metadata.has_video)
            })
            .filter(|clip| clip.contains_timeline_time(time))
            .collect()
    }

    /// Every audio-bearing clip audible at `time` (its layer not muted, the clip not muted,
    /// the asset carrying audio). Overlapping entries are meant to play simultaneously, the
    /// same set export feeds to `amix`.
    pub fn audible_clips_at(&self, time: TimelineTime) -> Vec<&Clip> {
        self.sequence
            .layers
            .iter()
            .filter(|layer| !layer.muted)
            .flat_map(|layer| layer.items.iter())
            .filter_map(|item| item.audio_clip())
            .filter(|clip| clip.contains_timeline_time(time))
            .filter(|clip| !clip.audio.muted)
            .filter(|clip| {
                self.asset(clip.asset_id)
                    .is_some_and(|asset| asset.metadata.has_audio)
            })
            .collect()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MediaAsset {
    pub id: AssetId,
    pub name: String,
    pub path: PathBuf,
    pub metadata: MediaMetadata,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct MediaMetadata {
    pub duration: TimelineTime,
    pub width: u32,
    pub height: u32,
    pub frame_rate: f64,
    pub has_video: bool,
    pub has_audio: bool,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    /// A still image (PNG/JPEG/WebP/...). It renders like a video clip but has no intrinsic
    /// duration and can be stretched freely, so the source range is not bounded by `duration`.
    #[serde(default)]
    pub is_image: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Sequence {
    pub id: Uuid,
    pub settings: SequenceSettings,
    /// General-purpose layer stack. Index 0 renders in front (topmost), matching how the
    /// layer list is displayed. Any layer can hold any mix of item kinds.
    pub layers: Vec<Layer>,
}

impl Default for Sequence {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            settings: SequenceSettings::default(),
            layers: vec![Layer::new("レイヤー 1")],
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SequenceSettings {
    pub width: u32,
    pub height: u32,
    pub frame_rate: f64,
    pub sample_rate: u32,
}

impl Default for SequenceSettings {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            frame_rate: 30.0,
            sample_rate: 48_000,
        }
    }
}

impl SequenceSettings {
    pub fn frame_size(&self) -> (f32, f32) {
        (self.width as f32, self.height as f32)
    }
}

/// A row on the timeline. Generic on purpose: unlike a fixed "video track" / "text track"
/// split, any layer can hold clips, text, or (in the future) other item kinds, matching
/// how layer-based tools such as YMM4 work.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub muted: bool,
    pub items: Vec<TimelineItem>,
}

impl Layer {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            locked: false,
            hidden: false,
            muted: false,
            items: Vec::new(),
        }
    }

    pub fn duration(&self) -> TimelineTime {
        self.items
            .iter()
            .map(TimelineItem::timeline_end)
            .max()
            .unwrap_or(TimelineTime::ZERO)
    }
}

/// Anchor point used both for on-canvas placement math and for the export compositor.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Anchor {
    pub const ALL: [Anchor; 9] = [
        Anchor::TopLeft,
        Anchor::Top,
        Anchor::TopRight,
        Anchor::Left,
        Anchor::Center,
        Anchor::Right,
        Anchor::BottomLeft,
        Anchor::Bottom,
        Anchor::BottomRight,
    ];

    /// (horizontal, vertical) fraction of the object's own box that sits at (x, y).
    pub const fn fraction(self) -> (f32, f32) {
        match self {
            Anchor::TopLeft => (0.0, 0.0),
            Anchor::Top => (0.5, 0.0),
            Anchor::TopRight => (1.0, 0.0),
            Anchor::Left => (0.0, 0.5),
            Anchor::Center => (0.5, 0.5),
            Anchor::Right => (1.0, 0.5),
            Anchor::BottomLeft => (0.0, 1.0),
            Anchor::Bottom => (0.5, 1.0),
            Anchor::BottomRight => (1.0, 1.0),
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Anchor::TopLeft => "左上",
            Anchor::Top => "上",
            Anchor::TopRight => "右上",
            Anchor::Left => "左",
            Anchor::Center => "中央",
            Anchor::Right => "右",
            Anchor::BottomLeft => "左下",
            Anchor::Bottom => "下",
            Anchor::BottomRight => "右下",
        }
    }
}

/// Shared placement data for anything drawn on the canvas: pixel-precise position (in
/// sequence pixel space), an anchor point, a uniform scale percentage and a rotation angle.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct Transform {
    pub x: f32,
    pub y: f32,
    pub anchor: Anchor,
    pub scale: f32,
    /// Rotation around the anchor point, in degrees (clockwise). `0.0` keeps old projects.
    #[serde(default)]
    pub rotation_degrees: f32,
}

impl Transform {
    pub fn centered(frame: (f32, f32)) -> Self {
        Self {
            x: frame.0 / 2.0,
            y: frame.1 / 2.0,
            anchor: Anchor::Center,
            scale: 100.0,
            rotation_degrees: 0.0,
        }
    }

    pub fn is_finite(&self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.scale.is_finite()
            && self.scale > 0.0
            && self.rotation_degrees.is_finite()
    }
}

/// Per-clip color / blur effects applied on top of the source video.
/// Defaults match "no effect", so older projects load unchanged.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct ClipEffects {
    /// FFmpeg `eq=brightness` range: -1.0 ..= 1.0 (0.0 = unchanged).
    pub brightness: f32,
    /// FFmpeg `eq=contrast` range: 0.0 ..= 2.0 (1.0 = unchanged).
    pub contrast: f32,
    /// FFmpeg `eq=saturation` range: 0.0 ..= 2.0 (1.0 = unchanged).
    pub saturation: f32,
    /// Hue shift in degrees.
    pub hue_degrees: f32,
    /// Gaussian blur sigma (0.0 = sharp).
    pub blur_radius: f32,
    pub grayscale: bool,
    pub invert: bool,
}

impl Default for ClipEffects {
    fn default() -> Self {
        Self {
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            hue_degrees: 0.0,
            blur_radius: 0.0,
            grayscale: false,
            invert: false,
        }
    }
}

impl ClipEffects {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn is_finite(&self) -> bool {
        self.brightness.is_finite()
            && self.contrast.is_finite()
            && self.saturation.is_finite()
            && self.hue_degrees.is_finite()
            && self.blur_radius.is_finite()
    }

    pub fn is_valid(&self) -> bool {
        self.is_finite()
            && (-1.0..=1.0).contains(&self.brightness)
            && (0.0..=2.0).contains(&self.contrast)
            && (0.0..=2.0).contains(&self.saturation)
            && self.blur_radius >= 0.0
            && self.blur_radius <= 64.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnimProperty {
    X,
    Y,
    Scale,
    Rotation,
    Opacity,
    Volume,
    Brightness,
    Contrast,
    Saturation,
    Hue,
    Blur,
}

impl AnimProperty {
    pub fn label(self) -> &'static str {
        match self {
            Self::X => "X座標",
            Self::Y => "Y座標",
            Self::Scale => "拡大率",
            Self::Rotation => "回転",
            Self::Opacity => "不透明度",
            Self::Volume => "音量",
            Self::Brightness => "明るさ",
            Self::Contrast => "コントラスト",
            Self::Saturation => "彩度",
            Self::Hue => "色相",
            Self::Blur => "ぼかし",
        }
    }

    pub const ALL: [AnimProperty; 11] = [
        Self::X,
        Self::Y,
        Self::Scale,
        Self::Rotation,
        Self::Opacity,
        Self::Volume,
        Self::Brightness,
        Self::Contrast,
        Self::Saturation,
        Self::Hue,
        Self::Blur,
    ];
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Easing {
    #[default]
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
}

impl Easing {
    /// Interpolates `t` in 0..=1 with the selected curve.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::EaseIn => t * t,
            Self::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            Self::EaseInOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    1.0 - 2.0 * (1.0 - t) * (1.0 - t)
                }
            }
        }
    }
}

/// A single animated value sample, offset from the owning item's `timeline_start`.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct Keyframe {
    pub offset: TimelineTime,
    pub value: f32,
    #[serde(default)]
    pub easing: Easing,
}

/// Keyframe track for one animatable property of an item.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AnimTrack {
    pub property: AnimProperty,
    pub keys: Vec<Keyframe>,
}

impl AnimTrack {
    /// Value at `offset` (relative to item start), interpolating between keys.
    /// Returns `None` when the track has no keys.
    pub fn sample(&self, offset: TimelineTime) -> Option<f32> {
        if self.keys.is_empty() {
            return None;
        }
        if self.keys.len() == 1 || offset <= self.keys[0].offset {
            return Some(self.keys[0].value);
        }
        let last = self.keys.last()?;
        if offset >= last.offset {
            return Some(last.value);
        }
        for pair in self.keys.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if offset >= a.offset && offset <= b.offset {
                let span = (b.offset - a.offset).as_micros() as f32;
                if span <= f32::EPSILON {
                    return Some(b.value);
                }
                let t = (offset - a.offset).as_micros() as f32 / span;
                let eased = a.easing.apply(t);
                return Some(a.value + (b.value - a.value) * eased);
            }
        }
        Some(last.value)
    }

    /// Re-base key offsets after a split at `split_offset` (relative to item start).
    /// Keys before the cut stay; keys after keep their absolute time by subtracting the cut.
    pub fn split_at(&self, split_offset: TimelineTime) -> (AnimTrack, AnimTrack) {
        let mut left_keys = Vec::new();
        let mut right_keys = Vec::new();
        for key in &self.keys {
            if key.offset < split_offset {
                left_keys.push(*key);
            } else {
                right_keys.push(Keyframe {
                    offset: key.offset - split_offset,
                    value: key.value,
                    easing: key.easing,
                });
            }
        }
        (
            AnimTrack {
                property: self.property,
                keys: left_keys,
            },
            AnimTrack {
                property: self.property,
                keys: right_keys,
            },
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

impl Default for TextAlign {
    fn default() -> Self {
        Self::Center
    }
}

/// A single object on the timeline. Kept as an enum (instead of separate per-kind
/// vectors) so a layer can freely mix clips and text, and so new kinds can be added later.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TimelineItem {
    Clip(Clip),
    Text(TextItem),
    Shape(ShapeItem),
    /// A VOICEVOX line: the synthesized audio and its subtitle as one item.
    Speech(SpeechItem),
}

impl TimelineItem {
    pub fn id(&self) -> ItemId {
        match self {
            Self::Clip(clip) => clip.id,
            Self::Text(text) => text.id,
            Self::Shape(shape) => shape.id,
            Self::Speech(speech) => speech.id(),
        }
    }

    pub fn timeline_start(&self) -> TimelineTime {
        match self {
            Self::Clip(clip) => clip.timeline_start,
            Self::Text(text) => text.timeline_start,
            Self::Shape(shape) => shape.timeline_start,
            Self::Speech(speech) => speech.timeline_start(),
        }
    }

    pub fn set_timeline_start(&mut self, value: TimelineTime) {
        match self {
            Self::Clip(clip) => clip.timeline_start = value,
            Self::Text(text) => text.timeline_start = value,
            Self::Shape(shape) => shape.timeline_start = value,
            Self::Speech(speech) => speech.set_timeline_start(value),
        }
    }

    pub fn duration(&self) -> TimelineTime {
        match self {
            Self::Clip(clip) => clip.duration(),
            Self::Text(text) => text.duration,
            Self::Shape(shape) => shape.duration,
            Self::Speech(speech) => speech.duration(),
        }
    }

    pub fn timeline_end(&self) -> TimelineTime {
        self.timeline_start() + self.duration()
    }

    pub fn contains_timeline_time(&self, time: TimelineTime) -> bool {
        self.timeline_start() <= time && time < self.timeline_end()
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Clip(clip) => &clip.name,
            Self::Text(text) => &text.text,
            Self::Shape(shape) => shape.kind.label(),
            Self::Speech(speech) => &speech.subtitle.text,
        }
    }

    pub fn as_clip(&self) -> Option<&Clip> {
        match self {
            Self::Clip(clip) => Some(clip),
            Self::Text(_) | Self::Shape(_) | Self::Speech(_) => None,
        }
    }

    pub fn as_clip_mut(&mut self) -> Option<&mut Clip> {
        match self {
            Self::Clip(clip) => Some(clip),
            Self::Text(_) | Self::Shape(_) | Self::Speech(_) => None,
        }
    }

    /// The audio-bearing clip of this item. A speech's synthesized audio is returned here so
    /// export and audio preview include it, while UI clip styling keeps using [`Self::as_clip`]
    /// so a speech is never mistaken for a video clip.
    pub fn audio_clip(&self) -> Option<&Clip> {
        match self {
            Self::Clip(clip) => Some(clip),
            Self::Speech(speech) => Some(&speech.audio),
            Self::Text(_) | Self::Shape(_) => None,
        }
    }

    pub fn audio_clip_mut(&mut self) -> Option<&mut Clip> {
        match self {
            Self::Clip(clip) => Some(clip),
            Self::Speech(speech) => Some(&mut speech.audio),
            Self::Text(_) | Self::Shape(_) => None,
        }
    }

    pub fn as_text(&self) -> Option<&TextItem> {
        match self {
            Self::Text(text) => Some(text),
            // A speech's subtitle is a full text item, so preview, export and font loading
            // treat it exactly like a plain text overlay.
            Self::Speech(speech) => Some(&speech.subtitle),
            Self::Clip(_) | Self::Shape(_) => None,
        }
    }

    pub fn as_text_mut(&mut self) -> Option<&mut TextItem> {
        match self {
            Self::Text(text) => Some(text),
            Self::Speech(speech) => Some(&mut speech.subtitle),
            Self::Clip(_) | Self::Shape(_) => None,
        }
    }

    pub fn as_shape(&self) -> Option<&ShapeItem> {
        match self {
            Self::Shape(shape) => Some(shape),
            Self::Clip(_) | Self::Text(_) | Self::Speech(_) => None,
        }
    }

    pub fn as_shape_mut(&mut self) -> Option<&mut ShapeItem> {
        match self {
            Self::Shape(shape) => Some(shape),
            Self::Clip(_) | Self::Text(_) | Self::Speech(_) => None,
        }
    }

    pub fn as_speech(&self) -> Option<&SpeechItem> {
        match self {
            Self::Speech(speech) => Some(speech),
            Self::Clip(_) | Self::Text(_) | Self::Shape(_) => None,
        }
    }

    pub fn as_speech_mut(&mut self) -> Option<&mut SpeechItem> {
        match self {
            Self::Speech(speech) => Some(speech),
            Self::Clip(_) | Self::Text(_) | Self::Shape(_) => None,
        }
    }

    /// Deep-clones this item with fresh item ids so a copy can sit alongside the original on
    /// the timeline. A speech assigns one new id shared by its audio and subtitle.
    pub fn detached_copy(&self) -> TimelineItem {
        let mut copy = self.clone();
        match &mut copy {
            Self::Clip(clip) => clip.id = Uuid::new_v4(),
            Self::Text(text) => text.id = Uuid::new_v4(),
            Self::Shape(shape) => shape.id = Uuid::new_v4(),
            Self::Speech(speech) => {
                speech.audio.id = Uuid::new_v4();
                speech.sync_subtitle();
            }
        }
        copy
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Clip {
    pub id: ClipId,
    pub asset_id: AssetId,
    pub name: String,
    pub timeline_start: TimelineTime,
    pub source_in: TimelineTime,
    pub source_out: TimelineTime,
    pub audio: AudioSettings,
    #[serde(default = "Clip::default_transform")]
    pub transform: Transform,
    #[serde(default = "Clip::default_opacity")]
    pub opacity: f32,
    /// Playback rate. 1.0 = normal, 2.0 = double speed (half the timeline length).
    #[serde(default = "Clip::default_speed")]
    pub speed: f64,
    #[serde(default)]
    pub effects: ClipEffects,
    /// Keyframe tracks overriding static transform / opacity / effects / volume over time.
    #[serde(default)]
    pub anim: Vec<AnimTrack>,
}

impl Clip {
    pub fn new(asset: &MediaAsset, timeline_start: TimelineTime, frame: (f32, f32)) -> Self {
        Self {
            id: Uuid::new_v4(),
            asset_id: asset.id,
            name: asset.name.clone(),
            timeline_start,
            source_in: TimelineTime::ZERO,
            source_out: asset.metadata.duration,
            audio: AudioSettings::default(),
            transform: Transform::centered(frame),
            opacity: Self::default_opacity(),
            speed: Self::default_speed(),
            effects: ClipEffects::default(),
            anim: Vec::new(),
        }
    }

    fn default_transform() -> Transform {
        Transform {
            x: 0.0,
            y: 0.0,
            anchor: Anchor::Center,
            scale: 100.0,
            rotation_degrees: 0.0,
        }
    }

    fn default_opacity() -> f32 {
        100.0
    }

    pub fn default_speed() -> f64 {
        1.0
    }

    pub fn speed(&self) -> f64 {
        if self.speed.is_finite() && self.speed > 0.0 {
            self.speed
        } else {
            Self::default_speed()
        }
    }

    pub fn set_speed(&mut self, speed: f64) -> bool {
        if !speed.is_finite() || !(0.1..=8.0).contains(&speed) {
            return false;
        }
        self.speed = speed;
        true
    }

    /// Timeline duration. Speed stretches or shrinks how much timeline the source range covers.
    pub fn duration(&self) -> TimelineTime {
        let source_span = (self.source_out - self.source_in).as_micros() as f64;
        let timeline_micros = source_span / self.speed();
        TimelineTime::from_micros(timeline_micros.round().max(0.0) as i64)
    }

    pub fn timeline_end(&self) -> TimelineTime {
        self.timeline_start + self.duration()
    }

    pub fn contains_timeline_time(&self, time: TimelineTime) -> bool {
        self.timeline_start <= time && time < self.timeline_end()
    }

    /// Source-media time corresponding to a timeline position (accounts for playback speed).
    pub fn source_time_at(&self, time: TimelineTime) -> TimelineTime {
        let elapsed = (time - self.timeline_start).as_micros() as f64;
        TimelineTime::from_micros(
            (self.source_in.as_micros() as f64 + elapsed * self.speed()).round() as i64,
        )
    }

    /// Keyframe offset (relative to clip start) corresponding to a timeline position.
    pub fn anim_offset_at(&self, time: TimelineTime) -> TimelineTime {
        time - self.timeline_start
    }

    pub fn track(&self, property: AnimProperty) -> Option<&AnimTrack> {
        self.anim.iter().find(|track| track.property == property)
    }

    /// Samples one animated property at `offset` (relative to clip start).
    pub fn sample_anim(&self, property: AnimProperty, offset: TimelineTime) -> Option<f32> {
        self.track(property)?.sample(offset)
    }

    /// Transform with animated overrides applied at `offset`.
    pub fn effective_transform(&self, offset: TimelineTime) -> Transform {
        let mut transform = self.transform;
        if let Some(value) = self.sample_anim(AnimProperty::X, offset) {
            transform.x = value;
        }
        if let Some(value) = self.sample_anim(AnimProperty::Y, offset) {
            transform.y = value;
        }
        if let Some(value) = self.sample_anim(AnimProperty::Scale, offset) {
            transform.scale = value;
        }
        if let Some(value) = self.sample_anim(AnimProperty::Rotation, offset) {
            transform.rotation_degrees = value;
        }
        transform
    }

    /// Opacity percentage with animated override applied at `offset`.
    pub fn effective_opacity(&self, offset: TimelineTime) -> f32 {
        self.sample_anim(AnimProperty::Opacity, offset)
            .unwrap_or(self.opacity)
    }

    /// Effects with animated overrides applied at `offset`.
    pub fn effective_effects(&self, offset: TimelineTime) -> ClipEffects {
        let mut effects = self.effects;
        if let Some(value) = self.sample_anim(AnimProperty::Brightness, offset) {
            effects.brightness = value;
        }
        if let Some(value) = self.sample_anim(AnimProperty::Contrast, offset) {
            effects.contrast = value;
        }
        if let Some(value) = self.sample_anim(AnimProperty::Saturation, offset) {
            effects.saturation = value;
        }
        if let Some(value) = self.sample_anim(AnimProperty::Hue, offset) {
            effects.hue_degrees = value;
        }
        if let Some(value) = self.sample_anim(AnimProperty::Blur, offset) {
            effects.blur_radius = value;
        }
        effects
    }

    /// Gain dB with animated override applied at `offset`.
    pub fn effective_gain_db(&self, offset: TimelineTime) -> f32 {
        self.sample_anim(AnimProperty::Volume, offset)
            .unwrap_or(self.audio.gain_db)
    }

    /// Splits `anim` tracks at `split_offset` into left/right halves (for SplitClip).
    pub fn split_anim_at(&mut self, split_offset: TimelineTime) -> Vec<AnimTrack> {
        let mut right_tracks = Vec::new();
        let mut left_tracks = Vec::new();
        for track in self.anim.drain(..) {
            let (left, right) = track.split_at(split_offset);
            left_tracks.push(left);
            if !right.keys.is_empty() {
                right_tracks.push(right);
            }
        }
        self.anim = left_tracks;
        right_tracks
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct AudioSettings {
    pub gain_db: f32,
    pub muted: bool,
    pub fade_in: TimelineTime,
    pub fade_out: TimelineTime,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            muted: false,
            fade_in: TimelineTime::ZERO,
            fade_out: TimelineTime::ZERO,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TextItem {
    pub id: TextId,
    pub text: String,
    pub timeline_start: TimelineTime,
    pub duration: TimelineTime,
    pub transform: Transform,

    #[serde(default)]
    pub font_family: Option<String>,
    pub font_size: f32,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    pub color_rgba: [u8; 4],
    #[serde(default)]
    pub align: TextAlign,
    #[serde(default)]
    pub line_spacing: f32,

    #[serde(default)]
    pub outline_enabled: bool,
    #[serde(default = "TextItem::default_outline_color")]
    pub outline_color_rgba: [u8; 4],
    #[serde(default = "TextItem::default_outline_width")]
    pub outline_width: f32,

    #[serde(default)]
    pub shadow_enabled: bool,
    #[serde(default = "TextItem::default_shadow_color")]
    pub shadow_color_rgba: [u8; 4],
    #[serde(default)]
    pub shadow_offset_x: f32,
    #[serde(default)]
    pub shadow_offset_y: f32,

    #[serde(default)]
    pub background_enabled: bool,
    #[serde(default = "TextItem::default_background_color")]
    pub background_color_rgba: [u8; 4],
    #[serde(default)]
    pub background_padding: f32,

    #[serde(default)]
    pub fade_in: TimelineTime,
    #[serde(default)]
    pub fade_out: TimelineTime,
}

impl TextItem {
    pub fn new(text: impl Into<String>, timeline_start: TimelineTime, frame: (f32, f32)) -> Self {
        Self {
            id: Uuid::new_v4(),
            text: text.into(),
            timeline_start,
            duration: TimelineTime::from_seconds(3.0),
            transform: Transform {
                x: frame.0 / 2.0,
                y: frame.1 * 0.82,
                anchor: Anchor::Center,
                scale: 100.0,
                rotation_degrees: 0.0,
            },
            font_family: None,
            font_size: 64.0,
            bold: false,
            italic: false,
            color_rgba: [255, 255, 255, 255],
            align: TextAlign::Center,
            line_spacing: 0.0,
            outline_enabled: true,
            outline_color_rgba: Self::default_outline_color(),
            outline_width: Self::default_outline_width(),
            shadow_enabled: false,
            shadow_color_rgba: Self::default_shadow_color(),
            shadow_offset_x: 3.0,
            shadow_offset_y: 3.0,
            background_enabled: false,
            background_color_rgba: Self::default_background_color(),
            background_padding: 12.0,
            fade_in: TimelineTime::ZERO,
            fade_out: TimelineTime::ZERO,
        }
    }

    pub fn timeline_end(&self) -> TimelineTime {
        self.timeline_start + self.duration
    }

    pub fn contains_timeline_time(&self, time: TimelineTime) -> bool {
        self.timeline_start <= time && time < self.timeline_end()
    }

    fn default_outline_color() -> [u8; 4] {
        [0, 0, 0, 255]
    }

    fn default_outline_width() -> f32 {
        4.0
    }

    fn default_shadow_color() -> [u8; 4] {
        [0, 0, 0, 160]
    }

    fn default_background_color() -> [u8; 4] {
        [0, 0, 0, 160]
    }
}

/// A spoken line: VOICEVOX-synthesized audio plus the subtitle that goes with it, kept as a
/// single timeline item so a line moves, trims and deletes as one unit instead of as two
/// separate items the user has to keep in sync by hand.
///
/// The audio clip owns the timing (its source range defines the item's duration), and the
/// subtitle is a full [`TextItem`] that carries all the text styling. `sync_subtitle` re-glues
/// the subtitle to the audio whenever the timing changes.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SpeechItem {
    pub audio: Clip,
    pub subtitle: TextItem,
}

impl SpeechItem {
    pub fn new(audio: Clip, mut subtitle: TextItem) -> Self {
        subtitle.id = audio.id;
        let mut speech = Self { audio, subtitle };
        speech.sync_subtitle();
        speech
    }

    /// The item id: shared with the subtitle so text edits (inspector, preview drag) and text
    /// lookups resolve to this one speech item.
    pub fn id(&self) -> ItemId {
        self.audio.id
    }

    pub fn timeline_start(&self) -> TimelineTime {
        self.audio.timeline_start
    }

    pub fn set_timeline_start(&mut self, value: TimelineTime) {
        self.audio.timeline_start = value;
        self.sync_subtitle();
    }

    pub fn duration(&self) -> TimelineTime {
        self.audio.duration()
    }

    pub fn timeline_end(&self) -> TimelineTime {
        self.timeline_start() + self.duration()
    }

    /// Keeps the subtitle's identity and span locked to the audio clip.
    pub fn sync_subtitle(&mut self) {
        self.subtitle.id = self.audio.id;
        self.subtitle.timeline_start = self.audio.timeline_start;
        self.subtitle.duration = self.audio.duration();
    }
}

/// The geometric primitives that can be placed on the timeline.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeKind {
    Rectangle,
    Ellipse,
}

impl ShapeKind {
    pub const ALL: [ShapeKind; 2] = [ShapeKind::Rectangle, ShapeKind::Ellipse];

    pub const fn label(self) -> &'static str {
        match self {
            ShapeKind::Rectangle => "長方形",
            ShapeKind::Ellipse => "楕円",
        }
    }
}

/// A filled geometric overlay on the timeline. Position, anchor and a uniform scale live
/// in `transform` (shared with clips and text), while `width`/`height` control the base
/// pixel size and therefore the aspect ratio.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ShapeItem {
    pub id: ShapeId,
    /// Serialized as `shape_kind`. `TimelineItem` is an internally tagged enum whose tag is
    /// also called `kind`, so a shape item would otherwise contain two `kind` keys and produce
    /// JSON that `serde_json` refuses to read back.
    #[serde(rename = "shape_kind")]
    pub kind: ShapeKind,
    pub timeline_start: TimelineTime,
    pub duration: TimelineTime,
    pub transform: Transform,
    pub width: f32,
    pub height: f32,
    pub color_rgba: [u8; 4],
    #[serde(default = "ShapeItem::default_opacity")]
    pub opacity: f32,
}

impl ShapeItem {
    pub fn new(kind: ShapeKind, timeline_start: TimelineTime, frame: (f32, f32)) -> Self {
        Self {
            id: Uuid::new_v4(),
            kind,
            timeline_start,
            duration: TimelineTime::from_seconds(3.0),
            transform: Transform::centered(frame),
            width: 320.0,
            height: 180.0,
            color_rgba: [255, 255, 255, 255],
            opacity: 100.0,
        }
    }

    pub fn timeline_end(&self) -> TimelineTime {
        self.timeline_start + self.duration
    }

    pub fn contains_timeline_time(&self, time: TimelineTime) -> bool {
        self.timeline_start <= time && time < self.timeline_end()
    }

    fn default_opacity() -> f32 {
        100.0
    }
}

/// A reusable, user-defined text style. Templates carry every visual property of a text
/// item but no content or timing: applying a template stamps its style (and placement)
/// onto a fresh or existing text item.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TextTemplate {
    pub id: Uuid,
    pub name: String,

    #[serde(default)]
    pub font_family: Option<String>,
    pub font_size: f32,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    pub color_rgba: [u8; 4],
    #[serde(default)]
    pub align: TextAlign,
    #[serde(default)]
    pub line_spacing: f32,

    #[serde(default)]
    pub outline_enabled: bool,
    #[serde(default = "TextItem::default_outline_color")]
    pub outline_color_rgba: [u8; 4],
    #[serde(default = "TextItem::default_outline_width")]
    pub outline_width: f32,

    #[serde(default)]
    pub shadow_enabled: bool,
    #[serde(default = "TextItem::default_shadow_color")]
    pub shadow_color_rgba: [u8; 4],
    #[serde(default)]
    pub shadow_offset_x: f32,
    #[serde(default)]
    pub shadow_offset_y: f32,

    #[serde(default)]
    pub background_enabled: bool,
    #[serde(default = "TextItem::default_background_color")]
    pub background_color_rgba: [u8; 4],
    #[serde(default)]
    pub background_padding: f32,

    pub transform: Transform,
}

impl TextTemplate {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            font_family: None,
            font_size: 64.0,
            bold: false,
            italic: false,
            color_rgba: [255, 255, 255, 255],
            align: TextAlign::Center,
            line_spacing: 0.0,
            outline_enabled: true,
            outline_color_rgba: TextItem::default_outline_color(),
            outline_width: TextItem::default_outline_width(),
            shadow_enabled: false,
            shadow_color_rgba: TextItem::default_shadow_color(),
            shadow_offset_x: 3.0,
            shadow_offset_y: 3.0,
            background_enabled: false,
            background_color_rgba: TextItem::default_background_color(),
            background_padding: 12.0,
            // Default to the middle of the frame instead of (0, 0): a freshly created
            // template must not place generated text half outside the canvas.
            transform: Transform::centered((1920.0, 1080.0)),
        }
    }

    pub fn from_text(name: impl Into<String>, text: &TextItem) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            font_family: text.font_family.clone(),
            font_size: text.font_size,
            bold: text.bold,
            italic: text.italic,
            color_rgba: text.color_rgba,
            align: text.align,
            line_spacing: text.line_spacing,
            outline_enabled: text.outline_enabled,
            outline_color_rgba: text.outline_color_rgba,
            outline_width: text.outline_width,
            shadow_enabled: text.shadow_enabled,
            shadow_color_rgba: text.shadow_color_rgba,
            shadow_offset_x: text.shadow_offset_x,
            shadow_offset_y: text.shadow_offset_y,
            background_enabled: text.background_enabled,
            background_color_rgba: text.background_color_rgba,
            background_padding: text.background_padding,
            transform: text.transform,
        }
    }

    /// Stamps this template's style (and placement) onto an existing text item, keeping
    /// its id, content and timing intact.
    pub fn apply_to(&self, text: &mut TextItem) {
        text.transform = self.transform;
        text.font_family = self.font_family.clone();
        text.font_size = self.font_size;
        text.bold = self.bold;
        text.italic = self.italic;
        text.color_rgba = self.color_rgba;
        text.align = self.align;
        text.line_spacing = self.line_spacing;
        text.outline_enabled = self.outline_enabled;
        text.outline_color_rgba = self.outline_color_rgba;
        text.outline_width = self.outline_width;
        text.shadow_enabled = self.shadow_enabled;
        text.shadow_color_rgba = self.shadow_color_rgba;
        text.shadow_offset_x = self.shadow_offset_x;
        text.shadow_offset_y = self.shadow_offset_y;
        text.background_enabled = self.background_enabled;
        text.background_color_rgba = self.background_color_rgba;
        text.background_padding = self.background_padding;
    }

    /// Builds a brand new text item from this template with the given content and timing.
    pub fn build_text(
        &self,
        text: impl Into<String>,
        timeline_start: TimelineTime,
        duration: TimelineTime,
    ) -> TextItem {
        let mut item = TextItem::new(text, timeline_start, (1920.0, 1080.0));
        item.duration = duration;
        self.apply_to(&mut item);
        item
    }
}

/// Voice tuning applied on top of VOICEVOX's `audio_query`, mirroring the adjustable fields of
/// that response. Stored per character so each cast member keeps its own delivery (pitch, tempo,
/// ...) instead of sharing the engine's neutral defaults.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct VoiceSettings {
    /// `speedScale` — 1.0 is the engine default speech rate.
    pub speed: f32,
    /// `pitchScale` — 0.0 is unshifted; positive raises, negative lowers.
    pub pitch: f32,
    /// `intonationScale` — 1.0 is the engine default expressiveness.
    pub intonation: f32,
    /// `volumeScale` — 1.0 is unmodified.
    pub volume: f32,
    /// `prePhonemeLength` — silence inserted before the line, in seconds.
    pub pre_silence: f32,
    /// `postPhonemeLength` — silence inserted after the line, in seconds.
    pub post_silence: f32,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            speed: 1.0,
            pitch: 0.0,
            intonation: 1.0,
            volume: 1.0,
            pre_silence: 0.1,
            post_silence: 0.1,
        }
    }
}

/// A cast member: a named VOICEVOX speaker bundled with its own voice tuning and subtitle style.
/// Characters are the unit of configuration for narration and dialogue, so one speaker can appear
/// as several characters (or several characters can share a speaker with different delivery).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Character {
    pub id: Uuid,
    pub name: String,
    /// VOICEVOX speaker (style) id passed to `audio_query` / `synthesis`.
    pub speaker: u32,
    #[serde(default)]
    pub voice: VoiceSettings,
    /// Subtitle template applied to this character's lines. `None` uses the default text style.
    #[serde(default)]
    pub template: Option<Uuid>,
}

impl Character {
    pub fn new(name: impl Into<String>, speaker: u32) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            speaker,
            voice: VoiceSettings::default(),
            template: None,
        }
    }
}
