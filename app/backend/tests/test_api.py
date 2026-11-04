import json
import subprocess
import time
from pathlib import Path

from fastapi.testclient import TestClient

import main


def test_import_edit_undo_export_and_reload(tmp_path: Path, monkeypatch) -> None:
    data_dir = tmp_path / "data"
    monkeypatch.setattr(main, "DATA_DIR", data_dir)
    monkeypatch.setattr(main, "MEDIA_DIR", data_dir / "media")
    monkeypatch.setattr(main, "PROJECT_DIR", data_dir / "projects")
    monkeypatch.setattr(main, "MEDIA_INDEX", data_dir / "media.json")
    main.MEDIA_DIR.mkdir(parents=True)
    main.PROJECT_DIR.mkdir(parents=True)
    sample = tmp_path / "sample.mp4"
    subprocess.run(
        [
            "ffmpeg", "-y", "-v", "error", "-f", "lavfi", "-i",
            "color=c=black:s=320x240:r=25", "-f", "lavfi", "-i",
            "sine=frequency=440:sample_rate=44100", "-t", "4", "-c:v",
            "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", str(sample),
        ],
        check=True,
    )
    client = TestClient(main.app)
    with sample.open("rb") as media_file:
        response = client.post(
            "/api/media/import",
            files={"file": ("sample.mp4", media_file, "video/mp4")},
        )
    assert response.status_code == 200
    media = response.json()
    assert media["has_video"] and media["has_audio"]
    stream = client.get(
        f"/api/media/{media['id']}/stream",
        headers={"Range": "bytes=0-63"},
    )
    assert stream.status_code == 206
    assert len(stream.content) == 64
    assert client.get(f"/api/media/{media['id']}/peaks?rate=50").json()["buckets"]

    project = client.post("/api/projects", json={"media_id": media["id"]}).json()
    project_id = project["id"]
    markers = [
        {"id": "topic-intro", "title": "Introdução", "time": 0.5},
        {"id": "topic-main", "title": "Assunto principal", "time": 2.0},
    ]
    saved_markers = client.put(
        f"/api/projects/{project_id}/topic-markers",
        json={"topic_markers": markers},
    )
    assert saved_markers.status_code == 200
    assert saved_markers.json()["topic_markers"] == markers
    assert saved_markers.json()["edl"] == project["edl"]
    assert client.get(f"/api/projects/{project_id}").json()["topic_markers"] == markers
    assert client.put(
        f"/api/projects/{project_id}/topic-markers",
        json={"topic_markers": [{"id": "too-late", "title": "Inválido", "time": 5}]},
    ).status_code == 422
    assert client.put(
        f"/api/projects/{project_id}/topic-markers",
        json={"topic_markers": [
            {"id": "duplicate-a", "title": "A", "time": 1},
            {"id": "duplicate-b", "title": "B", "time": 1.005},
        ]},
    ).status_code == 422

    project_path = data_dir / "projects" / f"{project_id}.json"
    saved_project = json.loads(project_path.read_text(encoding="utf-8"))
    saved_project["transcript"] = [
        {"id": "word-to-cut", "text": "palavra", "start": 1.001, "end": 2.001, "speaker": "Speaker 1"},
    ]
    project_path.write_text(json.dumps(saved_project), encoding="utf-8")
    word_cut = client.post(
        f"/api/projects/{project_id}/delete-words",
        json={"word_ids": ["word-to-cut"]},
    ).json()
    assert word_cut["edl"]["kept"] == [
        {"start": 0.0, "end": 1.0},
        {"start": 2.04, "end": 4.0},
    ]
    assert client.post(f"/api/projects/{project_id}/undo").json()["edl"]["edited_duration"] == 4

    edited = client.post(
        f"/api/projects/{project_id}/cuts",
        json={"start": 1.001, "end": 2.001},
    ).json()
    assert edited["edl"]["kept"] == [
        {"start": 0.0, "end": 1.0},
        {"start": 2.04, "end": 4.0},
    ]
    assert abs(edited["edl"]["edited_duration"] - 2.96) < 1e-9
    assert client.post(f"/api/projects/{project_id}/undo").json()["edl"]["edited_duration"] == 4
    assert abs(client.post(f"/api/projects/{project_id}/redo").json()["edl"]["edited_duration"] - 2.96) < 1e-9

    job = client.post(f"/api/projects/{project_id}/export", json={"format": "mp4"}).json()
    for _ in range(120):
        status = client.get(f"/api/jobs/{job['id']}").json()
        if status["status"] in {"completed", "failed"}:
            break
        time.sleep(0.25)
    assert status["status"] == "completed", status
    output = data_dir / "projects" / project_id / "exports" / status["result"]["filename"]
    probe_result = subprocess.run(
        [
            "ffprobe", "-v", "error", "-count_frames", "-show_entries",
            "stream=codec_type,nb_read_frames:format=duration",
            "-of", "json", str(output),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    rendered = json.loads(probe_result.stdout)
    duration = float(rendered["format"]["duration"])
    video_stream = next(stream for stream in rendered["streams"] if stream["codec_type"] == "video")
    assert abs(duration - 2.96) < 0.02
    assert int(video_stream["nb_read_frames"]) == 74
    reloaded = client.get(f"/api/projects/{project_id}").json()
    assert reloaded["edl"]["edited_duration"] == 2.96

    renamed = client.patch(
        f"/api/projects/{project_id}",
        json={"name": "  Entrevista final  "},
    )
    assert renamed.status_code == 200
    assert renamed.json()["name"] == "Entrevista final"
    assert client.get("/api/projects").json()[0]["name"] == "Entrevista final"

    deleted = client.delete(f"/api/projects/{project_id}")
    assert deleted.status_code == 200
    assert client.get(f"/api/projects/{project_id}").status_code == 404
    assert client.get(f"/api/media/{media['id']}").status_code == 200


def test_audio_cut_exports_exact_sample_count(tmp_path: Path, monkeypatch) -> None:
    data_dir = tmp_path / "data"
    monkeypatch.setattr(main, "DATA_DIR", data_dir)
    monkeypatch.setattr(main, "MEDIA_DIR", data_dir / "media")
    monkeypatch.setattr(main, "PROJECT_DIR", data_dir / "projects")
    monkeypatch.setattr(main, "MEDIA_INDEX", data_dir / "media.json")
    main.MEDIA_DIR.mkdir(parents=True)
    main.PROJECT_DIR.mkdir(parents=True)

    sample_rate = 48_000
    source = tmp_path / "sample.wav"
    subprocess.run(
        [
            "ffmpeg", "-y", "-v", "error", "-f", "lavfi", "-i",
            f"sine=frequency=440:sample_rate={sample_rate}:duration=2",
            "-c:a", "pcm_s16le", str(source),
        ],
        check=True,
    )
    client = TestClient(main.app)
    with source.open("rb") as media_file:
        media_response = client.post(
            "/api/media/import",
            files={"file": ("sample.wav", media_file, "audio/wav")},
        )
    assert media_response.status_code == 200
    project = client.post(
        "/api/projects",
        json={"media_id": media_response.json()["id"]},
    ).json()

    cut_start = 24_000.2 / sample_rate
    cut_end = 36_000.8 / sample_rate
    cut = client.post(
        f"/api/projects/{project['id']}/cuts",
        json={"start": cut_start, "end": cut_end},
    )
    assert cut.status_code == 200
    assert cut.json()["edl"]["kept"] == [
        {"start": 0.0, "end": 0.5},
        {"start": 36_001 / sample_rate, "end": 2.0},
    ]

    job = client.post(
        f"/api/projects/{project['id']}/export",
        json={"format": "wav"},
    ).json()
    for _ in range(120):
        status = client.get(f"/api/jobs/{job['id']}").json()
        if status["status"] in {"completed", "failed"}:
            break
        time.sleep(0.25)
    assert status["status"] == "completed", status

    output = (
        data_dir / "projects" / project["id"] / "exports"
        / status["result"]["filename"]
    )
    decoded = subprocess.run(
        [
            "ffmpeg", "-v", "error", "-i", str(output), "-f", "s16le",
            "-acodec", "pcm_s16le", "pipe:1",
        ],
        check=True,
        capture_output=True,
    )
    expected_samples = 2 * sample_rate - (36_001 - 24_000)
    assert len(decoded.stdout) // 2 == expected_samples
