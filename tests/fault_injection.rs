use tempfile::tempdir;

use agent_stride::{
    domain::Actor,
    runtime::{Command, CommandKind, RunId, TaskId},
    store::Store,
};

#[test]
fn reopening_the_database_recovers_an_expired_worker_lease() {
    let project = tempdir().expect("project");
    let database = project.path().join("runtime.db");
    let run_id = RunId::new("restart-run").expect("run");
    let task_id = TaskId::new("restart-task").expect("task");

    {
        let mut store = Store::open(&database).expect("open");
        store.migrate().expect("migrate");
        store
            .start_run(
                run_id.clone(),
                "plan-restart",
                Command::new(
                    "start-restart-run",
                    CommandKind::StartRun {
                        now: "2026-09-29T00:00:00Z".to_owned(),
                    },
                )
                .expect("start command"),
            )
            .expect("start run");
        store
            .apply_runtime_command(
                &run_id,
                Command::new(
                    "reconcile-restart-run",
                    CommandKind::ReconcilePlan {
                        new_plan_revision: "plan-restart".to_owned(),
                        discovered: vec![task_id.clone()],
                        ready: vec![task_id.clone()],
                        invalidated: Vec::new(),
                        superseded: Vec::new(),
                        now: "2026-09-29T00:00:01Z".to_owned(),
                    },
                )
                .expect("reconcile command"),
            )
            .expect("reconcile");
        store
            .poll_runtime_task(
                Actor::new("crashed-worker").expect("actor"),
                "2026-09-29T00:00:02Z",
                "2026-09-29T00:00:05Z",
            )
            .expect("lease task");
    }

    let mut recovered_store = Store::open(&database).expect("reopen");
    recovered_store.migrate().expect("migrate again");
    let report = recovered_store
        .recover_expired_runtime_tasks(&run_id, "2026-09-29T00:00:06Z", 1)
        .expect("recover");
    assert_eq!(report.recovered.len(), 1);

    let timer_id = agent_stride::runtime::TimerId::new(format!("timer-{}", report.recovered[0]))
        .expect("recovery timer id");
    recovered_store
        .fire_runtime_timer(timer_id, "2026-09-29T00:00:08Z")
        .expect("fire retry timer");
    let (retry, _) = recovered_store
        .poll_runtime_task(
            Actor::new("replacement-worker").expect("actor"),
            "2026-09-29T00:00:09Z",
            "2026-09-29T00:01:09Z",
        )
        .expect("poll retry");
    assert_eq!(retry.task_id, task_id);
    assert_ne!(retry.worker.as_str(), "crashed-worker");
    assert!(
        recovered_store
            .verify_runtime_state(&run_id)
            .expect("verify")
            .valid
    );
}
