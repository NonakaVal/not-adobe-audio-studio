use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Project {
    pub id: Uuid,
    pub title: String,
    pub language: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub media: Vec<Media>,
    #[serde(default)]
    pub media_cuts: Vec<MediaCut>,
    pub speakers: Vec<Speaker>,
    pub blocks: Vec<Block>,
}

impl Project {
    pub fn new(title: impl Into<String>) -> Self {
        let now = now_unix_seconds();
        Self {
            id: Uuid::new_v4(),
            title: title.into(),
            language: None,
            created_at: now,
            updated_at: now,
            media: Vec::new(),
            media_cuts: Vec::new(),
            speakers: Vec::new(),
            blocks: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), CoreError> {
        let media_ids: HashSet<_> = self.media.iter().map(|media| media.id).collect();
        if media_ids.len() != self.media.len() {
            return Err(CoreError::DuplicateId("media"));
        }
        let speaker_ids: HashSet<_> = self.speakers.iter().map(|speaker| speaker.id).collect();
        if speaker_ids.len() != self.speakers.len() {
            return Err(CoreError::DuplicateId("speaker"));
        }
        let block_ids: HashSet<_> = self.blocks.iter().map(|block| block.id).collect();
        if block_ids.len() != self.blocks.len() {
            return Err(CoreError::DuplicateId("block"));
        }

        let media_durations: HashMap<_, _> = self
            .media
            .iter()
            .map(|media| (media.id, media.duration_ms))
            .collect();
        let mut word_ids = HashSet::new();
        for block in &self.blocks {
            if let Some(speaker_id) = block.speaker_id
                && !speaker_ids.contains(&speaker_id)
            {
                return Err(CoreError::UnknownSpeaker(speaker_id));
            }
            for word in &block.words {
                if !word_ids.insert(word.id) {
                    return Err(CoreError::DuplicateId("word"));
                }
                let Some(duration_ms) = media_durations.get(&word.media_id) else {
                    return Err(CoreError::UnknownMedia(word.media_id));
                };
                if word.start_ms < 0 || word.end_ms <= word.start_ms || word.end_ms > *duration_ms {
                    return Err(CoreError::InvalidWordTiming(word.id));
                }
                if word.confidence.is_some_and(|confidence| {
                    !confidence.is_finite() || !(0.0..=1.0).contains(&confidence)
                }) {
                    return Err(CoreError::InvalidWordConfidence(word.id));
                }
            }
        }
        for media in &self.media {
            if media.duration_ms < 0 || media.sample_rate == 0 || media.channels == 0 {
                return Err(CoreError::InvalidMedia(media.id));
            }
        }
        for cut in &self.media_cuts {
            let Some(duration_ms) = media_durations.get(&cut.media_id) else {
                return Err(CoreError::UnknownMedia(cut.media_id));
            };
            if cut.start_ms < 0 || cut.end_ms <= cut.start_ms || cut.end_ms > *duration_ms {
                return Err(CoreError::InvalidCut(cut.media_id));
            }
        }
        Ok(())
    }

    pub fn timeline(&self) -> Result<Timeline, CoreError> {
        self.validate()?;
        let mut segments: Vec<TimelineSegment> = Vec::new();
        let mut output_cursor_ms = 0;
        for media in &self.media {
            let mut deleted: Vec<_> = self
                .media_cuts
                .iter()
                .filter(|cut| cut.media_id == media.id)
                .map(|cut| (cut.start_ms, cut.end_ms))
                .chain(self.blocks.iter().flat_map(|block| {
                    block
                        .words
                        .iter()
                        .filter(|word| word.media_id == media.id && word.deleted)
                        .map(|word| (word.start_ms, word.end_ms))
                }))
                .collect();
            deleted.sort_unstable();
            let mut merged_deleted: Vec<(i64, i64)> = Vec::new();
            for interval in deleted {
                if let Some(previous) = merged_deleted.last_mut()
                    && interval.0 <= previous.1
                {
                    previous.1 = previous.1.max(interval.1);
                } else {
                    merged_deleted.push(interval);
                }
            }

            let mut source_cursor_ms = 0;
            for (deleted_start_ms, deleted_end_ms) in merged_deleted {
                if deleted_start_ms > source_cursor_ms {
                    push_segment(
                        &mut segments,
                        media.id,
                        source_cursor_ms,
                        deleted_start_ms,
                        &mut output_cursor_ms,
                    );
                }
                source_cursor_ms = source_cursor_ms.max(deleted_end_ms);
            }
            push_segment(
                &mut segments,
                media.id,
                source_cursor_ms,
                media.duration_ms,
                &mut output_cursor_ms,
            );
        }
        Ok(Timeline {
            segments,
            duration_ms: output_cursor_ms,
        })
    }
}

fn push_segment(
    segments: &mut Vec<TimelineSegment>,
    media_id: Uuid,
    source_start_ms: i64,
    source_end_ms: i64,
    output_cursor_ms: &mut i64,
) {
    if source_end_ms <= source_start_ms {
        return;
    }
    let duration_ms = source_end_ms - source_start_ms;
    segments.push(TimelineSegment {
        media_id,
        source_start_ms,
        source_end_ms,
        output_start_ms: *output_cursor_ms,
        output_end_ms: *output_cursor_ms + duration_ms,
    });
    *output_cursor_ms += duration_ms;
}

fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs().min(i64::MAX as u64) as i64)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Media {
    pub id: Uuid,
    pub kind: MediaKind,
    pub path: String,
    #[serde(default)]
    pub original_filename: String,
    pub duration_ms: i64,
    pub sample_rate: u32,
    pub channels: u16,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub frame_rate_num: u32,
    #[serde(default = "one")]
    pub frame_rate_den: u32,
    #[serde(default)]
    pub audio_codec: Option<String>,
    pub sha256: Option<String>,
    pub enhanced_of: Option<Uuid>,
}

fn one() -> u32 {
    1
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MediaCut {
    pub media_id: Uuid,
    pub start_ms: i64,
    pub end_ms: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum MediaKind {
    Audio,
    Video,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Speaker {
    pub id: Uuid,
    pub name: String,
    pub color: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Block {
    pub id: Uuid,
    pub position: u32,
    pub speaker_id: Option<Uuid>,
    pub kind: BlockKind,
    pub words: Vec<Word>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum BlockKind {
    Transcript,
    Music,
    Placeholder,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Word {
    pub id: Uuid,
    pub media_id: Uuid,
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub confidence: Option<f32>,
    pub deleted: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Timeline {
    pub segments: Vec<TimelineSegment>,
    pub duration_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TimelineSegment {
    pub media_id: Uuid,
    pub source_start_ms: i64,
    pub source_end_ms: i64,
    pub output_start_ms: i64,
    pub output_end_ms: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum Command {
    RenameProject { title: String },
    DeleteWords { word_ids: Vec<Uuid> },
    RestoreWords { word_ids: Vec<Uuid> },
    EditWordText { word_id: Uuid, text: String },
    RenameSpeaker { speaker_id: Uuid, name: String },
    MoveBlock { block_id: Uuid, position: u32 },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Editor {
    pub project: Project,
    undo_stack: Vec<Command>,
    redo_stack: Vec<Command>,
}

impl Editor {
    pub fn new(project: Project) -> Result<Self, CoreError> {
        project.validate()?;
        Ok(Self {
            project,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        })
    }

    pub fn apply(&mut self, command: Command) -> Result<(), CoreError> {
        let inverse = apply_command(&mut self.project, command)?;
        self.project.updated_at = now_unix_seconds();
        self.undo_stack.push(inverse);
        self.redo_stack.clear();
        Ok(())
    }

    pub fn undo(&mut self) -> Result<bool, CoreError> {
        let Some(command) = self.undo_stack.pop() else {
            return Ok(false);
        };
        match apply_command(&mut self.project, command.clone()) {
            Ok(inverse) => {
                self.project.updated_at = now_unix_seconds();
                self.redo_stack.push(inverse);
                Ok(true)
            }
            Err(error) => {
                self.undo_stack.push(command);
                Err(error)
            }
        }
    }

    pub fn redo(&mut self) -> Result<bool, CoreError> {
        let Some(command) = self.redo_stack.pop() else {
            return Ok(false);
        };
        match apply_command(&mut self.project, command.clone()) {
            Ok(inverse) => {
                self.project.updated_at = now_unix_seconds();
                self.undo_stack.push(inverse);
                Ok(true)
            }
            Err(error) => {
                self.redo_stack.push(command);
                Err(error)
            }
        }
    }

    pub fn undo_stack(&self) -> &[Command] {
        &self.undo_stack
    }

    pub fn redo_stack(&self) -> &[Command] {
        &self.redo_stack
    }

    pub fn from_parts(
        project: Project,
        undo_stack: Vec<Command>,
        redo_stack: Vec<Command>,
    ) -> Result<Self, CoreError> {
        project.validate()?;
        Ok(Self {
            project,
            undo_stack,
            redo_stack,
        })
    }
}

fn apply_command(project: &mut Project, command: Command) -> Result<Command, CoreError> {
    match command {
        Command::RenameProject { title } => {
            if title.trim().is_empty() {
                return Err(CoreError::EmptyProjectTitle);
            }
            let previous_title = std::mem::replace(&mut project.title, title);
            Ok(Command::RenameProject {
                title: previous_title,
            })
        }
        Command::DeleteWords { word_ids } => {
            let requested: HashSet<_> = word_ids.iter().copied().collect();
            if requested.len() != word_ids.len()
                || requested
                    .iter()
                    .any(|id| find_word_mut(project, *id).is_none())
            {
                return Err(CoreError::UnknownWord);
            }
            let mut changed = Vec::new();
            for word in all_words_mut(project) {
                if requested.contains(&word.id) && !word.deleted {
                    word.deleted = true;
                    changed.push(word.id);
                }
            }
            Ok(Command::RestoreWords { word_ids: changed })
        }
        Command::RestoreWords { word_ids } => {
            let requested: HashSet<_> = word_ids.iter().copied().collect();
            if requested.len() != word_ids.len()
                || requested
                    .iter()
                    .any(|id| find_word_mut(project, *id).is_none())
            {
                return Err(CoreError::UnknownWord);
            }
            let mut changed = Vec::new();
            for word in all_words_mut(project) {
                if requested.contains(&word.id) && word.deleted {
                    word.deleted = false;
                    changed.push(word.id);
                }
            }
            Ok(Command::DeleteWords { word_ids: changed })
        }
        Command::EditWordText { word_id, text } => {
            let word = find_word_mut(project, word_id).ok_or(CoreError::UnknownWord)?;
            let previous_text = std::mem::replace(&mut word.text, text);
            Ok(Command::EditWordText {
                word_id,
                text: previous_text,
            })
        }
        Command::RenameSpeaker { speaker_id, name } => {
            let speaker = project
                .speakers
                .iter_mut()
                .find(|speaker| speaker.id == speaker_id)
                .ok_or(CoreError::UnknownSpeaker(speaker_id))?;
            let previous_name = std::mem::replace(&mut speaker.name, name);
            Ok(Command::RenameSpeaker {
                speaker_id,
                name: previous_name,
            })
        }
        Command::MoveBlock { block_id, position } => {
            let mut ordered_blocks = std::mem::take(&mut project.blocks);
            ordered_blocks.sort_by_key(|block| (block.position, block.id));
            let Some(previous_index) = ordered_blocks.iter().position(|block| block.id == block_id)
            else {
                project.blocks = ordered_blocks;
                return Err(CoreError::UnknownBlock(block_id));
            };
            let block = ordered_blocks.remove(previous_index);
            let target_index = (position as usize).min(ordered_blocks.len());
            ordered_blocks.insert(target_index, block);
            for (index, block) in ordered_blocks.iter_mut().enumerate() {
                block.position = index as u32;
            }
            project.blocks = ordered_blocks;
            Ok(Command::MoveBlock {
                block_id,
                position: previous_index as u32,
            })
        }
    }
}

fn all_words_mut(project: &mut Project) -> impl Iterator<Item = &mut Word> {
    project.blocks.iter_mut().flat_map(|block| &mut block.words)
}

fn find_word_mut(project: &mut Project, word_id: Uuid) -> Option<&mut Word> {
    project
        .blocks
        .iter_mut()
        .flat_map(|block| &mut block.words)
        .find(|word| word.id == word_id)
}

#[derive(Debug, Error, PartialEq)]
pub enum CoreError {
    #[error("duplicate {0} id")]
    DuplicateId(&'static str),
    #[error("project title cannot be empty")]
    EmptyProjectTitle,
    #[error("word {0} has invalid media timestamps")]
    InvalidWordTiming(Uuid),
    #[error("word {0} has invalid confidence")]
    InvalidWordConfidence(Uuid),
    #[error("word references unknown media {0}")]
    UnknownMedia(Uuid),
    #[error("media {0} has invalid metadata")]
    InvalidMedia(Uuid),
    #[error("media {0} has an invalid cut interval")]
    InvalidCut(Uuid),
    #[error("block references unknown speaker {0}")]
    UnknownSpeaker(Uuid),
    #[error("block {0} does not exist")]
    UnknownBlock(Uuid),
    #[error("word does not exist")]
    UnknownWord,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_project() -> (Project, Uuid, Uuid, Uuid) {
        let media_id = Uuid::new_v4();
        let speaker_id = Uuid::new_v4();
        let first_word_id = Uuid::new_v4();
        let second_word_id = Uuid::new_v4();
        let mut project = Project::new("Teste");
        project.media.push(Media {
            id: media_id,
            kind: MediaKind::Audio,
            path: "media/sample.wav".into(),
            original_filename: "sample.wav".into(),
            duration_ms: 2_000,
            sample_rate: 48_000,
            channels: 2,
            width: 0,
            height: 0,
            frame_rate_num: 0,
            frame_rate_den: 1,
            audio_codec: Some("pcm_s16le".into()),
            sha256: None,
            enhanced_of: None,
        });
        project.speakers.push(Speaker {
            id: speaker_id,
            name: "Falante".into(),
            color: 0x336699,
        });
        project.blocks.push(Block {
            id: Uuid::new_v4(),
            position: 0,
            speaker_id: Some(speaker_id),
            kind: BlockKind::Transcript,
            words: vec![
                Word {
                    id: first_word_id,
                    media_id,
                    text: "Olá".into(),
                    start_ms: 0,
                    end_ms: 500,
                    confidence: Some(0.98),
                    deleted: false,
                },
                Word {
                    id: second_word_id,
                    media_id,
                    text: "mundo".into(),
                    start_ms: 700,
                    end_ms: 1_200,
                    confidence: Some(0.97),
                    deleted: false,
                },
            ],
        });
        (project, first_word_id, second_word_id, speaker_id)
    }

    #[test]
    fn timeline_omits_deleted_words_and_remaps_output_time() {
        let (mut project, _, second_word_id, _) = sample_project();
        project.blocks[0].words[1].deleted = true;
        project.blocks[0].words.push(Word {
            id: Uuid::new_v4(),
            media_id: project.media[0].id,
            text: "depois".into(),
            start_ms: 1_500,
            end_ms: 1_800,
            confidence: None,
            deleted: false,
        });
        let timeline = project.timeline().unwrap();
        assert_eq!(timeline.duration_ms, 1_500);
        assert_eq!(timeline.segments.len(), 2);
        assert_eq!(timeline.segments[0].output_start_ms, 0);
        assert_eq!(timeline.segments[1].source_start_ms, 1_200);
        assert_eq!(timeline.segments[1].source_end_ms, 2_000);
        assert!(
            project.blocks[0]
                .words
                .iter()
                .any(|word| word.id == second_word_id)
        );
    }

    #[test]
    fn edit_delete_and_undo_preserve_word_times() {
        let (project, first_word_id, _, _) = sample_project();
        let original_word = project.blocks[0].words[0].clone();
        let mut editor = Editor::new(project).unwrap();
        editor
            .apply(Command::EditWordText {
                word_id: first_word_id,
                text: "Oi".into(),
            })
            .unwrap();
        editor
            .apply(Command::DeleteWords {
                word_ids: vec![first_word_id],
            })
            .unwrap();
        assert!(editor.project.blocks[0].words[0].deleted);
        assert!(editor.undo().unwrap());
        assert!(!editor.project.blocks[0].words[0].deleted);
        assert!(editor.undo().unwrap());
        assert_eq!(editor.project.blocks[0].words[0], original_word);
        assert!(editor.redo().unwrap());
        assert_eq!(editor.project.blocks[0].words[0].text, "Oi");
    }

    #[test]
    fn invalid_timestamps_are_rejected() {
        let (mut project, _, _, _) = sample_project();
        project.blocks[0].words[0].end_ms = 0;
        assert!(matches!(
            project.validate(),
            Err(CoreError::InvalidWordTiming(_))
        ));
    }

    #[test]
    fn move_block_and_undo_restore_order() {
        let (mut project, _, _, speaker_id) = sample_project();
        let moved_block_id = Uuid::new_v4();
        project.blocks.push(Block {
            id: moved_block_id,
            position: 1,
            speaker_id: Some(speaker_id),
            kind: BlockKind::Placeholder,
            words: Vec::new(),
        });
        let original_order: Vec<_> = project.blocks.iter().map(|block| block.id).collect();
        let first_block_id = project.blocks[0].id;
        let mut editor = Editor::new(project).unwrap();
        editor
            .apply(Command::MoveBlock {
                block_id: moved_block_id,
                position: 0,
            })
            .unwrap();
        assert_eq!(editor.project.blocks[0].id, moved_block_id);
        assert!(editor.undo().unwrap());
        assert_eq!(
            editor
                .project
                .blocks
                .iter()
                .map(|block| block.id)
                .collect::<Vec<_>>(),
            original_order
        );
        assert_eq!(editor.project.blocks[0].id, first_block_id);
    }
}
