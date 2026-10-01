//! Shared model-configuration editor.
//!
//! Embedding and chat models are separate slots, but both are configured the same way, so the
//! fields live here and each panel supplies the preset list that fits its purpose
//! (`models::PresetUse`).

use crate::models::{ModelConfig, ModelPreset, preset_by_id};
use eframe::egui::{self, Ui};

pub(super) fn model_config_editor(
    ui: &mut Ui,
    config: &mut ModelConfig,
    presets: &[&'static ModelPreset],
    salt: &str,
) {
    let mut selected_preset = None;
    let current_label = preset_by_id(&config.preset_id)
        .map(|preset| preset.label)
        .unwrap_or("カスタム");
    egui::ComboBox::from_id_salt(format!("{salt}_preset"))
        .selected_text(current_label)
        .show_ui(ui, |ui| {
            for preset in presets {
                if ui
                    .selectable_label(config.preset_id == preset.id, preset.label)
                    .clicked()
                {
                    selected_preset = Some(*preset);
                }
            }
        });
    if let Some(preset) = selected_preset {
        // A key typed for the previous preset is kept: switching providers should not force the
        // user to paste it again.
        let typed_key = std::mem::take(&mut config.api_key);
        *config = ModelConfig::from_preset(preset);
        config.api_key = typed_key;
    }

    ui.label(format!(
        "実行場所: {} ・ プロバイダ: {}",
        config.location.label(),
        config.provider.label()
    ));
    if let Some(preset) = preset_by_id(&config.preset_id) {
        ui.small(preset.note);
    }

    ui.horizontal(|ui| {
        ui.label("ベースURL");
        ui.add(
            egui::TextEdit::singleline(&mut config.base_url)
                .hint_text("https://api.deepseek.com/v1")
                .desired_width(260.0),
        );
    });

    ui.horizontal(|ui| {
        ui.label("モデル");
        ui.add(
            egui::TextEdit::singleline(&mut config.model)
                .hint_text("deepseek-chat")
                .desired_width(220.0),
        );
        let suggestions = preset_by_id(&config.preset_id)
            .map(|preset| preset.models)
            .unwrap_or(&[]);
        if !suggestions.is_empty() {
            egui::ComboBox::from_id_salt(format!("{salt}_model_suggestion"))
                .selected_text("候補")
                .show_ui(ui, |ui| {
                    for model in suggestions {
                        if ui.selectable_label(false, *model).clicked() {
                            config.model = (*model).to_owned();
                        }
                    }
                });
        }
    });

    ui.horizontal(|ui| {
        ui.label("APIキー");
        ui.add(
            egui::TextEdit::singleline(&mut config.api_key)
                .password(true)
                .hint_text("sk-…")
                .desired_width(220.0),
        );
        if config.api_key.trim().is_empty() {
            ui.small("（未入力）");
        } else {
            ui.small("✓ 入力済み（保存されません）");
        }
    });
    ui.small("入力したキーはこのセッションだけ使われ、ファイルには保存されません。");

    ui.horizontal(|ui| {
        ui.label("APIキー環境変数");
        ui.add(
            egui::TextEdit::singleline(&mut config.api_key_env)
                .hint_text("DEEPSEEK_API_KEY")
                .desired_width(180.0),
        );
        if config.api_key_env.trim().is_empty() {
            ui.small("（ローカルは不要）");
        } else if config.api_key_available() {
            ui.small("✓ 利用可能");
        } else {
            ui.colored_label(egui::Color32::from_rgb(220, 120, 60), "環境変数が未設定");
        }
    });

    egui::CollapsingHeader::new("詳細設定")
        .default_open(false)
        .show(ui, |ui| {
            ui.add(
                egui::Slider::new(&mut config.timeout_seconds, 5..=1800).text("タイムアウト (秒)"),
            );
            ui.add(egui::Slider::new(&mut config.batch_size, 1..=256).text("1回あたりの入力数"));
            ui.small(
                "ローカルの小さなモデルでは 8〜16 程度が安全です。APIでは 64 以上を推奨します。",
            );
        });
}
