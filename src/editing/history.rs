use super::{EditCommand, EditError, apply_command};
use crate::domain::Project;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Consecutive adjustments of the same property within this window become one undo step.
/// Slider/timeline drags emit a command every frame; without this the stack fills with
/// pixel-level snapshots and every frame pays for a full project clone.
const COALESCE_WINDOW: Duration = Duration::from_millis(400);

pub struct EditHistory {
    undo: VecDeque<Project>,
    redo: VecDeque<Project>,
    capacity: usize,
    /// Last continuous-edit fingerprint and when it landed, for undo coalescing.
    coalesce: Option<(u64, Instant)>,
}

impl Default for EditHistory {
    fn default() -> Self {
        Self {
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            capacity: 100,
            coalesce: None,
        }
    }
}

impl EditHistory {
    pub fn execute(
        &mut self,
        project: &mut Project,
        command: EditCommand,
    ) -> Result<(), EditError> {
        self.apply(project, std::iter::once(command), true)
    }

    /// Applies several commands as a single transaction and a single undo step. AEP application
    /// is transactional (AEP-SPEC.md section 5): if any command fails, nothing is committed.
    pub fn execute_batch(
        &mut self,
        project: &mut Project,
        commands: impl IntoIterator<Item = EditCommand>,
    ) -> Result<(), EditError> {
        self.apply(project, commands, false)
    }

    fn apply(
        &mut self,
        project: &mut Project,
        commands: impl IntoIterator<Item = EditCommand>,
        allow_coalesce: bool,
    ) -> Result<(), EditError> {
        // One snapshot only: apply in place and roll back on failure. `apply_command` may
        // mutate before it validates, so the snapshot is the undo record *and* the rollback.
        let before = project.clone();
        let mut fingerprint = None;
        let mut count = 0usize;
        let outcome = commands.into_iter().try_for_each(|command| {
            if count == 0 {
                fingerprint = continuous_fingerprint(&command);
            }
            count += 1;
            apply_command(project, command)
        });
        if let Err(error) = outcome {
            *project = before;
            return Err(error);
        }

        let coalesce_key = if allow_coalesce && count == 1 {
            fingerprint
        } else {
            None
        };
        let continues = match (coalesce_key, self.coalesce.take()) {
            (Some(key), Some((last_key, at))) => {
                key == last_key && at.elapsed() <= COALESCE_WINDOW
            }
            _ => false,
        };
        if continues {
            // Keep the snapshot from the start of the gesture; `before` is mid-drag.
            self.coalesce = coalesce_key.map(|key| (key, Instant::now()));
            return Ok(());
        }

        self.push_undo(before);
        self.redo.clear();
        self.coalesce = coalesce_key.map(|key| (key, Instant::now()));
        Ok(())
    }

    fn push_undo(&mut self, before: Project) {
        self.undo.push_back(before);
        while self.undo.len() > self.capacity {
            self.undo.pop_front();
        }
    }

    pub fn undo(&mut self, project: &mut Project) -> bool {
        self.coalesce = None;
        let Some(previous) = self.undo.pop_back() else {
            return false;
        };
        self.redo
            .push_back(std::mem::replace(project, previous));
        true
    }

    pub fn redo(&mut self, project: &mut Project) -> bool {
        self.coalesce = None;
        let Some(next) = self.redo.pop_back() else {
            return false;
        };
        self.undo.push_back(std::mem::replace(project, next));
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.coalesce = None;
    }
}

/// Identity of a continuous adjustment (same target + same kind of change). Structural edits
/// return `None` and always start a fresh undo step.
fn continuous_fingerprint(command: &EditCommand) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let key = match command {
        EditCommand::SetClipTransform { clip_id, .. } => ("transform", *clip_id),
        EditCommand::SetClipOpacity { clip_id, .. } => ("opacity", *clip_id),
        EditCommand::SetClipSpeed { clip_id, .. } => ("speed", *clip_id),
        EditCommand::SetClipEffects { clip_id, .. } => ("effects", *clip_id),
        EditCommand::SetClipGain { clip_id, .. } => ("gain", *clip_id),
        EditCommand::SetClipMuted { clip_id, .. } => ("muted", *clip_id),
        EditCommand::SetAnimTrack { clip_id, .. } => ("anim-track", *clip_id),
        EditCommand::RemoveAnimTrack { clip_id, .. } => ("anim-track", *clip_id),
        EditCommand::SetKeyframe { clip_id, .. } => ("keyframe", *clip_id),
        EditCommand::RemoveKeyframe { clip_id, .. } => ("keyframe", *clip_id),
        EditCommand::MoveItem { item_id, .. } => ("move", *item_id),
        EditCommand::MoveClip { clip_id, .. } => ("move", *clip_id),
        EditCommand::ResizeItemStart { item_id, .. } => ("resize-start", *item_id),
        EditCommand::ResizeItemEnd { item_id, .. } => ("resize-end", *item_id),
        EditCommand::TrimClip { clip_id, .. } => ("trim", *clip_id),
        EditCommand::UpdateText { text } => ("update-text", text.id),
        EditCommand::UpdateShape { shape } => ("update-shape", shape.id),
        EditCommand::RenameLayer { layer_id, .. } => ("rename-layer", *layer_id),
        EditCommand::SetLayerLocked { layer_id, .. } => ("layer-locked", *layer_id),
        EditCommand::SetLayerHidden { layer_id, .. } => ("layer-hidden", *layer_id),
        EditCommand::SetLayerMuted { layer_id, .. } => ("layer-muted", *layer_id),
        _ => return None,
    };
    key.hash(&mut hasher);
    Some(hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{TextItem, TimelineTime};
    use crate::editing::EditCommand;

    fn text_item(id_name: &str) -> TextItem {
        TextItem::new(id_name, TimelineTime::ZERO, (1920.0, 1080.0))
    }

    #[test]
    fn continuous_updates_share_one_undo_step() {
        let mut project = Project::new("test");
        let layer_id = project.sequence.layers[0].id;
        let mut history = EditHistory::default();
        let mut text = text_item("Title");
        text.duration = TimelineTime::from_seconds(2.0);
        history
            .execute(
                &mut project,
                EditCommand::AddText {
                    layer_id,
                    text: text.clone(),
                },
            )
            .unwrap();

        for x in [10.0_f32, 20.0, 30.0] {
            let mut updated = text.clone();
            updated.transform.x = x;
            history
                .execute(&mut project, EditCommand::UpdateText { text: updated })
                .unwrap();
        }
        assert_eq!(project.text(text.id).unwrap().transform.x, 30.0);
        // One undo undoes the whole drag, back to the post-add state.
        assert!(history.undo(&mut project));
        assert_eq!(project.text(text.id).unwrap().transform.x, text.transform.x);
        assert!(history.undo(&mut project));
        assert!(project.text(text.id).is_none());
    }

    #[test]
    fn different_targets_do_not_coalesce() {
        let mut project = Project::new("test");
        let layer_id = project.sequence.layers[0].id;
        let mut history = EditHistory::default();
        let a = text_item("A");
        let mut b = text_item("B");
        b.timeline_start = TimelineTime::from_seconds(5.0);
        history
            .execute(
                &mut project,
                EditCommand::AddText {
                    layer_id,
                    text: a.clone(),
                },
            )
            .unwrap();
        history
            .execute(
                &mut project,
                EditCommand::AddText {
                    layer_id,
                    text: b.clone(),
                },
            )
            .unwrap();
        let mut moved_a = a.clone();
        moved_a.transform.x = 1.0;
        history
            .execute(&mut project, EditCommand::UpdateText { text: moved_a })
            .unwrap();
        let mut moved_b = b.clone();
        moved_b.transform.x = 2.0;
        history
            .execute(&mut project, EditCommand::UpdateText { text: moved_b })
            .unwrap();
        assert!(history.undo(&mut project));
        assert_eq!(project.text(b.id).unwrap().transform.x, b.transform.x);
        assert_eq!(project.text(a.id).unwrap().transform.x, 1.0);
    }
}
