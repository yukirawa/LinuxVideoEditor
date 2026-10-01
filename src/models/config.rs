//! Model configuration: where a model runs, which one it is, and how to reach it.
//!
//! Two locations are supported and selectable:
//!
//! - **Local** — a model running on this machine behind an OpenAI-compatible server
//!   (Ollama, llama.cpp's `llama-server`, LM Studio). Small, laptop-friendly models such as
//!   `bge-m3` (embeddings) or `qwen2.5:7b` (chat) are the intended targets.
//! - **Network** — a hosted OpenAI-compatible API (OpenAI, DeepSeek, …). The API key is read
//!   from an environment variable or typed into the panel for this session; it is never written
//!   into the project, and the typed value is not persisted either.
//!
//! Every backend speaks the same OpenAI-compatible protocol, so exactly one adapter is needed and
//! a provider can be swapped without touching the editing core (AI-PROVIDERS.md).

use super::ModelError;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelLocation {
    /// A model served on this machine (loopback).
    #[default]
    Local,
    /// A hosted API reached over the network.
    Network,
}

impl ModelLocation {
    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "ローカル",
            Self::Network => "ネットワーク",
        }
    }
}

/// Which vendor the endpoint belongs to. The wire protocol is the same for every variant — this
/// exists so the UI can name the provider and so capabilities that genuinely differ (for example,
/// DeepSeek publishes no embeddings endpoint) can be reasoned about instead of guessed.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Any server exposing `/embeddings`, `/chat/completions` and `/models`.
    #[default]
    OpenAiCompatible,
    /// DeepSeek's hosted API (`https://api.deepseek.com`). OpenAI-compatible wire format; chat
    /// only, because DeepSeek does not offer an embeddings endpoint.
    DeepSeek,
}

impl ProviderKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "OpenAI互換",
            Self::DeepSeek => "DeepSeek",
        }
    }

    /// Whether this endpoint can serve `POST /embeddings`.
    pub fn supports_embeddings(self) -> bool {
        match self {
            Self::OpenAiCompatible => true,
            Self::DeepSeek => false,
        }
    }

    /// Whether this endpoint can serve `POST /chat/completions`.
    pub fn supports_chat(self) -> bool {
        true
    }
}

/// What a preset is useful for. Embedding and chat models are different enough (and hosted
/// providers often offer only one) that they are selected separately.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresetUse {
    Embedding,
    Chat,
}

/// A ready-made endpoint plus models known to work well there.
pub struct ModelPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub provider: ProviderKind,
    pub location: ModelLocation,
    pub base_url: &'static str,
    /// Environment variable holding the API key; empty means no key is sent.
    pub api_key_env: &'static str,
    /// Laptop-friendly suggestions, best first.
    pub models: &'static [&'static str],
    pub uses: &'static [PresetUse],
    pub note: &'static str,
}

const EMBEDDING_ONLY: &[PresetUse] = &[PresetUse::Embedding];
const CHAT_ONLY: &[PresetUse] = &[PresetUse::Chat];
const BOTH: &[PresetUse] = &[PresetUse::Embedding, PresetUse::Chat];

pub const PRESETS: &[ModelPreset] = &[
    ModelPreset {
        id: "ollama",
        label: "Ollama（ローカル・埋め込み）",
        provider: ProviderKind::OpenAiCompatible,
        location: ModelLocation::Local,
        base_url: "http://127.0.0.1:11434/v1",
        api_key_env: "",
        models: &[
            "bge-m3",
            "multilingual-e5-small",
            "paraphrase-multilingual",
            "nomic-embed-text",
        ],
        uses: EMBEDDING_ONLY,
        note: "`ollama pull bge-m3` のようにモデルを取得しておきます。多言語対応で日本語の検索に向きます。",
    },
    ModelPreset {
        id: "llama_cpp",
        label: "llama.cpp server（ローカル・埋め込み）",
        provider: ProviderKind::OpenAiCompatible,
        location: ModelLocation::Local,
        base_url: "http://127.0.0.1:8080/v1",
        api_key_env: "",
        models: &["bge-m3", "multilingual-e5-small", "nomic-embed-text-v1.5"],
        uses: EMBEDDING_ONLY,
        note: "`llama-server -m <model.gguf> --embeddings` で起動します。GGUFの小さな埋め込みモデルを指定してください。",
    },
    ModelPreset {
        id: "lm_studio",
        label: "LM Studio（ローカル・埋め込み）",
        provider: ProviderKind::OpenAiCompatible,
        location: ModelLocation::Local,
        base_url: "http://127.0.0.1:1234/v1",
        api_key_env: "",
        models: &["text-embedding-nomic-embed-text-v1.5", "bge-m3"],
        uses: EMBEDDING_ONLY,
        note: "LM Studioのローカルサーバーを有効にしてから使います。",
    },
    ModelPreset {
        id: "openai_embeddings",
        label: "OpenAI（ネットワーク・埋め込み）",
        provider: ProviderKind::OpenAiCompatible,
        location: ModelLocation::Network,
        base_url: "https://api.openai.com/v1",
        api_key_env: "OPENAI_API_KEY",
        models: &["text-embedding-3-small", "text-embedding-3-large"],
        uses: EMBEDDING_ONLY,
        note: "埋め込み専用の設定です。APIキーは環境変数 OPENAI_API_KEY、または上のAPIキー欄から入力します。",
    },
    ModelPreset {
        id: "ollama_chat",
        label: "Ollama（ローカル・チャット）",
        provider: ProviderKind::OpenAiCompatible,
        location: ModelLocation::Local,
        base_url: "http://127.0.0.1:11434/v1",
        api_key_env: "",
        models: &["qwen2.5:7b", "llama3.1:8b", "gemma2:9b"],
        uses: CHAT_ONLY,
        note: "`ollama pull qwen2.5:7b` のように取得します。ローカルで完結し、APIキーは不要です。",
    },
    ModelPreset {
        id: "deepseek",
        label: "DeepSeek（ネットワーク・チャット）",
        provider: ProviderKind::DeepSeek,
        location: ModelLocation::Network,
        base_url: "https://api.deepseek.com/v1",
        api_key_env: "DEEPSEEK_API_KEY",
        models: &["deepseek-chat", "deepseek-reasoner"],
        uses: CHAT_ONLY,
        note: "platform.deepseek.com で発行したAPIキーを、環境変数 DEEPSEEK_API_KEY か下のAPIキー欄に入れるだけで使えます。",
    },
    ModelPreset {
        id: "openai_chat",
        label: "OpenAI（ネットワーク・チャット）",
        provider: ProviderKind::OpenAiCompatible,
        location: ModelLocation::Network,
        base_url: "https://api.openai.com/v1",
        api_key_env: "OPENAI_API_KEY",
        models: &["gpt-4o-mini", "gpt-4o"],
        uses: CHAT_ONLY,
        note: "APIキーは環境変数 OPENAI_API_KEY、または上のAPIキー欄から入力します。",
    },
    ModelPreset {
        id: "openai_compatible",
        label: "OpenAI互換API（ネットワーク・カスタム）",
        provider: ProviderKind::OpenAiCompatible,
        location: ModelLocation::Network,
        base_url: "https://",
        api_key_env: "OPENAI_API_KEY",
        models: &[],
        uses: BOTH,
        note: "任意のOpenAI互換エンドポイントを指定できます。",
    },
];

pub fn preset_by_id(id: &str) -> Option<&'static ModelPreset> {
    PRESETS.iter().find(|preset| preset.id == id)
}

/// Presets whose models can be used for the given purpose, best first.
pub fn presets_for(usage: PresetUse) -> impl Iterator<Item = &'static ModelPreset> {
    PRESETS
        .iter()
        .filter(move |preset| preset.uses.contains(&usage))
}

/// Default chat model: DeepSeek, so that entering an API key is all that is needed.
pub fn default_chat_config() -> ModelConfig {
    preset_by_id("deepseek")
        .map(ModelConfig::from_preset)
        .unwrap_or_default()
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ModelConfig {
    pub location: ModelLocation,
    /// Older stored configurations predate provider selection, so this defaults rather than
    /// invalidating the whole config.
    #[serde(default)]
    pub provider: ProviderKind,
    pub base_url: String,
    pub model: String,
    /// API key typed into the panel for this session. Deliberately not persisted: keys belong in
    /// the environment or in memory, never in the project or in the editor's stored settings.
    #[serde(skip)]
    pub api_key: String,
    /// Name of the environment variable that holds the API key (never the key itself).
    pub api_key_env: String,
    pub timeout_seconds: u32,
    /// Inputs per HTTP request. Local servers prefer smaller batches.
    pub batch_size: usize,
    /// Preset this configuration was derived from, for the UI only.
    pub preset_id: String,
}

impl Default for ModelConfig {
    fn default() -> Self {
        let preset = preset_by_id("ollama").expect("ollama preset exists");
        Self::from_preset(preset)
    }
}

impl ModelConfig {
    pub fn from_preset(preset: &ModelPreset) -> Self {
        Self {
            location: preset.location,
            provider: preset.provider,
            base_url: preset.base_url.to_owned(),
            model: preset
                .models
                .first()
                .copied()
                .unwrap_or_default()
                .to_owned(),
            api_key: String::new(),
            api_key_env: preset.api_key_env.to_owned(),
            // Chat with a reasoning model can legitimately take minutes, so the network default is
            // generous. Local servers are usually faster but still worth more than a minute.
            timeout_seconds: if preset.location == ModelLocation::Local {
                180
            } else {
                600
            },
            batch_size: if preset.location == ModelLocation::Local {
                16
            } else {
                64
            },
            preset_id: preset.id.to_owned(),
        }
    }

    pub fn clamp(&mut self) {
        self.base_url = self.base_url.trim().trim_end_matches('/').to_owned();
        self.model = self.model.trim().to_owned();
        self.api_key = self.api_key.trim().to_owned();
        self.api_key_env = self.api_key_env.trim().to_owned();
        self.timeout_seconds = self.timeout_seconds.clamp(5, 1800);
        self.batch_size = self.batch_size.clamp(1, 256);
    }

    /// Raises a stored timeout that is still the old built-in default, which was too short for a
    /// reasoning model (it caused 120 s `curl` timeouts). Values the user chose are left alone.
    pub fn migrate_timeout(&mut self) {
        if self.is_local() {
            if self.timeout_seconds == 60 {
                self.timeout_seconds = 180;
            }
        } else if self.timeout_seconds == 120 {
            self.timeout_seconds = 600;
        }
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        if self.base_url.trim().is_empty() || self.model.trim().is_empty() {
            return Err(ModelError::NotConfigured);
        }
        Ok(())
    }

    pub fn is_local(&self) -> bool {
        self.location == ModelLocation::Local
    }

    pub fn label(&self) -> String {
        format!(
            "{} / {} / {}",
            self.location.label(),
            self.model,
            self.base_url
        )
    }

    pub fn endpoint(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    /// The API key, if any: a key typed into the panel takes precedence over the environment
    /// variable so a session can override the system default without exporting anything.
    pub fn api_key(&self) -> Option<String> {
        let typed = self.api_key.trim();
        if !typed.is_empty() {
            return Some(typed.to_owned());
        }
        let name = self.api_key_env.trim();
        if name.is_empty() {
            return None;
        }
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    }

    pub fn api_key_available(&self) -> bool {
        if !self.api_key.trim().is_empty() {
            return true;
        }
        if self.api_key_env.trim().is_empty() {
            return self.is_local();
        }
        self.api_key().is_some()
    }

    pub fn headers(&self) -> Vec<String> {
        let mut headers = vec!["Content-Type: application/json".to_owned()];
        if let Some(key) = self.api_key() {
            headers.push(format!("Authorization: Bearer {key}"));
        }
        headers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_targets_a_local_laptop_friendly_model() {
        let config = ModelConfig::default();
        assert!(config.is_local());
        assert_eq!(config.base_url, "http://127.0.0.1:11434/v1");
        assert!(!config.model.is_empty());
        assert!(config.api_key_env.is_empty());
    }

    #[test]
    fn every_preset_is_usable() {
        for preset in PRESETS {
            assert!(preset_by_id(preset.id).is_some());
            let config = ModelConfig::from_preset(preset);
            assert!(!config.base_url.is_empty(), "{}", preset.id);
            assert_eq!(config.provider, preset.provider, "{}", preset.id);
            // Local presets never require a key; network presets name an env var.
            if preset.location == ModelLocation::Local {
                assert!(preset.api_key_env.is_empty(), "{}", preset.id);
                assert!(config.api_key_available());
            } else {
                assert!(!preset.api_key_env.is_empty(), "{}", preset.id);
            }
            assert!(!preset.uses.is_empty(), "{}", preset.id);
        }
    }

    #[test]
    fn deepseek_is_available_for_chat_only() {
        let preset = preset_by_id("deepseek").expect("deepseek preset exists");
        assert_eq!(preset.provider, ProviderKind::DeepSeek);
        assert_eq!(preset.api_key_env, "DEEPSEEK_API_KEY");
        assert!(presets_for(PresetUse::Chat).any(|p| p.id == "deepseek"));
        assert!(!presets_for(PresetUse::Embedding).any(|p| p.id == "deepseek"));
        assert!(!ProviderKind::DeepSeek.supports_embeddings());
    }

    #[test]
    fn the_default_chat_model_is_deepseek() {
        let config = default_chat_config();
        assert_eq!(config.provider, ProviderKind::DeepSeek);
        assert_eq!(config.base_url, "https://api.deepseek.com/v1");
        assert_eq!(config.model, "deepseek-chat");
    }

    #[test]
    fn a_typed_key_takes_precedence_over_the_environment() {
        // Point at a variable that cannot be set in the test environment, then type a key.
        let mut config = ModelConfig {
            location: ModelLocation::Network,
            api_key_env: "YUKI_TEST_KEY_THAT_DOES_NOT_EXIST".to_owned(),
            ..ModelConfig::default()
        };
        assert!(!config.api_key_available());
        config.api_key = "  sk-typed  ".to_owned();
        assert_eq!(config.api_key().as_deref(), Some("sk-typed"));
        assert!(config.api_key_available());
        assert!(
            config
                .headers()
                .iter()
                .any(|header| header == "Authorization: Bearer sk-typed")
        );
    }

    #[test]
    fn only_the_old_default_timeouts_are_migrated() {
        let mut stored = ModelConfig {
            location: ModelLocation::Network,
            timeout_seconds: 120,
            ..ModelConfig::default()
        };
        stored.migrate_timeout();
        assert_eq!(stored.timeout_seconds, 600);
        // A deliberate value is preserved.
        let mut chosen = ModelConfig {
            location: ModelLocation::Network,
            timeout_seconds: 240,
            ..ModelConfig::default()
        };
        chosen.migrate_timeout();
        assert_eq!(chosen.timeout_seconds, 240);
    }

    #[test]
    fn validate_rejects_incomplete_configuration() {
        let no_url = ModelConfig {
            base_url: String::new(),
            ..ModelConfig::default()
        };
        assert!(matches!(no_url.validate(), Err(ModelError::NotConfigured)));
        let no_model = ModelConfig {
            model: "  ".to_owned(),
            ..ModelConfig::default()
        };
        assert!(matches!(
            no_model.validate(),
            Err(ModelError::NotConfigured)
        ));
    }

    #[test]
    fn endpoints_do_not_double_slash() {
        let config = ModelConfig {
            base_url: "http://127.0.0.1:11434/v1/".to_owned(),
            ..ModelConfig::default()
        };
        assert_eq!(
            config.endpoint("/embeddings"),
            "http://127.0.0.1:11434/v1/embeddings"
        );
    }
}
