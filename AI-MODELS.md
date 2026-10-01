# AI Models: Local and Network

The editor keeps its models **selectable**. A model can run on this machine (local) or be
reached over the network, and both cases use the same OpenAI-compatible protocol, so exactly one
adapter is needed and the editing core never depends on a specific vendor (`AI-PROVIDERS.md`).

```text
     選択 (UI)
        │
        ├── ローカル ── Ollama / llama.cpp / LM Studio  (127.0.0.1)
        └── ネットワーク ── OpenAI互換API (https://…)
                            │
                     OpenAICompatible adapter
                            │
                    VIR / 埋め込み / 検索
```

## 1. Why one adapter for both

Local servers and hosted APIs all expose the same endpoints:

| Endpoint | Purpose |
| --- | --- |
| `GET /models` | connection test, capability discovery |
| `POST /embeddings` | text → vector |
| `POST /chat/completions` | text → text (used by later phases) |

Because the difference is only a URL and an API key, "local or network" is a configuration
choice, not an architectural one. A provider can be removed without touching project files.

## 2. Presets

Embedding and chat models are selected separately, because a provider often offers only one kind
(DeepSeek, for example, publishes no embeddings endpoint).

### Embedding (semantic search)

| Preset | Location | Base URL | Suggested small models |
| --- | --- | --- | --- |
| Ollama | ローカル | `http://127.0.0.1:11434/v1` | `bge-m3`, `multilingual-e5-small`, `paraphrase-multilingual`, `nomic-embed-text` |
| llama.cpp server | ローカル | `http://127.0.0.1:8080/v1` | `bge-m3`, `multilingual-e5-small`, `nomic-embed-text-v1.5` |
| LM Studio | ローカル | `http://127.0.0.1:1234/v1` | `text-embedding-nomic-embed-text-v1.5`, `bge-m3` |
| OpenAI（埋め込み） | ネットワーク | `https://api.openai.com/v1` | `text-embedding-3-small`, `text-embedding-3-large` |
| OpenAI互換API | ネットワーク | 任意 | 任意 |

### Chat (AI assistant)

| Preset | Location | Base URL | Suggested models | API key env |
| --- | --- | --- | --- | --- |
| DeepSeek | ネットワーク | `https://api.deepseek.com/v1` | `deepseek-chat`, `deepseek-reasoner` | `DEEPSEEK_API_KEY` |
| OpenAI（チャット） | ネットワーク | `https://api.openai.com/v1` | `gpt-4o-mini`, `gpt-4o` | `OPENAI_API_KEY` |
| Ollama（チャット） | ローカル | `http://127.0.0.1:11434/v1` | `qwen2.5:7b`, `llama3.1:8b`, `gemma2:9b` | — |
| OpenAI互換API | ネットワーク | 任意 | 任意 | 任意 |

The presets are suggestions, not limits: the base URL, model name and API-key environment
variable are all editable. The default chat preset is **DeepSeek**, so entering an API key is all
that is needed to start.

### Local models that work on a laptop

The intent for local use is a **small model that still performs well**. For embeddings, and for
Japanese footage in particular:

- `bge-m3` — multilingual (Japanese included), ~568M parameters, the default suggestion.
- `multilingual-e5-small` — ~118M parameters, multilingual, the lightest good option.
- `paraphrase-multilingual` — small and multilingual.
- `nomic-embed-text` — very small, but English-centric; use it for English material.

Embedding large numbers of scenes on CPU is practical: embedding is a single forward pass per
scene and the scene count is usually in the hundreds.

Starting a local server:

```bash
# Ollama
ollama pull bge-m3

# llama.cpp (GGUF model, embeddings enabled)
llama-server -m bge-m3.gguf --embeddings --port 8080
```

## 3. API keys

- A key can be **typed into the panel** (used for that session only, never persisted) or read from
  an **environment variable** (`DEEPSEEK_API_KEY` / `OPENAI_API_KEY` by default). A typed key takes
  precedence, so a session can override the system default without exporting anything.
- The app never stores the key in the project or in the editor's persisted settings.
- The name of the environment variable is configurable, so any key-holding variable can be used.
- When a request carries an `Authorization` header, curl receives the headers through a
  **private (0600) file** (`-H @file`) instead of the argument list, because arguments are visible
  to other users through `ps`.
- Local presets need no key.

## 4. Connection test and capability negotiation

「接続テスト」 calls `GET /models` and reports the model IDs the endpoint advertises, or the
server's error message. This verifies URL/key/network before any real work, and it is the seam
where richer capability negotiation (does this model support vision? tools? structured output?)
will grow. Today the adapter reports protocol-level capabilities
(`protocol::AiCapabilities`); a local embedding-only server may of course ignore `/chat/completions`.

Timeouts default to 600 s for a network model and 180 s for a local one, because a reasoning model
(or a long agent turn) can legitimately take minutes; the per-model timeout is adjustable up to
1800 s. If a request ends with `curl: (28) ... timed out`, raise the timeout or pick a faster model.

## 5. How models are used

Two tasks are implemented: **scene embeddings for semantic search** and the **AI assistant**.

### Embeddings

1. Phase 2 analysis produces scenes with transcript, OCR and a summary.
2. 「埋め込みを生成」 embeds each scene's text (`transcript / OCR / summary`, deduplicated and
   length-capped — never raw video).
   - Inputs are batched (`batch_size`); local models prefer 8–16, network APIs 64+.
   - Progress is reported per batch, and the work is cancellable.
3. Vectors are stored in the media's cache directory as `embeddings.json`, keyed by scene ID
   (`VirScene::embedding_ref` records the model, per the VIR specification). Nothing is written
   into the project file.
4. 「検索」 embeds the query with the *same* model and ranks scenes by cosine similarity. The
   search is refused with an explanation when the configured model differs from the one that
   produced the vectors, or when the vectors belong to an older analysis (re-analysis changes
   scene content, so stale vectors are never used).

Retrieval itself (`knowledge::search`) is model-agnostic: it only sees vectors, so swapping from a
local to a network model changes nothing downstream.

### Assistant (tool-calling agent)

1. The assistant sends a system prompt plus a **compact** project/timeline context and, when a
   medium has been analyzed, its scenes (`agent::project_context`). No frame or clip is uploaded.
2. It advertises the editor tools (`agent::tools::tool_specs`) as OpenAI-compatible function
   definitions and the model replies with plain text, tool calls, or both.
3. The app executes the tools in order — synchronous edits inline, analysis/search/VOICEVOX/export
   on a worker thread while the loop waits — and feeds each result back as a `tool` message, then
   asks again. `get_frame` additionally attaches a keyframe JPEG to the conversation as
   OpenAI-style `image_url` content, so a vision-capable model can look at the footage. This
   continues until the model answers with no tool calls (the finished-video message).
4. Tool calls are parsed into typed invocations (`agent::tools::parse_call`); a bad call becomes a
   readable error the model can correct, not a crash. All edits go through `EditCommand`, so they
   are non-destructive and undoable, and the user can turn execution off to get proposals only.
5. Models that reply with an AEP JSON block instead of tool calls are still supported:
   `agent::extract_aep` pulls the plan out (times in **seconds**) and `protocol::validate_aep`
   checks it before `EditHistory::execute_batch` applies it transactionally.

Because `deepseek-chat`/`deepseek-reasoner` are text-only here, the assistant reasons over the VIR
rather than the pixels; progressive visual inspection (`get_frame`, `get_clip`) is a later phase.

## 6. Degradation

- No model configured or unreachable → analysis (Phase 2) still works; only the embedding,
  connection-test and search actions are unavailable or fail with a message.
- A local server that is not running produces a connection error, not a crash.
- Deleting the analysis cache also removes the embeddings; they are derived data and can be
  regenerated.

## 7. Not yet

- Cross-media search: embeddings are stored per media, but the search UI currently ranks the
  scenes of the medium being viewed. Ranking across a whole project is a small step from here.
- Visual tools: `get_frame` shows a keyframe to the model, but `get_frames` (several at once),
  `get_clip` and `inspect_audio` are not implemented. Frame support depends on a vision-capable
  model; the adapter still reports `vision: false` as a static hint.
- Embedding-space migration, re-ranking, hybrid (keyword + vector) search.
