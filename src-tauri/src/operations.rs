//! Session-only desktop jobs. Writers and all safety decisions live in nandunx-core.
use std::{
    collections::HashMap,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use nandunx_core::{
    InPlaceExpansionReport, RestoreDeviceObserver, RestoreDevicePhase, RestoreDeviceProgress,
    RestoreDeviceReport,
};
use serde::Serialize;
use uuid::Uuid;

#[derive(Default)]
pub(crate) struct Operations {
    active: AtomicBool,
    jobs: Mutex<HashMap<Uuid, OperationView>>,
}

#[derive(Clone, Serialize)]
pub(crate) struct OperationView {
    pub id: String,
    pub cancel_requested: bool,
    pub status: OperationStatus,
}

#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum OperationStatus {
    Running { progress: RestoreDeviceProgress },
    Finished { report: OperationReport },
    Failed { error: String },
}

#[derive(Clone, Serialize)]
#[serde(untagged)]
pub(crate) enum OperationReport {
    Restore(RestoreDeviceReport),
    InPlace(InPlaceExpansionReport),
}

// Held through both preparation and writing; every return/panic releases it.
pub(crate) struct ActiveOperation(Arc<Operations>);
impl Drop for ActiveOperation {
    fn drop(&mut self) {
        self.0.active.store(false, Ordering::Release);
    }
}

impl Operations {
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub fn reserve(self: &Arc<Self>) -> Result<ActiveOperation, String> {
        self.active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "Inna operacja jest już przygotowywana lub wykonywana.".to_owned())?;
        Ok(ActiveOperation(Arc::clone(self)))
    }

    pub fn view(&self, id: &str, cancel: bool) -> Result<OperationView, String> {
        let id =
            Uuid::parse_str(id).map_err(|_| "Nieprawidłowy identyfikator operacji.".to_owned())?;
        let mut jobs = self
            .jobs
            .lock()
            .map_err(|_| "Nie można odczytać stanu operacji.".to_owned())?;
        let job = jobs
            .get_mut(&id)
            .ok_or("Operacja nie jest dostępna w tej sesji.")?;
        if cancel && matches!(job.status, OperationStatus::Running { .. }) {
            job.cancel_requested = true;
        }
        Ok(job.clone())
    }

    pub fn start(
        self: &Arc<Self>,
        reservation: ActiveOperation,
        phase: RestoreDevicePhase,
        writer: impl FnOnce(&mut DesktopObserver) -> Result<OperationReport, String> + Send + 'static,
    ) -> Result<OperationView, String> {
        let id = Uuid::new_v4();
        let view = OperationView {
            id: id.to_string(),
            cancel_requested: false,
            status: OperationStatus::Running {
                progress: RestoreDeviceProgress {
                    phase,
                    processed_bytes: 0,
                    total_bytes: 0,
                },
            },
        };
        self.jobs
            .lock()
            .map_err(|_| "Nie można zapisać stanu operacji.".to_owned())?
            .insert(id, view.clone());
        #[cfg(target_os = "windows")]
        crate::diagnostics::diagnostic(&format!("operation {id} created; initial_phase={phase:?}"));
        let operations = Arc::clone(self);
        tauri::async_runtime::spawn_blocking(move || {
            let _reservation = reservation;
            let mut observer = DesktopObserver::new(id, Arc::clone(&operations));
            let result =
                catch_unwind(AssertUnwindSafe(|| writer(&mut observer))).unwrap_or_else(|_| {
                    Err(
                        "Worker został przerwany. Zweryfikuj nośnik przed ponowną operacją."
                            .to_owned(),
                    )
                });
            #[cfg(target_os = "windows")]
            match &result {
                Ok(_) => crate::diagnostics::diagnostic(&format!("operation {id} finished")),
                Err(error) => {
                    crate::diagnostics::diagnostic(&format!("operation {id} failed: {error}"))
                }
            }
            if let Ok(mut jobs) = operations.jobs.lock() {
                if let Some(job) = jobs.get_mut(&id) {
                    job.status = match result {
                        Ok(report) => OperationStatus::Finished { report },
                        Err(error) => OperationStatus::Failed { error },
                    };
                }
            }
        });
        Ok(view)
    }
}

pub(crate) struct DesktopObserver {
    id: Uuid,
    operations: Arc<Operations>,
    #[cfg(target_os = "windows")]
    last_logged: Option<RestoreDeviceProgress>,
}

impl DesktopObserver {
    fn new(id: Uuid, operations: Arc<Operations>) -> Self {
        Self {
            id,
            operations,
            #[cfg(target_os = "windows")]
            last_logged: None,
        }
    }
}

impl RestoreDeviceObserver for DesktopObserver {
    fn on_progress(&mut self, progress: RestoreDeviceProgress) -> bool {
        #[cfg(target_os = "windows")]
        if self.last_logged.is_none_or(|last| {
            last.phase != progress.phase
                || progress.processed_bytes == progress.total_bytes
                || progress
                    .processed_bytes
                    .saturating_sub(last.processed_bytes)
                    >= 16 * 1024 * 1024
        }) {
            crate::diagnostics::diagnostic(&format!(
                "operation {} progress: phase={:?} bytes={}/{}",
                self.id, progress.phase, progress.processed_bytes, progress.total_bytes
            ));
            self.last_logged = Some(progress);
        }
        let Ok(mut jobs) = self.operations.jobs.lock() else {
            return false;
        };
        let Some(job) = jobs.get_mut(&self.id) else {
            return false;
        };
        let OperationStatus::Running { progress: current } = &mut job.status else {
            return false;
        };
        *current = progress;
        !job.cancel_requested
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preparation_reserves_single_job_and_releases_on_failure() {
        let operations = Arc::new(Operations::default());
        let reservation = operations.reserve().unwrap();
        assert!(operations.is_active());
        assert!(operations.reserve().is_err());
        drop(reservation);
        assert!(!operations.is_active());
        assert!(operations.reserve().is_ok());
    }

    #[test]
    fn progress_and_cancellation_are_scoped_to_the_job() {
        let operations = Arc::new(Operations::default());
        let id = Uuid::new_v4();
        let progress = RestoreDeviceProgress {
            phase: RestoreDevicePhase::CopyingPayload,
            processed_bytes: 0,
            total_bytes: 32,
        };
        operations.jobs.lock().unwrap().insert(
            id,
            OperationView {
                id: id.to_string(),
                cancel_requested: false,
                status: OperationStatus::Running { progress },
            },
        );
        let mut observer = DesktopObserver::new(id, Arc::clone(&operations));
        assert!(observer.on_progress(RestoreDeviceProgress {
            processed_bytes: 16,
            ..progress
        }));
        let view = operations.view(&id.to_string(), false).unwrap();
        assert!(
            matches!(view.status, OperationStatus::Running { progress } if progress.processed_bytes == 16)
        );
        assert!(
            operations
                .view(&id.to_string(), true)
                .unwrap()
                .cancel_requested
        );
        assert!(!observer.on_progress(progress));
        assert!(operations.view(&Uuid::new_v4().to_string(), true).is_err());
        assert!(operations.view("bad-id", false).is_err());
        operations.jobs.lock().unwrap().get_mut(&id).unwrap().status = OperationStatus::Failed {
            error: "failure".into(),
        };
        assert!(!observer.on_progress(progress));
    }

    #[test]
    fn cancelling_a_finished_job_does_not_change_its_status() {
        let operations = Operations::default();
        let id = Uuid::new_v4();
        operations.jobs.lock().unwrap().insert(
            id,
            OperationView {
                id: id.to_string(),
                cancel_requested: false,
                status: OperationStatus::Failed {
                    error: "failure".into(),
                },
            },
        );
        let view = operations.view(&id.to_string(), true).unwrap();
        assert!(!view.cancel_requested);
        assert!(matches!(view.status, OperationStatus::Failed { .. }));
    }
    fn wait_for_worker(operations: &Operations, id: &str) -> OperationView {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while operations.is_active() {
            assert!(
                std::time::Instant::now() < deadline,
                "worker did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        operations.view(id, false).unwrap()
    }

    #[test]
    fn async_worker_delivers_both_report_types_and_releases_reservation() {
        let operations = Arc::new(Operations::default());
        for in_place in [false, true] {
            let view = operations
                .start(
                    operations.reserve().unwrap(),
                    RestoreDevicePhase::RelocatingUser,
                    move |_| {
                        Ok(if in_place {
                            OperationReport::InPlace(InPlaceExpansionReport {
                                status: nandunx_core::RestoreDeviceStatus::Completed,
                                current_user_sectors: 32,
                                target_user_sectors: 64,
                            })
                        } else {
                            OperationReport::Restore(RestoreDeviceReport {
                                status: nandunx_core::RestoreDeviceStatus::Completed,
                                raw_nand_bytes: 32,
                                verified_bytes: 32,
                            })
                        })
                    },
                )
                .unwrap();
            let finished = wait_for_worker(&operations, &view.id);
            match finished.status {
                OperationStatus::Finished {
                    report: OperationReport::InPlace(report),
                } if in_place => assert_eq!(report.target_user_sectors, 64),
                OperationStatus::Finished {
                    report: OperationReport::Restore(report),
                } if !in_place => assert_eq!(report.verified_bytes, 32),
                _ => panic!("incorrect report"),
            }
        }
    }

    #[test]
    fn async_worker_handles_errors_and_panics_without_sticking_running() {
        let operations = Arc::new(Operations::default());
        for should_panic in [false, true] {
            let view = operations
                .start(
                    operations.reserve().unwrap(),
                    RestoreDevicePhase::CopyingPayload,
                    move |_| {
                        if should_panic {
                            panic!("synthetic worker failure");
                        }
                        Err("synthetic write failure".to_owned())
                    },
                )
                .unwrap();
            let finished = wait_for_worker(&operations, &view.id);
            assert!(matches!(finished.status, OperationStatus::Failed { .. }));
            assert!(operations.reserve().is_ok());
        }
    }

    #[test]
    fn cancellation_reaches_running_worker_and_preserves_its_report() {
        let operations = Arc::new(Operations::default());
        let (proceed, wait) = std::sync::mpsc::channel();
        let view = operations
            .start(
                operations.reserve().unwrap(),
                RestoreDevicePhase::CopyingPayload,
                move |observer| {
                    wait.recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                    assert!(!observer.on_progress(RestoreDeviceProgress {
                        phase: RestoreDevicePhase::CopyingPayload,
                        processed_bytes: 16,
                        total_bytes: 32,
                    }));
                    Ok(OperationReport::Restore(RestoreDeviceReport {
                        status: nandunx_core::RestoreDeviceStatus::Cancelled {
                            phase: RestoreDevicePhase::CopyingPayload,
                            processed_bytes: 16,
                        },
                        raw_nand_bytes: 32,
                        verified_bytes: 0,
                    }))
                },
            )
            .unwrap();
        assert!(operations.reserve().is_err());
        assert!(operations.view(&view.id, true).unwrap().cancel_requested);
        proceed.send(()).unwrap();
        let finished = wait_for_worker(&operations, &view.id);
        assert!(finished.cancel_requested);
        assert!(matches!(
            finished.status,
            OperationStatus::Finished {
                report: OperationReport::Restore(RestoreDeviceReport {
                    status: nandunx_core::RestoreDeviceStatus::Cancelled {
                        phase: RestoreDevicePhase::CopyingPayload,
                        processed_bytes: 16
                    },
                    ..
                })
            }
        ));
    }
}
