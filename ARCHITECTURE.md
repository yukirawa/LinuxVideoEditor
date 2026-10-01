# System Architecture

## 1. Layer Model

```text
┌──────────────────────────────────────┐
│ Editor UI                            │
│ timeline / preview / media browser   │
└────────────────┬─────────────────────┘
                 ↓
┌──────────────────────────────────────┐
│ Editing Core                         │
│ timeline / operations / undo / redo  │
└────────────────┬─────────────────────┘
                 ↕
┌──────────────────────────────────────┐
│ Media Engine                         │
│ decode / encode / audio / proxies    │
└────────────────┬─────────────────────┘
                 ↕
┌──────────────────────────────────────┐
│ Video Knowledge Engine               │
│ scene / shot / OCR / ASR / vision    │
│ embeddings / retrieval               │
└────────────────┬─────────────────────┘
                 ↕
┌──────────────────────────────────────┐
│ AI Agent                             │
│ reasoning / planning / tool calling  │
└────────────────┬─────────────────────┘
                 ↕
┌──────────────────────────────────────┐
│ AI Provider Adapters                 │
│ cloud / local / external chat        │
└──────────────────────────────────────┘
```

## 2. Responsibilities

### Editor UI

UI is responsible for:
- timeline visualization
- media browser
- preview
- editing controls
- AI interaction panel
- review and approval

UI should not contain core editing logic.

### Editing Core

Responsible for:
- project state
- clips
- tracks
- transforms
- transitions
- subtitles
- undo/redo
- validation
- applying AEP operations
- render/export requests

### Media Engine

Responsible for:
- decoding
- encoding
- frame extraction
- audio extraction
- proxy generation
- thumbnails
- clip rendering

FFmpeg is a suitable implementation choice.

### Video Knowledge Engine

Responsible for:
- scene detection
- shot detection
- ASR
- OCR
- visual analysis
- audio analysis
- embeddings
- semantic indexing
- keyframe selection

### AI Agent

Responsible for:
- interpreting user intent
- querying project information
- selecting tools
- asking for visual evidence
- comparing scenes
- planning edits
- producing AEP

### Provider Adapter

Responsible for:
- model/API-specific protocol conversion
- authentication
- structured output normalization
- multimodal request conversion
- tool-call translation

The internal project representation MUST NOT depend on provider-specific schemas.

## 3. Data Flow

```text
Source media
↓
Media metadata
↓
Analysis pipeline
↓
VIR + cache + indexes
↓
AI query
↓
Retrieval
↓
optional frame/clip observation
↓
AI edit plan
↓
AEP validation
↓
Editing Core
↓
new timeline state
↓
render/export
```

## 4. Non-Destructive Editing

Source media remains unchanged.

The editor stores references and operations:

```text
Source media
+ timeline state
+ transformation operations
= edited result
```

Export is a derived artifact.

## 5. Technology Direction

Selected initial Linux stack:

- Core: Rust
- Media: FFmpeg / FFprobe / FFplay
- UI: egui + eframe + egui_dock (native wgpu renderer)
- Project storage: versioned JSON (`.yukivproj`)
- Future local structured storage: SQLite
- Future vector search: embedded/vector-capable local solution
- Future analysis: Python and/or Rust workers behind a process boundary

Phase 2 realizes the first part of the knowledge engine in-process: `src/knowledge` shells out to
FFmpeg, tesseract and whisper.cpp through a backend-neutral pipeline and caches the resulting
VIR. Heavier or model-based analyzers (embeddings, vision captions) can later move behind a
process boundary without changing the VIR or the editing core; the seam is the analyzer stage,
not the UI. See `VIDEO-INTELLIGENCE.md`.
- Future AI communication: provider-neutral adapters over HTTP/REST or local IPC

Model communication is now implemented (`src/models`): a provider-neutral `AIProvider` boundary
plus one OpenAI-compatible adapter that serves *both* a local server (Ollama, llama.cpp,
LM Studio) and a network API (OpenAI, DeepSeek), so a model is selectable without changing the
editing core. HTTP goes through the system `curl` and a fully testable transport seam; API keys
come from an environment variable or a session-only input, never from persisted storage. See
`AI-MODELS.md`.

The **AI Agent** layer (`src/agent`) is also in place. It builds a compact context from the
project and VIR, defines the editor tools the model may call (`agent::tools`) and parses its
replies, so a request like "make a 30-second introduction" turns into a sequence of edits,
VOICEVOX speech with subtitles, telops and an export. The agent module is provider-neutral and
does not execute anything itself: the app runs each tool through the ordinary `EditCommand` path,
so every change stays non-destructive, validated and undoable. It can also extract an AEP plan for
models that answer with JSON instead of tool calls, which the editing core validates and applies
transactionally.

The native egui stack is intentionally used instead of embedded web technology. Idle UI is event-driven, preview work uses bounded queues, and FFmpeg worker/thread counts are limited by the selected power profile.

Technology is replaceable. Architecture is not.
