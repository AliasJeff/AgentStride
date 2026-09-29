use std::process::Command as ProcessCommand;

use tempfile::tempdir;

use agent_stride::{
    domain::{IssueDraft, IssueId, IssueKind, Priority},
    plan::{GitRepository, PlanGraph, PlanProposal, ProposalStatus, export_snapshot, reconcile},
    runtime::{Command, CommandKind, RunId},
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

#[test]
fn research_run_survives_restart_and_approved_plan_evolution() {
    let project = tempdir().expect("project");
    git(project.path(), &["init", "-q"]);
    git(
        project.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(project.path(), &["config", "user.name", "AgentStride"]);

    let database = project.path().join("runtime.db");
    let run_id = RunId::new("research-run").expect("run");
    let mut store = Store::open(&database).expect("open");
    store.migrate().expect("migrate");
    store
        .insert_issue(&issue(
            "ag-aaaaaa",
            "collect source A",
            "2026-09-29T00:00:00Z",
        ))
        .expect("insert source");
    export_snapshot(&store, project.path()).expect("export base plan");
    git(project.path(), &["add", ".tasks"]);
    git(project.path(), &["commit", "-qm", "base plan"]);
    let repository = GitRepository::new(project.path());
    let base = repository.current_revision().expect("base revision");

    store
        .start_run(
            run_id.clone(),
            &base,
            Command::new(
                "start-research-run",
                CommandKind::StartRun {
                    now: "2026-09-29T00:00:01Z".to_owned(),
                },
            )
            .expect("start command"),
        )
        .expect("start run");
    let base_graph = PlanGraph::from_git(&repository, &base).expect("read base graph");
    let state = store.runtime_state(&run_id).expect("state");
    let initial_change =
        reconcile(&base_graph, &base_graph, &state, Vec::new(), "initial load").expect("reconcile");
    store
        .apply_runtime_command(
            &run_id,
            state
                .reconcile_plan(
                    base.clone(),
                    initial_change.discovered,
                    initial_change.ready,
                    initial_change.invalidated,
                    initial_change.superseded,
                    "2026-09-29T00:00:02Z",
                )
                .expect("reconcile command"),
        )
        .expect("load base plan");

    let (lease, _) = store
        .poll_runtime_task(
            agent_stride::domain::Actor::new("researcher-a").expect("actor"),
            "2026-09-29T00:00:03Z",
            "2026-09-29T00:00:05Z",
        )
        .expect("claim source");
    drop(store);

    let mut store = Store::open(&database).expect("reopen after worker crash");
    store.migrate().expect("migrate after restart");
    store
        .insert_issue(&issue(
            "ag-bbbbbb",
            "cross-check sources",
            "2026-09-29T00:00:06Z",
        ))
        .expect("insert evolved task");
    export_snapshot(&store, project.path()).expect("export evolved plan");
    git(project.path(), &["add", ".tasks"]);
    git(project.path(), &["commit", "-qm", "evolved plan"]);
    let head = repository.current_revision().expect("head revision");

    store
        .create_plan_proposal(&PlanProposal {
            proposal_id: "research-proposal".to_owned(),
            run_id: run_id.to_string(),
            base_revision: base.clone(),
            head_revision: head.clone(),
            author: "planner".to_owned(),
            reason: "add cross-check after source collection".to_owned(),
            status: ProposalStatus::Proposed,
            created_at: "2026-09-29T00:00:07Z".to_owned(),
            approved_by: None,
            approved_at: None,
        })
        .expect("proposal");
    store
        .approve_plan_proposal(
            "research-proposal",
            "human-reviewer",
            "2026-09-29T00:00:08Z",
        )
        .expect("approve");

    let state = store.runtime_state(&run_id).expect("state after restart");
    let evolved_graph = PlanGraph::from_git(&repository, &head).expect("read evolved graph");
    let change = reconcile(
        &base_graph,
        &evolved_graph,
        &state,
        Vec::new(),
        "approved evolution",
    )
    .expect("evolved reconcile");
    let command = state
        .reconcile_plan(
            head.clone(),
            change.discovered,
            change.ready,
            change.invalidated,
            change.superseded,
            "2026-09-29T00:00:09Z",
        )
        .expect("evolved command");
    store
        .apply_runtime_command(&run_id, command)
        .expect("activate evolved plan");

    let report = store
        .recover_expired_runtime_tasks(&run_id, "2026-09-29T00:00:06Z", 0)
        .expect("recover crashed worker");
    assert_eq!(report.recovered, vec![lease.durable_task_id.clone()]);
    store
        .fire_runtime_timer(
            agent_stride::runtime::TimerId::new(format!("timer-{}", lease.durable_task_id))
                .expect("timer"),
            "2026-09-29T00:00:10Z",
        )
        .expect("fire retry");

    for index in 0..2 {
        let (retry, _) = store
            .poll_runtime_task(
                agent_stride::domain::Actor::new(format!("researcher-{}", index + 1))
                    .expect("actor"),
                "2026-09-29T00:00:11Z",
                "2026-09-29T00:01:11Z",
            )
            .expect("poll after evolution");
        let token = store.issue_runtime_task_token(&retry).expect("token");
        store
            .start_runtime_task(&token, "2026-09-29T00:00:12Z")
            .expect("start");
        store
            .complete_runtime_task(
                &token,
                serde_json::json!({ "worker": index + 1 }),
                "2026-09-29T00:00:13Z",
            )
            .expect("complete");
    }

    let final_state = store.runtime_state(&run_id).expect("final state");
    assert_eq!(final_state.plan_revision, head);
    assert_eq!(
        final_state
            .tasks
            .values()
            .filter(|task| task.status == agent_stride::runtime::TaskStatus::Succeeded)
            .count(),
        2
    );
    assert!(store.verify_runtime_state(&run_id).expect("verify").valid);
}
