from enhance import EnhancementParams, PRESETS, build_filter_chain
from fastapi.testclient import TestClient

import main


def test_filter_chain_contains_configured_processing_stages() -> None:
    params = EnhancementParams(noise_reduction=75, speech_gate=30, deesser_mix=25)
    chain = build_filter_chain(params)
    assert "highpass=f=70" in chain
    assert "afftdn=" in chain
    assert "deesser=" in chain
    assert "agate=" in chain
    assert "loudnorm=I=-16" in chain


def test_presets_are_valid_parameter_overrides() -> None:
    defaults = EnhancementParams().model_dump()
    for preset in PRESETS.values():
        assert EnhancementParams(**{**defaults, **preset})


def test_preview_accepts_full_recordings_over_five_minutes() -> None:
    request = main.PreviewRequest(params=EnhancementParams(), seconds=301)
    assert request.seconds == 301


def test_custom_preset_is_persisted(tmp_path, monkeypatch) -> None:
    monkeypatch.setattr(main, "DATA_DIR", tmp_path)
    client = TestClient(main.app)
    response = client.post(
        "/api/enhance/presets",
        json={"name": "My voice", "params": {"noise_reduction": 68}},
    )
    assert response.status_code == 200
    assert response.json()["My voice"]["noise_reduction"] == 68
    assert client.get("/api/enhance/presets").json()["My voice"]["noise_reduction"] == 68
