use super::{Command, MutableState, RuntimeError, TaskId, TaskSupersession};

pub fn command(
    state: &MutableState,
    new_plan_revision: impl Into<String>,
    discovered: Vec<TaskId>,
    ready: Vec<TaskId>,
    invalidated: Vec<TaskId>,
    superseded: Vec<TaskSupersession>,
    now: impl Into<String>,
) -> Result<Command, RuntimeError> {
    Command::new(
        format!("reconcile-{}-{}", state.run_id, state.history_version),
        super::CommandKind::ReconcilePlan {
            new_plan_revision: new_plan_revision.into(),
            discovered,
            ready,
            invalidated,
            superseded,
            now: now.into(),
        },
    )
}
