//! The tools the AI agent may call, and the parser that turns a raw tool call into a typed
//! request. The tool list is the *contract* between the model and the editor: it changes only
//! here, and the app is the only thing that executes it, so the model still never touches media
//! or the timeline directly.

use crate::protocol::{AiToolCall, AiToolSpec};
use serde_json::{Value, json};
use uuid::Uuid;

/// A parsed tool call. Everything the model asked for is validated into these types before the
/// app acts on it, so a bad call becomes a tool error the model can read rather than a panic.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolInvocation {
    /// Read the current timeline, assets and analyzed scenes.
    GetProjectState,
    /// Analyze a medium (Phase 2) so its scenes/transcript/OCR become available.
    AnalyzeMedia {
        asset_name: Option<String>,
    },
    /// Semantic search over the analyzed scenes.
    SearchScenes {
        query: String,
    },
    /// Fetch a keyframe image and attach it to the next message (vision).
    GetFrame {
        scene_id: Option<String>,
        timestamp: Option<f64>,
    },
    /// Move the playhead, so later "at the playhead" defaults land where the model expects.
    Seek {
        timestamp: f64,
    },
    AddMedia {
        path: Option<String>,
        asset_name: Option<String>,
        timeline_start: Option<f64>,
    },
    SplitClip {
        clip_id: Uuid,
        timestamp: f64,
    },
    TrimClip {
        clip_id: Uuid,
        source_in: f64,
        source_out: f64,
    },
    DeleteClip {
        clip_id: Uuid,
    },
    MoveClip {
        clip_id: Uuid,
        timeline_start: f64,
    },
    SetVolume {
        clip_id: Uuid,
        gain_db: f32,
    },
    MuteClip {
        clip_id: Uuid,
        muted: bool,
    },
    /// A text/subtitle item.
    AddText {
        text: String,
        timeline_start: Option<f64>,
        duration: Option<f64>,
        template: Option<String>,
    },
    /// VOICEVOX speech plus the matching subtitle, added together.
    AddSpeech {
        text: String,
        timeline_start: Option<f64>,
        speaker: Option<u32>,
        character: Option<String>,
        template: Option<String>,
    },
    ExportVideo {
        file_name: Option<String>,
    },
}

impl ToolInvocation {
    /// Whether the tool needs to wait (a network synthesis or a long FFmpeg export) instead of
    /// completing inside the current frame.
    pub fn is_async(&self) -> bool {
        matches!(
            self,
            Self::AnalyzeMedia { .. }
                | Self::SearchScenes { .. }
                | Self::AddSpeech { .. }
                | Self::ExportVideo { .. }
        )
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::GetProjectState => "get_project_state",
            Self::AnalyzeMedia { .. } => "analyze_media",
            Self::SearchScenes { .. } => "search_scenes",
            Self::GetFrame { .. } => "get_frame",
            Self::Seek { .. } => "seek",
            Self::AddMedia { .. } => "add_media",
            Self::SplitClip { .. } => "split_clip",
            Self::TrimClip { .. } => "trim_clip",
            Self::DeleteClip { .. } => "delete_clip",
            Self::MoveClip { .. } => "move_clip",
            Self::SetVolume { .. } => "set_volume",
            Self::MuteClip { .. } => "mute_clip",
            Self::AddText { .. } => "add_text",
            Self::AddSpeech { .. } => "add_speech",
            Self::ExportVideo { .. } => "export_video",
        }
    }
}

/// Every tool the agent exposes, with the JSON Schema the model uses to call it.
pub fn tool_specs() -> Vec<AiToolSpec> {
    vec![
        AiToolSpec::new(
            "get_project_state",
            "現在のタイムライン（レイヤー・クリップID・時間・素材）、素材一覧、再生位置、全体の長さ、解析済みシーンを取得する。編集の前に必ず一度呼ぶこと。",
            json!({ "type": "object", "properties": {}, "required": [] }),
        ),
        AiToolSpec::new(
            "analyze_media",
            "素材を解析してシーン・文字起こし・OCRを抽出する（時間がかかる）。素材の内容を踏まえて編集したいときに呼ぶ。asset_name 省略時は対象メディアを自動選択。",
            json!({
                "type": "object",
                "properties": {
                    "asset_name": { "type": "string", "description": "素材名（省略可）" }
                },
                "required": []
            }),
        ),
        AiToolSpec::new(
            "search_scenes",
            "解析済みシーンを意味で検索する。埋め込みが無ければ自動生成する。「〇〇の場面」を探すときに使う。",
            json!({
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"]
            }),
        ),
        AiToolSpec::new(
            "get_frame",
            "シーンのキーフレーム画像を取得し、次のメッセージに画像として添付する（見た目の確認）。scene_id か timestamp（秒）で指定。省略時は再生位置に最も近いシーン。",
            json!({
                "type": "object",
                "properties": {
                    "scene_id": { "type": "string" },
                    "timestamp": { "type": "number", "description": "秒" }
                },
                "required": []
            }),
        ),
        AiToolSpec::new(
            "seek",
            "再生位置（プレイヘッド）を秒で移動する。timeline_start を省略した追加系ツールの既定位置になる。",
            json!({
                "type": "object",
                "properties": { "timestamp": { "type": "number", "description": "秒" } },
                "required": ["timestamp"]
            }),
        ),
        AiToolSpec::new(
            "add_media",
            "動画/音声/画像をタイムラインに追加する。既に読み込み済みの素材は asset_name で、ファイルは絶対パス path で指定する。timeline_start 省略時は現在の再生位置。",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "ファイルの絶対パス" },
                    "asset_name": { "type": "string", "description": "既存素材の名前" },
                    "timeline_start": { "type": "number", "description": "秒" }
                },
                "required": []
            }),
        ),
        AiToolSpec::new(
            "split_clip",
            "クリップを指定したタイムライン時刻（秒）で分割する。",
            json!({
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string" },
                    "timestamp": { "type": "number", "description": "秒" }
                },
                "required": ["clip_id", "timestamp"]
            }),
        ),
        AiToolSpec::new(
            "trim_clip",
            "クリップの使用区間を素材内の秒で指定する（source_in から source_out）。",
            json!({
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string" },
                    "source_in": { "type": "number", "description": "素材内の開始秒" },
                    "source_out": { "type": "number", "description": "素材内の終了秒" }
                },
                "required": ["clip_id", "source_in", "source_out"]
            }),
        ),
        AiToolSpec::new(
            "delete_clip",
            "クリップをタイムラインから削除する（元メディアは消さない）。",
            json!({
                "type": "object",
                "properties": { "clip_id": { "type": "string" } },
                "required": ["clip_id"]
            }),
        ),
        AiToolSpec::new(
            "move_clip",
            "クリップの開始位置をタイムライン上で秒指定して移動する。",
            json!({
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string" },
                    "timeline_start": { "type": "number", "description": "秒" }
                },
                "required": ["clip_id", "timeline_start"]
            }),
        ),
        AiToolSpec::new(
            "set_volume",
            "クリップの音量をdBで設定する（0が原音、負で小さく）。",
            json!({
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string" },
                    "gain_db": { "type": "number" }
                },
                "required": ["clip_id", "gain_db"]
            }),
        ),
        AiToolSpec::new(
            "mute_clip",
            "クリップの音声をミュートする／解除する。",
            json!({
                "type": "object",
                "properties": {
                    "clip_id": { "type": "string" },
                    "muted": { "type": "boolean" }
                },
                "required": ["clip_id", "muted"]
            }),
        ),
        AiToolSpec::new(
            "add_text",
            "字幕やテロップのテキストを追加する。duration 省略時はテンプレートか既定の長さ。template でテンプレート名を指定できる。",
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" },
                    "timeline_start": { "type": "number", "description": "秒。省略時は再生位置" },
                    "duration": { "type": "number", "description": "秒" },
                    "template": { "type": "string", "description": "テンプレート名" }
                },
                "required": ["text"]
            }),
        ),
        AiToolSpec::new(
            "add_speech",
            "VOICEVOXで音声を生成し、同じ長さの字幕と合わせて1つのセリフアイテムとしてタイムラインへ追加する（音声＋字幕が一体で移動・トリム・削除できる）。ナレーションや掛け合いに使う。character でキャラ名を指定すると、そのキャラの話者・声の設定・字幕スタイルが適用される。template を省略すると話者に紐付いた字幕スタイルを使う。",
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "読み上げる文章" },
                    "timeline_start": { "type": "number", "description": "秒。省略時は再生位置" },
                    "speaker": { "type": "integer", "description": "VOICEVOXの話者ID。character 省略時に使う" },
                    "character": { "type": "string", "description": "キャラ名。話者・声・字幕スタイルをまとめて指定" },
                    "template": { "type": "string", "description": "字幕テンプレート名。省略すると話者に紐付いたテンプレートを使う" }
                },
                "required": ["text"]
            }),
        ),
        AiToolSpec::new(
            "export_video",
            "編集中のプロジェクトをMP4として書き出す。完成したら最後に一度だけ呼ぶ。",
            json!({
                "type": "object",
                "properties": {
                    "file_name": { "type": "string", "description": "拡張子なしのファイル名（省略可）" }
                },
                "required": []
            }),
        ),
    ]
}

/// Parses a raw tool call into a typed invocation, or returns a message the model can act on.
pub fn parse_call(call: &AiToolCall) -> Result<ToolInvocation, String> {
    let arguments: Value = if call.arguments.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&call.arguments)
            .map_err(|error| format!("引数がJSONではありません: {error}"))?
    };
    let args = Arguments(&arguments);
    match call.name.as_str() {
        "get_project_state" => Ok(ToolInvocation::GetProjectState),
        "analyze_media" => Ok(ToolInvocation::AnalyzeMedia {
            asset_name: args.optional_string("asset_name"),
        }),
        "search_scenes" => Ok(ToolInvocation::SearchScenes {
            query: args.string("query")?,
        }),
        "get_frame" => Ok(ToolInvocation::GetFrame {
            scene_id: args.optional_string("scene_id"),
            timestamp: args.optional_number("timestamp"),
        }),
        "seek" => Ok(ToolInvocation::Seek {
            timestamp: args.number("timestamp")?,
        }),
        "add_media" => Ok(ToolInvocation::AddMedia {
            path: args.optional_string("path"),
            asset_name: args.optional_string("asset_name"),
            timeline_start: args.optional_number("timeline_start"),
        }),
        "split_clip" => Ok(ToolInvocation::SplitClip {
            clip_id: args.uuid("clip_id")?,
            timestamp: args.number("timestamp")?,
        }),
        "trim_clip" => Ok(ToolInvocation::TrimClip {
            clip_id: args.uuid("clip_id")?,
            source_in: args.number("source_in")?,
            source_out: args.number("source_out")?,
        }),
        "delete_clip" => Ok(ToolInvocation::DeleteClip {
            clip_id: args.uuid("clip_id")?,
        }),
        "move_clip" => Ok(ToolInvocation::MoveClip {
            clip_id: args.uuid("clip_id")?,
            timeline_start: args.number("timeline_start")?,
        }),
        "set_volume" => Ok(ToolInvocation::SetVolume {
            clip_id: args.uuid("clip_id")?,
            gain_db: args.number("gain_db")? as f32,
        }),
        "mute_clip" => Ok(ToolInvocation::MuteClip {
            clip_id: args.uuid("clip_id")?,
            muted: args.boolean("muted")?,
        }),
        "add_text" => Ok(ToolInvocation::AddText {
            text: args.string("text")?,
            timeline_start: args.optional_number("timeline_start"),
            duration: args.optional_number("duration"),
            template: args.optional_string("template"),
        }),
        "add_speech" => Ok(ToolInvocation::AddSpeech {
            text: args.string("text")?,
            timeline_start: args.optional_number("timeline_start"),
            speaker: args
                .optional_number("speaker")
                .map(|value| value.max(0.0) as u32),
            character: args.optional_string("character"),
            template: args.optional_string("template"),
        }),
        "export_video" => Ok(ToolInvocation::ExportVideo {
            file_name: args.optional_string("file_name"),
        }),
        other => Err(format!("未知のツールです: {other}")),
    }
}

/// A short line for the conversation transcript.
pub fn summarize_call(call: &AiToolCall) -> String {
    let detail = match parse_call(call) {
        Ok(invocation) => match invocation {
            ToolInvocation::GetProjectState => "現在の状態を確認".to_owned(),
            ToolInvocation::AnalyzeMedia { asset_name } => format!(
                "素材を解析{}",
                asset_name
                    .map(|name| format!(": {name}"))
                    .unwrap_or_default()
            ),
            ToolInvocation::SearchScenes { query } => {
                format!("シーンを検索「{}」", ellipsize(&query, 20))
            }
            ToolInvocation::GetFrame { .. } => "フレーム画像を取得".to_owned(),
            ToolInvocation::Seek { timestamp } => format!("再生位置を {timestamp:.2}s へ"),
            ToolInvocation::AddMedia {
                asset_name, path, ..
            } => format!(
                "素材を追加: {}",
                asset_name.or(path).unwrap_or_else(|| "?".to_owned())
            ),
            ToolInvocation::SplitClip { timestamp, .. } => format!("分割 @ {timestamp:.2}s"),
            ToolInvocation::TrimClip {
                source_in,
                source_out,
                ..
            } => format!("トリム {source_in:.2}s–{source_out:.2}s"),
            ToolInvocation::DeleteClip { .. } => "クリップを削除".to_owned(),
            ToolInvocation::MoveClip { timeline_start, .. } => {
                format!("移動 → {timeline_start:.2}s")
            }
            ToolInvocation::SetVolume { gain_db, .. } => format!("音量 {gain_db:.1} dB"),
            ToolInvocation::MuteClip { muted, .. } => {
                format!("ミュート {}", if muted { "オン" } else { "オフ" })
            }
            ToolInvocation::AddText { text, .. } => format!("字幕「{}」", ellipsize(&text, 20)),
            ToolInvocation::AddSpeech { text, .. } => {
                format!("読み上げ「{}」", ellipsize(&text, 20))
            }
            ToolInvocation::ExportVideo { .. } => "動画を書き出し".to_owned(),
        },
        Err(message) => message,
    };
    format!("{}: {detail}", call.name)
}

fn ellipsize(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    text.chars().take(max_chars).collect::<String>() + "…"
}

/// Small typed accessor over tool arguments, so every field reports a readable error.
struct Arguments<'a>(&'a Value);

impl Arguments<'_> {
    fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key).filter(|value| !value.is_null())
    }

    fn string(&self, key: &str) -> Result<String, String> {
        self.get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("`{key}` は必須です"))
    }

    fn optional_string(&self, key: &str) -> Option<String> {
        self.get(key).and_then(Value::as_str).map(str::to_owned)
    }

    fn number(&self, key: &str) -> Result<f64, String> {
        self.get(key)
            .and_then(Value::as_f64)
            .ok_or_else(|| format!("`{key}` は数値で必須です"))
    }

    fn optional_number(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(Value::as_f64)
    }

    fn boolean(&self, key: &str) -> Result<bool, String> {
        self.get(key)
            .and_then(Value::as_bool)
            .ok_or_else(|| format!("`{key}` は真偽値で必須です"))
    }

    fn uuid(&self, key: &str) -> Result<Uuid, String> {
        let raw = self.string(key)?;
        Uuid::parse_str(raw.trim()).map_err(|_| format!("`{key}` がUUIDではありません: {raw}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, arguments: &str) -> AiToolCall {
        AiToolCall {
            id: "c1".to_owned(),
            name: name.to_owned(),
            arguments: arguments.to_owned(),
        }
    }

    #[test]
    fn every_spec_parses_as_a_function_schema() {
        for spec in tool_specs() {
            assert_eq!(spec.parameters["type"], "object", "{}", spec.name);
            assert!(!spec.description.is_empty(), "{}", spec.name);
        }
    }

    #[test]
    fn parses_a_trim_call() {
        let id = Uuid::new_v4();
        let parsed = parse_call(&call(
            "trim_clip",
            &format!("{{\"clip_id\":\"{id}\",\"source_in\":2.0,\"source_out\":12.0}}"),
        ))
        .unwrap();
        assert_eq!(
            parsed,
            ToolInvocation::TrimClip {
                clip_id: id,
                source_in: 2.0,
                source_out: 12.0
            }
        );
    }

    #[test]
    fn optional_arguments_may_be_omitted() {
        let parsed = parse_call(&call("add_speech", "{\"text\":\"こんにちは\"}")).unwrap();
        assert_eq!(
            parsed,
            ToolInvocation::AddSpeech {
                text: "こんにちは".to_owned(),
                timeline_start: None,
                speaker: None,
                character: None,
                template: None
            }
        );
        assert!(parsed.is_async());
    }

    #[test]
    fn parses_the_analysis_and_search_tools() {
        assert_eq!(
            parse_call(&call("analyze_media", "{}")).unwrap(),
            ToolInvocation::AnalyzeMedia { asset_name: None }
        );
        assert_eq!(
            parse_call(&call("analyze_media", "{\"asset_name\":\"a.mp4\"}")).unwrap(),
            ToolInvocation::AnalyzeMedia {
                asset_name: Some("a.mp4".to_owned())
            }
        );
        let search = parse_call(&call("search_scenes", "{\"query\":\"雪の駅\"}")).unwrap();
        assert_eq!(
            search,
            ToolInvocation::SearchScenes {
                query: "雪の駅".to_owned()
            }
        );
        assert!(search.is_async());
        assert!(parse_call(&call("search_scenes", "{}")).is_err());
    }

    #[test]
    fn malformed_calls_report_a_usable_error() {
        assert!(parse_call(&call("trim_clip", "not json")).is_err());
        assert!(parse_call(&call("trim_clip", "{\"source_in\":1}")).is_err());
        assert!(parse_call(&call("nope", "{}")).is_err());
        let bad_uuid = parse_call(&call("delete_clip", "{\"clip_id\":\"abc\"}"));
        assert!(bad_uuid.unwrap_err().contains("UUID"));
    }
}
