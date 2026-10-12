//! AI assistant panel (Phase 4/5).
//!
//! The user describes the video they want in natural language. The model receives a compact
//! project context and a set of tools (`agent::tools`) that drive the editor's real features —
//! cuts, arrangement, VOICEVOX speech with subtitles, text, export — and works through them until
//! the video is finished. Provider details stay in the model configuration, and nothing is ever
//! applied outside the editing core.

use super::analysis::AnalysisState;
use super::panels::UiAction;
use crate::agent::{self, tools};
use crate::models::{ModelConfig, default_chat_config};
use crate::protocol::{AepDocument, AiImage, AiToolCall, AiTurn};
use eframe::egui::{self, Ui};
use std::time::Instant;

#[derive(Clone, Debug, Default)]
pub enum ChatStatus {
    #[default]
    Idle,
    Waiting,
    Failed(String),
}

/// UI-side conversation state. `turns` is both displayed and replayed as the request history.
pub struct ChatState {
    pub config: ModelConfig,
    pub turns: Vec<AiTurn>,
    pub input: String,
    pub status: ChatStatus,
    /// What the agent is doing right now ("動画を書き出しています…").
    pub activity: Option<String>,
    /// When `activity` was set, so the UI can show that a long step is still progressing.
    pub activity_started: Option<Instant>,
    /// Result of the last apply attempt or loop, shown until the next message.
    pub notice: Option<String>,
    /// Tool calls the model requested but which have not been executed yet.
    pub pending_calls: Vec<AiToolCall>,
    /// When the model replies with AEP JSON instead of tool calls, it is offered for review here.
    pub draft: Option<Result<AepDocument, String>>,
    /// Whether tool calls are executed. Turning this off makes the assistant propose only.
    pub allow_tools: bool,
}

impl Default for ChatState {
    /// The default chat model is DeepSeek, so entering an API key is enough to start.
    fn default() -> Self {
        Self {
            config: default_chat_config(),
            turns: Vec::new(),
            input: String::new(),
            status: ChatStatus::Idle,
            activity: None,
            activity_started: None,
            notice: None,
            pending_calls: Vec::new(),
            draft: None,
            allow_tools: true,
        }
    }
}

impl ChatState {
    pub fn is_waiting(&self) -> bool {
        matches!(self.status, ChatStatus::Waiting)
    }

    pub fn push_user(&mut self, text: String) {
        self.turns.push(AiTurn::user(text));
    }

    pub fn push_assistant(&mut self, content: String, tool_calls: Vec<AiToolCall>) {
        self.turns.push(AiTurn::Assistant {
            content,
            tool_calls,
        });
    }

    pub fn push_tool_result(&mut self, call_id: &str, name: &str, result: String) {
        self.turns
            .push(AiTurn::tool(call_id.to_owned(), name.to_owned(), result));
    }

    /// Sets the current activity and starts its timer, so slow steps show elapsed seconds.
    pub fn set_activity(&mut self, text: impl Into<String>) {
        self.activity = Some(text.into());
        self.activity_started = Some(Instant::now());
    }

    /// Attaches frames the agent asked for, so the model sees them on the next turn.
    pub fn push_images(&mut self, text: String, images: Vec<AiImage>) {
        self.turns.push(AiTurn::images(text, images));
    }

    pub fn clear(&mut self) {
        self.turns.clear();
        self.draft = None;
        self.notice = None;
        self.activity = None;
        self.pending_calls.clear();
        self.status = ChatStatus::Idle;
    }
}

pub(super) fn assistant_section(
    ui: &mut Ui,
    analysis: &mut AnalysisState,
    actions: &mut Vec<UiAction>,
) {
    ui.separator();
    egui::CollapsingHeader::new("③ AIアシスタント（動画制作）")
        .default_open(true)
        .show(ui, |ui| {
            ui.small(
                "作りたい動画を自然言語で指示すると、AIがタイムライン編集・カット・解析・検索・VOICEVOXの声入れ（字幕同時生成）・テロップ・書き出しまで順に進めます。モデルは「① AIモデル」で設定します。すべての編集は「元に戻す」で取り消せます。",
            );
            ui.separator();
            conversation(ui, analysis);
            ui.separator();
            composer(ui, analysis, actions);
            draft_section(ui, analysis, actions);
        });
}

fn conversation(ui: &mut Ui, analysis: &mut AnalysisState) {
    let chat = &analysis.chat;
    if chat.turns.is_empty() {
        ui.small("例:「この素材で30秒の紹介動画を作って。ナレーションも入れて」「冒頭の無音を切って、最後にテロップを入れて書き出して」");
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("chat_transcript")
        .max_height(320.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for turn in &chat.turns {
                match turn {
                    AiTurn::User(text) => {
                        ui.horizontal_wrapped(|ui| {
                            ui.strong("あなた:");
                            ui.label(text);
                        });
                    }
                    AiTurn::Assistant {
                        content,
                        tool_calls,
                    } => {
                        if !content.trim().is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                ui.strong("AI:");
                                ui.label(content);
                            });
                        }
                        for call in tool_calls {
                            ui.small(format!("🛠 {}", tools::summarize_call(call)));
                        }
                    }
                    AiTurn::Tool { content, .. } => {
                        ui.small(
                            egui::RichText::new(format!("↳ {}", ellipsize(content, 140))).weak(),
                        );
                    }
                    AiTurn::Images { text, images } => {
                        ui.horizontal_wrapped(|ui| {
                            ui.strong("フレーム送信:");
                            ui.label(text);
                            ui.small(format!("（{}枚）", images.len()));
                        });
                    }
                }
                ui.add_space(2.0);
            }
        });
}

fn composer(ui: &mut Ui, analysis: &mut AnalysisState, actions: &mut Vec<UiAction>) {
    let chat = &mut analysis.chat;
    let has_analysis = analysis.result.is_some();
    ui.horizontal(|ui| {
        // The box stays typeable while the agent works: only sending is blocked. Disabling it
        // as well would make the input dead for as long as a job runs (or hangs).
        let response = ui
            .add(
                egui::TextEdit::singleline(&mut chat.input)
                    // A stable id keeps focus (and the IME) across layout changes.
                    .id_salt("chat_input")
                    .hint_text("どんな動画にしたいか入力…")
                    .desired_width(320.0),
            )
            .on_hover_text("Escで入力リセット（IMEが文字を渡さなくなったとき）");
        let submitted =
            response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        // Esc blurs the field; taking the focus back in the same frame forces a one-frame IME
        // disable/enable cycle — the no-restart fix for an IME that stopped committing text.
        let reset_input = response.ctx.memory(|memory| {
            memory.had_focus_last_frame(response.id)
                && ui.input(|input| input.key_pressed(egui::Key::Escape))
        });
        let can_send = !chat.is_waiting() && !chat.input.trim().is_empty();
        let clicked = ui
            .add_enabled(can_send, egui::Button::new("送信"))
            .clicked();
        if (submitted || clicked) && can_send {
            actions.push(UiAction::SendChat);
        }
        if (submitted || clicked || reset_input) && !response.has_focus() {
            response.request_focus();
        }
        if ui
            .add_enabled(chat.is_waiting(), egui::Button::new("■ 中止"))
            .clicked()
        {
            actions.push(UiAction::CancelChat);
        }
        if ui
            .add_enabled(!chat.turns.is_empty(), egui::Button::new("クリア"))
            .clicked()
        {
            actions.push(UiAction::ClearChat);
        }
    });

    ui.horizontal(|ui| {
        ui.checkbox(
            &mut chat.allow_tools,
            "ツール実行を許可（オフにすると提案のみ）",
        );
        if has_analysis {
            ui.small("解析済みシーンの要約を文脈に含めます。");
        }
    });
    if !has_analysis {
        ui.small("まだ解析結果がありません。解析すると内容を踏まえた編集ができます。");
    }

    match &chat.status {
        ChatStatus::Idle => {}
        ChatStatus::Waiting => {
            let label = match (&chat.activity, chat.activity_started) {
                (Some(text), Some(started)) => {
                    format!("{text}（{:.0}秒）", started.elapsed().as_secs_f32())
                }
                (Some(text), None) => text.clone(),
                (None, _) => "考えています…".to_owned(),
            };
            ui.horizontal(|ui| {
                ui.spinner();
                ui.small(label);
            });
        }
        ChatStatus::Failed(message) => {
            ui.colored_label(
                egui::Color32::from_rgb(220, 100, 100),
                format!("失敗: {message}"),
            );
        }
    }
}

fn draft_section(ui: &mut Ui, analysis: &mut AnalysisState, actions: &mut Vec<UiAction>) {
    let chat = &mut analysis.chat;
    match &chat.draft {
        Some(Ok(document)) => {
            ui.separator();
            ui.heading(format!(
                "提案された編集プラン（{} 操作）",
                document.operations.len()
            ));
            for operation in &document.operations {
                ui.small(format!("・{}", agent::operation_summary(operation)));
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !document.operations.is_empty(),
                        egui::Button::new("▶ タイムラインに適用"),
                    )
                    .clicked()
                {
                    actions.push(UiAction::ApplyChatPlan);
                }
                if ui.button("破棄").clicked() {
                    actions.push(UiAction::DiscardChatPlan);
                }
            });
        }
        Some(Err(message)) => {
            ui.separator();
            ui.colored_label(
                egui::Color32::from_rgb(220, 120, 60),
                format!("編集プランを解釈できませんでした: {message}"),
            );
            if ui.button("破棄").clicked() {
                actions.push(UiAction::DiscardChatPlan);
            }
        }
        None => {}
    }
    if let Some(notice) = &chat.notice {
        ui.small(notice);
    }
}

fn ellipsize(text: &str, max_chars: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        return flat;
    }
    flat.chars().take(max_chars).collect::<String>() + "…"
}
