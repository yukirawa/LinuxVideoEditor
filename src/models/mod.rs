//! Selectable AI model backends.
//!
//! A model can run **locally** (a small, laptop-friendly model behind a local
//! OpenAI-compatible server) or over the **network** (a hosted API), and the choice is made in
//! the UI. Both speak the same protocol, so a single adapter covers them and the editing core
//! never depends on a specific provider (AI-PROVIDERS.md).
//!
//! Credentials live in environment variables only; see `ModelConfig::api_key`.

mod config;
mod error;
mod openai;
mod transport;

pub use config::{
    ModelConfig, ModelLocation, ModelPreset, PRESETS, PresetUse, ProviderKind, default_chat_config,
    preset_by_id, presets_for,
};
pub use error::ModelError;
pub use openai::{OpenAiCompatible, ProbeResult};
pub use transport::{CurlTransport, HttpResponse, HttpTransport};
