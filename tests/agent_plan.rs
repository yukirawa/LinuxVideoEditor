//! End-to-end check of the assistant path without a network or a UI: a fake transport returns a
//! chat reply containing an AEP plan, the agent extracts it, the editor validates it, and the
//! plan is applied as one transactional, undoable edit.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use uuid::Uuid;
use yuki_video_editor::agent;
use yuki_video_editor::domain::{
    Clip, MediaAsset, MediaMetadata, Project, TimelineItem, TimelineTime,
};
use yuki_video_editor::editing::EditHistory;
use yuki_video_editor::models::{
    HttpResponse, HttpTransport, ModelConfig, ModelError, ModelLocation, OpenAiCompatible,
    ProviderKind,
};
use yuki_video_editor::protocol::{AepOperation, AiPermission, AiProvider, AiToolRequest, AiTurn};

#[derive(Clone, Default)]
struct FakeTransport {
    responses: Arc<Mutex<VecDeque<HttpResponse>>>,
}

impl FakeTransport {
    fn with(body: &str) -> Self {
        Self {
            responses: Arc::new(Mutex::new(
                vec![HttpResponse {
                    status: 200,
                    body: body.to_owned(),
                }]
                .into(),
            )),
        }
    }
}

impl HttpTransport for FakeTransport {
    fn request(
        &self,
        _method: &str,
        _url: &str,
        _headers: &[String],
        _body: Option<&str>,
        _timeout_seconds: u32,
    ) -> Result<HttpResponse, ModelError> {
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| ModelError::Connection("no fake response queued".to_owned()))
    }
}

/// Wraps assistant text the way an OpenAI-compatible `chat/completions` response does.
fn chat_envelope(content: &str) -> String {
    serde_json::json!({
        "choices": [ { "message": { "role": "assistant", "content": content } } ]
    })
    .to_string()
}

/// Wraps a tool-calling assistant turn the way an OpenAI-compatible response does.
fn tool_envelope(id: &str, name: &str, arguments: &str) -> String {
    serde_json::json!({
        "choices": [ { "message": {
            "role": "assistant",
            "content": serde_json::Value::Null,
            "tool_calls": [ { "id": id, "type": "function", "function": { "name": name, "arguments": arguments } } ]
        } } ]
    })
    .to_string()
}

fn config() -> ModelConfig {
    ModelConfig {
        location: ModelLocation::Network,
        provider: ProviderKind::DeepSeek,
        base_url: "https://api.deepseek.com/v1".to_owned(),
        model: "deepseek-chat".to_owned(),
        api_key: String::new(),
        api_key_env: "DEEPSEEK_API_KEY".to_owned(),
        timeout_seconds: 30,
        batch_size: 8,
        preset_id: "deepseek".to_owned(),
    }
}

fn project_with_clip() -> (Project, Uuid) {
    let mut project = Project::new("カット");
    let metadata = MediaMetadata {
        duration: TimelineTime::from_seconds(30.0),
        has_video: true,
        ..MediaMetadata::default()
    };
    let asset = MediaAsset {
        id: Uuid::new_v4(),
        name: "take.mp4".to_owned(),
        path: "/tmp/take.mp4".into(),
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
fn assistant_plan_is_extracted_validated_and_applied_transactionally() {
    let (mut project, clip_id) = project_with_clip();
    // A reply with prose plus a plan that trims the clip and mutes it.
    let reply = format!(
        "冒頭の冗長な部分を削って、音を消しておきます。\n```json\n{{\n  \"protocol\": \"AEP\",\n  \"version\": \"1.0\",\n  \"operations\": [\n    {{ \"type\": \"trim_clip\", \"clip_id\": \"{clip_id}\", \"source_in\": 2.0, \"source_out\": 12.0, \"reason\": \"冗長\" }},\n    {{ \"type\": \"mute\", \"clip_id\": \"{clip_id}\", \"muted\": true }}\n  ]\n}}\n```"
    );

    let provider = OpenAiCompatible::with_transport(
        config(),
        Box::new(FakeTransport::with(&chat_envelope(&reply))),
    );
    let response = agent::ask(
        &provider,
        agent::system_prompt(),
        Vec::new(),
        "冒頭を詰めて無音にして".to_owned(),
    )
    .expect("the fake transport answers");

    let document = agent::extract_aep(&response.message, project.id)
        .expect("the reply contains JSON")
        .expect("the JSON is a valid AEP plan");
    assert_eq!(document.operations.len(), 2);

    let commands = yuki_video_editor::protocol::validate_aep(&project, &document)
        .expect("the plan targets this project");
    let mut history = EditHistory::default();
    history
        .execute_batch(&mut project, commands)
        .expect("the plan applies");

    let clip = project.clip(clip_id).expect("clip still exists");
    assert!((clip.source_in.as_seconds() - 2.0).abs() < 1e-6);
    assert!((clip.source_out.as_seconds() - 12.0).abs() < 1e-6);
    assert!(clip.audio.muted);

    // Both operations are a single undo step.
    let mut undo_project = project.clone();
    assert!(history.undo(&mut undo_project));
    let reverted = undo_project.clip(clip_id).unwrap();
    assert_eq!(reverted.source_in, TimelineTime::ZERO);
    assert!(!reverted.audio.muted);
    // The original source range is restored, proving the edit was non-destructive.
    assert!((reverted.source_out.as_seconds() - 30.0).abs() < 1e-6);
}

#[test]
fn a_plan_for_a_missing_clip_does_not_modify_the_project() {
    let (mut project, clip_id) = project_with_clip();
    let document = yuki_video_editor::protocol::AepDocument {
        protocol: "AEP".to_owned(),
        version: "1.0".to_owned(),
        project_id: project.id,
        operations: vec![AepOperation::DeleteClip {
            operation_id: Uuid::new_v4(),
            clip_id: Uuid::new_v4(),
            reason: None,
            confidence: None,
        }],
    };
    let commands = yuki_video_editor::protocol::validate_aep(&project, &document).unwrap();
    let mut history = EditHistory::default();
    assert!(history.execute_batch(&mut project, commands).is_err());
    // Nothing was committed: the timeline still has exactly the one original clip.
    assert_eq!(project.sequence.layers[0].items.len(), 1);
    assert!(project.clip(clip_id).is_some());
    // And no undo entry was recorded.
    assert!(!history.can_undo());
}

#[test]
fn the_agent_advertises_the_whole_workflow_and_parses_tool_calls() {
    let provider = OpenAiCompatible::with_transport(
        config(),
        Box::new(FakeTransport::with(&tool_envelope(
            "c1",
            "add_speech",
            "{\"text\":\"こんにちは\"}",
        ))),
    );
    let tools = yuki_video_editor::agent::tools::tool_specs();
    let response = provider
        .send_tools(AiToolRequest {
            system_context: yuki_video_editor::agent::agent_system_prompt(),
            turns: vec![AiTurn::user("ナレーション入りの動画を作って")],
            tools: tools.clone(),
            permission: AiPermission::Assist,
        })
        .unwrap();

    let invocation = yuki_video_editor::agent::tools::parse_call(&response.tool_calls[0]).unwrap();
    assert_eq!(invocation.name(), "add_speech");
    assert!(invocation.is_async());

    // Every stage of "make a whole video" must be callable by the model.
    let names: Vec<String> = tools.into_iter().map(|spec| spec.name).collect();
    for expected in [
        "get_project_state",
        "add_media",
        "split_clip",
        "trim_clip",
        "delete_clip",
        "move_clip",
        "set_volume",
        "add_text",
        "add_speech",
        "export_video",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "missing {expected}"
        );
    }
}
