# AI Provider Architecture

## Goal

Support arbitrary compatible AI models without changing the editor core.

## 1. Internal Abstraction

The editor should expose an internal provider-neutral interface such as:

```text
AIProvider
 ├── send_message()
 ├── send_image()
 ├── request_structured_output()
 ├── tool_call()
 └── capabilities()
```

Actual names may change.

Implemented today as `protocol::AiProvider`: `send` (one chat turn, with history) and `send_tools`
(a turn that may return tool calls, with a default that reports "tool calling not supported" so
the agent degrades to plain chat). `AiToolSpec` / `AiToolCall` / `AiTurn` are the neutral types
behind it.

## 2. Provider Adapters

Possible adapters:

```text
OpenAIAdapter
GeminiAdapter
ClaudeAdapter
OpenAICompatibleAdapter
LocalModelAdapter
ExternalChatAdapter
```

These names are examples.

Implemented today: a single `OpenAiCompatibleAdapter` (`models::OpenAiCompatible`) that also serves
DeepSeek, because DeepSeek's API is OpenAI-compatible. `ProviderKind::DeepSeek` records the
identity (label, default URL, no embeddings endpoint) without adding a second wire format. Embedding
and chat are configured as separate model slots.

## 3. Capability Negotiation

Models have different capabilities.

Example capability flags:

```json
{
  "vision": true,
  "tool_calling": true,
  "structured_output": true,
  "large_context": true,
  "streaming": true
}
```

The agent should adapt behavior to available capabilities.

## 4. Model-Neutral Core

The following MUST remain provider-neutral:

- VIR
- AEP
- project format
- timeline representation
- tool semantics
- media IDs
- scene IDs
- operation IDs

## 5. API Mode

```text
Editor
→ Provider Adapter
→ AI API
→ Agent
→ Tool calls
→ Editor
```

## 6. External Chat Mode

```text
Editor
→ context package
→ user
→ external chat AI
→ AEP
→ editor
```

Direct automation of consumer chat applications must not be assumed.

## 7. Local AI

Local models may be used for:
- semantic indexing
- summarization
- retrieval
- classification
- privacy-sensitive processing

Cloud models may be used for:
- complex reasoning
- difficult visual interpretation
- high-level editorial planning

Hybrid execution is desirable.

## 8. No Vendor Lock-In

A provider should be removable without changing project files.

Projects must remain readable if a provider disappears.
