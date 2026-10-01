# AI Tool Interface

## Purpose

The AI must interact with video through explicit tools.

It should not need the entire raw video in context.

## 1. Read Tools

Suggested interface:

```text
get_project_info()
get_media_info(media_id)
get_scenes(media_id?)
get_scene(scene_id)
get_shots(scene_id)

search_scenes(query)
search_objects(query)
search_text(query)
search_transcript(query)
search_ocr(query)

get_frame(media_id, timestamp)
get_frames(media_id, timestamps)
get_clip(media_id, start, end)

inspect_audio(media_id, start, end)
```

## 2. Write Tools

```text
create_clip()
split_clip()
trim_clip()
delete_clip()
move_clip()
duplicate_clip()

add_text()
modify_text()
delete_text()

add_audio()
trim_audio()
move_audio()

add_transition()
remove_transition()

set_volume()
set_speed()

apply_edit_plan()
```

## 3. Implemented Tool Set

The agent (`src/agent/tools.rs`) exposes this set today. Names and arguments are the real
contract sent to the model; `export_video` and `add_speech` run asynchronously.

Read:
- `get_project_state` — timeline layers/items, assets (name → path), playhead, duration, scenes
- `analyze_media` — run Phase 2 analysis (async) so scenes/transcript/OCR become available
- `search_scenes` — semantic search over analyzed scenes (async; generates embeddings if missing)
- `get_frame` — fetch a scene's keyframe and attach the image to the next message (vision)

Write:
- `add_media` — import an existing asset by name, or a file by path
- `split_clip`, `trim_clip`, `delete_clip`, `move_clip`
- `set_volume`, `mute_clip`
- `add_text` — subtitles / telops (optional template)
- `add_speech` — VOICEVOX speech **plus** its subtitle as a single speech item, generated together (a `character` name selects the speaker, voice tuning and subtitle template; `speaker`/`template` decide when none is named)
- `seek` — move the playhead, so later defaults land where the model expects
- `export_video` — encode MP4 (the finish step)

Every write tool maps to an ordinary `EditCommand`, so the editing core validates it
(non-overlap, valid ranges) and the change is undoable. The user can disable execution in the UI
("ツール実行を許可"), in which case the model only proposes tool calls. Long tools (`analyze_media`,
`search_scenes`, `add_speech`, `export_video`) run in the background and the loop waits for them.
`get_frame` attaches a keyframe JPEG to the conversation, so a vision-capable model can look at the
footage; `get_frames` (several at once), `get_clip` and `inspect_audio` are not implemented.

## 4. Read / Write Separation

Read tools should be unrestricted or low-risk.

Write tools should require:
- validation
- permission
- optional user confirmation

Suggested modes:

```text
OBSERVE
SUGGEST
ASSIST
AUTONOMOUS
```

## 4. Progressive Visual Inspection

AI should use the cheapest sufficient observation.

Example:

```text
Need semantic candidate
→ search_scenes()

Need visual identity
→ get_frame()

Need comparison
→ get_frames()

Need motion/timing
→ get_clip()
```

Do not retrieve video unnecessarily.

## 5. Retrieval Cost

The system may expose estimated cost.

Example:

```text
get_frame = 1
get_frames(5) = 5
get_clip(5 sec) = 20
high_res_frame = 3
```

This encourages efficient tool usage.

Actual cost is implementation dependent.

## 6. AI Loop

```text
user intent
↓
inspect project summary
↓
search
↓
candidate scenes
↓
visual inspection when uncertain
↓
compare
↓
create plan
↓
validate
↓
execute
```

## 7. Tool Results

Tool results should be compact and structured.

Example:

```json
{
  "scene_id": "scene_042",
  "start": 1123.4,
  "end": 1151.9,
  "type": "train_departure",
  "importance": 0.94,
  "visual_available": true
}
```

Avoid returning huge uncontrolled text blobs.

## 8. External Chat Mode

When direct tool calls are impossible, export an AI context package:

```text
project_context/
├── project.json
├── scenes.json
├── timeline.json
├── transcript.txt
├── semantic_index.json
├── selected_frames/
└── task.txt
```

The external AI returns AEP, which is then imported and validated.

This allows ordinary chat systems to participate without requiring direct API integration.
