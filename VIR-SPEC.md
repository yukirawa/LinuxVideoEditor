# VIR Specification

## Video Intermediate Representation

VIR is the semantic intermediate representation of audiovisual media.

It exists to prevent large raw videos from becoming large LLM contexts.

The original media file remains the source of truth.
VIR is derived metadata and semantic representation.

## 1. Hierarchy

```text
PROJECT
└── MEDIA
    └── VIDEO
        └── SCENE
            └── SHOT
                └── FRAME / KEYFRAME
```

Audio and transcript information may exist at any level.

## 2. Project

Example:

```json
{
  "project_id": "project_001",
  "schema_version": "0.1",
  "title": "Hokkaido Trip",
  "media_ids": ["media_001"],
  "timeline_id": "timeline_001"
}
```

## 3. Media

```json
{
  "media_id": "media_001",
  "path": "media/IMG_001.mp4",
  "duration": 3600.42,
  "width": 3840,
  "height": 2160,
  "fps": 59.94,
  "audio_tracks": 1,
  "analysis_version": "0.1"
}
```

## 4. Scene

```json
{
  "scene_id": "scene_042",
  "media_id": "media_001",
  "start": 1123.4,
  "end": 1151.9,
  "type": "train_departure",
  "location": "Asahikawa Station",
  "events": ["train_departure"],
  "objects": ["train", "platform", "station_sign"],
  "transcript": "まもなく列車が発車します",
  "ocr": ["旭川"],
  "mood": ["exciting"],
  "importance": 0.94,
  "visual_embedding": "embedding_ref",
  "keyframes": ["frame_100", "frame_101"]
}
```

## 5. Shot

A shot is a continuous visual segment with stable shot characteristics.

Suggested fields:

- shot_id
- scene_id
- start
- end
- camera_motion
- visual_energy
- audio_energy
- dominant_objects
- transcript_range
- representative_frames

## 6. Frame

```json
{
  "frame_id": "frame_100",
  "media_id": "media_001",
  "timestamp": 1135.2,
  "caption": "雪のホームへ列車が進入している",
  "objects": ["train", "platform", "snow"],
  "ocr": ["旭川"],
  "quality_score": 0.87,
  "embedding": "embedding_ref"
}
```

## 7. Progressive Disclosure

AI should receive information in increasing detail:

```text
Level 0: metadata
Level 1: semantic text
Level 2: one frame
Level 3: multiple frames
Level 4: short clip
```

The system should not send maximum visual context by default.

## 8. Importance

`importance` is an editorial estimate, not absolute truth.

Possible inputs:
- semantic relevance
- uniqueness
- event significance
- narrative role
- visual quality
- audio intensity
- user preferences
- repetition

## 9. Uncertainty

Derived judgments may expose confidence:

```json
{
  "decision": "keep",
  "confidence": 0.61,
  "reason": "visual evidence incomplete",
  "recommended_action": "inspect_frame"
}
```

## 10. Versioning

VIR MUST carry schema and analysis versions.

Example:

```json
{
  "schema_version": "0.1",
  "analysis_version": "2026.1",
  "vision_model": "provider:model",
  "asr_model": "provider:model",
  "embedding_model": "provider:model"
}
```

Cached analysis is invalidated when required inputs or versions change.

## 11. Design Rule

VIR must be:
- machine-readable
- versioned
- extensible
- model-independent
- deterministic where possible
- useful without raw video access

VIR is the project's semantic memory of media.
