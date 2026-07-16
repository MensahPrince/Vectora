use super::*;

/// Cache-management calls must not wait forever behind a stuck render.
#[allow(dead_code)] // Used once the cache registry consumes this Phase 2b RPC.
pub(super) const PREVIEW_CACHE_RPC_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const PREVIEW_CACHE_RPC_WAIT_SLICE: Duration = Duration::from_millis(25);
#[allow(dead_code)] // Used by the staged maintenance entry point below.
pub(super) const PROJECT_MAINTENANCE_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
/// Total queue + execution deadline for acknowledged project mutations.
///
/// Cancellation can prove "not started" only while the operation is pending.
/// Once claimed, cancellation is ignored and the caller waits for the real
/// result until this deadline. A deadline or disconnect after claim is
/// reported as "outcome unknown": the worker may already have mutated the
/// engine and may still finish the operation.
pub(super) const PROJECT_RPC_TIMEOUT: Duration = Duration::from_secs(30);
const WORKER_RPC_PENDING: u8 = 0;
const WORKER_RPC_RUNNING: u8 = 1;
const WORKER_RPC_ABANDONED: u8 = 2;

/// Shared claim state for bounded worker RPCs. Only a request still in
/// `PENDING` may be started by the worker or abandoned by its caller.
pub(super) struct WorkerRpcOperation(AtomicU8);

impl WorkerRpcOperation {
    pub(super) fn pending() -> Self {
        Self(AtomicU8::new(WORKER_RPC_PENDING))
    }

    pub(super) fn claim(&self) -> bool {
        self.0
            .compare_exchange(
                WORKER_RPC_PENDING,
                WORKER_RPC_RUNNING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(super) fn abandon(&self) -> bool {
        self.0
            .compare_exchange(
                WORKER_RPC_PENDING,
                WORKER_RPC_ABANDONED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

/// Work the preview worker performs synchronously before it resumes its normal
/// queue after a project-maintenance lease.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum ProjectMaintenanceResumeAction {
    /// Resume without changing any worker-owned runtime state.
    #[default]
    Resume,
    /// Proxy cache storage moved; discard old bindings and look them up again.
    RefreshProxies,
}

/// Exclusive maintenance lease over a coherent live-project snapshot.
///
/// While this value is alive the preview worker is paused and consumes no
/// edit, import, relink, template, or other queued messages. The guard owns no
/// engine state and is safe to move to the background maintenance thread that
/// performs the filesystem operation.
#[must_use = "dropping the maintenance guard resumes the preview worker"]
#[allow(dead_code)] // Constructed once cache relocation consumes the staged API.
pub(crate) struct ProjectMaintenanceGuard {
    pub(super) project: Project,
    pub(super) resume: Option<Sender<ProjectMaintenanceResumeAction>>,
    pub(super) resume_action: ProjectMaintenanceResumeAction,
}

impl ProjectMaintenanceGuard {
    #[allow(dead_code)] // Read by the cache relocation coordination slice.
    pub(crate) fn project(&self) -> &Project {
        &self.project
    }

    /// Refresh every runtime proxy binding before normal preview work resumes.
    #[allow(dead_code)] // Called by cache relocation once its move is published.
    pub(crate) fn refresh_proxies_on_resume(&mut self) {
        self.resume_action = ProjectMaintenanceResumeAction::RefreshProxies;
    }
}

impl Drop for ProjectMaintenanceGuard {
    fn drop(&mut self) {
        if let Some(resume) = self.resume.take() {
            // Capacity one means this never waits for the worker. Sending is
            // best-effort; dropping the sole sender immediately afterward
            // still wakes a receiver if the channel has disconnected.
            let _ = resume.try_send(self.resume_action);
        }
    }
}

#[allow(dead_code)] // Reachable through the intentionally staged API below.
pub(super) fn project_maintenance_result(
    reply: Result<Project, ()>,
    resume: Sender<ProjectMaintenanceResumeAction>,
) -> Result<ProjectMaintenanceGuard, String> {
    match reply {
        Ok(project) => Ok(ProjectMaintenanceGuard {
            project,
            resume: Some(resume),
            resume_action: ProjectMaintenanceResumeAction::Resume,
        }),
        Err(()) => Err("project maintenance request was refused by preview worker".into()),
    }
}

/// Claim, snapshot, and pause for one maintenance request. The blocking
/// receive has no timeout: only guard drop resumes worker message processing.
/// A disconnected action channel safely means an ordinary resume.
pub(super) fn serve_project_maintenance(
    project: &Project,
    reply: Sender<Result<Project, ()>>,
    resume: Receiver<ProjectMaintenanceResumeAction>,
    operation: Arc<WorkerRpcOperation>,
) -> ProjectMaintenanceResumeAction {
    if !operation.claim() {
        return ProjectMaintenanceResumeAction::Resume;
    }
    if reply.send(Ok(project.clone())).is_ok() {
        resume.recv().unwrap_or_default()
    } else {
        ProjectMaintenanceResumeAction::Resume
    }
}

/// Claim a queued acknowledged operation before invoking its handler.
///
/// A caller that cancelled or timed out while this message was queued changes
/// the operation to `ABANDONED`; the failed claim then guarantees `run` is
/// never called and therefore cannot mutate the engine.
pub(super) fn serve_worker_rpc<T>(
    reply: Sender<Result<T, String>>,
    operation: Arc<WorkerRpcOperation>,
    run: impl FnOnce() -> Result<T, String>,
) {
    if operation.claim() {
        let _ = reply.send(run());
    }
}
