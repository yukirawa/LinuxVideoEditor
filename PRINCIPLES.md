# Engineering Principles

## 1. Standards Before Features

A flashy AI feature is less valuable than a stable representation and protocol.

Prioritize:
- VIR
- AEP
- project schema
- tool semantics
- versioning

## 2. Model Independence

Never write core logic around one model's quirks.

Bad:

```text
if OpenAI:
  ...
```

in the editing core.

Good:

```text
AIProvider
AEP
Tool Interface
VIR
```

## 3. Raw Video Is External Memory

The LLM context should contain only enough information for the current decision.

The video remains in an external media store.

## 4. Inspect Before Acting

The agent should inspect relevant evidence before making high-impact edits.

## 5. Cheap Before Expensive

Prefer:

```text
metadata
→ text
→ one frame
→ multiple frames
→ clip
```

## 6. Non-Destructive

Never destroy source media.

## 7. Determinism

The same AEP applied to the same project state should produce the same logical timeline result.

## 8. Version Everything

Version:
- project schema
- VIR schema
- AEP
- analysis pipelines
- embeddings
- model-generated derived data

## 9. Explicit Memory

AI memory must be:
- visible
- editable
- removable
- attributable

## 10. User Control

AI should be able to:
- observe
- suggest
- assist
- autonomously execute

These are separate permission levels.

## 11. Debuggability

Every important AI action should be traceable:

```text
user request
→ retrieval
→ observations
→ AEP
→ validation
→ timeline change
```

Do not depend on hidden model reasoning for reproducibility.

## 12. Build for Interoperability

The final product should ideally allow other:
- AI models
- editors
- analyzers
- indexing engines

to interact through standardized representations.

That interoperability is a core product feature, not a side effect.
