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

    edited = client.post(
        f"/api/projects/{project_id}/cuts",
        json={"start": 1, "end": 2},
    ).json()
    assert abs(edited["edl"]["edited_duration"] - 3) < 0.05
    assert client.post(f"/api/projects/{project_id}/undo").json()["edl"]["edited_duration"] == 4
    assert abs(client.post(f"/api/projects/{project_id}/redo").json()["edl"]["edited_duration"] - 3) < 0.05

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
            "ffprobe", "-v", "error", "-show_entries", "format=duration",
            "-of", "json", str(output),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    duration = float(json.loads(probe_result.stdout)["format"]["duration"])
    assert abs(duration - 3) < 0.1
    reloaded = client.get(f"/api/projects/{project_id}").json()
    assert reloaded["edl"]["edited_duration"] == 3

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
