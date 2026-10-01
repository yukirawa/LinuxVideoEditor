use crate::domain::{ClipId, Project, ProjectId, TimelineTime};
use crate::editing::EditCommand;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use thiserror::Error;
use uuid::Uuid;

pub const AEP_PROTOCOL: &str = "AEP";
pub const AEP_VERSION: &str = "1.0";

/// `TimelineTime` is stored as integer microseconds, but AEP is a published, model-facing
/// protocol whose examples express times in **seconds** (AEP-SPEC.md). Serializing these fields as
/// fractional seconds keeps the JSON readable for a model and matches the specification, while the
/// editing core keeps its drift-free integer representation.
mod seconds {
    use crate::domain::TimelineTime;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(time: &TimelineTime, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(time.as_seconds())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<TimelineTime, D::Error> {
        Ok(TimelineTime::from_seconds(f64::deserialize(deserializer)?))
    }
}

fn default_protocol() -> String {
    AEP_PROTOCOL.to_owned()
}

fn default_version() -> String {
    AEP_VERSION.to_owned()
}

/// AEP is meant to be produced by a model, so the envelope is forgiving: a missing protocol,
/// version, target project or operation id is filled in rather than rejected. Anything that would
/// actually change the meaning of an edit (clip id, times) stays required.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AepDocument {
    #[serde(default = "default_protocol")]
    pub protocol: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub project_id: ProjectId,
    /// Required: without operations there is no plan, and requiring it keeps an unrelated JSON
    /// object in a reply from being mistaken for AEP.
    pub operations: Vec<AepOperation>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AepOperation {
    SplitClip {
        #[serde(default = "Uuid::new_v4")]
        operation_id: Uuid,
        clip_id: ClipId,
        #[serde(with = "seconds")]
        timeline_time: TimelineTime,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        confidence: Option<f32>,
    },
    TrimClip {
        #[serde(default = "Uuid::new_v4")]
        operation_id: Uuid,
        clip_id: ClipId,
        #[serde(with = "seconds")]
        source_in: TimelineTime,
        #[serde(with = "seconds")]
        source_out: TimelineTime,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        confidence: Option<f32>,
    },
    DeleteClip {
        #[serde(default = "Uuid::new_v4")]
        operation_id: Uuid,
        clip_id: ClipId,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        confidence: Option<f32>,
    },
    MoveClip {
        #[serde(default = "Uuid::new_v4")]
        operation_id: Uuid,
        clip_id: ClipId,
        #[serde(with = "seconds")]
        timeline_start: TimelineTime,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        confidence: Option<f32>,
    },
    SetVolume {
        #[serde(default = "Uuid::new_v4")]
        operation_id: Uuid,
        clip_id: ClipId,
        gain_db: f32,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        confidence: Option<f32>,
    },
    Mute {
        #[serde(default = "Uuid::new_v4")]
        operation_id: Uuid,
        clip_id: ClipId,
        muted: bool,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        confidence: Option<f32>,
    },
}

impl AepOperation {
    fn operation_id(&self) -> Uuid {
        match self {
            Self::SplitClip { operation_id, .. }
            | Self::TrimClip { operation_id, .. }
            | Self::DeleteClip { operation_id, .. }
            | Self::MoveClip { operation_id, .. }
            | Self::SetVolume { operation_id, .. }
            | Self::Mute { operation_id, .. } => *operation_id,
        }
    }

    fn to_command(&self) -> EditCommand {
        match self {
            Self::SplitClip {
                clip_id,
                timeline_time,
                ..
            } => EditCommand::SplitClip {
                clip_id: *clip_id,
                timeline_time: *timeline_time,
            },
            Self::TrimClip {
                clip_id,
                source_in,
                source_out,
                ..
            } => EditCommand::TrimClip {
                clip_id: *clip_id,
                source_in: *source_in,
                source_out: *source_out,
            },
            Self::DeleteClip { clip_id, .. } => EditCommand::DeleteClip { clip_id: *clip_id },
            Self::MoveClip {
                clip_id,
                timeline_start,
                ..
            } => EditCommand::MoveClip {
                clip_id: *clip_id,
                timeline_start: *timeline_start,
            },
            Self::SetVolume {
                clip_id, gain_db, ..
            } => EditCommand::SetClipGain {
                clip_id: *clip_id,
                gain_db: *gain_db,
            },
            Self::Mute { clip_id, muted, .. } => EditCommand::SetClipMuted {
                clip_id: *clip_id,
                muted: *muted,
            },
        }
    }
}

#[derive(Debug, Error)]
pub enum AepError {
    #[error("not an AEP document")]
    WrongProtocol,
    #[error("unsupported AEP version: {0}")]
    UnsupportedVersion(String),
    #[error("AEP targets another project")]
    ProjectMismatch,
    #[error("duplicate operation id: {0}")]
    DuplicateOperation(Uuid),
    #[error("operation confidence must be between 0 and 1")]
    InvalidConfidence,
}

/// Performs provider-neutral envelope validation and returns ordinary edit commands.
/// The caller must transactionally dry-run the commands before committing them.
pub fn validate_aep(
    project: &Project,
    document: &AepDocument,
) -> Result<Vec<EditCommand>, AepError> {
    if document.protocol != AEP_PROTOCOL {
        return Err(AepError::WrongProtocol);
    }
    if document.version.split('.').next() != Some("1") {
        return Err(AepError::UnsupportedVersion(document.version.clone()));
    }
    if document.project_id != project.id {
        return Err(AepError::ProjectMismatch);
    }

    let mut operation_ids = HashSet::new();
    for operation in &document.operations {
        if !operation_ids.insert(operation.operation_id()) {
            return Err(AepError::DuplicateOperation(operation.operation_id()));
        }
        let confidence = match operation {
            AepOperation::SplitClip { confidence, .. }
            | AepOperation::TrimClip { confidence, .. }
            | AepOperation::DeleteClip { confidence, .. }
            | AepOperation::MoveClip { confidence, .. }
            | AepOperation::SetVolume { confidence, .. }
            | AepOperation::Mute { confidence, .. } => confidence,
        };
        if confidence.is_some_and(|value| !(0.0..=1.0).contains(&value)) {
            return Err(AepError::InvalidConfidence);
        }
    }
    Ok(document
        .operations
        .iter()
        .map(AepOperation::to_command)
        .collect())
}
