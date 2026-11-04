use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use thiserror::Error;
use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JobStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct JobSnapshot {
    pub id: Uuid,
    pub label: String,
    pub status: JobStatus,
    pub progress: f64,
    pub message: String,
    pub error: Option<String>,
    pub result: Option<String>,
}

#[derive(Clone)]
pub struct JobManager {
    jobs: Arc<Mutex<HashMap<Uuid, StoredJob>>>,
    sender: Sender<QueuedJob>,
}

#[derive(Clone)]
struct StoredJob {
    snapshot: JobSnapshot,
    cancellation: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct JobContext {
    id: Uuid,
    jobs: Arc<Mutex<HashMap<Uuid, StoredJob>>>,
    cancellation: Arc<AtomicBool>,
}

struct QueuedJob {
    id: Uuid,
    context: JobContext,
    work: Box<dyn FnOnce(JobContext) -> Result<String, String> + Send>,
}

impl JobManager {
    pub fn new() -> Self {
        let jobs = Arc::new(Mutex::new(HashMap::new()));
        let (sender, receiver) = mpsc::channel::<QueuedJob>();
        let worker_jobs = jobs.clone();
        thread::spawn(move || {
            while let Ok(queued) = receiver.recv() {
                if queued.context.is_cancelled() {
                    update_job(&worker_jobs, queued.id, |snapshot| {
                        snapshot.status = JobStatus::Cancelled;
                        snapshot.message = "Cancelado".into();
                    });
                    continue;
                }
                update_job(&worker_jobs, queued.id, |snapshot| {
                    snapshot.status = JobStatus::Running;
                    snapshot.message = "Iniciando".into();
                });
                let result = (queued.work)(queued.context.clone());
                update_job(&worker_jobs, queued.id, |snapshot| match result {
                    Ok(_result) if queued.context.is_cancelled() => {
                        snapshot.status = JobStatus::Cancelled;
                        snapshot.message = "Cancelado".into();
                        snapshot.result = None;
                    }
                    Ok(result) => {
                        snapshot.status = JobStatus::Completed;
                        snapshot.progress = 1.0;
                        snapshot.message = "Concluído".into();
                        snapshot.result = Some(result);
                    }
                    Err(_error) if queued.context.is_cancelled() => {
                        snapshot.status = JobStatus::Cancelled;
                        snapshot.message = "Cancelado".into();
                        snapshot.error = None;
                    }
                    Err(error) => {
                        snapshot.status = JobStatus::Failed;
                        snapshot.message = "Falhou".into();
                        snapshot.error = Some(error);
                    }
                });
            }
        });
        Self { jobs, sender }
    }

    pub fn start<F>(&self, label: impl Into<String>, work: F) -> Uuid
    where
        F: FnOnce(JobContext) -> Result<String, String> + Send + 'static,
    {
        let id = Uuid::new_v4();
        let cancellation = Arc::new(AtomicBool::new(false));
        let snapshot = JobSnapshot {
            id,
            label: label.into(),
            status: JobStatus::Queued,
            progress: 0.0,
            message: "Na fila".into(),
            error: None,
            result: None,
        };
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.insert(
                id,
                StoredJob {
                    snapshot,
                    cancellation: cancellation.clone(),
                },
            );
        }

        let context = JobContext {
            id,
            jobs: self.jobs.clone(),
            cancellation,
        };
        if self
            .sender
            .send(QueuedJob {
                id,
                context,
                work: Box::new(work),
            })
            .is_err()
        {
            update_job(&self.jobs, id, |snapshot| {
                snapshot.status = JobStatus::Failed;
                snapshot.message = "Falhou".into();
                snapshot.error = Some("A fila de tarefas foi encerrada.".into());
            });
        }
        id
    }

    pub fn cancel(&self, id: Uuid) -> bool {
        let Ok(jobs) = self.jobs.lock() else {
            return false;
        };
        let Some(job) = jobs.get(&id) else {
            return false;
        };
        if matches!(
            job.snapshot.status,
            JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled
        ) {
            return false;
        }
        job.cancellation.store(true, Ordering::Release);
        true
    }

    pub fn snapshot(&self, id: Uuid) -> Option<JobSnapshot> {
        self.jobs
            .lock()
            .ok()?
            .get(&id)
            .map(|job| job.snapshot.clone())
    }

    pub fn snapshots(&self) -> Vec<JobSnapshot> {
        let Ok(jobs) = self.jobs.lock() else {
            return Vec::new();
        };
        let mut snapshots: Vec<_> = jobs.values().map(|job| job.snapshot.clone()).collect();
        snapshots.sort_by_key(|snapshot| snapshot.id);
        snapshots
    }

    pub fn clear_finished(&self) {
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.retain(|_, job| {
                !matches!(
                    job.snapshot.status,
                    JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled
                )
            });
        }
    }
}

impl Default for JobManager {
    fn default() -> Self {
        Self::new()
    }
}

impl JobContext {
    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.load(Ordering::Acquire)
    }

    pub fn check_cancelled(&self) -> Result<(), JobError> {
        if self.is_cancelled() {
            Err(JobError::Cancelled)
        } else {
            Ok(())
        }
    }

    pub fn report_progress(
        &self,
        progress: f64,
        message: impl Into<String>,
    ) -> Result<(), JobError> {
        self.check_cancelled()?;
        if !progress.is_finite() {
            return Err(JobError::InvalidProgress);
        }
        let progress = progress.clamp(0.0, 1.0);
        let message = message.into();
        update_job(&self.jobs, self.id, |snapshot| {
            snapshot.progress = progress;
            snapshot.message = message;
        });
        self.check_cancelled()
    }
}

fn update_job(
    jobs: &Arc<Mutex<HashMap<Uuid, StoredJob>>>,
    id: Uuid,
    update: impl FnOnce(&mut JobSnapshot),
) {
    if let Ok(mut jobs) = jobs.lock()
        && let Some(job) = jobs.get_mut(&id)
    {
        update(&mut job.snapshot);
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum JobError {
    #[error("job cancelled")]
    Cancelled,
    #[error("progress must be a finite number")]
    InvalidProgress,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn job_reports_progress_and_result() {
        let manager = JobManager::new();
        let id = manager.start("teste", |context| {
            context.report_progress(0.5, "Metade").unwrap();
            Ok("pronto".into())
        });
        for _ in 0..100 {
            if manager
                .snapshot(id)
                .is_some_and(|job| job.status == JobStatus::Completed)
            {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let snapshot = manager.snapshot(id).unwrap();
        assert_eq!(snapshot.status, JobStatus::Completed);
        assert_eq!(snapshot.result.as_deref(), Some("pronto"));
        assert_eq!(snapshot.progress, 1.0);
    }

    #[test]
    fn job_cancellation_is_cooperative() {
        let manager = JobManager::new();
        let (started_tx, started_rx) = mpsc::channel();
        let id = manager.start("cancelável", move |context| {
            started_tx.send(()).unwrap();
            loop {
                context
                    .check_cancelled()
                    .map_err(|error| error.to_string())?;
                thread::sleep(Duration::from_millis(2));
            }
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(manager.cancel(id));
        for _ in 0..100 {
            if manager
                .snapshot(id)
                .is_some_and(|job| job.status == JobStatus::Cancelled)
            {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(manager.snapshot(id).unwrap().status, JobStatus::Cancelled);
    }

    #[test]
    fn unknown_job_cannot_be_cancelled() {
        assert!(!JobManager::new().cancel(Uuid::new_v4()));
    }

    #[test]
    fn jobs_run_in_fifo_order() {
        let manager = JobManager::new();
        let order = Arc::new(Mutex::new(Vec::new()));
        for value in [1, 2, 3] {
            let order = order.clone();
            manager.start(format!("job-{value}"), move |_| {
                order.lock().unwrap().push(value);
                thread::sleep(Duration::from_millis(5));
                Ok(value.to_string())
            });
        }
        for _ in 0..200 {
            if order.lock().unwrap().len() == 3 {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(*order.lock().unwrap(), vec![1, 2, 3]);
    }
}
