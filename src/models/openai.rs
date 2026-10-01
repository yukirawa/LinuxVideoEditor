//! OpenAI-compatible adapter.
//!
//! One adapter serves every supported backend because local servers (Ollama, llama.cpp,
//! LM Studio) and hosted APIs all speak the same protocol. Provider-specific detail stops here:
//! `protocol::AiProvider` and the rest of the app only see VIR/AEP-neutral types.

use super::config::ModelConfig;
use super::transport::CurlTransport;
use super::{HttpTransport, ModelError};
use crate::protocol::{
    AiCapabilities, AiError, AiMessage, AiProvider, AiRequest, AiResponse, AiToolCall,
    AiToolRequest, AiToolResponse, AiToolSpec, AiTurn,
};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct ProbeResult {
    pub models: Vec<String>,
}

pub struct OpenAiCompatible {
    name: String,
    config: ModelConfig,
    transport: Box<dyn HttpTransport>,
}

impl OpenAiCompatible {
    pub fn new(config: ModelConfig) -> Self {
        Self::with_transport(config, Box::new(CurlTransport))
    }

    pub fn with_transport(config: ModelConfig, transport: Box<dyn HttpTransport>) -> Self {
        let name = format!("{}:{}", config.provider.label(), config.model.trim());
        Self {
            name,
            config,
            transport,
        }
    }

    pub fn config(&self) -> &ModelConfig {
        &self.config
    }

    fn validate_endpoint(&self) -> Result<(), ModelError> {
        if self.config.base_url.trim().is_empty() {
            return Err(ModelError::NotConfigured);
        }
        Ok(())
    }

    /// Lists the models the endpoint advertises. Used by the UI's connection test, so it does
    /// not require a model name to be chosen yet.
    pub fn probe(&self) -> Result<ProbeResult, ModelError> {
        self.validate_endpoint()?;
        let response = self.transport.request(
            "GET",
            &self.config.endpoint("models"),
            &self.config.headers(),
            None,
            self.config.timeout_seconds,
        )?;
        Ok(ProbeResult {
            models: parse_models_response(response.status, &response.body)?,
        })
    }

    /// Embeds inputs, splitting them into `batch_size` requests. Results stay in input order.
    pub fn embeddings(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, ModelError> {
        self.config.validate()?;
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let batch = self.config.batch_size.max(1);
        let mut vectors = Vec::with_capacity(inputs.len());
        for chunk in inputs.chunks(batch) {
            let mut batch_vectors = self.embeddings_batch(chunk)?;
            if batch_vectors.len() != chunk.len() {
                return Err(ModelError::InvalidOutput(format!(
                    "埋め込み数が一致しません（要求 {} / 応答 {}）",
                    chunk.len(),
                    batch_vectors.len()
                )));
            }
            vectors.append(&mut batch_vectors);
        }
        Ok(vectors)
    }

    /// A single embeddings request, without batching.
    pub fn embeddings_batch(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, ModelError> {
        self.config.validate()?;
        if !self.config.provider.supports_embeddings() {
            return Err(ModelError::Unsupported(format!(
                "{} は埋め込みAPIを提供していません。意味検索には別のモデルを選んでください。",
                self.config.provider.label()
            )));
        }
        let body = build_embeddings_body(&self.config.model, inputs);
        let response = self.transport.request(
            "POST",
            &self.config.endpoint("embeddings"),
            &self.config.headers(),
            Some(&body),
            self.config.timeout_seconds,
        )?;
        parse_embeddings_response(response.status, &response.body)
    }

    pub fn chat(&self, system: Option<&str>, user: &str) -> Result<String, ModelError> {
        self.chat_messages(system, &[], user)
    }

    /// A multi-turn chat request. `history` is oldest-first and carries earlier user/assistant
    /// turns so the stateless HTTP API still sees the conversation (AI-TOOLS.md).
    pub fn chat_messages(
        &self,
        system: Option<&str>,
        history: &[AiMessage],
        user: &str,
    ) -> Result<String, ModelError> {
        self.config.validate()?;
        let mut messages = Vec::new();
        if let Some(system) = system {
            messages.push(json!({ "role": "system", "content": system }));
        }
        for turn in history {
            messages.push(json!({ "role": turn.role.api_name(), "content": turn.content }));
        }
        messages.push(json!({ "role": "user", "content": user }));
        let body = json!({ "model": self.config.model, "messages": messages }).to_string();
        let response = self.transport.request(
            "POST",
            &self.config.endpoint("chat/completions"),
            &self.config.headers(),
            Some(&body),
            self.config.timeout_seconds,
        )?;
        parse_chat_response(response.status, &response.body)
    }

    /// A tool-calling turn. `turns` includes the assistant's previous tool calls and the tool
    /// results, which is what makes a multi-step agent loop possible over a stateless API.
    pub fn chat_with_tools(
        &self,
        system: Option<&str>,
        turns: &[AiTurn],
        tools: &[AiToolSpec],
    ) -> Result<AiToolResponse, ModelError> {
        self.config.validate()?;
        let body = build_tools_body(&self.config.model, system, turns, tools);
        let response = self.transport.request(
            "POST",
            &self.config.endpoint("chat/completions"),
            &self.config.headers(),
            Some(&body),
            self.config.timeout_seconds,
        )?;
        parse_tool_response(response.status, &response.body)
    }

    /// Protocol-level capabilities. Individual models differ; richer negotiation (probing the
    /// endpoint, reading model metadata) is a later phase.
    pub fn capabilities(&self) -> AiCapabilities {
        AiCapabilities {
            // Hosted chat APIs (including DeepSeek) are text-only here: no frames are sent yet.
            vision: false,
            tool_calling: true,
            structured_output: true,
            streaming: false,
        }
    }
}

impl AiProvider for OpenAiCompatible {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> AiCapabilities {
        OpenAiCompatible::capabilities(self)
    }

    fn send(&self, request: AiRequest) -> Result<AiResponse, AiError> {
        let system =
            (!request.system_context.trim().is_empty()).then_some(request.system_context.as_str());
        self.chat_messages(system, &request.history, &request.user_message)
            .map(|message| AiResponse {
                message,
                structured_aep: None,
            })
            .map_err(|error| match error {
                ModelError::NotConfigured => AiError::NotConfigured,
                ModelError::InvalidOutput(message) => AiError::InvalidOutput(message),
                other => AiError::Request(other.to_string()),
            })
    }

    fn send_tools(&self, request: AiToolRequest) -> Result<AiToolResponse, AiError> {
        let system =
            (!request.system_context.trim().is_empty()).then_some(request.system_context.as_str());
        self.chat_with_tools(system, &request.turns, &request.tools)
            .map_err(|error| match error {
                ModelError::NotConfigured => AiError::NotConfigured,
                ModelError::InvalidOutput(message) => AiError::InvalidOutput(message),
                other => AiError::Request(other.to_string()),
            })
    }
}

pub fn build_embeddings_body(model: &str, inputs: &[String]) -> String {
    json!({ "model": model, "input": inputs }).to_string()
}

/// Builds a `chat/completions` body with tool definitions and a full agent transcript.
fn build_tools_body(
    model: &str,
    system: Option<&str>,
    turns: &[AiTurn],
    tools: &[AiToolSpec],
) -> String {
    let mut messages = Vec::new();
    if let Some(system) = system {
        messages.push(json!({ "role": "system", "content": system }));
    }
    for turn in turns {
        match turn {
            AiTurn::User(content) => messages.push(json!({ "role": "user", "content": content })),
            AiTurn::Assistant {
                content,
                tool_calls,
            } => {
                let mut message = json!({
                    "role": "assistant",
                    "content": if content.trim().is_empty() { Value::Null } else { json!(content) },
                });
                if !tool_calls.is_empty() {
                    message["tool_calls"] = Value::Array(
                        tool_calls
                            .iter()
                            .map(|call| {
                                json!({
                                    "id": call.id,
                                    "type": "function",
                                    "function": { "name": call.name, "arguments": call.arguments }
                                })
                            })
                            .collect(),
                    );
                }
                messages.push(message);
            }
            AiTurn::Tool {
                call_id, content, ..
            } => messages.push(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": content,
            })),
            AiTurn::Images { text, images } => {
                if images.is_empty() {
                    messages.push(json!({ "role": "user", "content": text }));
                } else {
                    let mut content = vec![json!({ "type": "text", "text": text })];
                    for image in images {
                        content.push(json!({
                            "type": "image_url",
                            "image_url": { "url": image.data_url() }
                        }));
                    }
                    messages.push(json!({ "role": "user", "content": content }));
                }
            }
        }
    }
    let mut body = json!({ "model": model, "messages": messages });
    if !tools.is_empty() {
        body["tools"] = Value::Array(
            tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                        }
                    })
                })
                .collect(),
        );
        body["tool_choice"] = json!("auto");
    }
    body.to_string()
}

pub fn parse_embeddings_response(status: u16, body: &str) -> Result<Vec<Vec<f32>>, ModelError> {
    let value = success_value(status, body)?;
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| ModelError::InvalidOutput("`data` がありません".to_owned()))?;

    let mut items: Vec<(usize, Vec<f32>)> = Vec::with_capacity(data.len());
    for (position, item) in data.iter().enumerate() {
        let index = item
            .get("index")
            .and_then(Value::as_u64)
            .map(|index| index as usize)
            .unwrap_or(position);
        let values = item
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| ModelError::InvalidOutput("`embedding` がありません".to_owned()))?;
        let vector = values
            .iter()
            .map(|value| value.as_f64().map(|number| number as f32))
            .collect::<Option<Vec<f32>>>()
            .ok_or_else(|| {
                ModelError::InvalidOutput("埋め込みに数値以外が含まれています".to_owned())
            })?;
        if vector.is_empty() {
            return Err(ModelError::InvalidOutput("埋め込みが空です".to_owned()));
        }
        items.push((index, vector));
    }
    // Some servers do not guarantee response order; `index` restores it.
    items.sort_by_key(|(index, _)| *index);
    Ok(items.into_iter().map(|(_, vector)| vector).collect())
}

pub fn parse_models_response(status: u16, body: &str) -> Result<Vec<String>, ModelError> {
    let value = success_value(status, body)?;
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| ModelError::InvalidOutput("`data` がありません".to_owned()))?;
    Ok(data
        .iter()
        .filter_map(|entry| entry.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect())
}

pub fn parse_chat_response(status: u16, body: &str) -> Result<String, ModelError> {
    let value = success_value(status, body)?;
    value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            ModelError::InvalidOutput("`choices[0].message.content` がありません".to_owned())
        })
}

/// Parses a turn that may contain tool calls. `content` may legitimately be null when the model
/// only asked for tools, so an absent content becomes an empty string rather than an error.
pub fn parse_tool_response(status: u16, body: &str) -> Result<AiToolResponse, ModelError> {
    let value = success_value(status, body)?;
    let message = value
        .pointer("/choices/0/message")
        .ok_or_else(|| ModelError::InvalidOutput("`choices[0].message` がありません".to_owned()))?;
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let mut tool_calls = Vec::new();
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let name = call
                .pointer("/function/name")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ModelError::InvalidOutput("tool_call に function.name がありません".to_owned())
                })?
                .to_owned();
            let arguments = call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}")
                .to_owned();
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("call_{}", tool_calls.len()));
            tool_calls.push(AiToolCall {
                id,
                name,
                arguments,
            });
        }
    }
    Ok(AiToolResponse {
        content,
        tool_calls,
    })
}

fn success_value(status: u16, body: &str) -> Result<Value, ModelError> {
    let http_error = |status: u16, body: &str| ModelError::Http {
        status,
        message: parse_error_message(body),
    };
    if !(200..300).contains(&status) {
        return Err(http_error(status, body));
    }
    serde_json::from_str(body).map_err(|error| {
        // A 2xx response that is not JSON is more likely a proxy/HTML error page than a bug.
        ModelError::InvalidOutput(format!("{error}: {}", parse_error_message(body)))
    })
}

/// Extracts a human-readable message from an error body, tolerating HTML and empty bodies.
pub fn parse_error_message(body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        if let Some(message) = value.pointer("/error/message").and_then(Value::as_str) {
            return message.to_owned();
        }
        if let Some(message) = value.get("message").and_then(Value::as_str) {
            return message.to_owned();
        }
    }
    let trimmed = body.trim();
    if trimmed.is_empty() {
        "応答本文なし".to_owned()
    } else {
        trimmed.chars().take(300).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{HttpResponse, ModelConfig, ModelLocation, ProviderKind};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct FakeTransport {
        responses: Arc<Mutex<VecDeque<HttpResponse>>>,
        requests: Arc<Mutex<Vec<(String, String, String)>>>,
    }

    impl FakeTransport {
        fn with(responses: Vec<HttpResponse>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn requests(&self) -> Vec<(String, String, String)> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl HttpTransport for FakeTransport {
        fn request(
            &self,
            method: &str,
            url: &str,
            _headers: &[String],
            body: Option<&str>,
            _timeout_seconds: u32,
        ) -> Result<HttpResponse, ModelError> {
            self.requests.lock().unwrap().push((
                method.to_owned(),
                url.to_owned(),
                body.unwrap_or_default().to_owned(),
            ));
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| ModelError::Connection("no fake response queued".to_owned()))
        }
    }

    fn config(batch_size: usize) -> ModelConfig {
        ModelConfig {
            location: ModelLocation::Local,
            provider: ProviderKind::OpenAiCompatible,
            base_url: "http://127.0.0.1:11434/v1".to_owned(),
            model: "bge-m3".to_owned(),
            api_key: String::new(),
            api_key_env: String::new(),
            timeout_seconds: 30,
            batch_size,
            preset_id: "ollama".to_owned(),
        }
    }

    fn ok(body: &str) -> HttpResponse {
        HttpResponse {
            status: 200,
            body: body.to_owned(),
        }
    }

    #[test]
    fn embeddings_restore_input_order_from_index() {
        let transport = FakeTransport::with(vec![ok(
            r#"{"data":[{"index":1,"embedding":[0.2,0.2]},{"index":0,"embedding":[0.1,0.1]}]}"#,
        )]);
        let provider = OpenAiCompatible::with_transport(config(8), Box::new(transport.clone()));
        let vectors = provider
            .embeddings(&["a".to_owned(), "b".to_owned()])
            .unwrap();
        assert_eq!(vectors, vec![vec![0.1, 0.1], vec![0.2, 0.2]]);
        assert_eq!(transport.requests().len(), 1);
    }

    #[test]
    fn embeddings_are_split_into_batches() {
        let transport = FakeTransport::with(vec![
            ok(r#"{"data":[{"index":0,"embedding":[1.0]},{"index":1,"embedding":[2.0]}]}"#),
            ok(r#"{"data":[{"index":0,"embedding":[3.0]}]}"#),
        ]);
        let provider = OpenAiCompatible::with_transport(config(2), Box::new(transport.clone()));
        let vectors = provider
            .embeddings(&["a".to_owned(), "b".to_owned(), "c".to_owned()])
            .unwrap();
        assert_eq!(vectors, vec![vec![1.0], vec![2.0], vec![3.0]]);
        // Two inputs per request => two requests.
        assert_eq!(transport.requests().len(), 2);
    }

    #[test]
    fn embeddings_send_the_configured_model_to_the_embeddings_endpoint() {
        let transport =
            FakeTransport::with(vec![ok(r#"{"data":[{"index":0,"embedding":[1.0]}]}"#)]);
        let provider = OpenAiCompatible::with_transport(config(8), Box::new(transport.clone()));
        provider.embeddings(&["hello".to_owned()]).unwrap();
        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, "POST");
        assert_eq!(requests[0].1, "http://127.0.0.1:11434/v1/embeddings");
        assert!(requests[0].2.contains("\"model\":\"bge-m3\""));
        assert!(requests[0].2.contains("\"input\":[\"hello\"]"));
    }

    #[test]
    fn http_errors_surface_the_server_message() {
        let response = HttpResponse {
            status: 404,
            body: r#"{"error":{"message":"model not found"}}"#.to_owned(),
        };
        let error = parse_embeddings_response(response.status, &response.body).unwrap_err();
        match error {
            ModelError::Http { status, message } => {
                assert_eq!(status, 404);
                assert_eq!(message, "model not found");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn invalid_bodies_are_rejected() {
        assert!(matches!(
            parse_embeddings_response(200, "<html>oops</html>"),
            Err(ModelError::InvalidOutput(_))
        ));
        assert!(parse_embeddings_response(200, r#"{"data":[]}"#).is_ok());
    }

    #[test]
    fn probe_parses_the_model_list() {
        let (status, body) = (
            200,
            r#"{"data":[{"id":"bge-m3"},{"id":"nomic-embed-text"}]}"#,
        );
        assert_eq!(
            parse_models_response(status, body).unwrap(),
            vec!["bge-m3".to_owned(), "nomic-embed-text".to_owned()]
        );
    }

    #[test]
    fn chat_response_is_extracted_and_mapped_to_the_provider_trait() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"こんにちは"}}]}"#;
        assert_eq!(parse_chat_response(200, body).unwrap(), "こんにちは");

        let transport = FakeTransport::with(vec![ok(body)]);
        let provider = OpenAiCompatible::with_transport(config(8), Box::new(transport.clone()));
        let response = provider
            .send(AiRequest {
                system_context: "be brief".to_owned(),
                history: Vec::new(),
                user_message: "hello".to_owned(),
                permission: crate::protocol::AiPermission::Observe,
            })
            .unwrap();
        assert_eq!(response.message, "こんにちは");
        assert!(provider.capabilities().structured_output);
        assert_eq!(
            transport.requests()[0].1,
            "http://127.0.0.1:11434/v1/chat/completions"
        );
    }

    #[test]
    fn chat_includes_history_between_system_and_user() {
        let body = r#"{"choices":[{"message":{"content":"ok"}}]}"#;
        let transport = FakeTransport::with(vec![ok(body)]);
        let provider = OpenAiCompatible::with_transport(config(8), Box::new(transport.clone()));
        provider
            .chat_messages(
                Some("sys"),
                &[AiMessage::user("first"), AiMessage::assistant("reply")],
                "second",
            )
            .unwrap();
        let sent = &transport.requests()[0].2;
        let position = |needle: &str| {
            sent.find(needle)
                .unwrap_or_else(|| panic!("missing {needle}"))
        };
        assert!(position("\"sys\"") < position("\"first\""));
        assert!(position("\"first\"") < position("\"reply\""));
        assert!(position("\"reply\"") < position("\"second\""));
    }

    #[test]
    fn tool_body_contains_definitions_and_the_transcript() {
        let turns = vec![
            AiTurn::user("切って"),
            AiTurn::Assistant {
                content: String::new(),
                tool_calls: vec![AiToolCall {
                    id: "call_1".to_owned(),
                    name: "split_clip".to_owned(),
                    arguments: "{\"clip_id\":\"abc\"}".to_owned(),
                }],
            },
            AiTurn::tool("call_1", "split_clip", "分割しました"),
        ];
        let tools = vec![AiToolSpec::new(
            "split_clip",
            "split",
            json!({"type": "object", "properties": {}}),
        )];
        let body = build_tools_body("deepseek-chat", Some("sys"), &turns, &tools);
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["tool_choice"], "auto");
        assert_eq!(value["tools"][0]["function"]["name"], "split_clip");
        assert_eq!(value["messages"][1]["content"], "切って");
        // Tool calls are attached to the assistant message they belong to.
        assert_eq!(
            value["messages"][2]["tool_calls"][0]["function"]["arguments"],
            "{\"clip_id\":\"abc\"}"
        );
        assert_eq!(value["messages"][3]["role"], "tool");
        assert_eq!(value["messages"][3]["tool_call_id"], "call_1");
    }

    #[test]
    fn tool_response_parses_calls_and_tolerates_null_content() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"c1","type":"function","function":{"name":"export_video","arguments":"{}"}}]}}]}"#;
        let response = parse_tool_response(200, body).unwrap();
        assert!(response.content.is_empty());
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].name, "export_video");
        assert_eq!(response.tool_calls[0].id, "c1");
    }

    #[test]
    fn trait_send_tools_round_trips_a_tool_call() {
        let body = r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"c1","type":"function","function":{"name":"get_project_state","arguments":"{}"}}]}}]}"#;
        let transport = FakeTransport::with(vec![ok(body)]);
        let provider = OpenAiCompatible::with_transport(config(8), Box::new(transport.clone()));
        let response = provider
            .send_tools(crate::protocol::AiToolRequest {
                system_context: "sys".to_owned(),
                turns: vec![AiTurn::user("動画を作って")],
                tools: vec![AiToolSpec::new("get_project_state", "state", json!({}))],
                permission: crate::protocol::AiPermission::Assist,
            })
            .unwrap();
        assert_eq!(response.tool_calls[0].name, "get_project_state");
        assert!(response.content.is_empty());
        // The tool definitions really went out on the wire.
        assert!(transport.requests()[0].2.contains("\"tools\""));
    }

    #[test]
    fn image_turns_use_the_openai_vision_content_array() {
        let turns = vec![AiTurn::images(
            "frame",
            vec![crate::protocol::AiImage::jpeg("AAAA".to_owned())],
        )];
        let body = build_tools_body("m", None, &turns, &[]);
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["messages"][0]["role"], "user");
        assert_eq!(value["messages"][0]["content"][0]["type"], "text");
        assert_eq!(value["messages"][0]["content"][1]["type"], "image_url");
        assert_eq!(
            value["messages"][0]["content"][1]["image_url"]["url"],
            "data:image/jpeg;base64,AAAA"
        );
    }

    #[test]
    fn providers_without_embeddings_are_rejected_with_a_clear_message() {
        let mut deepseek = config(8);
        deepseek.provider = ProviderKind::DeepSeek;
        deepseek.base_url = "https://api.deepseek.com/v1".to_owned();
        deepseek.model = "deepseek-chat".to_owned();
        let transport = FakeTransport::with(vec![]);
        let provider = OpenAiCompatible::with_transport(deepseek, Box::new(transport.clone()));
        let error = provider.embeddings(&["x".to_owned()]).unwrap_err();
        assert!(matches!(error, ModelError::Unsupported(_)));
        // No request was attempted.
        assert!(transport.requests().is_empty());
    }

    #[test]
    fn unconfigured_models_are_rejected_before_any_request() {
        let mut broken = config(8);
        broken.model = String::new();
        let transport = FakeTransport::with(vec![]);
        let provider = OpenAiCompatible::with_transport(broken, Box::new(transport.clone()));
        assert!(matches!(
            provider.embeddings(&["x".to_owned()]),
            Err(ModelError::NotConfigured)
        ));
        // Probing only needs a base URL, so it is still allowed.
        assert!(provider.probe().is_ok() || transport.requests().len() == 1);
    }
}
