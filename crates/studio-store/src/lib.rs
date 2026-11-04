use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use studio_core::{Block, Command, Editor, Media, MediaCut, Project, Speaker, Word};
use thiserror::Error;
use uuid::Uuid;

pub struct ProjectStore {
    project_dir: PathBuf,
    connection: Connection,
}

impl ProjectStore {
    pub fn create(project_dir: impl AsRef<Path>, editor: &Editor) -> Result<Self, StoreError> {
        let project_dir = project_dir.as_ref().to_path_buf();
        for directory in ["media", "cache", "exports"] {
            fs::create_dir_all(project_dir.join(directory))?;
        }
        let database_path = project_dir.join("project.db");
        if database_path.exists() {
            return Err(StoreError::AlreadyExists(database_path));
        }
        let connection = Connection::open(&database_path)?;
        let mut store = Self {
            project_dir,
            connection,
        };
        store.initialize()?;
        store.save(editor)?;
        Ok(store)
    }

    pub fn open(project_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let project_dir = project_dir.as_ref().to_path_buf();
        let database_path = project_dir.join("project.db");
        if !database_path.is_file() {
            return Err(StoreError::MissingDatabase(database_path));
        }
        let connection = Connection::open(&database_path)?;
        let mut store = Self {
            project_dir,
            connection,
        };
        store.initialize()?;
        Ok(store)
    }

    pub fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    pub fn save(&mut self, editor: &Editor) -> Result<(), StoreError> {
        editor.project.validate()?;
        let transaction = self.connection.transaction()?;
        save_project(&transaction, &editor.project)?;
        transaction.execute("DELETE FROM history", [])?;
        for (stack_name, commands) in [("undo", editor.undo_stack()), ("redo", editor.redo_stack())]
        {
            for (position, command) in commands.iter().enumerate() {
                transaction.execute(
                    "INSERT INTO history (stack_name, position, payload_json) VALUES (?1, ?2, ?3)",
                    params![stack_name, position as i64, serde_json::to_string(command)?],
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn load(&self) -> Result<Editor, StoreError> {
        let project = load_project(&self.connection)?.ok_or(StoreError::NoProject)?;
        let mut stacks = [Vec::new(), Vec::new()];
        let mut statement = self.connection.prepare(
            "SELECT stack_name, payload_json FROM history ORDER BY stack_name, position",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (stack_name, payload) = row?;
            let stack = match stack_name.as_str() {
                "undo" => &mut stacks[0],
                "redo" => &mut stacks[1],
                _ => return Err(StoreError::UnknownHistoryStack(stack_name)),
            };
            stack.push(serde_json::from_str(&payload)?);
        }
        Ok(Editor::from_parts(
            project,
            stacks[0].clone(),
            stacks[1].clone(),
        )?)
    }

    fn initialize(&mut self) -> Result<(), StoreError> {
        self.connection.pragma_update(None, "journal_mode", "WAL")?;
        self.connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS project (
                 id TEXT PRIMARY KEY NOT NULL,
                 title TEXT NOT NULL,
                 language TEXT,
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS media (
                 id TEXT PRIMARY KEY NOT NULL,
                 payload_json TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS media_cuts (
                 media_id TEXT NOT NULL,
                 position INTEGER NOT NULL,
                 payload_json TEXT NOT NULL,
                 PRIMARY KEY (media_id, position)
             );
             CREATE TABLE IF NOT EXISTS speakers (
                 id TEXT PRIMARY KEY NOT NULL,
                 payload_json TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS blocks (
                 id TEXT PRIMARY KEY NOT NULL,
                 position INTEGER NOT NULL,
                 payload_json TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS words (
                 id TEXT PRIMARY KEY NOT NULL,
                 block_id TEXT NOT NULL,
                 position INTEGER NOT NULL,
                 payload_json TEXT NOT NULL,
                 FOREIGN KEY (block_id) REFERENCES blocks(id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS history (
                 stack_name TEXT NOT NULL CHECK (stack_name IN ('undo', 'redo')),
                 position INTEGER NOT NULL,
                 payload_json TEXT NOT NULL,
                 PRIMARY KEY (stack_name, position)
             );",
        )?;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectSummary {
    pub id: Uuid,
    pub title: String,
    pub updated_at: i64,
    pub duration_ms: i64,
    pub preview: String,
}

pub struct ProjectLibrary {
    root: PathBuf,
}

impl ProjectLibrary {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn default_root() -> Result<PathBuf, StoreError> {
        if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
            return Ok(PathBuf::from(data_home)
                .join("podcast-studio")
                .join("projects"));
        }
        let home = std::env::var_os("HOME").ok_or(StoreError::MissingHome)?;
        Ok(PathBuf::from(home).join(".local/share/podcast-studio/projects"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn project_path(&self, id: Uuid) -> PathBuf {
        self.root.join(id.to_string())
    }

    pub fn create(&self, editor: &Editor) -> Result<ProjectSummary, StoreError> {
        let path = self.project_path(editor.project.id);
        let store = ProjectStore::create(&path, editor)?;
        project_summary(&store.load()?)
    }

    pub fn open(&self, id: Uuid) -> Result<ProjectStore, StoreError> {
        ProjectStore::open(self.project_path(id))
    }

    pub fn rename(&self, id: Uuid, title: &str) -> Result<ProjectSummary, StoreError> {
        let mut store = self.open(id)?;
        let mut editor = store.load()?;
        if editor.project.id != id {
            return Err(StoreError::ProjectFolderMismatch {
                folder: id,
                project: editor.project.id,
            });
        }
        editor.apply(Command::RenameProject {
            title: title.to_owned(),
        })?;
        store.save(&editor)?;
        project_summary(&editor)
    }

    pub fn list(&self) -> Result<Vec<ProjectSummary>, StoreError> {
        let mut summaries = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Ok(id) = Uuid::parse_str(&entry.file_name().to_string_lossy()) else {
                continue;
            };
            let store = ProjectStore::open(entry.path())?;
            let editor = store.load()?;
            if editor.project.id != id {
                return Err(StoreError::ProjectFolderMismatch {
                    folder: id,
                    project: editor.project.id,
                });
            }
            summaries.push(project_summary(&editor)?);
        }
        summaries.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
        });
        Ok(summaries)
    }

    pub fn duplicate(&self, id: Uuid) -> Result<ProjectSummary, StoreError> {
        self.duplicate_with_progress(id, |_| Ok(()))
    }

    pub fn duplicate_with_progress(
        &self,
        id: Uuid,
        mut report_progress: impl FnMut(f64) -> Result<(), StoreError>,
    ) -> Result<ProjectSummary, StoreError> {
        let source_path = self.project_path(id);
        let source_store = ProjectStore::open(&source_path)?;
        let editor = source_store.load()?;
        drop(source_store);
        if editor.project.id != id {
            return Err(StoreError::ProjectFolderMismatch {
                folder: id,
                project: editor.project.id,
            });
        }

        let new_id = Uuid::new_v4();
        let destination = self.project_path(new_id);
        let staging = self.root.join(format!(".{new_id}.tmp"));
        if destination.exists() || staging.exists() {
            return Err(StoreError::AlreadyExists(destination));
        }
        let total_bytes = directory_size(&source_path)?.max(1);
        let mut copied_bytes = 0;
        if let Err(error) = copy_project_directory(
            &source_path,
            &staging,
            &mut copied_bytes,
            total_bytes,
            &mut report_progress,
        ) {
            if staging.exists() {
                fs::remove_dir_all(&staging)?;
            }
            return Err(error);
        }
        let summary = (|| {
            let mut store = ProjectStore::open(&staging)?;
            let mut duplicate = store.load()?;
            duplicate.project.id = new_id;
            duplicate.project.title = format!("{} (cópia)", editor.project.title);
            duplicate.project.updated_at = now_unix_seconds();
            store.save(&duplicate)?;
            project_summary(&store.load()?)
        })();
        let summary = match summary {
            Ok(summary) => summary,
            Err(error) => {
                fs::remove_dir_all(&staging)?;
                return Err(error);
            }
        };
        if let Err(error) = fs::rename(&staging, &destination) {
            fs::remove_dir_all(&staging)?;
            return Err(error.into());
        }
        Ok(summary)
    }

    pub fn delete(&self, id: Uuid) -> Result<(), StoreError> {
        let path = self.project_path(id);
        if !path.is_dir() {
            return Err(StoreError::MissingProject(id));
        }
        let store = ProjectStore::open(&path)?;
        let project_id = store.load()?.project.id;
        drop(store);
        if project_id != id {
            return Err(StoreError::ProjectFolderMismatch {
                folder: id,
                project: project_id,
            });
        }
        fs::remove_dir_all(path)?;
        Ok(())
    }
}

fn project_summary(editor: &Editor) -> Result<ProjectSummary, StoreError> {
    let duration_ms = editor.project.timeline()?.duration_ms;
    let preview = editor
        .project
        .blocks
        .iter()
        .flat_map(|block| &block.words)
        .filter(|word| !word.deleted)
        .map(|word| word.text.as_str())
        .filter(|word| !word.is_empty())
        .take(32)
        .collect::<Vec<_>>()
        .join(" ");
    Ok(ProjectSummary {
        id: editor.project.id,
        title: editor.project.title.clone(),
        updated_at: editor.project.updated_at,
        duration_ms,
        preview,
    })
}

fn directory_size(path: &Path) -> Result<u64, StoreError> {
    let mut total = 0_u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            total = total.saturating_add(directory_size(&entry.path())?);
        } else if file_type.is_file() {
            total = total.saturating_add(entry.metadata()?.len());
        } else {
            return Err(StoreError::UnsupportedProjectEntry(entry.path()));
        }
    }
    Ok(total)
}

fn copy_project_directory(
    source: &Path,
    destination: &Path,
    copied_bytes: &mut u64,
    total_bytes: u64,
    report_progress: &mut impl FnMut(f64) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_project_directory(
                &entry.path(),
                &target,
                copied_bytes,
                total_bytes,
                report_progress,
            )?;
        } else if file_type.is_file() {
            let mut input = fs::File::open(entry.path())?;
            let mut output = fs::File::create(target)?;
            let mut buffer = vec![0_u8; 1024 * 1024];
            loop {
                let count = std::io::Read::read(&mut input, &mut buffer)?;
                if count == 0 {
                    break;
                }
                std::io::Write::write_all(&mut output, &buffer[..count])?;
                *copied_bytes = copied_bytes.saturating_add(count as u64);
                report_progress((*copied_bytes as f64 / total_bytes as f64).min(1.0))?;
            }
            std::io::Write::flush(&mut output)?;
        } else {
            return Err(StoreError::UnsupportedProjectEntry(entry.path()));
        }
    }
    Ok(())
}

fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs().min(i64::MAX as u64) as i64)
}

fn save_project(transaction: &Transaction<'_>, project: &Project) -> Result<(), StoreError> {
    transaction.execute("DELETE FROM words", [])?;
    transaction.execute("DELETE FROM blocks", [])?;
    transaction.execute("DELETE FROM media", [])?;
    transaction.execute("DELETE FROM media_cuts", [])?;
    transaction.execute("DELETE FROM speakers", [])?;
    transaction.execute("DELETE FROM project", [])?;
    transaction.execute(
        "INSERT INTO project (id, title, language, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            project.id.to_string(),
            project.title,
            project.language,
            project.created_at,
            project.updated_at
        ],
    )?;
    for media in &project.media {
        transaction.execute(
            "INSERT INTO media (id, payload_json) VALUES (?1, ?2)",
            params![media.id.to_string(), serde_json::to_string(media)?],
        )?;
    }
    let mut cut_positions = std::collections::HashMap::<Uuid, i64>::new();
    for cut in &project.media_cuts {
        let position = cut_positions.entry(cut.media_id).or_default();
        transaction.execute(
            "INSERT INTO media_cuts (media_id, position, payload_json) VALUES (?1, ?2, ?3)",
            params![
                cut.media_id.to_string(),
                *position,
                serde_json::to_string(cut)?
            ],
        )?;
        *position += 1;
    }
    for speaker in &project.speakers {
        transaction.execute(
            "INSERT INTO speakers (id, payload_json) VALUES (?1, ?2)",
            params![speaker.id.to_string(), serde_json::to_string(speaker)?],
        )?;
    }
    for block in &project.blocks {
        transaction.execute(
            "INSERT INTO blocks (id, position, payload_json) VALUES (?1, ?2, ?3)",
            params![
                block.id.to_string(),
                block.position,
                serde_json::to_string(block)?
            ],
        )?;
        for (position, word) in block.words.iter().enumerate() {
            transaction.execute(
                "INSERT INTO words (id, block_id, position, payload_json) VALUES (?1, ?2, ?3, ?4)",
                params![
                    word.id.to_string(),
                    block.id.to_string(),
                    position as i64,
                    serde_json::to_string(word)?
                ],
            )?;
        }
    }
    Ok(())
}

fn load_project(connection: &Connection) -> Result<Option<Project>, StoreError> {
    let project_record = connection
        .query_row(
            "SELECT id, title, language, created_at, updated_at FROM project LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((id, title, language, created_at, updated_at)) = project_record else {
        return Ok(None);
    };

    let media = load_payloads::<Media>(connection, "SELECT payload_json FROM media ORDER BY id")?;
    let media_cuts = load_payloads::<MediaCut>(
        connection,
        "SELECT payload_json FROM media_cuts ORDER BY media_id, position",
    )?;
    let speakers =
        load_payloads::<Speaker>(connection, "SELECT payload_json FROM speakers ORDER BY id")?;

    let mut blocks_statement =
        connection.prepare("SELECT id, payload_json FROM blocks ORDER BY position, id")?;
    let block_rows = blocks_statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut blocks = Vec::new();
    for row in block_rows {
        let (block_id, payload) = row?;
        let mut block: Block = serde_json::from_str(&payload)?;
        block.words = load_words(connection, &block_id)?;
        blocks.push(block);
    }

    Ok(Some(Project {
        id: Uuid::parse_str(&id)?,
        title,
        language,
        created_at,
        updated_at,
        media,
        media_cuts,
        speakers,
        blocks,
    }))
}

fn load_payloads<T: serde::de::DeserializeOwned>(
    connection: &Connection,
    query: &str,
) -> Result<Vec<T>, StoreError> {
    let mut statement = connection.prepare(query)?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
}

fn load_words(connection: &Connection, block_id: &str) -> Result<Vec<Word>, StoreError> {
    let mut statement = connection
        .prepare("SELECT payload_json FROM words WHERE block_id = ?1 ORDER BY position")?;
    let rows = statement.query_map([block_id], |row| row.get::<_, String>(0))?;
    rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid UUID in project database: {0}")]
    Uuid(#[from] uuid::Error),
    #[error("project database does not contain a project")]
    NoProject,
    #[error("project database already exists at {0}")]
    AlreadyExists(PathBuf),
    #[error("project database was not found at {0}")]
    MissingDatabase(PathBuf),
    #[error("unknown history stack {0}")]
    UnknownHistoryStack(String),
    #[error("project data is invalid: {0}")]
    Core(#[from] studio_core::CoreError),
    #[error("home directory could not be determined")]
    MissingHome,
    #[error("project {0} does not exist")]
    MissingProject(Uuid),
    #[error("project folder {folder} contains project {project}")]
    ProjectFolderMismatch { folder: Uuid, project: Uuid },
    #[error("unsupported filesystem entry in project folder: {0}")]
    UnsupportedProjectEntry(PathBuf),
    #[error("project operation cancelled")]
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;
    use studio_core::{BlockKind, Command, MediaKind, Word};

    #[test]
    fn project_and_undo_history_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let media_id = Uuid::new_v4();
        let speaker_id = Uuid::new_v4();
        let word_id = Uuid::new_v4();
        let mut project = Project::new("Persistido");
        project.media.push(Media {
            id: media_id,
            kind: MediaKind::Audio,
            path: "media/voice.wav".into(),
            original_filename: "voice.wav".into(),
            duration_ms: 1_000,
            sample_rate: 48_000,
            channels: 1,
            width: 0,
            height: 0,
            frame_rate_num: 0,
            frame_rate_den: 1,
            audio_codec: Some("pcm_s16le".into()),
            sha256: Some("digest".into()),
            enhanced_of: None,
        });
        project.speakers.push(Speaker {
            id: speaker_id,
            name: "Pessoa".into(),
            color: 0,
        });
        project.blocks.push(Block {
            id: Uuid::new_v4(),
            position: 0,
            speaker_id: Some(speaker_id),
            kind: BlockKind::Transcript,
            words: vec![Word {
                id: word_id,
                media_id,
                text: "Olá".into(),
                start_ms: 100,
                end_ms: 300,
                confidence: Some(0.9),
                deleted: false,
            }],
        });
        let mut editor = Editor::new(project).unwrap();
        editor
            .apply(Command::EditWordText {
                word_id,
                text: "Oi".into(),
            })
            .unwrap();
        let mut store = ProjectStore::create(directory.path(), &editor).unwrap();
        let mut reopened = ProjectStore::open(directory.path())
            .unwrap()
            .load()
            .unwrap();
        assert_eq!(reopened, editor);
        assert!(reopened.undo().unwrap());
        store.save(&reopened).unwrap();
        assert_eq!(store.load().unwrap().project.blocks[0].words[0].text, "Olá");
    }

    #[test]
    fn project_library_lists_duplicates_and_deletes() {
        let directory = tempfile::tempdir().unwrap();
        let library = ProjectLibrary::new(directory.path().join("projects")).unwrap();
        let mut project = Project::new("Entrevista");
        let media_id = Uuid::new_v4();
        project.media.push(Media {
            id: media_id,
            kind: MediaKind::Audio,
            path: "media/audio.wav".into(),
            original_filename: "audio.wav".into(),
            duration_ms: 10_000,
            sample_rate: 48_000,
            channels: 1,
            width: 0,
            height: 0,
            frame_rate_num: 0,
            frame_rate_den: 1,
            audio_codec: Some("pcm_s16le".into()),
            sha256: None,
            enhanced_of: None,
        });
        project.media_cuts.push(MediaCut {
            media_id,
            start_ms: 2_000,
            end_ms: 4_000,
        });
        let editor = Editor::new(project).unwrap();
        let original = library.create(&editor).unwrap();
        assert_eq!(original.duration_ms, 8_000);
        assert_eq!(library.list().unwrap(), vec![original.clone()]);

        let duplicate = library.duplicate(original.id).unwrap();
        assert_ne!(duplicate.id, original.id);
        assert_eq!(duplicate.title, "Entrevista (cópia)");
        assert_eq!(duplicate.duration_ms, original.duration_ms);
        assert_eq!(library.list().unwrap().len(), 2);

        library.delete(duplicate.id).unwrap();
        assert_eq!(library.list().unwrap(), vec![original]);
    }

    #[test]
    fn duplicate_is_hidden_from_library_until_it_is_complete() {
        let directory = tempfile::tempdir().unwrap();
        let library = ProjectLibrary::new(directory.path().join("projects")).unwrap();
        let editor = Editor::new(Project::new("Projeto em cópia")).unwrap();
        let original = library.create(&editor).unwrap();
        let cache_dir = library.project_path(original.id).join("cache");
        fs::write(
            cache_dir.join("large-cache-file"),
            vec![0_u8; 2 * 1024 * 1024],
        )
        .unwrap();

        let duplicate = library
            .duplicate_with_progress(original.id, |_| {
                assert_eq!(library.list()?.len(), 1);
                Ok(())
            })
            .unwrap();

        assert_eq!(library.list().unwrap().len(), 2);
        assert_eq!(duplicate.title, "Projeto em cópia (cópia)");
    }
}
