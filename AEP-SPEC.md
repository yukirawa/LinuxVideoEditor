# AEP Specification

## AI Editing Protocol

AEP defines communication between an AI agent and the editing core.

Working name: `AEP`.

The exact name can change, but the protocol must remain:
- vendor-neutral
- versioned
- machine-readable
- deterministic where possible
- extensible
- independently documented

## 1. Architecture

```text
AI Model
  ↓
AI Agent
  ↓
AEP
  ↓
AEP Validator
  ↓
Editing Core
  ↓
Timeline
```

The AI MUST NOT directly mutate source media.

## 2. Example

```json
{
  "protocol": "AEP",
  "version": "1.0",
  "project_id": "project_001",
  "operations": [
    {
      "type": "trim",
      "clip_id": "clip_023",
      "source_in": 12.4,
      "source_out": 18.7
    },
    {
      "type": "delete",
      "clip_id": "clip_041"
    },
    {
      "type": "add_text",
      "timestamp": 32.4,
      "text": "旭川駅"
    }
  ]
}
```

## 3. Operation Classes

### Timeline

- create_clip
- split_clip
- trim_clip
- delete_clip
- move_clip
- duplicate_clip
- reorder_clip

### Visual

- add_text
- modify_text
- delete_text
- crop
- transform
- speed
- freeze_frame

### Audio

- add_audio
- trim_audio
- move_audio
- set_volume
- fade
- mute

### Transition

- add_transition
- remove_transition

### Analysis/reference

Operations may refer to semantic targets:

```json
{
  "target_type": "scene",
  "target_id": "scene_042"
}
```

The editor resolves semantic targets to concrete timeline/media entities.

## 4. Validation

Before execution:

- all IDs must exist
- source ranges must be valid
- timeline ranges must be valid
- operation dependencies must resolve
- unsupported operations must be rejected
- source media must remain untouched

Invalid AEP MUST NOT corrupt project state.

## 5. Transactions

AEP application should be transactional.

```text
AEP received
↓
validate all operations
↓
preview/dry-run
↓
commit
```

Optional mode:

```text
apply operation 1
apply operation 2
...
```

with rollback support.

## 6. Reason Metadata

Operations may include:

```json
{
  "type": "delete",
  "clip_id": "clip_042",
  "reason": "redundant scenery",
  "confidence": 0.89
}
```

Reason metadata is for transparency, debugging and evaluation.

It is not hidden model chain-of-thought.

## 7. Idempotency

Where possible, operations should use stable IDs and operation IDs.

Example:

```json
{
  "operation_id": "op_00192"
}
```

The system should detect duplicate application.

## 8. Versioning

AEP MUST include protocol version.

Breaking changes require a new major version.

Readers should reject unsupported major versions rather than guessing.

## 9. Long-Term Goal

AEP should become independent from this specific editor.

A compatible AI could theoretically control multiple editors through the same protocol.
