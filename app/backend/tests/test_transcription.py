import json
from pathlib import Path

from fastapi.testclient import TestClient

import main
import transcription
from transcription import model_device_configuration


def test_cpu_configuration_is_available_without_cuda() -> None:
    assert model_device_configuration(False) == ("cpu", "int8")
    assert model_device_configuration(True) == ("cuda", "int8_float16")


def test_groq_word_timestamps_are_normalized() -> None:
    words, language = transcription._provider_words(
        "groq",
        {
            "language": "pt",
            "words": [
                {"word": " Olá,", "start": 0.1, "end": 0.4},
                {"word": "", "start": 0.5, "end": 0.6},
            ],
        },
    )
    assert words == [("Olá,", 0.1, 0.4)]
    assert language == "pt"


def test_deepgram_word_timestamps_are_normalized() -> None:
    words, language = transcription._provider_words(
        "deepgram",
        {
            "results": {
                "channels": [{
                    "detected_language": "pt-BR",
                    "alternatives": [{
                        "words": [
                            {"word": "ola", "punctuated_word": "Olá,", "start": 0.1, "end": 0.4},
                        ],
                    }],
                }],
            },
        },
    )
    assert words == [("Olá,", 0.1, 0.4)]
    assert language == "pt-BR"


def test_provider_requests_authenticate_without_logging_keys(monkeypatch, tmp_path: Path) -> None:
    audio_path = tmp_path / "chunk.mp3"
    audio_path.write_bytes(b"sample audio")
    requests = []

    class Response:
        def __enter__(self):
            return self

        def __exit__(self, *_args) -> None:
            return None

        def read(self) -> bytes:
            return b'{"words":[]}'

    def fake_urlopen(request, timeout):
        requests.append((request, timeout))
        return Response()

    monkeypatch.setattr(transcription, "urlopen", fake_urlopen)
    transcription._provider_request("groq", "test-groq-key", audio_path)
    transcription._provider_request("deepgram", "test-deepgram-key", audio_path)

    assert requests[0][0].get_header("Authorization") == "Bearer test-groq-key"
    assert requests[1][0].get_header("Authorization") == "Token test-deepgram-key"
    assert requests[0][1] == requests[1][1] == 300


def test_remote_transcription_offsets_chunks(monkeypatch, tmp_path: Path) -> None:
    def make_audio(command, **_kwargs) -> None:
        Path(command[-1]).write_bytes(b"audio")

    monkeypatch.setattr(transcription.subprocess, "run", make_audio)
    monkeypatch.setattr(
        transcription,
        "_provider_request",
        lambda _provider, _key, _path: {"language": "pt", "words": [{"word": "oi", "start": 0.1, "end": 0.3}]},
    )
    progress: list[tuple[float, str]] = []
    words, language = transcription.transcribe_remote(
        tmp_path / "media.mp4",
        601,
        "groq",
        "test-key",
        lambda value, message: progress.append((value, message)),
        lambda: False,
    )
    assert [(word["text"], word["start"], word["end"]) for word in words] == [
        ("oi", 0.1, 0.3),
        ("oi", 600.1, 600.3),
    ]
    assert language == "pt"
    assert progress[-1][0] == 0.9


def test_remote_key_is_required_and_not_saved_in_project(monkeypatch, tmp_path: Path) -> None:
    data_dir = tmp_path / "data"
    monkeypatch.setattr(main, "PROJECT_DIR", data_dir / "projects")
    monkeypatch.setattr(main, "MEDIA_INDEX", data_dir / "media.json")
    main.PROJECT_DIR.mkdir(parents=True)

    project_id = "a" * 32
    media_id = "b" * 32
    media = {
        "id": media_id,
        "path": str(tmp_path / "media.mp4"),
        "duration": 1,
        "has_audio": True,
    }
    main.MEDIA_INDEX.write_text(json.dumps({media_id: media}), encoding="utf-8")
    project_path = main.PROJECT_DIR / f"{project_id}.json"
    project_path.write_text(json.dumps({
        "id": project_id,
        "media_id": media_id,
        "transcript": [],
    }), encoding="utf-8")
    client = TestClient(main.app)

    missing_key = client.post(
        f"/api/projects/{project_id}/transcribe",
        json={"provider": "groq"},
    )
    assert missing_key.status_code == 422

    secret = "sensitive-test-token"

    def transcribe_stub(_path, _duration, _provider, api_key, _progress, _cancelled):
        assert api_key == secret
        return [{"id": "word-id", "text": "Olá", "start": 0, "end": 0.5, "speaker": "Speaker 1"}], "pt"

    def start_job_stub(_kind, operation):
        result = operation("job-id")
        return {"id": "job-id", "status": "completed", "result": result}

    monkeypatch.setattr(main, "transcribe_remote", transcribe_stub)
    monkeypatch.setattr(main, "_start_job", start_job_stub)
    response = client.post(
        f"/api/projects/{project_id}/transcribe",
        json={"provider": "groq", "api_key": secret},
    )
    assert response.status_code == 200
    assert secret not in project_path.read_text(encoding="utf-8")
    assert response.json()["result"]["word_count"] == 1
