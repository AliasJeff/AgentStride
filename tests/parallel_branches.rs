use std::process::Command as ProcessCommand;

use tempfile::tempdir;

use agent_stride::{
    domain::{Edge, EdgeType, IssueDraft, IssueId, IssueKind, Priority},
    plan::{GitRepository, PlanGraph, export_snapshot, reconcile},
    runtime::{RunId, TaskId, TaskStatus, TimerId},
    store::Store,
};

fn git(root: &std::path::Path, args: &[&str]) {
    let status = ProcessCommand::new("git")
        .args(args)
        .current_dir(root)
        .status()
        .expect("git should run");
    assert!(status.success(), "git command failed: {args:?}");
}

fn issue(id: &str, title: &str, created_at: &str) -> agent_stride::domain::Issue {
    agent_stride::domain::Issue::from_draft(
        IssueId::new(id).expect("issue id"),
        IssueDraft {
            title: title.to_owned(),
            description: String::new(),
            priority: Priority::new(1).expect("priority"),
            kind: IssueKind::Task,
            created_at: created_at.to_owned(),
        },
    )
    .expect("issue")
}

fn edge(dependent: &str, blocker: &str, created_at: &str) -> Edge {
    Edge::new(
        IssueId::new(dependent).expect("dependent"),
        IssueId::new(blocker).expect("blocker"),
        EdgeType::Blocks,
        created_at.to_owned(),
    )
    .expect("edge")
}

fn task_id(id: &str) -> TaskId {
    TaskId::new(id).expect("task id")
}

#[test]
fn parallel_branches_fan_out_join_and_recover_behind_a_retry_timer() {
    let project = tempdir().expect("project");
    git(project.path(), &["init", "-q"]);
    git(
        project.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(project.path(), &["config", "user.name", "AgentStride"]);

    let database = project.path().join("runtime.db");
    let run_id = RunId::new("release-run").expect("run");
    let mut store = Store::open(&database).expect("open");
    store.migrate().expect("migrate");

    let created = "2026-09-29T00:00:00Z";
    for (id, title) in [
        ("ag-aaaaaa", "draft release plan"),
        ("ag-bbbbbb", "build backend"),
        ("ag-cccccc", "build frontend"),
        ("ag-dddddd", "write docs"),
        ("ag-eeeeee", "run integration tests"),
        ("ag-ffffff", "cut release"),
    ] {
        store
            .insert_issue(&issue(id, title, created))
            .expect("insert issue");
    }
    let plan_edges = [
        edge("ag-bbbbbb", "ag-aaaaaa", created),
        edge("ag-cccccc", "ag-aaaaaa", created),
        edge("ag-dddddd", "ag-aaaaaa", created),
        edge("ag-eeeeee", "ag-bbbbbb", created),
        edge("ag-eeeeee", "ag-cccccc", created),
        edge("ag-ffffff", "ag-eeeeee", created),
        edge("ag-ffffff", "ag-dddddd", created),
    ];
    for plan_edge in plan_edges {
        store.insert_edge(plan_edge).expect("insert edge");
    }

    export_snapshot(&store, project.path()).expect("export base plan");
    git(project.path(), &["add", ".tasks"]);
    git(project.path(), &["commit", "-qm", "branching release plan"]);
    let repository = GitRepository::new(project.path());
    let base = repository.current_revision().expect("base revision");

    store
        .start_run(
            run_id.clone(),
            &base,
            agent_stride::runtime::Command::new(
                "start-release-run",
                agent_stride::runtime::CommandKind::StartRun {
                    now: "2026-09-29T00:00:01Z".to_owned(),
                },
            )
            .expect("start command"),
        )
        .expect("start run");

    // Wave 1: only the root is executable.
    let graph = PlanGraph::from_git(&repository, &base).expect("base graph");
    let state = store.runtime_state(&run_id).expect("state");
    let change = reconcile(&graph, &graph, &state, Vec::new(), "initial load").expect("reconcile");
    let command = state
        .reconcile_plan(
            base.clone(),
            change.discovered,
            change.ready,
            change.invalidated,
            change.superseded,
            "2026-09-29T00:00:02Z",
        )
        .expect("reconcile command");
    let result = store
        .apply_runtime_command(&run_id, command)
        .expect("load base plan");
    let scheduled: Vec<TaskId> = result
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            agent_stride::runtime::EventKind::TaskScheduled { task_id } => Some(task_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(scheduled, vec![task_id("ag-aaaaaa")]);

    // Execute the root, then the three branches become ready together.
    let (root_lease, _) = store
        .poll_runtime_task(
            agent_stride::domain::Actor::new("plan-worker").expect("actor"),
            "2026-09-29T00:00:03Z",
            "2026-09-29T00:10:00Z",
        )
        .expect("poll root");
    assert_eq!(root_lease.task_id, task_id("ag-aaaaaa"));
    let root_token = store.issue_runtime_task_token(&root_lease).expect("token");
    store
        .start_runtime_task(&root_token, "2026-09-29T00:00:04Z")
        .expect("start root");
    store
        .complete_runtime_task(&root_token, serde_json::json!({}), "2026-09-29T00:00:05Z")
        .expect("complete root");

    // Wave 2: three branches become ready together.
    let wave_two = advance_frontier(
        &mut store,
        &run_id,
        &repository,
        &base,
        "2026-09-29T00:00:06Z",
    );
    assert_eq!(
        wave_two,
        vec![
            task_id("ag-bbbbbb"),
            task_id("ag-cccccc"),
            task_id("ag-dddddd")
        ]
    );

    // Three workers claim distinct branches; the queue serializes claims.
    let mut branches = Vec::new();
    for index in 0..3 {
        let (lease, _) = store
            .poll_runtime_task(
                agent_stride::domain::Actor::new(format!("branch-worker-{index}")).expect("actor"),
                "2026-09-29T00:00:07Z",
                "2026-09-29T00:10:00Z",
            )
            .expect("poll branch");
        let token = store.issue_runtime_task_token(&lease).expect("token");
        store
            .start_runtime_task(&token, "2026-09-29T00:00:08Z")
            .expect("start branch");
        branches.push((lease, token));
    }
    let mut claimed = branches
        .iter()
        .map(|(lease, _)| lease.task_id.clone())
        .collect::<Vec<_>>();
    claimed.sort();
    assert_eq!(
        claimed,
        vec![
            task_id("ag-bbbbbb"),
            task_id("ag-cccccc"),
            task_id("ag-dddddd")
        ]
    );

    // Two branches succeed; the backend branch fails transiently and parks
    // behind a durable retry timer instead of blocking the other workers.
    for (lease, token) in &branches {
        if lease.task_id == task_id("ag-bbbbbb") {
            continue;
        }
        store
            .complete_runtime_task(
                token,
                serde_json::json!({ "branch": lease.task_id.to_string() }),
                "2026-09-29T00:00:09Z",
            )
            .expect("complete branch");
    }
    let (backend_lease, backend_token) = branches
        .iter()
        .find(|(lease, _)| lease.task_id == task_id("ag-bbbbbb"))
        .expect("backend branch lease")
        .clone();
    store
        .retry_runtime_task(
            &backend_token,
            TimerId::new("retry-backend").expect("timer"),
            "2026-09-29T00:05:00Z",
            "transient backend failure",
            "2026-09-29T00:00:10Z",
        )
        .expect("schedule retry");

    // Dependency gate: nothing downstream unlocks while the branch waits.
    let gated = advance_frontier(
        &mut store,
        &run_id,
        &repository,
        &base,
        "2026-09-29T00:00:11Z",
    );
    assert!(
        gated.is_empty(),
        "gate must schedule nothing, got {gated:?}"
    );

    // Fire the durable timer and finish the parked branch on attempt 2.
    store
        .fire_runtime_timer(
            TimerId::new("retry-backend").expect("timer"),
            "2026-09-29T00:05:01Z",
        )
        .expect("fire retry timer");
    let (retry_lease, _) = store
        .poll_runtime_task(
            agent_stride::domain::Actor::new("branch-worker-retry").expect("actor"),
            "2026-09-29T00:05:02Z",
            "2026-09-29T00:15:00Z",
        )
        .expect("poll retried branch");
    assert_eq!(retry_lease.task_id, backend_lease.task_id);
    assert_eq!(retry_lease.attempt, 2);
    let retry_token = store.issue_runtime_task_token(&retry_lease).expect("token");
    store
        .start_runtime_task(&retry_token, "2026-09-29T00:05:03Z")
        .expect("start retry");
    store
        .complete_runtime_task(
            &retry_token,
            serde_json::json!({ "recovered": true }),
            "2026-09-29T00:05:04Z",
        )
        .expect("complete retry");

    // Wave 3: the diamond join unlocks after both build branches succeeded.
    let wave_three = advance_frontier(
        &mut store,
        &run_id,
        &repository,
        &base,
        "2026-09-29T00:05:05Z",
    );
    assert_eq!(wave_three, vec![task_id("ag-eeeeee")]);
    let (tests_lease, _) = store
        .poll_runtime_task(
            agent_stride::domain::Actor::new("tests-worker").expect("actor"),
            "2026-09-29T00:05:06Z",
            "2026-09-29T00:15:00Z",
        )
        .expect("poll tests");
    let tests_token = store.issue_runtime_task_token(&tests_lease).expect("token");
    store
        .start_runtime_task(&tests_token, "2026-09-29T00:05:07Z")
        .expect("start tests");
    store
        .complete_runtime_task(&tests_token, serde_json::json!({}), "2026-09-29T00:05:08Z")
        .expect("complete tests");

    // Wave 4: the fan-in join unlocks after tests AND docs succeeded.
    let wave_four = advance_frontier(
        &mut store,
        &run_id,
        &repository,
        &base,
        "2026-09-29T00:05:09Z",
    );
    assert_eq!(wave_four, vec![task_id("ag-ffffff")]);
    let (release_lease, _) = store
        .poll_runtime_task(
            agent_stride::domain::Actor::new("release-worker").expect("actor"),
            "2026-09-29T00:05:10Z",
            "2026-09-29T00:15:00Z",
        )
        .expect("poll release");
    let release_token = store
        .issue_runtime_task_token(&release_lease)
        .expect("token");
    store
        .start_runtime_task(&release_token, "2026-09-29T00:05:11Z")
        .expect("start release");
    store
        .complete_runtime_task(
            &release_token,
            serde_json::json!({}),
            "2026-09-29T00:05:12Z",
        )
        .expect("complete release");

    let final_state = store.runtime_state(&run_id).expect("final state");
    assert_eq!(
        final_state
            .tasks
            .values()
            .filter(|task| task.status == TaskStatus::Succeeded)
            .count(),
        6
    );
    let backend_task = final_state
        .tasks
        .get(&task_id("ag-bbbbbb"))
        .expect("backend task");
    assert_eq!(backend_task.attempt, 2);
    assert!(backend_task.failure.is_some());
    assert!(store.verify_runtime_state(&run_id).expect("verify").valid);
}

/// Reconcile the run against its current revision and return the tasks the
/// reconciler scheduled, in the order the store recorded them.
fn advance_frontier(
    store: &mut Store,
    run_id: &RunId,
    repository: &GitRepository,
    revision: &str,
    now: &str,
) -> Vec<TaskId> {
    let state = store.runtime_state(run_id).expect("runtime state");
    let graph = PlanGraph::from_git(repository, revision).expect("plan graph");
    let change = reconcile(&graph, &graph, &state, Vec::new(), "advance").expect("reconcile");
    let command = state
        .reconcile_plan(
            revision.to_owned(),
            change.discovered,
            change.ready,
            change.invalidated,
            change.superseded,
            now,
        )
        .expect("reconcile command");
    let result = store
        .apply_runtime_command(run_id, command)
        .expect("apply reconcile");
    result
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            agent_stride::runtime::EventKind::TaskScheduled { task_id } => Some(task_id.clone()),
            _ => None,
        })
        .collect()
}
