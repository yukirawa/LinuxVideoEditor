# Video Intelligence (Phase 2)

Phase 2 turns source media into a VIR. It is the first half of the Video Knowledge Engine
described in `ARCHITECTURE.md`:

```text
source media
→ metadata
→ scene detection
→ shot detection
→ keyframe extraction / thumbnails
→ optional OCR
→ optional transcript
→ VIR
```

Everything here is *derived data*: it is cached, rebuildable, and never authoritative. Source
media is never modified, and no AI model is contacted — provider connections are Phase 6.

## 1. Backends

The engine shells out to well-known system tools rather than embedding codecs or models, so a
backend can be swapped without touching the editing core.

| Stage | Backend | Required | Notes |
| --- | --- | --- | --- |
| Scene/shot detection | FFmpeg (`select` + `metadata` filters) | yes | also used for keyframe extraction |
| Keyframes / thumbnails | FFmpeg | yes | one representative JPEG per scene |
| OCR | tesseract | optional | enabled per analysis; language packs must be installed |
| Transcript | whisper.cpp (`whisper-cli` / `whisper-cpp`) | optional | model file path is configured by the user |

Capabilities are detected once at startup on a worker thread (`knowledge::detect_capabilities`).
Missing optional backends never fail an analysis: the stage is skipped and a warning is
recorded. A missing FFmpeg is fatal, because nothing can be analyzed without it.

## 2. Scene and shot detection

FFmpeg's scene-change score is sampled per frame. Frames whose score exceeds
`cut_threshold` (default `0.25`) are printed with their timestamps and become the **cut list**.

Grouping is deterministic and model-free (semantic grouping needs visual embeddings and is a
later phase):

1. **Shots** are the intervals between cuts. Shots shorter than `min_shot_seconds` (default
   `0.35`) are absorbed into the previous shot; a short leading shot is merged forward.
2. **Scenes** group consecutive shots. A cut at or above `scene_threshold` (default `0.40`)
   starts a new scene, while weaker cuts only start a new shot. A scene is also force-closed
   once joining another shot would exceed `max_scene_seconds` (default `30`), so a long static
   video does not become a single endless scene.
3. Scenes shorter than `MIN_SCENE_SECONDS` (`0.6`) are folded into the previous scene.
4. A cut-free (static) medium still yields exactly one scene covering its whole duration.

Rationale: a cut is a *visual* discontinuity, so a weak one (a flash, a fast pan) is a shot
change inside the same scene, while a strong one marks a new scene. This keeps the hierarchy
`MEDIA → SCENE → SHOT` meaningful even before a vision model is available.

## 3. Keyframes and the thumbnail system

One representative still is extracted per scene at its midpoint (a midpoint avoids transition
frames at the scene edges). Still frames are scaled to `thumbnail_width` and written as JPEGs
into the media's cache directory.

The VIR references a thumbnail by file name relative to that directory, so the cache is
relocatable and the VIR stays free of machine-specific absolute paths. The UI decodes a
thumbnail into a texture only when a scene row becomes visible, and caches the handle (a failed
decode is cached as a failure so it is not retried every frame).

`thumbnail_width` is the only thumbnail knob; a multi-frame storyboard per scene is a natural
extension of the same data model (`VirScene::keyframes` is already a list).

## 4. OCR

When enabled, each scene's keyframe is passed to tesseract. The language string is the user's
setting, or, when blank, `jpn+eng` if both packs are installed, otherwise the first available
pack. Output is cleaned by `normalize_lines`: blank and punctuation-only lines are dropped,
whitespace is collapsed, duplicates removed, and the result is capped (32 lines / 400 chars) so
a noisy frame cannot flood the VIR.

Recognized lines are stored on both the scene and its keyframe. A per-frame failure is a
warning, not a fatal error.

## 5. Transcript

When enabled, audio is extracted to 16 kHz mono PCM (what whisper.cpp expects) and the
configured model is run with `-oj`. Segments are parsed from `offsets.from`/`offsets.to`
(milliseconds) and `text`.

The full segment list is stored on the media (`VirMedia::transcript`), and each scene
additionally carries the concatenation of the segments that overlap it
(`VirScene::transcript`). Missing backend/model, a failed extraction, or a failed run all
degrade to a warning.

## 6. Derived fields

- **summary** — the scene transcript (or first OCR line) truncated to 60 characters, else
  `"<n.n>秒のシーン"`. Semantic captions need a vision model and arrive later.
- **importance** — an editorial estimate (VIR spec section 8), not ground truth:
  `0.05 + 0.55·scene_score + 0.25·min(duration/8s, 1) + 0.15·has_text`.
- **objects / events / mood** — intentionally empty in Phase 2; they require a vision model.
- **provenance** — which detector produced what, as opaque `provider:model` strings.

## 7. Cache and versioning

Analysis is derived, so it lives in a cache directory, not in the project file:

```text
$XDG_CACHE_HOME/yuki-video-editor/analysis/<fingerprint>/
├── vir.json
├── meta.json
├── thumb_0000.jpg
└── scene_cuts.txt        (transient FFmpeg output)
```

The fingerprint hashes the canonical path, file size and modification time. It follows the
*file*, not the import, so re-importing the same footage into another project reuses the
analysis even though the project assigns a new media ID.

`meta.json` stores the cache format and an options signature covering the VIR schema version,
the analysis version, and every option that can change the result. A mismatch is a cache miss.
Semantic IDs (scene/shot/frame) are derived deterministically from the fingerprint and their
position, so they survive a cache rebuild and can be referenced by a later AEP.

## 8. VIR additions since 0.1

- `VirDocument`: `analysis_version`, `provenance`
- `VirMedia`: `fingerprint`, `path`, `duration`, `width`, `height`, `fps`, `has_video`,
  `has_audio`, `transcript`
- `VirScene`: `shots`, `keyframes`, `events`, `mood` (plus existing fields)
- `VirShot`, `VirFrame`, `VirTranscriptSegment`

Every added field is `serde`-defaulted, and unknown fields are ignored, so a VIR 0.1 document
still loads (`tests/video_intelligence.rs::legacy_vir_documents_still_load`).

## 9. What Phase 2 does not do

- semantic scene grouping, object/event/mood detection, captions (need a vision model)
- embeddings and semantic search (Phase 3)
- tool interface for an AI (Phase 4)
- AEP planning and application (Phase 5)
- provider connections (Phase 6)
