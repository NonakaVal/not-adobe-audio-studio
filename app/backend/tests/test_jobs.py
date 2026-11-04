import time

from fastapi.testclient import TestClient

import main


def test_running_job_can_be_cancelled() -> None:
    def wait_for_cancel(job_id: str) -> dict[str, bool]:
        for _ in range(100):
            if main._cancel_requested(job_id):
                raise main.JobCancelled
            time.sleep(0.01)
        return {"finished": True}

    job = main._start_job("test", wait_for_cancel)
    client = TestClient(main.app)
    response = client.post(f"/api/jobs/{job['id']}/cancel")
    assert response.status_code == 200
    for _ in range(100):
        status = client.get(f"/api/jobs/{job['id']}").json()
        if status["status"] == "cancelled":
            break
        time.sleep(0.01)
    assert status["status"] == "cancelled"
