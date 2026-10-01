# Development Roadmap

## Phase 0: Architecture

Deliver:
- repository structure
- core module boundaries
- project schema draft
- VIR schema draft
- AEP schema draft
- AI tool schema draft

Do not overbuild UI yet.

## Phase 1: Basic Editor

Implement:
- media import
- video playback
- timeline
- split
- trim
- move
- delete
- basic audio
- export

AI is not required for this phase.

## Phase 2: Video Intelligence

Implement:
- metadata extraction
- scene detection
- shot detection
- transcript
- OCR
- keyframe extraction
- thumbnail system

Result:
VIR can be generated from media.

Status: implemented. See `VIDEO-INTELLIGENCE.md` for the pipeline, backends, heuristics and
cache format. Scene/shot grouping is deterministic and model-free; OCR and transcript are
optional backends that degrade to warnings when unavailable. Semantic captions, embeddings
and object/event detection need a vision model and move to later phases.

## Phase 3: Video RAG

Implement:
- scene indexing
- semantic search
- embeddings
- transcript search
- OCR search
- object/event search

Result:
Large projects become searchable without opening every source video.

Status: partly implemented. Scene embeddings and cosine-based semantic search over the analyzed
medium are in place, with the embedding model selectable between a local server and a network
API (`AI-MODELS.md`). Ranking currently covers the medium being viewed; project-wide indexing
over the per-media embedding cache, transcript/OCR keyword search, and object/event search are
still to come.

## Phase 4: AI Observation Tools

Implement:
- search_scenes
- get_scene
- get_frame
- get_frames
- get_clip
- get_transcript
- get_ocr
- inspect_audio

Result:
AI can actively investigate video.

Status: implemented for *editing, discovery, production and frame observation*. The assistant
receives a compact project/timeline context and can call a set of editor tools
(`src/agent/tools.rs`) in a loop: read state, run analysis (`analyze_media`), semantically search
scenes (`search_scenes`), look at a keyframe (`get_frame`: the image is attached to the next
message for a vision model), cut and arrange clips, add media, synthesize VOICEVOX speech with
subtitles, add text, set volume, and export. Still missing from the suggested set: `get_frames`
(several frames at once), `get_clip` (motion/timing) and `inspect_audio`.

## Phase 5: AI Editing

Implement:
- AEP validation
- AI edit planning
- preview/dry-run
- timeline application
- undo/rollback

Result:
AI can propose and apply edits.

Status: implemented for the assistant workflow. The agent drives the editor through tools
(`split_clip`, `trim_clip`, `delete_clip`, `move_clip`, `add_media`, `add_text`, `add_speech`,
`set_volume`, `mute_clip`, `export_video`) until the video is finished, then says it is ready to
preview. Every tool maps to an ordinary `EditCommand`, so edits are non-destructive and undoable,
and long tools (VOICEVOX synthesis, export) run in the background while the loop waits. The AEP
path (`agent::extract_aep` → `protocol::validate_aep` → `EditHistory::execute_batch`) remains for
models that answer with a JSON plan instead of tool calls; AEP now serializes times in seconds and
fills in missing operation IDs.

## Phase 6: Provider Abstraction

Implement:
- provider adapter
- OpenAI-compatible adapter
- cloud providers
- local models
- capability negotiation

Result:
Model is replaceable.

Status: partly implemented ahead of schedule, because Phase 3 needed a model. The
provider-neutral boundary (`protocol::AiProvider`), an OpenAI-compatible adapter that serves both
local servers and cloud APIs, selectable local/network configuration, a dedicated `curl`
transport, and API keys read from an environment variable or typed in for the session are all in
place (`src/models`, `AI-MODELS.md`). Native presets cover Ollama, llama.cpp, LM Studio, OpenAI
and **DeepSeek**, and separate embedding/chat model slots are wired up. Richer capability
negotiation and vision remain.

## Phase 7: External Chat Mode

Implement:
- context package export
- frame package export
- task export
- AEP import
- AEP validation

Result:
Ordinary external chat AI can participate.

## Phase 8: Advanced Agent

Implement:
- uncertainty
- adaptive observation
- visual comparison
- narrative planning
- editorial memory
- user-specific preferences

## Phase 9: Ecosystem

Potential future work:
- public VIR specification
- public AEP specification
- third-party AI adapters
- third-party analysis engines
- editor interoperability
- plugin SDK

## Definition of MVP

MVP is complete when:

```text
large video project
→ analyze once
→ search semantically
→ AI retrieves frames
→ AI creates AEP
→ editor validates
→ timeline changes
→ export
```

This loop must work reliably before adding advanced effects.
