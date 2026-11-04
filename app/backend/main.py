from __future__ import annotations

import hashlib
import json
import mimetypes
import re
import shutil
import subprocess
import threading
import time
import uuid
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime
from pathlib import Path
from typing import Any, Callable

from fastapi import FastAPI, File, Header, HTTPException, UploadFile
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import FileResponse, StreamingResponse
from pydantic import BaseModel, Field, model_validator
from starlette.concurrency import run_in_threadpool

from edl import EditDecisionList
from enhance import EnhancementParams, PRESETS, build_filter_chain
from media import probe, waveform
from transcription import model_device_configuration, transcribe_remote


BASE_DIR = Path(__file__).resolve().parent
DATA_DIR = BASE_DIR / "data"
MEDIA_DIR = DATA_DIR / "media"
PROJECT_DIR = DATA_DIR / "projects"
MEDIA_INDEX = DATA_DIR / "media.json"
for directory in (MEDIA_DIR, PROJECT_DIR):
    directory.mkdir(parents=True, exist_ok=True)

app = FastAPI(title="Podcast Studio", version="0.1.0")
app.add_middleware(
    CORSMiddleware,
    allow_origins=["http://localhost:5173", "http://127.0.0.1:5173"],
    allow_methods=["*"],
    allow_headers=["*"],
)

executor = ThreadPoolExecutor(max_workers=2, thread_name_prefix="podcast-job")
jobs: dict[str, dict[str, Any]] = {}
job_processes: dict[str, subprocess.Popen[str]] = {}
job_cancel_events: dict[str, threading.Event] = {}
job_lock = threading.Lock()


class CutRequest(BaseModel):
    start: float = Field(ge=0)
    end: float = Field(ge=0)

    @model_validator(mode="after")
    def validate_order(self) -> CutRequest:
        if self.end <= self.start:
            raise ValueError("end must be greater than start")
        return self


class PasteRequest(BaseModel):
    intervals: list[CutRequest] = Field(min_length=1)
    target: float = Field(ge=0)


class TextCorrection(BaseModel):
    text: str = Field(min_length=1, max_length=500)


class TopicMarker(BaseModel):
    id: str = Field(min_length=1, max_length=64)
    title: str = Field(min_length=1, max_length=120)
    time: float = Field(ge=0)

    @model_validator(mode="after")
    def validate_title(self) -> TopicMarker:
        self.title = self.title.strip()
        if not self.title:
            raise ValueError("Topic title cannot be empty")
        return self


class TopicMarkersRequest(BaseModel):
    topic_markers: list[TopicMarker] = Field(max_length=200)

    @model_validator(mode="after")
    def validate_unique_markers(self) -> TopicMarkersRequest:
        ids = [marker.id for marker in self.topic_markers]
        if len(ids) != len(set(ids)):
            raise ValueError("Topic marker IDs must be unique")
        times = sorted(marker.time for marker in self.topic_markers)
        if any(right - left < 0.01 for left, right in zip(times, times[1:])):
            raise ValueError("Topic markers must be at least 0.01 seconds apart")
        return self


class RenameProject(BaseModel):
    name: str = Field(min_length=1, max_length=120)

    @model_validator(mode="after")
    def validate_name(self) -> RenameProject:
        self.name = self.name.strip()
        if not self.name:
            raise ValueError("Project name cannot be empty")
        return self


class ExportRequest(BaseModel):
    format: str = Field(default="mp4", pattern="^(mp4|webm|wav|mp3|m4a|aac|raw)$")
    quality: int = Field(default=23, ge=0, le=51)
    enhancement: EnhancementParams | None = None


class PreviewRequest(BaseModel):
    params: EnhancementParams
    seconds: float = Field(default=30, gt=0)


class TranscriptionOptions(BaseModel):
    provider: str = Field(default="local", pattern="^(local|deepgram|assemblyai)$")
    model: str = Field(default="small", pattern="^(tiny|base|small|medium)$")
    api_key: str | None = Field(default=None, min_length=1, max_length=512)


class PresetRequest(BaseModel):
    name: str = Field(min_length=1, max_length=32)
    params: EnhancementParams

    @model_validator(mode="after")
    def validate_name(self) -> PresetRequest:
        self.name = self.name.strip()
        if not self.name or self.name in PRESETS:
            raise ValueError("Choose a non-empty name that is not a built-in preset")
        return self


class SequenceMediaRequest(BaseModel):
    media_id: str = Field(min_length=1, max_length=64)


class SequenceOrderRequest(BaseModel):
    project_ids: list[str] = Field(min_length=1, max_length=100)


class JobCancelled(Exception):
    pass


def _read_json(path: Path, default: Any) -> Any:
    if not path.exists():
        return default
    return json.loads(path.read_text(encoding="utf-8"))


def _write_json(path: Path, data: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8")
    temporary.replace(path)


def _media_registry() -> dict[str, dict[str, Any]]:
    return _read_json(MEDIA_INDEX, {})


def _get_media(media_id: str) -> dict[str, Any]:
    media = _media_registry().get(media_id)
    if not media:
        raise HTTPException(status_code=404, detail="Media not found")
    return media


def _project_path(project_id: str) -> Path:
    if not re.fullmatch(r"[a-f0-9]{32}", project_id):
        raise HTTPException(status_code=404, detail="Project not found")
    return PROJECT_DIR / f"{project_id}.json"


def _get_project(project_id: str) -> dict[str, Any]:
    path = _project_path(project_id)
    if not path.exists():
        raise HTTPException(status_code=404, detail="Project not found")
    return _read_json(path, {})


def _save_project(project: dict[str, Any]) -> None:
    project["updated_at"] = datetime.now().astimezone().isoformat()
    _write_json(_project_path(project["id"]), project)


def _start_job(kind: str, operation: Callable[[str], Any]) -> dict[str, Any]:
    job_id = uuid.uuid4().hex
    with job_lock:
        jobs[job_id] = {"id": job_id, "kind": kind, "status": "queued", "progress": 0.0}
        job_cancel_events[job_id] = threading.Event()

    def run() -> None:
        with job_lock:
            if job_cancel_events[job_id].is_set():
                jobs[job_id].update(status="cancelled", message="Operation cancelled")
                return
            jobs[job_id]["status"] = "running"
        try:
            result = operation(job_id)
            with job_lock:
                jobs[job_id].update(status="completed", progress=1.0, result=result)
        except JobCancelled:
            with job_lock:
                jobs[job_id].update(status="cancelled", message="Operation cancelled")
        except Exception as error:
            with job_lock:
                jobs[job_id].update(status="failed", error=str(error))
        finally:
            with job_lock:
                job_processes.pop(job_id, None)
                job_cancel_events.pop(job_id, None)

    executor.submit(run)
    return jobs[job_id].copy()


def _get_job(job_id: str) -> dict[str, Any]:
    with job_lock:
        job = jobs.get(job_id)
        if not job:
            raise HTTPException(status_code=404, detail="Job not found")
        return job.copy()


def _set_progress(job_id: str, progress: float, message: str) -> None:
    with job_lock:
        if job_id in jobs:
            jobs[job_id].update(progress=max(0.0, min(progress, 0.99)), message=message)


def _cancel_requested(job_id: str) -> bool:
    with job_lock:
        event = job_cancel_events.get(job_id)
        return event.is_set() if event else False


def _run_ffmpeg_job(job_id: str, command: list[str], duration: float) -> None:
    if _cancel_requested(job_id):
        raise JobCancelled
    process_command = command[:-1] + ["-progress", "pipe:1", "-nostats", command[-1]]
    process = subprocess.Popen(
        process_command,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
    )
    with job_lock:
        job_processes[job_id] = process
    started = time.monotonic()
    assert process.stdout is not None
    for line in process.stdout:
        if _cancel_requested(job_id):
            process.terminate()
            break
        if line.startswith("out_time_ms="):
            try:
                elapsed = int(line.partition("=")[2]) / 1_000_000
            except ValueError:
                continue
            _set_progress(job_id, elapsed / max(duration, 0.1), "Rendering edited media")
        if time.monotonic() - started > 3600:
            process.kill()
            process.wait()
            raise TimeoutError("FFmpeg exceeded the one-hour render limit")
    stderr = process.stderr.read() if process.stderr else ""
    return_code = process.wait()
    with job_lock:
        job_processes.pop(job_id, None)
    if _cancel_requested(job_id):
        raise JobCancelled
    if return_code:
        raise RuntimeError(stderr[-3000:] or "FFmpeg render failed")


def _range_response(path: Path, range_header: str | None, content_type: str) -> StreamingResponse:
    size = path.stat().st_size
    start, end = 0, size - 1
    status_code = 200
    headers = {"Accept-Ranges": "bytes"}
    if range_header:
        match = re.fullmatch(r"bytes=(\d*)-(\d*)", range_header.strip())
        if not match or (not match.group(1) and not match.group(2)):
            raise HTTPException(status_code=416, detail="Invalid byte range", headers={"Content-Range": f"bytes */{size}"})
        if match.group(1):
            start = int(match.group(1))
            end = int(match.group(2)) if match.group(2) else size - 1
        else:
            suffix = int(match.group(2))
            start = max(0, size - suffix)
        if start >= size or end < start:
            raise HTTPException(status_code=416, detail="Range not satisfiable", headers={"Content-Range": f"bytes */{size}"})
        end = min(end, size - 1)
        status_code = 206
        headers["Content-Range"] = f"bytes {start}-{end}/{size}"
    headers["Content-Length"] = str(end - start + 1)

    def chunks():
        with path.open("rb") as stream:
            stream.seek(start)
            remaining = end - start + 1
            while remaining:
                data = stream.read(min(1024 * 1024, remaining))
                if not data:
                    break
                remaining -= len(data)
                yield data

    return StreamingResponse(chunks(), status_code=status_code, media_type=content_type, headers=headers)


@app.get("/api/health")
def health() -> dict[str, object]:
    return {"ok": True, "transcription_available": _transcription_available()}


def _transcription_available() -> bool:
    try:
        import faster_whisper  # noqa: F401
    except ImportError:
        return False
    return True


@app.post("/api/media/import")
async def import_media(file: UploadFile = File(...)) -> dict[str, Any]:
    original_name = Path(file.filename or "recording").name
    extension = Path(original_name).suffix.lower()
    if extension not in {".mp3", ".wav", ".m4a", ".aac", ".ogg", ".flac", ".mp4", ".mov", ".mkv", ".webm"}:
        raise HTTPException(status_code=415, detail="Unsupported media type")
    media_id = uuid.uuid4().hex
    destination = MEDIA_DIR / f"{media_id}{extension}"
    digest = hashlib.sha256()
    try:
        with destination.open("wb") as target:
            while chunk := await file.read(1024 * 1024):
                digest.update(chunk)
                target.write(chunk)
        metadata = await run_in_threadpool(probe, destination)
    except Exception as error:
        destination.unlink(missing_ok=True)
        raise HTTPException(status_code=422, detail=f"Could not read media: {error}") from error
    if metadata["duration"] <= 0 or not metadata["has_audio"]:
        destination.unlink(missing_ok=True)
        raise HTTPException(status_code=422, detail="The file must contain an audio track")
    entry = {
        "id": media_id,
        "original_filename": original_name,
        "path": str(destination),
        "sha256": digest.hexdigest(),
        **metadata,
    }
    registry = _media_registry()
    registry[media_id] = entry
    _write_json(MEDIA_INDEX, registry)
    return {key: value for key, value in entry.items() if key != "path"}


@app.get("/api/media")
def list_media() -> list[dict[str, Any]]:
    return [{key: value for key, value in item.items() if key != "path"} for item in _media_registry().values()]


@app.get("/api/media/{media_id}")
def media_details(media_id: str) -> dict[str, Any]:
    return {key: value for key, value in _get_media(media_id).items() if key != "path"}


@app.get("/api/media/{media_id}/stream")
def stream_media(media_id: str, range_header: str | None = Header(default=None, alias="Range")) -> StreamingResponse:
    media = _get_media(media_id)
    path = Path(media["path"])
    return _range_response(path, range_header, media["mime_type"])


@app.get("/api/media/{media_id}/peaks")
def media_peaks(media_id: str, rate: int = 100) -> dict[str, Any]:
    if rate < 10 or rate > 500:
        raise HTTPException(status_code=422, detail="Rate must be between 10 and 500")
    media = _get_media(media_id)
    try:
        buckets = waveform(Path(media["path"]), rate, media["duration"])
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        raise HTTPException(status_code=422, detail=f"Waveform extraction failed: {error}") from error
    return {"rate": rate, "duration": media["duration"], "buckets": buckets}


@app.post("/api/projects")
def create_project(payload: dict[str, str]) -> dict[str, Any]:
    media = _get_media(payload.get("media_id", ""))
    project_id = uuid.uuid4().hex
    edl = EditDecisionList.full(media["duration"])
    project = {
        "id": project_id,
        "name": Path(media["original_filename"]).stem,
        "media_id": media["id"],
        "transcript": [],
        "topic_markers": [],
        "edl": edl.to_dict(),
        "undo_stack": [],
        "redo_stack": [],
        "enhancement": EnhancementParams().model_dump(),
        "export_options": {"format": "mp4", "quality": 23},
        "created_at": datetime.now().astimezone().isoformat(),
    }
    _save_project(project)
    return project


@app.get("/api/projects")
def list_projects() -> list[dict[str, Any]]:
    projects = [_read_json(path, {}) for path in PROJECT_DIR.glob("*.json")]
    sequence_children = {child for item in projects for child in item.get("media_sequence", []) if child != item.get("id")}
    return [{"id": item["id"], "name": item["name"], "media_id": item["media_id"], "updated_at": item.get("updated_at")} for item in projects if item.get("id") not in sequence_children]


@app.get("/api/projects/{project_id}")
def get_project(project_id: str) -> dict[str, Any]:
    return _get_project(project_id)


@app.patch("/api/projects/{project_id}")
def rename_project(project_id: str, request: RenameProject) -> dict[str, Any]:
    project = _get_project(project_id)
    project["name"] = request.name
    _save_project(project)
    return project


def _media_sequence(project: dict[str, Any]) -> list[dict[str, Any]]:
    project_ids = project.get("media_sequence") or [project["id"]]
    items = []
    for item_id in project_ids:
        item_project = _get_project(item_id)
        media = _get_media(item_project["media_id"])
        items.append({
            "project_id": item_project["id"],
            "media_id": media["id"],
            "name": media["original_filename"],
            "duration": media["duration"],
            "has_video": media["has_video"],
        })
    return items


@app.get("/api/projects/{project_id}/media-sequence")
def get_media_sequence(project_id: str) -> list[dict[str, Any]]:
    return _media_sequence(_get_project(project_id))


@app.post("/api/projects/{project_id}/media-sequence")
def add_media_sequence_item(project_id: str, request: SequenceMediaRequest) -> list[dict[str, Any]]:
    project = _get_project(project_id)
    _get_media(request.media_id)
    if not project.get("media_sequence"):
        project["media_sequence"] = [project["id"]]
    item = create_project({"media_id": request.media_id})
    project["media_sequence"].append(item["id"])
    _save_project(project)
    return _media_sequence(project)


@app.put("/api/projects/{project_id}/media-sequence")
def reorder_media_sequence(project_id: str, request: SequenceOrderRequest) -> list[dict[str, Any]]:
    project = _get_project(project_id)
    current_ids = project.get("media_sequence") or [project["id"]]
    if len(request.project_ids) != len(set(request.project_ids)) or set(request.project_ids) != set(current_ids):
        raise HTTPException(status_code=422, detail="Sequence order must contain each project exactly once")
    for item_id in request.project_ids:
        _get_project(item_id)
    project["media_sequence"] = request.project_ids
    _save_project(project)
    return _media_sequence(project)


def _commit_cut(project: dict[str, Any], start: float, end: float) -> None:
    edl = EditDecisionList.from_dict(project["edl"])
    before = edl.to_dict()
    media = _get_media(project["media_id"])
    edl.delete(start, end, media["fps"] if media["has_video"] else None)
    if edl.kept != EditDecisionList.from_dict(before).kept:
        project["undo_stack"].append(before)
        project["redo_stack"] = []
        project["edl"] = edl.to_dict()


@app.post("/api/projects/{project_id}/cuts")
def cut_project(project_id: str, request: CutRequest) -> dict[str, Any]:
    project = _get_project(project_id)
    _commit_cut(project, request.start, request.end)
    _save_project(project)
    return project


@app.post("/api/projects/{project_id}/paste")
def paste_project(project_id: str, request: PasteRequest) -> dict[str, Any]:
    project = _get_project(project_id)
    edl = EditDecisionList.from_dict(project["edl"])
    before = edl.to_dict()
    intervals = [(item.start, item.end) for item in request.intervals]
    if request.target > edl.duration:
        raise HTTPException(status_code=422, detail="O ponto de inserção está fora da mídia.")
    for start, end in intervals:
        if end > edl.duration:
            raise HTTPException(status_code=422, detail="O trecho copiado está fora da mídia.")
    try:
        edl.insert(intervals, request.target)
    except ValueError as reason:
        raise HTTPException(status_code=422, detail=str(reason)) from reason
    if edl.kept != EditDecisionList.from_dict(before).kept:
        project["undo_stack"].append(before)
        project["redo_stack"] = []
        project["edl"] = edl.to_dict()
    _save_project(project)
    return project


@app.post("/api/projects/{project_id}/delete-words")
def delete_words(project_id: str, payload: dict[str, list[str]]) -> dict[str, Any]:
    project = _get_project(project_id)
    selected = set(payload.get("word_ids", []))
    transcript = project["transcript"]
    indexed = [(index, word) for index, word in enumerate(transcript) if word["id"] in selected]
    words = [word for _, word in indexed]
    if not words:
        raise HTTPException(status_code=404, detail="No matching transcript words")
    edl = EditDecisionList.from_dict(project["edl"])
    before = edl.to_dict()
    transcript_indexes = {word["id"]: index for index, word in enumerate(transcript)}
    groups: list[list[dict[str, Any]]] = []
    previous_index = -2
    for index, word in indexed:
        if index > previous_index + 1:
            groups.append([word])
        else:
            groups[-1].append(word)
        previous_index = index
    for group in groups:
        first_index = min(transcript_indexes[word["id"]] for word in group)
        last_index = max(transcript_indexes[word["id"]] for word in group)

        start = min(word["start"] for word in group)
        end = max(word["end"] for word in group)
        if first_index > 0:
            start = max(start, transcript[first_index - 1]["end"])
        if last_index + 1 < len(transcript):
            end = min(end, transcript[last_index + 1]["start"])

        # Keep neighboring words intact if the recognizer returned overlapping
        # timestamps. Refuse an ambiguous cut instead of removing either neighbor.
        if end <= start:
            raise HTTPException(
                status_code=422,
                detail="Os tempos desta palavra se sobrepõem aos das palavras vizinhas. O corte foi interrompido para preservá-las.",
            )
        edl.delete(start, end)
    if edl.kept != EditDecisionList.from_dict(before).kept:
        project["undo_stack"].append(before)
        project["redo_stack"] = []
        project["edl"] = edl.to_dict()
    _save_project(project)
    return project


@app.patch("/api/projects/{project_id}/words/{word_id}")
def correct_word(project_id: str, word_id: str, request: TextCorrection) -> dict[str, Any]:
    project = _get_project(project_id)
    word = next((item for item in project["transcript"] if item["id"] == word_id), None)
    if not word:
        raise HTTPException(status_code=404, detail="Transcript word not found")
    word["text"] = request.text
    _save_project(project)
    return project


@app.put("/api/projects/{project_id}/topic-markers")
def save_topic_markers(project_id: str, request: TopicMarkersRequest) -> dict[str, Any]:
    project = _get_project(project_id)
    duration = float(project["edl"]["duration"])
    if any(marker.time > duration for marker in request.topic_markers):
        raise HTTPException(status_code=422, detail="Topic marker time exceeds media duration")
    project["topic_markers"] = [marker.model_dump() for marker in request.topic_markers]
    _save_project(project)
    return project


@app.post("/api/projects/{project_id}/undo")
def undo_project(project_id: str) -> dict[str, Any]:
    project = _get_project(project_id)
    if project["undo_stack"]:
        project["redo_stack"].append(project["edl"])
        project["edl"] = project["undo_stack"].pop()
    _save_project(project)
    return project


@app.post("/api/projects/{project_id}/redo")
def redo_project(project_id: str) -> dict[str, Any]:
    project = _get_project(project_id)
    if project["redo_stack"]:
        project["undo_stack"].append(project["edl"])
        project["edl"] = project["redo_stack"].pop()
    _save_project(project)
    return project


@app.post("/api/projects/{project_id}/transcribe")
def transcribe_project(
    project_id: str,
    options: TranscriptionOptions = TranscriptionOptions(),
    model: str | None = None,
) -> dict[str, Any]:
    project = _get_project(project_id)
    media = _get_media(project["media_id"])
    selected_model = model or options.model
    if selected_model not in {"tiny", "base", "small", "medium"}:
        raise HTTPException(status_code=422, detail="Model must be tiny, base, small, or medium")
    if options.provider != "local" and not options.api_key:
        raise HTTPException(status_code=422, detail="Informe a chave de API do provedor selecionado.")
    if not media["has_audio"]:
        raise HTTPException(status_code=422, detail="This media has no audio track")

    def operation(job_id: str) -> dict[str, Any]:
        if options.provider == "local":
            try:
                from faster_whisper import WhisperModel
            except ImportError as error:
                raise RuntimeError("Local transcription is not installed. Run `pip install -e '.[transcription]'` in app/backend.") from error
            device, compute_type = model_device_configuration()
            _set_progress(job_id, 0.05, f"Loading {selected_model} speech model on {device}")
            model_runner = WhisperModel(selected_model, device=device, compute_type=compute_type)
            if _cancel_requested(job_id):
                raise JobCancelled
            _set_progress(job_id, 0.25, "Transcribing audio")
            segments, info = model_runner.transcribe(media["path"], word_timestamps=True, vad_filter=True)
            words: list[dict[str, Any]] = []
            for segment in segments:
                if _cancel_requested(job_id):
                    raise JobCancelled
                for word in segment.words or []:
                    value = word.word.strip()
                    if value:
                        words.append({
                            "id": uuid.uuid4().hex,
                            "text": value,
                            "start": round(float(word.start), 3),
                            "end": round(float(word.end), 3),
                            "speaker": "Speaker 1",
                        })
            language = info.language
        else:
            assert options.api_key is not None
            try:
                words, language = transcribe_remote(
                    media["path"],
                    float(media["duration"]),
                    options.provider,
                    options.api_key,
                    lambda progress, message: _set_progress(job_id, progress, message),
                    lambda: _cancel_requested(job_id),
                )
            except InterruptedError as error:
                raise JobCancelled from error
        if _cancel_requested(job_id):
            raise JobCancelled
        if not words:
            raise RuntimeError("The transcription model returned no words.")
        current = _get_project(project_id)
        current["transcript"] = words
        if language:
            current["language"] = language
        _save_project(current)
        _set_progress(job_id, 0.95, f"Detected language: {language}" if language else "Transcription completed")
        return {"word_count": len(words), "language": language or "", "project_id": project_id}

    return _start_job("transcription", operation)


@app.get("/api/jobs/{job_id}")
def job_status(job_id: str) -> dict[str, Any]:
    return _get_job(job_id)


@app.post("/api/jobs/{job_id}/cancel")
def cancel_job(job_id: str) -> dict[str, Any]:
    with job_lock:
        job = jobs.get(job_id)
        if not job:
            raise HTTPException(status_code=404, detail="Job not found")
        if job["status"] in {"completed", "failed", "cancelled"}:
            raise HTTPException(status_code=409, detail="Job is already finished")
        event = job_cancel_events.get(job_id)
        if event is None:
            raise HTTPException(status_code=409, detail="Job can no longer be cancelled")
        event.set()
        job.update(status="cancelling", message="Cancelling operation")
        process = job_processes.get(job_id)
    if process and process.poll() is None:
        process.terminate()
    return _get_job(job_id)


@app.get("/api/enhance/presets")
def enhancement_presets() -> dict[str, Any]:
    defaults = EnhancementParams().model_dump()
    custom = _read_json(DATA_DIR / "presets.json", {})
    return {
        **{name: {**defaults, **preset} for name, preset in PRESETS.items()},
        **{name: EnhancementParams(**params).model_dump() for name, params in custom.items()},
    }


@app.post("/api/enhance/presets")
def save_enhancement_preset(request: PresetRequest) -> dict[str, Any]:
    custom = _read_json(DATA_DIR / "presets.json", {})
    custom[request.name] = request.params.model_dump()
    _write_json(DATA_DIR / "presets.json", custom)
    return enhancement_presets()


@app.put("/api/projects/{project_id}/enhancement")
def save_enhancement(project_id: str, params: EnhancementParams) -> dict[str, Any]:
    project = _get_project(project_id)
    project["enhancement"] = params.model_dump()
    _save_project(project)
    return project


@app.post("/api/projects/{project_id}/enhance-preview")
def enhance_preview(project_id: str, request: PreviewRequest) -> dict[str, Any]:
    project = _get_project(project_id)
    media = _get_media(project["media_id"])
    cache_key = f"{request.params.model_dump_json()}:{request.seconds:.6f}"
    digest = hashlib.sha256(cache_key.encode()).hexdigest()[:16]
    destination = DATA_DIR / "previews" / f"{project_id}-{digest}.m4a"
    if destination.exists():
        return {"status": "completed", "url": f"/api/previews/{project_id}-{digest}.m4a", "cached": True}

    def operation(job_id: str) -> dict[str, str]:
        destination.parent.mkdir(parents=True, exist_ok=True)
        command = [
            "ffmpeg", "-y", "-v", "error", "-i", media["path"], "-t",
            str(min(request.seconds, media["duration"])), "-vn", "-af",
            build_filter_chain(request.params), "-c:a", "aac", "-b:a", "128k",
            "-movflags", "+faststart", str(destination),
        ]
        _set_progress(job_id, 0.05, "Applying audio filters")
        _run_ffmpeg_job(job_id, command, min(request.seconds, media["duration"]))
        return {"url": f"/api/previews/{destination.name}", "cached": False}

    return _start_job("enhancement", operation)


@app.get("/api/previews/{filename}")
def get_preview(filename: str) -> FileResponse:
    if not re.fullmatch(r"[a-f0-9]{32}-[a-f0-9]{16}\.(?:m4a|wav)", filename):
        raise HTTPException(status_code=404, detail="Preview not found")
    path = DATA_DIR / "previews" / filename
    if not path.exists():
        raise HTTPException(status_code=404, detail="Preview not found")
    media_type = "audio/mp4" if path.suffix == ".m4a" else "audio/wav"
    return FileResponse(path, media_type=media_type)


def _export_command(media: dict[str, Any], project: dict[str, Any], request: ExportRequest, output: Path) -> list[str]:
    edl = EditDecisionList.from_dict(project["edl"])
    intervals = edl.kept
    if not intervals:
        raise ValueError("Nothing remains to export")
    has_video = media["has_video"] and request.format in {"mp4", "webm", "raw"}
    filters: list[str] = []
    labels: list[str] = []
    for index, (start, end) in enumerate(intervals):
        if has_video:
            filters.append(f"[0:v]trim=start={start:.6f}:end={end:.6f},setpts=PTS-STARTPTS[v{index}]")
        filters.append(f"[0:a]atrim=start={start:.6f}:end={end:.6f},asetpts=PTS-STARTPTS[a{index}]")
        labels.extend(([f"[v{index}]"] if has_video else []) + [f"[a{index}]"])
    filters.append(f"{''.join(labels)}concat=n={len(intervals)}:v={1 if has_video else 0}:a=1[video_out][audio_out]" if has_video else f"{''.join(labels)}concat=n={len(intervals)}:v=0:a=1[audio_out]")
    audio_label = "audio_out"
    if request.enhancement and request.format != "raw":
        filters.append(f"[audio_out]{build_filter_chain(request.enhancement)}[enhanced]")
        audio_label = "enhanced"
    command = ["ffmpeg", "-y", "-v", "error", "-i", media["path"], "-filter_complex", ";".join(filters)]
    if has_video:
        command.extend(["-map", "[video_out]"])
        if request.format == "raw":
            command.extend(["-c:v", "ffv1", "-level", "3"])
        elif request.format == "mp4":
            command.extend(["-c:v", "libx264"])
            command.extend(["-crf", str(request.quality), "-preset", "veryfast"])
        else:
            command.extend(["-c:v", "libvpx-vp9"])
            command.extend(["-crf", str(request.quality), "-b:v", "0"])
    command.extend(["-map", f"[{audio_label}]"])
    if request.format == "raw":
        command.extend(["-c:a", "flac"])
    elif request.format == "wav":
        command.extend(["-c:a", "pcm_s16le"])
    elif request.format == "mp3":
        command.extend(["-c:a", "libmp3lame", "-q:a", "2"])
    elif request.format == "aac":
        command.extend(["-c:a", "aac", "-b:a", "192k"])
    elif request.format == "m4a":
        command.extend(["-c:a", "aac", "-b:a", "192k"])
    elif request.format == "mp4":
        command.extend(["-c:a", "aac", "-b:a", "192k", "-movflags", "+faststart"])
    else:
        command.extend(["-c:a", "libopus", "-b:a", "160k"])
    command.append(str(output))
    return command


@app.post("/api/projects/{project_id}/export")
def export_project(project_id: str, request: ExportRequest) -> dict[str, Any]:
    project = _get_project(project_id)
    media = _get_media(project["media_id"])
    if request.format == "raw" and request.enhancement is not None:
        request = request.model_copy(update={"enhancement": None})
    edl = EditDecisionList.from_dict(project["edl"])
    raw_full_source = (
        request.format == "raw"
        and request.enhancement is None
        and len(edl.kept) == 1
        and abs(edl.kept[0][0]) <= 1e-9
        and abs(edl.kept[0][1] - edl.duration) <= 1e-6
    )
    if request.format == "raw":
        extension = Path(media["path"]).suffix.lower() if raw_full_source else (".mkv" if media["has_video"] else ".flac")
    else:
        extension = f".{request.format}"
    output = PROJECT_DIR / project_id / "exports" / f"edited-{uuid.uuid4().hex[:8]}{extension}"
    project["export_options"] = request.model_dump()
    if request.enhancement:
        project["enhancement"] = request.enhancement.model_dump()
    _save_project(project)

    def operation(job_id: str) -> dict[str, str]:
        output.parent.mkdir(parents=True, exist_ok=True)
        if raw_full_source:
            _set_progress(job_id, 0.9, "Copying original media without re-encoding")
            shutil.copyfile(media["path"], output)
            return {"filename": output.name, "url": f"/api/projects/{project_id}/exports/{output.name}"}
        command = _export_command(media, project, request, output)
        duration = EditDecisionList.from_dict(project["edl"]).edited_duration()
        _set_progress(job_id, 0.05, "Rendering edited media")
        _run_ffmpeg_job(job_id, command, duration)
        return {"filename": output.name, "url": f"/api/projects/{project_id}/exports/{output.name}"}

    return _start_job("export", operation)


@app.get("/api/projects/{project_id}/exports/{filename}")
def download_export(project_id: str, filename: str) -> FileResponse:
    if not re.fullmatch(r"edited-[a-f0-9]{8}\.[a-z0-9]{1,8}", filename):
        raise HTTPException(status_code=404, detail="Export not found")
    path = _project_path(project_id).parent / project_id / "exports" / filename
    if not path.exists():
        raise HTTPException(status_code=404, detail="Export not found")
    return FileResponse(path, filename=filename, media_type=mimetypes.guess_type(filename)[0])


@app.delete("/api/projects/{project_id}")
def delete_project(project_id: str) -> dict[str, bool]:
    path = _project_path(project_id)
    if not path.exists():
        raise HTTPException(status_code=404, detail="Project not found")
    project = _read_json(path, {})
    for child_id in project.get("media_sequence", []):
        if child_id != project_id:
            _project_path(child_id).unlink(missing_ok=True)
    path.unlink()
    return {"deleted": True}
