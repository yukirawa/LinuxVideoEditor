# Project Format

## Goal

Create a portable, versioned project representation that contains editing state and AI-understandable media information without unnecessarily embedding the original media.

Working extension:

`.yukivproj`

The extension is provisional.

## 1. Example Layout

```text
project/
├── project.yukivproj
├── media/
├── analysis/
├── timeline/
├── thumbnails/
├── proxies/
├── cache/
└── ai/
```

## 2. project.yukivproj

The main manifest should identify:

- project ID
- schema version
- media references
- timeline reference
- analysis version
- project metadata
- optional AI memory reference

## 3. Media References

Prefer references to source media.

Do not duplicate 100 GB media solely because the project file is saved.

References may include:
- relative path
- absolute path as optional fallback
- content hash
- media UUID

Example:

```json
{
  "media_id": "media_001",
  "path": "media/IMG_001.mp4",
  "sha256": "..."
}
```

## 4. Timeline

Timeline must be semantic and machine-readable.

It should include:
- tracks
- clips
- source references
- source in/out
- timeline in/out
- transforms
- audio settings
- text
- transitions

## 5. Analysis

Analysis data may include:
- scenes
- shots
- transcript
- OCR
- embeddings
- keyframes
- visual descriptions
- audio features

Analysis is derived data and should be rebuildable.

## 6. Cache

Cache may contain:
- proxies
- thumbnails
- extracted frames
- model results
- intermediate files

Cache should never be the sole source of project truth.

## 7. AI Memory

AI memory should be explicit, editable and deletable.

Example:

```json
{
  "memory_type": "editorial_preference",
  "key": "duplicate_scenery",
  "value": "prefer_shorter_or_remove"
}
```

## 8. Portability

A project should be recoverable on another machine when:
- referenced media is available
- required codecs are available
- project schema is supported

## 9. Migration

Every schema version should have migration rules.

Do not silently interpret incompatible versions.

### Known migrations

- **0.2 → 0.3**: shape items used to serialize their geometric kind under the key `kind`,
  which collided with the `TimelineItem` tag (`{"kind": "shape", ..., "kind": "ellipse"}`).
  The inner key is now `shape_kind`, and the loader rewrites the legacy duplicate key before
  parsing so previously saved shape projects still open. Loaded projects are stamped with the
  current schema version.
