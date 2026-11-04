use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;
use sha2::{Digest, Sha256};
use studio_core::{Block, BlockKind, Editor, Media, MediaCut, MediaKind, Project, Speaker, Word};
use studio_jobs::{JobContext, JobError};
use studio_store::{ProjectLibrary, ProjectStore};
use thiserror::Error;
use uuid::Uuid;

const SUPPORTED_EXTENSIONS: &[&str] = &[
    "mp3", "wav", "m4a", "aac", "ogg", "flac", "mp4", "mov", "mkv", "webm",
];
const COPY_BUFFER_SIZE: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct MediaProbe {
    pub duration_ms: i64,
    pub has_audio: bool,
    pub has_video: bool,
    pub sample_rate: u32,
    pub channels: u16,
    pub width: u32,
    pub height: u32,
    pub frame_rate_num: u32,
    pub frame_rate_den: u32,
    pub audio_codec: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaStorageMode {
    CopyIntoProject,
    ReferenceOriginal,
}

pub fn probe_media(path: impl AsRef<Path>) -> Result<MediaProbe, ImportError> {
    let path = path.as_ref();
    ensure_supported(path)?;
    if !path.is_file() {
        return Err(ImportError::NotAFile(path.to_path_buf()));
    }
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration:stream=codec_type,sample_rate,channels,width,height,r_frame_rate,codec_name",
            "-of",
            "json",
        ])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| ImportError::FfprobeUnavailable(error.to_string()))?;
    if !output.status.success() {
        return Err(ImportError::FfprobeFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let response: Value = serde_json::from_slice(&output.stdout)?;
    parse_probe_response(&response)
}

fn parse_probe_response(response: &Value) -> Result<MediaProbe, ImportError> {
    let streams = response
        .get("streams")
        .and_then(Value::as_array)
        .ok_or(ImportError::InvalidProbeResponse)?;
    let audio = streams
        .iter()
        .find(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("audio"));
    let video = streams
        .iter()
        .find(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("video"));
    let audio = audio.ok_or(ImportError::NoAudioStream)?;
    let duration_seconds = response
        .pointer("/format/duration")
        .and_then(value_as_f64)
        .or_else(|| {
            streams
                .iter()
                .filter_map(|stream| stream.get("duration").and_then(value_as_f64))
                .reduce(f64::max)
        })
        .filter(|duration| duration.is_finite() && *duration > 0.0)
        .ok_or(ImportError::InvalidDuration)?;
    let duration_ms = (duration_seconds * 1_000.0).round();
    if !duration_ms.is_finite() || duration_ms <= 0.0 || duration_ms > i64::MAX as f64 {
        return Err(ImportError::InvalidDuration);
    }
    let sample_rate = audio
        .get("sample_rate")
        .and_then(value_as_u32)
        .ok_or(ImportError::InvalidAudioMetadata)?;
    let channels = audio
        .get("channels")
        .and_then(value_as_u32)
        .and_then(|value| u16::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or(ImportError::InvalidAudioMetadata)?;

    let (width, height, frame_rate_num, frame_rate_den) = video.map_or((0, 0, 0, 1), |stream| {
        let (numerator, denominator) = stream
            .get("r_frame_rate")
            .and_then(Value::as_str)
            .and_then(parse_rational)
            .unwrap_or((0, 1));
        (
            stream.get("width").and_then(value_as_u32).unwrap_or(0),
            stream.get("height").and_then(value_as_u32).unwrap_or(0),
            numerator,
            denominator,
        )
    });

    Ok(MediaProbe {
        duration_ms: duration_ms as i64,
        has_audio: true,
        has_video: video.is_some(),
        sample_rate,
        channels,
        width,
        height,
        frame_rate_num,
        frame_rate_den,
        audio_codec: audio
            .get("codec_name")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn value_as_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

fn value_as_u32(value: &Value) -> Option<u32> {
    value
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        .filter(|value| *value > 0)
}

fn parse_rational(value: &str) -> Option<(u32, u32)> {
    let (numerator, denominator) = value.split_once('/')?;
    let numerator: u32 = numerator.parse().ok()?;
    let denominator: u32 = denominator.parse().ok()?;
    (denominator > 0).then_some((numerator, denominator))
}

pub fn import_media(
    source: impl AsRef<Path>,
    project_dir: impl AsRef<Path>,
    storage_mode: MediaStorageMode,
    context: &JobContext,
) -> Result<Media, ImportError> {
    let source = source.as_ref();
    let project_dir = project_dir.as_ref();
    context.check_cancelled()?;
    let probe = probe_media(source)?;
    context.report_progress(0.05, "Analisando mídia")?;
    let original_filename = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ImportError::InvalidFilename(source.to_path_buf()))?
        .to_owned();
    let media_id = Uuid::new_v4();
    let (stored_path, sha256) = match storage_mode {
        MediaStorageMode::CopyIntoProject => {
            let extension = source
                .extension()
                .and_then(|extension| extension.to_str())
                .ok_or_else(|| ImportError::UnsupportedFormat(source.to_path_buf()))?;
            let media_dir = project_dir.join("media");
            fs::create_dir_all(&media_dir)?;
            let stored_filename = format!("{media_id}.{}", extension.to_ascii_lowercase());
            let destination = media_dir.join(&stored_filename);
            let temporary = media_dir.join(format!(".{stored_filename}.part"));
            let copy_result = copy_and_hash(source, &temporary, context);
            let digest = match copy_result {
                Ok(digest) => digest,
                Err(error) => {
                    let _ = fs::remove_file(&temporary);
                    return Err(error);
                }
            };
            if let Err(error) = context.check_cancelled() {
                let _ = fs::remove_file(&temporary);
                return Err(error.into());
            }
            fs::rename(&temporary, &destination)?;
            (format!("media/{stored_filename}"), digest)
        }
        MediaStorageMode::ReferenceOriginal => (
            source
                .to_str()
                .ok_or_else(|| ImportError::InvalidFilename(source.to_path_buf()))?
                .to_owned(),
            hash_file(source, context)?,
        ),
    };

    Ok(Media {
        id: media_id,
        kind: if probe.has_video {
            MediaKind::Video
        } else {
            MediaKind::Audio
        },
        path: stored_path,
        original_filename,
        duration_ms: probe.duration_ms,
        sample_rate: probe.sample_rate,
        channels: probe.channels,
        width: probe.width,
        height: probe.height,
        frame_rate_num: probe.frame_rate_num,
        frame_rate_den: probe.frame_rate_den,
        audio_codec: probe.audio_codec,
        sha256: Some(sha256),
        enhanced_of: None,
    })
}

fn hash_file(source: &Path, context: &JobContext) -> Result<String, ImportError> {
    let source_file = File::open(source)?;
    let size = source_file.metadata()?.len().max(1);
    let mut reader = BufReader::with_capacity(COPY_BUFFER_SIZE, source_file);
    let mut hasher = Sha256::new();
    let mut processed = 0_u64;
    let mut buffer = vec![0_u8; COPY_BUFFER_SIZE];
    loop {
        context.check_cancelled()?;
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        processed += count as u64;
        context.report_progress(
            (processed as f64 / size as f64).min(1.0),
            "Verificando mídia referenciada",
        )?;
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn copy_and_hash(
    source: &Path,
    destination: &Path,
    context: &JobContext,
) -> Result<String, ImportError> {
    let source_file = File::open(source)?;
    let size = source_file.metadata()?.len().max(1);
    let mut reader = BufReader::with_capacity(COPY_BUFFER_SIZE, source_file);
    let mut output = BufWriter::with_capacity(
        COPY_BUFFER_SIZE,
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?,
    );
    let mut hasher = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = vec![0_u8; COPY_BUFFER_SIZE];
    loop {
        context.check_cancelled()?;
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
        copied += count as u64;
        context.report_progress(
            0.05 + 0.9 * (copied as f64 / size as f64).min(1.0),
            "Copiando mídia para o projeto",
        )?;
    }
    output.flush()?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn ensure_supported(path: &Path) -> Result<(), ImportError> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    if extension.is_some_and(|extension| SUPPORTED_EXTENSIONS.contains(&extension.as_str())) {
        Ok(())
    } else {
        Err(ImportError::UnsupportedFormat(path.to_path_buf()))
    }
}

pub fn sanitize_filename(filename: &str) -> String {
    let path = Path::new(filename);
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("media");
    let sanitized: String = stem
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let sanitized = sanitized.trim_matches('_');
    if sanitized.is_empty() {
        "media".into()
    } else {
        sanitized.chars().take(80).collect()
    }
}

pub fn migrate_legacy_projects(
    legacy_data_dir: impl AsRef<Path>,
    projects_dir: impl AsRef<Path>,
    context: &JobContext,
) -> Result<usize, ImportError> {
    let legacy_data_dir = legacy_data_dir.as_ref();
    let projects_dir = projects_dir.as_ref();
    let projects_source = legacy_data_dir.join("projects");
    let media_index_path = legacy_data_dir.join("media.json");
    if !projects_source.is_dir() {
        return Err(ImportError::LegacyProjectsNotFound(projects_source));
    }
    let media_index: Value = serde_json::from_slice(&fs::read(media_index_path)?)?;
    let mut existing_ids = ProjectLibrary::new(projects_dir)?
        .list()?
        .into_iter()
        .map(|project| project.id)
        .collect::<std::collections::HashSet<_>>();
    let mut files = fs::read_dir(&projects_source)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    files.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "json")
    });
    files.sort();
    let total = files.len().max(1);
    let mut imported = 0;

    for (index, project_path) in files.iter().enumerate() {
        context.check_cancelled()?;
        let legacy: Value = serde_json::from_slice(&fs::read(project_path)?)?;
        let project_id = legacy
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| Uuid::parse_str(id).ok())
            .ok_or_else(|| ImportError::InvalidLegacyProject(project_path.clone()))?;
        if existing_ids.contains(&project_id) {
            context.report_progress(
                (index + 1) as f64 / total as f64,
                "Projeto legado já importado",
            )?;
            continue;
        }

        let project_dir = projects_dir.join(project_id.to_string());
        fs::create_dir_all(&project_dir)?;
        let result = (|| {
            let media_id = legacy
                .get("media_id")
                .and_then(Value::as_str)
                .ok_or_else(|| ImportError::InvalidLegacyProject(project_path.clone()))?;
            let media_record = media_index
                .get(media_id)
                .ok_or_else(|| ImportError::MissingLegacyMedia(media_id.to_owned()))?;
            let source_path = media_record
                .get("path")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .unwrap_or_else(|| {
                    legacy_data_dir.join(
                        media_record
                            .get("path")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    )
                });
            let media = import_media(
                &source_path,
                &project_dir,
                MediaStorageMode::CopyIntoProject,
                context,
            )?;
            let project = convert_legacy_project(&legacy, media)?;
            let editor = Editor::new(project)?;
            let store = ProjectStore::create(&project_dir, &editor)?;
            store.load()?;
            Ok::<(), ImportError>(())
        })();
        if let Err(error) = result {
            fs::remove_dir_all(&project_dir)?;
            return Err(error);
        }
        existing_ids.insert(project_id);
        imported += 1;
        context.report_progress(
            (index + 1) as f64 / total as f64,
            format!("Projet legado {}/{}", index + 1, total),
        )?;
    }
    Ok(imported)
}

fn convert_legacy_project(legacy: &Value, media: Media) -> Result<Project, ImportError> {
    let project_id = legacy
        .get("id")
        .and_then(Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or(ImportError::InvalidLegacyData)?;
    let mut project = Project::new(
        legacy
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("Projeto importado"),
    );
    project.id = project_id;
    project.language = legacy
        .get("language")
        .and_then(Value::as_str)
        .map(str::to_owned);
    project.created_at = legacy
        .get("created_at")
        .and_then(parse_legacy_timestamp)
        .unwrap_or(project.created_at);
    project.updated_at = legacy
        .get("updated_at")
        .and_then(parse_legacy_timestamp)
        .unwrap_or(project.created_at);
    let duration_ms = media.duration_ms;
    let media_id = media.id;
    project.media.push(media);

    project.media_cuts = legacy_cuts(legacy, media_id, duration_ms)?;
    let mut speakers = HashMap::<String, Uuid>::new();
    let mut words = Vec::new();
    if let Some(transcript) = legacy.get("transcript").and_then(Value::as_array) {
        for item in transcript {
            let start_ms = seconds_to_ms(item.get("start"), duration_ms)?;
            let end_ms = seconds_to_ms(item.get("end"), duration_ms)?;
            if end_ms <= start_ms {
                return Err(ImportError::InvalidLegacyData);
            }
            let speaker_name = item
                .get("speaker")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("Falante")
                .to_owned();
            speakers.entry(speaker_name.clone()).or_insert_with(|| {
                let id = Uuid::new_v4();
                project.speakers.push(Speaker {
                    id,
                    name: speaker_name,
                    color: 0x5C6BC0,
                });
                id
            });
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| Uuid::parse_str(id).ok())
                .unwrap_or_else(Uuid::new_v4);
            let deleted = project
                .media_cuts
                .iter()
                .any(|cut| cut.start_ms < end_ms && cut.end_ms > start_ms);
            words.push(Word {
                id,
                media_id,
                text: item
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                start_ms,
                end_ms,
                confidence: None,
                deleted,
            });
        }
    }
    if !words.is_empty() {
        words.sort_by_key(|word| (word.start_ms, word.end_ms, word.id));
        project.blocks.push(Block {
            id: Uuid::new_v4(),
            position: 0,
            speaker_id: None,
            kind: BlockKind::Transcript,
            words,
        });
    }
    project.validate()?;
    Ok(project)
}

fn legacy_cuts(
    legacy: &Value,
    media_id: Uuid,
    duration_ms: i64,
) -> Result<Vec<MediaCut>, ImportError> {
    let Some(edl) = legacy.get("edl") else {
        return Ok(Vec::new());
    };
    let Some(kept) = edl.get("kept").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut kept_intervals = Vec::new();
    for interval in kept {
        let start = seconds_to_ms(interval.get("start"), duration_ms)?;
        let end = seconds_to_ms(interval.get("end"), duration_ms)?;
        if end > start {
            kept_intervals.push((start, end));
        }
    }
    kept_intervals.sort_unstable();
    let mut merged_kept: Vec<(i64, i64)> = Vec::new();
    for interval in kept_intervals {
        if let Some(previous) = merged_kept.last_mut()
            && interval.0 <= previous.1
        {
            previous.1 = previous.1.max(interval.1);
        } else {
            merged_kept.push(interval);
        }
    }
    let mut cuts = Vec::new();
    let mut cursor = 0;
    for (start, end) in merged_kept {
        if start > cursor {
            cuts.push(MediaCut {
                media_id,
                start_ms: cursor,
                end_ms: start,
            });
        }
        cursor = cursor.max(end);
    }
    if duration_ms > cursor {
        cuts.push(MediaCut {
            media_id,
            start_ms: cursor,
            end_ms: duration_ms,
        });
    }
    Ok(cuts)
}

fn seconds_to_ms(value: Option<&Value>, duration_ms: i64) -> Result<i64, ImportError> {
    let seconds = value
        .and_then(value_as_f64)
        .ok_or(ImportError::InvalidLegacyData)?;
    if !seconds.is_finite() {
        return Err(ImportError::InvalidLegacyData);
    }
    let millis = (seconds * 1_000.0).round().clamp(0.0, duration_ms as f64);
    Ok(millis as i64)
}

fn parse_legacy_timestamp(value: &Value) -> Option<i64> {
    let timestamp = value.as_str()?;
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|timestamp| timestamp.timestamp())
}

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("unsupported audio/video format: {0}")]
    UnsupportedFormat(PathBuf),
    #[error("path is not a regular file: {0}")]
    NotAFile(PathBuf),
    #[error("file has no audio stream")]
    NoAudioStream,
    #[error("file has no valid duration")]
    InvalidDuration,
    #[error("audio stream metadata is missing or invalid")]
    InvalidAudioMetadata,
    #[error("ffprobe could not be started: {0}")]
    FfprobeUnavailable(String),
    #[error("ffprobe failed: {0}")]
    FfprobeFailed(String),
    #[error("ffprobe returned invalid metadata")]
    InvalidProbeResponse,
    #[error("filename is not valid UTF-8: {0}")]
    InvalidFilename(PathBuf),
    #[error("invalid ffprobe JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("job failed: {0}")]
    Job(#[from] JobError),
    #[error("legacy projects were not found at {0}")]
    LegacyProjectsNotFound(PathBuf),
    #[error("legacy project file is invalid: {0}")]
    InvalidLegacyProject(PathBuf),
    #[error("legacy project data is malformed")]
    InvalidLegacyData,
    #[error("legacy media {0} is missing from the index")]
    MissingLegacyMedia(String),
    #[error("project library operation failed: {0}")]
    Store(#[from] studio_store::StoreError),
    #[error("project data is invalid: {0}")]
    Core(#[from] studio_core::CoreError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::Duration;
    use studio_core::MediaCut;
    use studio_jobs::{JobManager, JobStatus};
    use studio_store::ProjectLibrary;

    #[test]
    fn parses_audio_and_video_probe_data() {
        let value = json!({
            "format": {"duration": "12.712"},
            "streams": [
                {"codec_type": "video", "width": 1920, "height": 1080, "r_frame_rate": "30000/1001"},
                {"codec_type": "audio", "sample_rate": "48000", "channels": 2, "codec_name": "aac"}
            ]
        });
        let probe = parse_probe_response(&value).unwrap();
        assert_eq!(probe.duration_ms, 12_712);
        assert!(probe.has_audio);
        assert!(probe.has_video);
        assert_eq!((probe.width, probe.height), (1920, 1080));
        assert_eq!((probe.frame_rate_num, probe.frame_rate_den), (30000, 1001));
        assert_eq!(probe.audio_codec.as_deref(), Some("aac"));
    }

    #[test]
    fn rejects_video_without_audio() {
        let value = json!({
            "format": {"duration": "4"},
            "streams": [{"codec_type": "video", "width": 1280, "height": 720}]
        });
        assert!(matches!(
            parse_probe_response(&value),
            Err(ImportError::NoAudioStream)
        ));
    }

    #[test]
    fn sanitizes_localized_filenames() {
        assert_eq!(
            sanitize_filename("episódio 01.final.wav"),
            "epis_dio_01_final"
        );
        assert_eq!(sanitize_filename("..mp4"), "media");
    }

    #[test]
    fn legacy_project_keeps_transcript_metadata_and_exact_cuts() {
        let legacy = json!({
            "id": "00dce485e1b241a781edf0a4f358aa57",
            "name": "Projeto antigo",
            "language": "pt",
            "created_at": "2026-11-03T18:37:53.631073-03:00",
            "updated_at": "2026-11-03T18:41:49.825187-03:00",
            "edl": {"duration": 10.0, "kept": [
                {"start": 0.0, "end": 2.5},
                {"start": 4.5, "end": 10.0}
            ]},
            "transcript": [
                {"id": "8056189b5e9443838ca36f392b4372cd", "text": "Olá", "start": 0.5, "end": 1.0, "speaker": "Pessoa 1"},
                {"id": "a647829a66164db28e37b3f1963180fd", "text": "mundo", "start": 5.0, "end": 5.5, "speaker": "Pessoa 1"}
            ]
        });
        let media = Media {
            id: Uuid::new_v4(),
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
            sha256: Some("digest".into()),
            enhanced_of: None,
        };
        let project = convert_legacy_project(&legacy, media).unwrap();
        assert_eq!(project.title, "Projeto antigo");
        assert_eq!(project.language.as_deref(), Some("pt"));
        assert_eq!(project.media_cuts.len(), 1);
        assert_eq!(
            project.media_cuts[0],
            MediaCut {
                media_id: project.media[0].id,
                start_ms: 2_500,
                end_ms: 4_500
            }
        );
        assert_eq!(project.timeline().unwrap().duration_ms, 8_000);
        assert_eq!(project.blocks[0].words[0].text, "Olá");
        assert_eq!(project.blocks[0].words[0].start_ms, 500);
        assert_eq!(project.speakers[0].name, "Pessoa 1");
    }

    fn create_sample_wav(path: &Path) {
        let output = Command::new("ffmpeg")
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-t",
                "0.5",
                "-c:a",
                "pcm_s16le",
            ])
            .arg(path)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .expect("ffmpeg must be installed for media import tests");
        assert!(
            output.status.success(),
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn imports_media_with_probe_metadata_and_hash() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("sample.wav");
        let project_dir = directory.path().join("project");
        create_sample_wav(&source);
        let manager = JobManager::new();
        let (sender, receiver) = mpsc::channel();
        manager.start("import", move |context| {
            let media = import_media(
                &source,
                &project_dir,
                MediaStorageMode::CopyIntoProject,
                &context,
            )
            .map_err(|error| error.to_string())?;
            sender.send(media.clone()).unwrap();
            Ok(media.path)
        });
        let media = receiver.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(media.duration_ms, 500);
        assert_eq!(media.sample_rate, 48_000);
        assert_eq!(media.kind, MediaKind::Audio);
        assert_eq!(media.sha256.as_ref().map(String::len), Some(64));
        assert!(directory.path().join("project").join(media.path).is_file());
    }

    #[test]
    fn references_original_media_without_copying_it() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("sample.wav");
        let project_dir = directory.path().join("project");
        create_sample_wav(&source);
        let manager = JobManager::new();
        let (sender, receiver) = mpsc::channel();
        let expected_source = source.clone();
        manager.start("reference", move |context| {
            let media = import_media(
                &source,
                &project_dir,
                MediaStorageMode::ReferenceOriginal,
                &context,
            )
            .map_err(|error| error.to_string())?;
            sender.send(media.clone()).unwrap();
            Ok(media.path)
        });
        let media = receiver.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(Path::new(&media.path), expected_source.as_path());
        assert!(!directory.path().join("project/media").exists());
    }

    #[test]
    fn migrates_a_legacy_project_end_to_end_and_is_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let legacy_root = directory.path().join("legacy");
        let source_media = legacy_root.join("media/source.wav");
        let projects = directory.path().join("new-projects");
        fs::create_dir_all(legacy_root.join("projects")).unwrap();
        fs::create_dir_all(source_media.parent().unwrap()).unwrap();
        create_sample_wav(&source_media);
        let project_id = "00dce485e1b241a781edf0a4f358aa57";
        let media_id = "58c5a303ad874bde8e7144fcdec7aee7";
        fs::write(
            legacy_root.join("media.json"),
            serde_json::to_vec(&json!({
                (media_id): {
                    "id": media_id,
                    "path": source_media,
                    "original_filename": "source.wav"
                }
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            legacy_root
                .join("projects")
                .join(format!("{project_id}.json")),
            serde_json::to_vec(&json!({
                "id": project_id,
                "name": "Projeto migrado",
                "language": "pt",
                "created_at": "2026-11-03T18:37:53-03:00",
                "updated_at": "2026-11-03T18:41:49-03:00",
                "media_id": media_id,
                "edl": {"duration": 0.5, "kept": [
                    {"start": 0.0, "end": 0.2},
                    {"start": 0.3, "end": 0.5}
                ]},
                "transcript": [{
                    "id": "8056189b5e9443838ca36f392b4372cd",
                    "text": "Olá",
                    "start": 0.05,
                    "end": 0.15,
                    "speaker": "Pessoa 1"
                }],
                "undo_stack": [],
                "redo_stack": []
            }))
            .unwrap(),
        )
        .unwrap();

        let manager = JobManager::new();
        let (sender, receiver) = mpsc::channel();
        let legacy_path = legacy_root.clone();
        let project_path = projects.clone();
        let job_id = manager.start("migration", move |context| {
            let count = migrate_legacy_projects(legacy_path, project_path, &context)
                .map_err(|error| error.to_string())?;
            sender.send(count).unwrap();
            Ok(count.to_string())
        });
        assert_eq!(receiver.recv_timeout(Duration::from_secs(10)).unwrap(), 1);
        for _ in 0..100 {
            if manager
                .snapshot(job_id)
                .is_some_and(|job| job.status == JobStatus::Completed)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            manager.snapshot(job_id).unwrap().status,
            JobStatus::Completed
        );
        let library = ProjectLibrary::new(&projects).unwrap();
        let [summary] = library.list().unwrap().try_into().unwrap();
        assert_eq!(summary.id, Uuid::parse_str(project_id).unwrap());
        assert_eq!(summary.title, "Projeto migrado");
        assert_eq!(summary.duration_ms, 400);
        let editor = library.open(summary.id).unwrap().load().unwrap();
        assert_eq!(editor.project.language.as_deref(), Some("pt"));
        assert_eq!(editor.project.blocks[0].words[0].text, "Olá");

        let (sender, receiver) = mpsc::channel();
        let job_id = manager.start("migration-repeat", move |context| {
            let count = migrate_legacy_projects(legacy_root, projects, &context)
                .map_err(|error| error.to_string())?;
            sender.send(count).unwrap();
            Ok(count.to_string())
        });
        assert_eq!(receiver.recv_timeout(Duration::from_secs(10)).unwrap(), 0);
        for _ in 0..100 {
            if manager
                .snapshot(job_id)
                .is_some_and(|job| job.status == JobStatus::Completed)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            manager.snapshot(job_id).unwrap().status,
            JobStatus::Completed
        );
        assert_eq!(library.list().unwrap().len(), 1);
    }
}
