//! AI agent layer (Phase 4/5).
//!
//! This module sits between the Video Knowledge Engine and the provider adapter
//! (ARCHITECTURE.md). It is deliberately provider-neutral:
//!
//! - it builds a **compact** context from the project and the analyzed VIR — never the video;
//! - it asks the model for an edit plan;
//! - it extracts an AEP document from the reply, for the editing core to validate and apply.
//!
//! It does not touch FFmpeg, the timeline or the project store, and it never mutates media.

use crate::domain::{Project, ProjectId, TimelineItem};
use crate::models::{ModelConfig, OpenAiCompatible};
use crate::protocol::{
    AEP_PROTOCOL, AEP_VERSION, AepDocument, AiError, AiMessage, AiPermission, AiProvider,
    AiRequest, AiResponse, AiToolRequest, AiToolResponse, AiToolSpec, AiTurn, VirMedia,
};

pub mod tools;

/// Upper bounds that keep a prompt small no matter how large the project is. The AI is expected to
/// ask for more through search rather than receive everything at once (AI-TOOLS.md section 4).
const MAX_TIMELINE_ITEMS: usize = 120;
const MAX_SCENES: usize = 80;
const MAX_SCENE_TEXT_CHARS: usize = 200;

/// Instructions shared by every request: what the editor is, and the exact AEP shape to answer
/// with when proposing edits.
pub fn system_prompt() -> String {
    format!(
        "You are the editing assistant of an AI-native, non-destructive video editor.\n\
         You help edit an existing project. You never touch source media directly: edits are\n\
         expressed as {AEP_PROTOCOL} (AI Editing Protocol) documents that the editor validates and\n\
         applies.\n\n\
         Rules:\n\
         - Answer the user's question in Japanese, briefly.\n\
         - When (and only when) you want to change the timeline, end your reply with a single\n\
           fenced ```json block containing an {AEP_PROTOCOL} document.\n\
         - Times are in SECONDS (floating point), measured from the start of the video.\n\
         - Use the clip_id values listed in the context; never invent ids.\n\
         - Supported operations: split_clip, trim_clip, delete_clip, move_clip, set_volume, mute.\n\
         - You may omit operation_id and project_id; the editor fills them in.\n\
         - If nothing should change, answer without a JSON block.\n\n\
         AEP JSON shape:\n\
         ```json\n\
         {{\n\
           \"protocol\": \"{AEP_PROTOCOL}\",\n\
           \"version\": \"{AEP_VERSION}\",\n\
           \"operations\": [\n\
             {{ \"type\": \"trim_clip\", \"clip_id\": \"<uuid>\", \"source_in\": 12.4, \"source_out\": 18.7, \"reason\": \"...\" }}\n\
           ]\n\
         }}\n\
         ```"
    )
}

/// Compact, human-readable description of the project timeline and, when available, the analyzed
/// medium (scenes with their transcript/OCR). Bounded in size on purpose.
pub fn project_context(project: &Project, media: Option<&VirMedia>) -> String {
    let mut out = String::new();
    out.push_str("## Project\n");
    out.push_str(&format!("title: {}\n", project.title));
    out.push_str(&format!("project_id: {}\n", project.id));
    out.push_str(&format!(
        "canvas: {}x{} @ {:.3}fps\n",
        project.sequence.settings.width,
        project.sequence.settings.height,
        project.sequence.settings.frame_rate
    ));
    out.push_str(&format!(
        "duration: {}\n",
        seconds(project.duration().as_seconds())
    ));
    out.push_str(&format!(
        "assets: {}\n",
        project
            .assets
            .iter()
            .map(|asset| format!(
                "{} ({})",
                asset.name,
                if asset.metadata.is_image {
                    "image"
                } else if asset.metadata.has_video {
                    "video"
                } else {
                    "audio"
                }
            ))
            .collect::<Vec<_>>()
            .join(", ")
    ));

    out.push_str("\n## Layers (top to bottom) and items\n");
    let mut items = 0usize;
    'layers: for layer in &project.sequence.layers {
        out.push_str(&format!("- layer \"{}\"\n", layer.name));
        for item in &layer.items {
            if items >= MAX_TIMELINE_ITEMS {
                out.push_str("  … (truncated)\n");
                break 'layers;
            }
            items += 1;
            match item {
                TimelineItem::Clip(clip) => {
                    let asset = project
                        .asset(clip.asset_id)
                        .map(|asset| asset.name.as_str())
                        .unwrap_or("?");
                    out.push_str(&format!(
                        "  - clip {} \"{}\" {}–{} (source {}–{})\n",
                        clip.id,
                        asset,
                        seconds(clip.timeline_start.as_seconds()),
                        seconds(clip.timeline_end().as_seconds()),
                        seconds(clip.source_in.as_seconds()),
                        seconds(clip.source_out.as_seconds()),
                    ));
                }
                TimelineItem::Text(text) => {
                    out.push_str(&format!(
                        "  - text {} \"{}\" {}–{}\n",
                        text.id,
                        truncate(&text.text, 40),
                        seconds(text.timeline_start.as_seconds()),
                        seconds(text.timeline_end().as_seconds()),
                    ));
                }
                TimelineItem::Speech(speech) => {
                    let asset = project
                        .asset(speech.audio.asset_id)
                        .map(|asset| asset.name.as_str())
                        .unwrap_or("?");
                    out.push_str(&format!(
                        "  - speech {} \"{}\" {}–{} (audio {}, {})\n",
                        speech.id(),
                        truncate(&speech.subtitle.text, 40),
                        seconds(speech.timeline_start().as_seconds()),
                        seconds(speech.timeline_end().as_seconds()),
                        asset,
                        seconds(speech.duration().as_seconds()),
                    ));
                }
                TimelineItem::Shape(shape) => {
                    out.push_str(&format!(
                        "  - shape {} {} {}–{}\n",
                        shape.id,
                        shape.kind.label(),
                        seconds(shape.timeline_start.as_seconds()),
                        seconds(shape.timeline_end().as_seconds()),
                    ));
                }
            }
        }
    }

    if let Some(media) = media {
        out.push_str("\n## Analyzed scenes\n");
        if media.scenes.is_empty() {
            out.push_str("(none detected)\n");
        }
        for (index, scene) in media.scenes.iter().enumerate() {
            if index >= MAX_SCENES {
                out.push_str("… (truncated)\n");
                break;
            }
            out.push_str(&format!(
                "- scene {} {}–{}",
                scene.scene_id,
                seconds(scene.start.as_seconds()),
                seconds(scene.end.as_seconds()),
            ));
            if let Some(importance) = scene.importance {
                out.push_str(&format!(" importance={importance:.2}"));
            }
            let mut text = scene.summary.clone();
            if text.is_empty()
                && let Some(transcript) = &scene.transcript
            {
                text = transcript.clone();
            }
            if !scene.ocr.is_empty() {
                if !text.is_empty() {
                    text.push_str(" / ");
                }
                text.push_str(&scene.ocr.join(" "));
            }
            if !text.is_empty() {
                out.push_str(&format!(": {}", truncate(&text, MAX_SCENE_TEXT_CHARS)));
            }
            out.push('\n');
        }
    }

    out
}

/// System prompt for the tool-using agent: it turns a natural-language request into a finished
/// video by driving the editor's own features, and ends by telling the user to preview the result.
pub fn agent_system_prompt() -> String {
    "あなたはAIネイティブな動画編集ソフトの制作アシスタントです。\n\
         ユーザーの自然言語の要望（どんな動画にしたいか）だけを頼りに、提供されたツールを使っ\n\
         て1本の動画を最後まで作り上げます。素材は非破壊で、あなたは元ファイルを変更しません。\n\
         \n\
         進め方:\n\
         1. まず get_project_state で現在のタイムライン・素材・再生位置・長さ・解析済みシーンを確認する。\n\
         2. シーン情報が無く内容を踏まえたいときは analyze_media で素材を解析する（長いので完了まで待つ）。\n\
         3. 「〇〇の場面」を探すときは search_scenes で意味検索し、見た目を確認したいときは get_frame で画像を取得する（画像は次のメッセージに添付される）。\n\
         4. 不要な部分を split_clip / delete_clip / trim_clip / move_clip で整える。\n\
         5. 必要なら add_media で素材を並べる。クリップは同じレイヤーで重ならないようにする。\n\
         6. ナレーションや掛け合いは add_speech（VOICEVOXで音声＋字幕を1つのセリフアイテムとして生成。character でキャラ名を指定すると、そのキャラの話者・声の設定・字幕スタイルが適用される）。\n\
         7. テロップや字幕は add_text。見出しは大きめのテンプレートを template で指定してよい。\n\
         8. 音量は set_volume / mute_clip で調整する。\n\
         9. 完成したら最後に export_video を1回だけ呼ぶ。\n\
         10. 書き出しが終わったら「動画が完成しました。再生してご確認ください。」と短く伝えて終了する。\n\
         \n\
         厳守事項:\n\
         - すべての時刻は秒（小数可）。\n\
         - clip_id は get_project_state が返した値だけを使う。新しいIDを捏造しない。\n\
         - ツールの結果に エラー と書かれていたら、原因を考えて引数を直して再試行する。\n\
         - 説明は日本語で簡潔に。長いJSONや内部IDの羅列をユーザーに見せない。\n\
         - ツール実行が許可されていない場合は、実行せずに手順を提案するだけにする。\n\
         - 素材が1つも無いときは、勝手に作らず、ユーザーに素材の絶対パスを尋ねる。\n\
         - VOICEVOXが使えない（add_speech が失敗する）場合は、add_text の字幕で代替する。"
        .to_owned()
}

/// Runs one tool-calling turn for the agent loop.
pub fn ask_tools(
    provider: &dyn AiProvider,
    system: String,
    turns: Vec<AiTurn>,
    tools: Vec<AiToolSpec>,
) -> Result<AiToolResponse, AiError> {
    provider.send_tools(AiToolRequest {
        system_context: system,
        turns,
        tools,
        permission: AiPermission::Assist,
    })
}

/// Convenience for worker threads: builds the adapter from a configuration and asks once.
pub fn ask_tools_config(
    config: ModelConfig,
    system: String,
    turns: Vec<AiTurn>,
    tools: Vec<AiToolSpec>,
) -> Result<AiToolResponse, AiError> {
    ask_tools(&OpenAiCompatible::new(config), system, turns, tools)
}

/// Sends one turn to the provider. History is oldest-first and holds prior user/assistant turns.
pub fn ask(
    provider: &dyn AiProvider,
    system: String,
    history: Vec<AiMessage>,
    user: String,
) -> Result<AiResponse, AiError> {
    provider.send(AiRequest {
        system_context: system,
        history,
        user_message: user,
        // The assistant proposes; the user explicitly applies. Nothing is written automatically.
        permission: AiPermission::Suggest,
    })
}

/// Convenience for worker threads: builds the adapter from a configuration and asks once.
pub fn ask_config(
    config: ModelConfig,
    system: String,
    history: Vec<AiMessage>,
    user: String,
) -> Result<AiResponse, AiError> {
    ask(&OpenAiCompatible::new(config), system, history, user)
}

/// Extracts an AEP document from a model reply.
///
/// Returns:
/// - `None` when the reply contains no JSON object at all (a pure explanation);
/// - `Some(Ok(document))` when an object parses as AEP;
/// - `Some(Err(message))` when JSON was present but could not be read as AEP.
///
/// `project_id` is written into the result, so a model that omits or invents the id still targets
/// the project being edited.
pub fn extract_aep(reply: &str, project_id: ProjectId) -> Option<Result<AepDocument, String>> {
    let objects = json_objects(reply);
    if objects.is_empty() {
        return None;
    }
    let mut last_error = String::new();
    for object in objects {
        match serde_json::from_str::<AepDocument>(object) {
            Ok(mut document) => {
                document.project_id = project_id;
                return Some(Ok(document));
            }
            Err(error) => last_error = error.to_string(),
        }
    }
    Some(Err(last_error))
}

/// Collects balanced `{…}` substrings, ignoring braces inside JSON strings. Fenced code blocks are
/// preferred so a JSON example quoted inside prose is not mistaken for the plan.
fn json_objects(text: &str) -> Vec<&str> {
    let mut objects = Vec::new();
    for (start, _) in text.match_indices("```json") {
        if let Some(body) = fenced_body(&text[start..]) {
            objects.extend(balanced_objects(body));
        }
    }
    if !objects.is_empty() {
        return objects;
    }
    for (start, _) in text.match_indices("```") {
        if let Some(body) = fenced_body(&text[start..]) {
            objects.extend(balanced_objects(body));
        }
    }
    if !objects.is_empty() {
        return objects;
    }
    balanced_objects(text)
}

/// The text between the first line of a ``` fence and its closing fence.
fn fenced_body(text: &str) -> Option<&str> {
    let after_open = text.find('\n')? + 1;
    let rest = &text[after_open..];
    let close = rest.find("```")?;
    Some(&rest[..close])
}

fn balanced_objects(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut objects = Vec::new();
    let mut start: Option<usize> = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, &byte) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => {
                if depth == 0 {
                    start = Some(index);
                }
                depth += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0
                    && let Some(begin) = start.take()
                {
                    objects.push(&text[begin..=index]);
                }
            }
            _ => {}
        }
    }
    objects
}

fn seconds(value: f64) -> String {
    format!("{value:.2}")
}

/// One-line, human-readable summary of an operation, for the UI's plan preview.
pub fn operation_summary(operation: &crate::protocol::AepOperation) -> String {
    use crate::protocol::AepOperation::*;
    match operation {
        SplitClip {
            clip_id,
            timeline_time,
            ..
        } => format!(
            "クリップを分割 {} @ {:.2}s",
            short_id(clip_id),
            timeline_time.as_seconds()
        ),
        TrimClip {
            clip_id,
            source_in,
            source_out,
            ..
        } => format!(
            "トリム {} → {:.2}s–{:.2}s",
            short_id(clip_id),
            source_in.as_seconds(),
            source_out.as_seconds()
        ),
        DeleteClip { clip_id, .. } => format!("クリップを削除 {}", short_id(clip_id)),
        MoveClip {
            clip_id,
            timeline_start,
            ..
        } => format!(
            "クリップを移動 {} → {:.2}s",
            short_id(clip_id),
            timeline_start.as_seconds()
        ),
        SetVolume {
            clip_id, gain_db, ..
        } => format!("音量 {} → {gain_db:.1} dB", short_id(clip_id)),
        Mute { clip_id, muted, .. } => format!(
            "ミュート {} → {}",
            short_id(clip_id),
            if *muted { "オン" } else { "オフ" }
        ),
    }
}

fn short_id(id: &uuid::Uuid) -> String {
    id.to_string().chars().take(8).collect()
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut truncated: String = text.chars().take(max_chars).collect();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Clip, MediaAsset, MediaMetadata, TimelineTime};
    use crate::protocol::AepOperation;
    use uuid::Uuid;

    fn project_with_clip() -> (Project, Uuid) {
        let mut project = Project::new("テスト");
        let metadata = MediaMetadata {
            duration: TimelineTime::from_seconds(60.0),
            has_video: true,
            ..MediaMetadata::default()
        };
        let asset = MediaAsset {
            id: Uuid::new_v4(),
            name: "clip.mp4".to_owned(),
            path: "/tmp/clip.mp4".into(),
            metadata,
        };
        let clip = Clip::new(&asset, TimelineTime::ZERO, (1920.0, 1080.0));
        let clip_id = clip.id;
        project.assets.push(asset);
        project.sequence.layers[0]
            .items
            .push(TimelineItem::Clip(clip));
        (project, clip_id)
    }

    #[test]
    fn context_lists_clip_ids_and_bounds_size() {
        let (project, clip_id) = project_with_clip();
        let context = project_context(&project, None);
        assert!(context.contains(&clip_id.to_string()));
        assert!(context.contains("project_id:"));
        // Times are seconds, not microseconds.
        assert!(context.contains("0.00–60.00"));
    }

    #[test]
    fn pure_prose_has_no_aep() {
        assert!(extract_aep("これは説明だけの返答です。", Uuid::nil()).is_none());
    }

    #[test]
    fn fenced_aep_is_extracted_and_times_are_seconds() {
        let clip = Uuid::new_v4();
        let reply = format!(
            "了解しました。\n```json\n{{\n  \"protocol\": \"AEP\",\n  \"operations\": [\n    {{ \"type\": \"trim_clip\", \"clip_id\": \"{clip}\", \"source_in\": 12.4, \"source_out\": 18.7 }}\n  ]\n}}\n```\n"
        );
        let project_id = Uuid::new_v4();
        let document = extract_aep(&reply, project_id).unwrap().unwrap();
        assert_eq!(document.project_id, project_id);
        match &document.operations[0] {
            AepOperation::TrimClip {
                source_in,
                source_out,
                ..
            } => {
                assert!((source_in.as_seconds() - 12.4).abs() < 1e-6);
                assert!((source_out.as_seconds() - 18.7).abs() < 1e-6);
            }
            other => panic!("unexpected operation: {other:?}"),
        }
    }

    #[test]
    fn a_json_example_in_prose_is_not_confused_with_a_plan() {
        let reply = "例: {\"hello\": \"world\"} のような形です。";
        // The object parses as JSON but not as AEP, so it is an error, not silently ignored.
        assert!(matches!(extract_aep(reply, Uuid::nil()), Some(Err(_))));
    }

    #[test]
    fn missing_operation_id_is_filled_in() {
        let clip = Uuid::new_v4();
        let reply = format!(
            "```json\n{{\"operations\":[{{\"type\":\"delete_clip\",\"clip_id\":\"{clip}\"}}]}}\n```"
        );
        let document = extract_aep(&reply, Uuid::nil()).unwrap().unwrap();
        assert_eq!(document.operations.len(), 1);
        assert_eq!(document.protocol, AEP_PROTOCOL);
        assert_eq!(document.version, AEP_VERSION);
    }
}
