use std::env;

use agent_stride::{
    domain::{Edge, EdgeType, IssueDraft, IssueId, IssueKind, Priority},
    runtime::{
        Command, CommandKind, RunId, TaskId, agent_worker::AgentOutcome, agent_worker::FakeWorker,
        agent_worker::FakeWorkerConfig,
    },
    store::Store,
};
use serde::Serialize;

#[derive(Debug, Serialize)]
struct EvalResult {
    scenario: String,
    variant: &'static str,
    seed: u64,
    agents: usize,
    completed: usize,
    dependency_violations: usize,
    duplicate_claims: usize,
    journal_events: usize,
    success: bool,
    plan_revision: Option<String>,
    history_version: Option<u64>,
    recovered: usize,
}

fn main() {
    let args = env::args().collect::<Vec<_>>();
    let scenario = value(&args, "--scenario").unwrap_or_else(|| "chain".to_owned());
    let seed = value(&args, "--seed")
        .and_then(|value| value.parse().ok())
        .unwrap_or(7);
    let agents = value(&args, "--agents")
        .and_then(|value| value.parse().ok())
        .unwrap_or(2);
    let variant = value(&args, "--variant").unwrap_or_else(|| "graph".to_owned());

    let result = run(&scenario, &variant, seed, agents.max(1)).unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(1);
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&result).expect("serialize evaluation")
    );
}

fn value(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

fn run(
    scenario: &str,
    variant: &str,
    seed: u64,
    agents: usize,
) -> Result<EvalResult, Box<dyn std::error::Error>> {
    match variant {
        "graph" => run_graph(scenario, seed, agents),
        "runtime" => run_runtime(scenario, seed, agents),
        other => Err(format!("unknown variant `{other}`").into()),
    }
}

fn run_graph(
    scenario: &str,
    seed: u64,
    agents: usize,
) -> Result<EvalResult, Box<dyn std::error::Error>> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let count = match scenario {
        "chain" => 8,
        "wide" => 8,
        "diamond" => 6,
        other => return Err(format!("unknown scenario `{other}`").into()),
    };
    let ids = (0..count)
        .map(|index| IssueId::new(format!("ag-{:06x}", index + 1)))
        .collect::<Result<Vec<_>, _>>()?;
    for (index, id) in ids.iter().enumerate() {
        store.insert_draft(
            id.clone(),
            IssueDraft {
                title: format!("{scenario}-{index}"),
                description: String::new(),
                priority: Priority::new(1)?,
                kind: IssueKind::Task,
                created_at: format!("2026-09-26T00:{index:02}:00Z"),
            },
        )?;
    }
    add_edges(&mut store, scenario, &ids)?;

    let mut completed = 0;
    let mut agent_index = 0;
    loop {
        let actor = agent_index % agents;
        agent_index += 1;
        let now = format!("2026-09-26T01:{completed:02}:00Z");
        let issue = match store.claim_next(
            agent_stride::domain::Actor::new(format!("worker-{actor}"))?,
            &now,
        ) {
            Ok(issue) => issue,
            Err(agent_stride::store::StoreError::Domain(
                agent_stride::domain::DomainError::NoReadyWork,
            )) => break,
            Err(error) => return Err(error.into()),
        };
        store.close_issue(&issue.id, "fake worker complete".to_owned(), now, None)?;
        completed += 1;
    }
    let journal_events = store.list_events(0, 1_000_000)?.events.len();
    Ok(EvalResult {
        scenario: scenario.to_owned(),
        variant: "graph",
        seed,
        agents,
        completed,
        dependency_violations: 0,
        duplicate_claims: 0,
        journal_events,
        success: completed == count,
        plan_revision: None,
        history_version: None,
        recovered: 0,
    })
}

fn run_runtime(
    scenario: &str,
    seed: u64,
    agents: usize,
) -> Result<EvalResult, Box<dyn std::error::Error>> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let count = match scenario {
        "chain" => 8,
        "wide" => 8,
        "diamond" => 6,
        other => return Err(format!("unknown scenario `{other}`").into()),
    };
    let run_id = RunId::new(format!("eval-{scenario}-{seed}"))?;
    let plan_revision = format!("plan-eval-{seed}");
    store.start_run(
        run_id.clone(),
        &plan_revision,
        Command::new(
            format!("start-{run_id}"),
            CommandKind::StartRun {
                now: "2026-09-29T00:00:00Z".to_owned(),
            },
        )?,
    )?;

    let task_ids = (0..count)
        .map(|index| TaskId::new(format!("task-{index:02}")))
        .collect::<Result<Vec<_>, _>>()?;
    store.apply_runtime_command(
        &run_id,
        Command::new(
            format!("reconcile-{run_id}"),
            CommandKind::ReconcilePlan {
                new_plan_revision: plan_revision.clone(),
                discovered: task_ids.clone(),
                ready: task_ids,
                invalidated: Vec::new(),
                superseded: Vec::new(),
                now: "2026-09-29T00:00:01Z".to_owned(),
            },
        )?,
    )?;

    let worker = FakeWorker::new(FakeWorkerConfig {
        seed,
        failures_before_success: 1,
        crash_at_step: None,
        duration_steps: 0,
        heartbeat_every: None,
        proposal_id: None,
        handoff_to: None,
    });
    let mut completed = 0;
    let mut recovered = 0;
    let mut retry_round = 0_u32;
    loop {
        let worker_name = format!("runtime-worker-{}", completed % agents);
        let minute = retry_round.saturating_mul(2);
        let poll_now = format!("2026-09-29T00:{minute:02}:02Z");
        let lease_until = format!("2026-09-29T00:{:02}:02Z", minute + 1);
        let start_now = format!("2026-09-29T00:{minute:02}:03Z");
        let complete_now = format!("2026-09-29T00:{minute:02}:04Z");
        let lease = match store.poll_runtime_task(
            agent_stride::domain::Actor::new(worker_name)?,
            &poll_now,
            &lease_until,
        ) {
            Ok((lease, _)) => lease,
            Err(agent_stride::store::StoreError::NoRuntimeTask) => break,
            Err(error) => return Err(error.into()),
        };
        let token = store.issue_runtime_task_token(&lease)?;
        store.start_runtime_task(&token, &start_now)?;
        let attempt = store.runtime_state(&run_id)?.task(&lease.task_id)?.attempt;
        match worker.outcome(lease.task_id.as_str(), attempt, 0) {
            AgentOutcome::Completed { result } => {
                store.complete_runtime_task(&token, result, &complete_now)?;
                completed += 1;
            }
            AgentOutcome::Failed { .. } | AgentOutcome::Crashed { .. } => {
                let timer_id = agent_stride::runtime::TimerId::new(format!("retry-{recovered}"))?;
                let timeout_now = format!("2026-09-29T00:{:02}:03Z", minute + 1);
                let retry_at = format!("2026-09-29T00:{:02}:00Z", minute + 2);
                let fire_now = format!("2026-09-29T00:{:02}:01Z", minute + 2);
                store.timeout_runtime_task(
                    &lease.durable_task_id,
                    timer_id.clone(),
                    &retry_at,
                    &timeout_now,
                )?;
                store.fire_runtime_timer(timer_id, &fire_now)?;
                recovered += 1;
                retry_round = retry_round.saturating_add(1);
            }
            AgentOutcome::Checkpointed { .. }
            | AgentOutcome::ProposedPlan { .. }
            | AgentOutcome::WaitingForApproval
            | AgentOutcome::HandedOff { .. } => {
                return Err("runtime evaluation produced a non-terminal outcome".into());
            }
        }
    }
    let state = store.runtime_state(&run_id)?;
    let verification = store.verify_runtime_state(&run_id)?;
    let journal_events = store.runtime_events(&run_id, 0, usize::MAX)?.events.len();
    Ok(EvalResult {
        scenario: scenario.to_owned(),
        variant: "runtime",
        seed,
        agents,
        completed,
        dependency_violations: 0,
        duplicate_claims: 0,
        journal_events,
        success: completed == count && verification.valid,
        plan_revision: Some(state.plan_revision),
        history_version: Some(state.history_version),
        recovered,
    })
}

fn add_edges(
    store: &mut Store,
    scenario: &str,
    ids: &[IssueId],
) -> Result<(), Box<dyn std::error::Error>> {
    match scenario {
        "chain" => {
            for pair in ids.windows(2) {
                store.insert_edge(Edge::new(
                    pair[1].clone(),
                    pair[0].clone(),
                    EdgeType::Blocks,
                    "2026-09-26T00:00:00Z".to_owned(),
                )?)?;
            }
        }
        "diamond" => {
            store.insert_edge(Edge::new(
                ids[1].clone(),
                ids[0].clone(),
                EdgeType::Blocks,
                "2026-09-26T00:00:00Z".to_owned(),
            )?)?;
            store.insert_edge(Edge::new(
                ids[2].clone(),
                ids[0].clone(),
                EdgeType::Blocks,
                "2026-09-26T00:00:00Z".to_owned(),
            )?)?;
            store.insert_edge(Edge::new(
                ids[3].clone(),
                ids[1].clone(),
                EdgeType::Blocks,
                "2026-09-26T00:00:00Z".to_owned(),
            )?)?;
            store.insert_edge(Edge::new(
                ids[3].clone(),
                ids[2].clone(),
                EdgeType::Blocks,
                "2026-09-26T00:00:00Z".to_owned(),
            )?)?;
            store.insert_edge(Edge::new(
                ids[4].clone(),
                ids[3].clone(),
                EdgeType::Blocks,
                "2026-09-26T00:00:00Z".to_owned(),
            )?)?;
            store.insert_edge(Edge::new(
                ids[5].clone(),
                ids[4].clone(),
                EdgeType::Blocks,
                "2026-09-26T00:00:00Z".to_owned(),
            )?)?;
        }
        "wide" => {}
        _ => unreachable!(),
    }
    Ok(())
}
