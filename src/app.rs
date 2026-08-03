use std::{
    env,
    fs::{self, File, OpenOptions},
    io::BufRead,
    io::{self, Write},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use serde::Serialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    cli::{
        AddArgs, CheckpointArgs, ClaimArgs, ClaimNextArgs, Cli, CloseArgs, Command, CompactArgs,
        ContextArgs, EventsArgs, HandoffArgs, HeartbeatArgs, ImportArgs, LeaseOwnerArgs, ListArgs,
        PlanCommand, PrimeArgs, ReadyArgs, ReclaimArgs, RememberArgs, ReplayArgs, RestoreArgs,
        RunArgs, RuntimeCommand, SetupArgs, ShowArgs, TokenArgs, UpdateArgs, WorkerCommand,
    },
    domain::{Actor, IssueId, IssuePatch, Priority, Revision, Status},
    output::OutputMode,
    plan::{
        GitRepository, PlanError, PlanGraph, PlanProposal, ProposalStatus, export_snapshot,
        reconcile,
    },
    runtime::{
        Command as RuntimeCommandInput, CommandKind, RunId, TaskId, TaskSupersession,
        agent_worker::{AgentRunResult, AgentRunner, AgentWorker, FakeWorker, FakeWorkerConfig},
        provider::OpenAiAgentWorker,
        token::RuntimeTaskToken,
    },
    store::{CompactionCandidate, CompactionMetadata, RuntimeStateResult, Store},
};

const WORKSPACE_DIR: &str = ".agent-stride";
const DATABASE_FILE: &str = "agent-stride.db";
const LEGACY_WORKSPACE_DIR: &str = ".agent-graph";
const LEGACY_DATABASE_FILE: &str = "agent-graph.db";

#[derive(Debug, Serialize)]
pub struct InitResult {
    pub workspace: PathBuf,
    pub database: PathBuf,
    pub created: bool,
}

#[derive(Debug)]
pub struct App;

impl App {
    pub fn run(cli: Cli) -> Result<(), AppError> {
        let mode = OutputMode::new(cli.json, cli.quiet);

        match cli.command {
            Command::Init(args) => {
                let quiet = cli.quiet || args.quiet;
                let result = initialize(cli.workspace.as_deref())?;
                if !quiet {
                    mode.print_json_or_human(&result, format_init(&result))?;
                }
                Ok(())
            }
            Command::Add(args) => {
                let (workspace, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let issue = add_issue(&workspace, &mut store, args)?;
                mode.print_json_or_human(&issue, format_issue(&issue))?;
                Ok(())
            }
            Command::List(args) => {
                let (_, store) = open_existing_store(cli.workspace.as_deref())?;
                let issues = list_issues(&store, args)?;
                mode.print_json_or_human(&issues, format_issue_list(&issues))?;
                Ok(())
            }
            Command::Show(args) => {
                let (_, store) = open_existing_store(cli.workspace.as_deref())?;
                let issue = show_issue(&store, args)?;
                mode.print_json_or_human(&issue, format_issue(&issue))?;
                Ok(())
            }
            Command::Update(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let issue = update_issue(&mut store, args)?;
                mode.print_json_or_human(&issue, format_issue(&issue))?;
                Ok(())
            }
            Command::Close(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let issue = close_issue(&mut store, args)?;
                mode.print_json_or_human(&issue, format_issue(&issue))?;
                Ok(())
            }
            Command::Link(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let edge = link_edge(&mut store, args)?;
                mode.print_json_or_human(&edge, format_edge(&edge))?;
                Ok(())
            }
            Command::Unlink(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let removed = unlink_edge(&mut store, args)?;
                mode.print_json_or_human(
                    &serde_json::json!({ "removed": removed }),
                    if removed {
                        "removed dependency".to_owned()
                    } else {
                        "dependency not found".to_owned()
                    },
                )?;
                Ok(())
            }
            Command::Graph(args) => {
                let (_, store) = open_existing_store(cli.workspace.as_deref())?;
                let edges = graph_edges(&store, args)?;
                mode.print_json_or_human(&edges, format_edge_list(&edges))?;
                Ok(())
            }
            Command::Ready(args) => {
                let (_, store) = open_existing_store(cli.workspace.as_deref())?;
                let issues = ready_issues(&store, args)?;
                mode.print_json_or_human(&issues, format_issue_list(&issues))?;
                Ok(())
            }
            Command::Epic(args) => {
                let (_, store) = open_existing_store(cli.workspace.as_deref())?;
                let summary = store.epic_summary(&IssueId::new(args.id)?)?;
                mode.print_json_or_human(&summary, format_epic_summary(&summary))?;
                Ok(())
            }
            Command::Blocked(_args) => {
                let (_, store) = open_existing_store(cli.workspace.as_deref())?;
                let issues = store.list_blocked()?;
                mode.print_json_or_human(&issues, format_issue_list(&issues))?;
                Ok(())
            }
            Command::Claim(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let issue = claim_issue(&mut store, args)?;
                mode.print_json_or_human(&issue, format_issue(&issue))?;
                Ok(())
            }
            Command::ClaimNext(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let issue = claim_next(&mut store, args)?;
                mode.print_json_or_human(&issue, format_issue(&issue))?;
                Ok(())
            }
            Command::Events(args) => {
                let (_, store) = open_existing_store(cli.workspace.as_deref())?;
                read_events(&store, args, mode)
            }
            Command::Export(_args) => {
                let (_, store) = open_existing_store(cli.workspace.as_deref())?;
                export_workspace(&store)
            }
            Command::Import(args) => import_workspace(args, mode),
            Command::Replay(args) => replay_workspace(args, mode),
            Command::Verify(args) => {
                let workspace = args.path.as_deref().or(cli.workspace.as_deref());
                let (_, store) = open_existing_store(workspace)?;
                let result = verify_workspace(&store)?;
                mode.print_json_or_human(&result, format_verify(&result))?;
                Ok(())
            }
            Command::Remember(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let memory = remember_memory(&mut store, args)?;
                mode.print_json_or_human(&memory, format_memory_record(&memory))?;
                Ok(())
            }
            Command::Recall(args) => {
                let (_, store) = open_existing_store(cli.workspace.as_deref())?;
                let memory = store
                    .get_memory(&args.key)?
                    .ok_or_else(|| AppError::MemoryNotFound { key: args.key })?;
                mode.print_json_or_human(&memory, format_memory(&memory))?;
                Ok(())
            }
            Command::Memories(args) => {
                let (_, store) = open_existing_store(cli.workspace.as_deref())?;
                let memories = store.list_memories(
                    args.search.as_deref(),
                    args.scope.as_deref(),
                    args.include_archived,
                    Some(&now()?),
                )?;
                mode.print_json_or_human(
                    &memories,
                    memories
                        .iter()
                        .map(format_memory)
                        .collect::<Vec<_>>()
                        .join("\n"),
                )?;
                Ok(())
            }
            Command::TouchMemories(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                store.mark_memories_used(&args.keys, &now()?)?;
                mode.print_json_or_human(
                    &serde_json::json!({ "touched": args.keys }),
                    "memory usage recorded".to_owned(),
                )?;
                Ok(())
            }
            Command::Forget(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let memory = store
                    .delete_memory(&args.key, &now()?)?
                    .ok_or_else(|| AppError::MemoryNotFound { key: args.key })?;
                mode.print_json_or_human(
                    &serde_json::json!({ "deleted": memory }),
                    format!("forgot {}", memory.key),
                )?;
                Ok(())
            }
            Command::GovernMemories(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let stale = store.archive_stale_memories(&now()?, args.dry_run)?;
                mode.print_json_or_human(
                    &serde_json::json!({
                        "dry_run": args.dry_run,
                        "archived": stale.iter().map(|memory| &memory.key).collect::<Vec<_>>(),
                        "count": stale.len(),
                    }),
                    format!(
                        "{} stale memor{}",
                        stale.len(),
                        if stale.len() == 1 { "y" } else { "ies" }
                    ),
                )?;
                Ok(())
            }
            Command::Checkpoint(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let checkpoint = save_checkpoint(&mut store, args)?;
                mode.print_json_or_human(&checkpoint, format_checkpoint(&checkpoint))?;
                Ok(())
            }
            Command::Context(args) => {
                let (_, store) = open_existing_store(cli.workspace.as_deref())?;
                let context = task_context(&store, args)?;
                mode.print_json_or_human(&context, format_task_context(&context))?;
                Ok(())
            }
            Command::Prime(args) => {
                let (workspace, store) = open_existing_store(cli.workspace.as_deref())?;
                let context = prime_context(&store, &workspace, args)?;
                mode.print_json_or_human(&context, crate::agent::prime::render_human(&context))?;
                Ok(())
            }
            Command::AgentHook(args) => {
                let (workspace, store) = open_existing_store(cli.workspace.as_deref())?;
                let event =
                    crate::agent::hooks::HookEvent::parse(&args.event).ok_or_else(|| {
                        AppError::InvalidArguments {
                            message: format!("unknown agent hook event `{}`", args.event),
                        }
                    })?;
                let result = crate::agent::hooks::handle(
                    &store,
                    &workspace,
                    event,
                    args.actor.as_deref(),
                    &args.session,
                )
                .map_err(|error| AppError::InvalidArguments {
                    message: error.to_string(),
                })?;
                let human = if let Some(context) = &result.context {
                    crate::agent::prime::render_human(context)
                } else if result.refresh {
                    "context refresh requested".to_owned()
                } else {
                    "hook handled".to_owned()
                };
                mode.print_json_or_human(&result, human)?;
                Ok(())
            }
            Command::Heartbeat(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let lease = heartbeat(&mut store, args)?;
                mode.print_json_or_human(&lease, format_lease(&lease))?;
                Ok(())
            }
            Command::Release(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let issue = release_lease(&mut store, args)?;
                mode.print_json_or_human(&issue, format_issue(&issue))?;
                Ok(())
            }
            Command::Reclaim(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let issue = reclaim_lease(&mut store, args)?;
                mode.print_json_or_human(&issue, format_issue(&issue))?;
                Ok(())
            }
            Command::Handoff(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let handoff = handoff(&mut store, args)?;
                mode.print_json_or_human(&handoff, format_handoff(&handoff))?;
                Ok(())
            }
            Command::Token(args) => {
                let (_, store) = open_existing_store(cli.workspace.as_deref())?;
                let token = issue_runner_token(&store, args)?;
                mode.print_json_or_human(&serde_json::json!({ "token": token }), token)?;
                Ok(())
            }
            Command::Run(args) => {
                let (_, store) = open_existing_store(cli.workspace.as_deref())?;
                let result = run_runner_command(&store, args)?;
                mode.print_json_or_human(
                    &result,
                    format!(
                        "command {}",
                        if result.success {
                            "succeeded"
                        } else {
                            "failed"
                        }
                    ),
                )?;
                if result.success {
                    Ok(())
                } else {
                    Err(AppError::InvalidArguments {
                        message: "runner command exited unsuccessfully".to_owned(),
                    })
                }
            }
            Command::Setup(args) => {
                let report = setup_agent(args)?;
                mode.print_json_or_human(
                    &report,
                    if report.drifted {
                        "integration drift detected".to_owned()
                    } else if report.removed {
                        "integration removed".to_owned()
                    } else if report.changed {
                        "integration installed".to_owned()
                    } else {
                        "integration is current".to_owned()
                    },
                )?;
                if report.drifted {
                    return Err(AppError::InvalidArguments {
                        message: "integration files are out of date".to_owned(),
                    });
                }
                Ok(())
            }
            Command::Compact(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let result = compact_issue(&mut store, args)?;
                match &result {
                    CompactResult::Applied { issue } => {
                        mode.print_json_or_human(issue, format_issue(issue))?
                    }
                    _ => mode.print_json_or_human(&result, format_compact_result(&result))?,
                }
                Ok(())
            }
            Command::Restore(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let issue = restore_issue(&mut store, args)?;
                mode.print_json_or_human(&issue, format_issue(&issue))?;
                Ok(())
            }
            Command::Mcp(_args) => {
                crate::mcp::serve(cli.workspace.as_deref()).map_err(|source| AppError::Io {
                    operation: "run MCP server",
                    path: PathBuf::from("stdin/stdout"),
                    source,
                })
            }
            Command::Send(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let message = store.send_message(
                    &args.from,
                    &args.to,
                    &args.subject,
                    &args.body,
                    args.thread.as_deref(),
                    args.parent.as_deref(),
                    &args.kind,
                    &now()?,
                )?;
                mode.print_json_or_human(&message, format_message(&message))?;
                Ok(())
            }
            Command::Inbox(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let mut messages = store.list_inbox(&args.actor, args.thread.as_deref())?;
                if args.mark_read {
                    let timestamp = now()?;
                    for message in &messages {
                        store.mark_message_read(&message.id, &timestamp)?;
                    }
                    messages = store.list_inbox(&args.actor, args.thread.as_deref())?;
                }
                mode.print_json_or_human(
                    &messages,
                    messages
                        .iter()
                        .map(format_message)
                        .collect::<Vec<_>>()
                        .join("\n"),
                )?;
                Ok(())
            }
            Command::Thread(args) => {
                let (_, store) = open_existing_store(cli.workspace.as_deref())?;
                let messages = store.list_thread(&args.id)?;
                mode.print_json_or_human(
                    &messages,
                    messages
                        .iter()
                        .map(format_message)
                        .collect::<Vec<_>>()
                        .join("\n"),
                )?;
                Ok(())
            }
            Command::Reply(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let thread = store.list_thread(&args.id)?;
                let parent = thread.last().map(|message| message.id.as_str());
                let subject = thread
                    .first()
                    .map(|message| format!("Re: {}", message.subject))
                    .unwrap_or_default();
                let message = store.send_message(
                    &args.from,
                    &args.to,
                    &subject,
                    &args.body,
                    Some(&args.id),
                    parent,
                    "reply",
                    &now()?,
                )?;
                mode.print_json_or_human(&message, format_message(&message))?;
                Ok(())
            }
            Command::Runtime(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                match args.command {
                    RuntimeCommand::Start(args) => {
                        let run_id = RunId::new(args.run_id)?;
                        let command = RuntimeCommandInput::new(
                            format!("start-{run_id}"),
                            CommandKind::StartRun { now: now()? },
                        )?;
                        let result = store.start_run(run_id, &args.plan, command)?;
                        mode.print_json_or_human(
                            &result,
                            format!(
                                "started run {} at plan {}",
                                result.state.run_id, result.state.plan_revision
                            ),
                        )?;
                    }
                    RuntimeCommand::Show(args) => {
                        let run_id = RunId::new(args.run_id)?;
                        let state = store.runtime_state(&run_id)?;
                        mode.print_json_or_human(
                            &state,
                            format!(
                                "run {}: {:?}, plan {}, history {}",
                                state.run_id,
                                state.status,
                                state.plan_revision,
                                state.history_version
                            ),
                        )?;
                    }
                    RuntimeCommand::Events(args) => {
                        let run_id = RunId::new(args.run_id)?;
                        let page = store.runtime_events(&run_id, args.since, args.limit)?;
                        mode.print_json_or_human(
                            &page,
                            format!("{} runtime event(s), head {}", page.events.len(), page.head),
                        )?;
                    }
                    RuntimeCommand::Recover(args) => {
                        let run_id = RunId::new(args.run_id)?;
                        let report = store.recover_expired_runtime_tasks(
                            &run_id,
                            &now()?,
                            args.retry_delay,
                        )?;
                        mode.print_json_or_human(
                            &report,
                            format!("recovered {} task(s)", report.recovered.len()),
                        )?;
                    }
                    RuntimeCommand::Fire(args) => {
                        let timer_id = crate::runtime::TimerId::new(args.timer_id)?;
                        let result = store.fire_runtime_timer(timer_id, &now()?)?;
                        mode.print_json_or_human(
                            &result,
                            format!("fired timer; {} event(s)", result.events.len()),
                        )?;
                    }
                }
                Ok(())
            }
            Command::Worker(args) => {
                let (_, mut store) = open_existing_store(cli.workspace.as_deref())?;
                match args.command {
                    WorkerCommand::Run(args) => {
                        let worker = Actor::new(args.worker)?;
                        let started_at = now()?;
                        let lease_until = after_seconds(&started_at, args.lease_seconds)?;
                        let result = match args.provider.as_str() {
                            "fake" => {
                                let fake_worker = FakeWorker::new(FakeWorkerConfig {
                                    seed: args.seed,
                                    failures_before_success: args.failures_before_success,
                                    crash_at_step: args.crash_at_step,
                                    duration_steps: args.duration_steps,
                                    heartbeat_every: None,
                                    proposal_id: args.proposal_id,
                                    handoff_to: args.handoff_to,
                                });
                                run_agent_worker_once(
                                    &mut store,
                                    worker,
                                    fake_worker,
                                    &started_at,
                                    &lease_until,
                                    args.max_steps,
                                )?
                            }
                            "openai" => {
                                let openai_worker =
                                    OpenAiAgentWorker::from_env(args.model, args.timeout_seconds)?;
                                run_agent_worker_once(
                                    &mut store,
                                    worker,
                                    openai_worker,
                                    &started_at,
                                    &lease_until,
                                    args.max_steps,
                                )?
                            }
                            provider => {
                                return Err(AppError::InvalidArguments {
                                    message: format!("unsupported worker provider `{provider}`"),
                                });
                            }
                        };
                        mode.print_json_or_human(&result, format!("worker run: {:?}", result))?;
                    }
                    WorkerCommand::Poll(args) => {
                        let worker = Actor::new(args.worker)?;
                        let (lease, result) =
                            store.poll_runtime_task(worker, &now()?, &args.lease_until)?;
                        let token = store.issue_runtime_task_token(&lease)?.encode()?;
                        mode.print_json_or_human(
                            &serde_json::json!({
                                "lease": lease,
                                "token": token,
                                "state": result.state,
                                "events": result.events,
                            }),
                            "task leased".to_owned(),
                        )?;
                    }
                    WorkerCommand::Start(args) => {
                        let token = RuntimeTaskToken::decode(&args.token)?;
                        let result = store.start_runtime_task(&token, &now()?)?;
                        mode.print_json_or_human(
                            &result,
                            format!("started attempt {}", token.attempt_id),
                        )?;
                    }
                    WorkerCommand::Heartbeat(args) => {
                        let token = RuntimeTaskToken::decode(&args.token)?;
                        let lease =
                            store.heartbeat_runtime_task(&token, &now()?, &args.lease_until)?;
                        let refreshed_token = store.issue_runtime_task_token(&lease)?.encode()?;
                        mode.print_json_or_human(
                            &serde_json::json!({
                                "lease": lease,
                                "token": refreshed_token,
                            }),
                            "lease extended".to_owned(),
                        )?;
                    }
                    WorkerCommand::Complete(args) => {
                        let token = RuntimeTaskToken::decode(&args.token)?;
                        let result_value = serde_json::from_str(&args.result)?;
                        let result = store.complete_runtime_task(&token, result_value, &now()?)?;
                        mode.print_json_or_human(
                            &result,
                            format!("completed task {}", token.task_id),
                        )?;
                    }
                    WorkerCommand::Fail(args) => {
                        let token = RuntimeTaskToken::decode(&args.token)?;
                        let result = store.fail_runtime_task(&token, &args.error, &now()?)?;
                        mode.print_json_or_human(
                            &result,
                            format!("failed task {}", token.task_id),
                        )?;
                    }
                    WorkerCommand::Handoff(args) => {
                        let token = RuntimeTaskToken::decode(&args.token)?;
                        let to_worker = args.to_worker.map(Actor::new).transpose()?;
                        let result = store.handoff_runtime_task(
                            &token,
                            to_worker,
                            &args.summary,
                            &args.artifacts,
                            &args.next_step,
                            &now()?,
                        )?;
                        mode.print_json_or_human(
                            &result,
                            format!("handed off task {}", token.task_id),
                        )?;
                    }
                }
                Ok(())
            }
            Command::Plan(args) => {
                let (workspace, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let project_root = workspace.parent().unwrap_or(&workspace);
                let repository = GitRepository::new(project_root);
                match args.command {
                    PlanCommand::Export(_) => {
                        let snapshot = export_snapshot(&store, project_root)?;
                        mode.print_json_or_human(
                            &snapshot,
                            format!(
                                "exported {} issue(s) and {} edge(s) to {}",
                                snapshot.issues,
                                snapshot.edges,
                                snapshot.root.display()
                            ),
                        )?;
                    }
                    PlanCommand::Status(_) => {
                        let status = repository.status()?;
                        mode.print_json_or_human(
                            &status,
                            if status.dirty {
                                format!("plan revision {} has uncommitted changes", status.revision)
                            } else {
                                format!("plan revision {} is clean", status.revision)
                            },
                        )?;
                    }
                    PlanCommand::Diff(args) => {
                        let diff = repository.diff(&args.base, args.head.as_deref())?;
                        if cli.json {
                            mode.print_json_or_human(
                                &serde_json::json!({
                                    "base": args.base,
                                    "head": args.head,
                                    "diff": diff,
                                }),
                                diff,
                            )?;
                        } else {
                            print!("{diff}");
                        }
                    }
                    PlanCommand::Propose(args) => {
                        let proposal = PlanProposal {
                            proposal_id: args.id,
                            run_id: args.run_id,
                            base_revision: args.base,
                            head_revision: args.head,
                            author: args.author,
                            reason: args.reason,
                            status: ProposalStatus::Proposed,
                            created_at: now()?,
                            approved_by: None,
                            approved_at: None,
                        };
                        let result = store.create_plan_proposal(&proposal)?;
                        mode.print_json_or_human(
                            &result,
                            format!("created plan proposal {}", result.proposal_id),
                        )?;
                    }
                    PlanCommand::Approve(args) => {
                        let result =
                            store.approve_plan_proposal(&args.proposal_id, &args.actor, &now()?)?;
                        mode.print_json_or_human(
                            &result,
                            format!("approved plan proposal {}", result.proposal_id),
                        )?;
                    }
                    PlanCommand::Activate(args) => {
                        let result = reconcile_run(
                            &workspace,
                            &mut store,
                            &args.run_id,
                            &args.revision,
                            "approved plan revision",
                            &[],
                        )?;
                        mode.print_json_or_human(
                            &result,
                            format!(
                                "activated plan {} for run {} ({} event(s))",
                                result.state.plan_revision,
                                result.state.run_id,
                                result.events.len()
                            ),
                        )?;
                    }
                }
                Ok(())
            }
            Command::Reconcile(args) => {
                let (workspace, mut store) = open_existing_store(cli.workspace.as_deref())?;
                let result = reconcile_run(
                    &workspace,
                    &mut store,
                    &args.run_id,
                    &args.revision,
                    &args.reason,
                    &args.supersede,
                )?;
                mode.print_json_or_human(
                    &result,
                    format!(
                        "reconciled run {} to {} ({} event(s))",
                        result.state.run_id,
                        result.state.plan_revision,
                        result.events.len()
                    ),
                )?;
                Ok(())
            }
            Command::Ui(args) => {
                let (workspace, _store) = open_existing_store(cli.workspace.as_deref())?;
                crate::web::serve(&workspace, args.run.as_deref(), args.port).map_err(|error| {
                    AppError::Ui {
                        message: error.to_string(),
                    }
                })
            }
        }
    }
}

fn reconcile_run(
    workspace: &Path,
    store: &mut Store,
    run_id_value: &str,
    revision: &str,
    reason: &str,
    supersession_values: &[String],
) -> Result<RuntimeStateResult, AppError> {
    let run_id = RunId::new(run_id_value)?;
    let state = store.runtime_state(&run_id)?;
    if state.plan_revision != revision {
        store.ensure_approved_plan_revision(run_id.as_str(), revision)?;
    }
    let project_root = workspace.parent().unwrap_or(workspace);
    let repository = GitRepository::new(project_root);
    let old_plan = PlanGraph::from_git(&repository, &state.plan_revision)?;
    let new_plan = PlanGraph::from_git(&repository, revision)?;
    let superseded = parse_supersessions(supersession_values, reason)?;
    let change = reconcile(&old_plan, &new_plan, &state, superseded, reason)?;
    let command = state.reconcile_plan(
        revision.to_owned(),
        change.discovered,
        change.ready,
        change.invalidated,
        change.superseded,
        now()?,
    )?;
    Ok(store.apply_runtime_command(&run_id, command)?)
}

fn parse_supersessions(values: &[String], reason: &str) -> Result<Vec<TaskSupersession>, AppError> {
    values
        .iter()
        .map(|value| {
            let (old, replacement) =
                value
                    .split_once('=')
                    .ok_or_else(|| AppError::InvalidArguments {
                        message: format!("supersession must use OLD=NEW syntax: `{value}`"),
                    })?;
            if old.trim().is_empty() || replacement.trim().is_empty() {
                return Err(AppError::InvalidArguments {
                    message: format!("supersession must use OLD=NEW syntax: `{value}`"),
                });
            }
            Ok(TaskSupersession {
                task_id: TaskId::new(old.trim())?,
                replacement_task_id: TaskId::new(replacement.trim())?,
                reason: reason.to_owned(),
            })
        })
        .collect()
}

fn format_message(message: &crate::store::Message) -> String {
    format!(
        "[{}] {} -> {} ({})\n{}\n{}",
        message.thread_id,
        message.from_actor,
        message.to_actor,
        message.kind,
        message.subject,
        message.body
    )
}

fn initialize(explicit_workspace: Option<&Path>) -> Result<InitResult, AppError> {
    let workspace = resolve_init_workspace(explicit_workspace)?;
    fs::create_dir_all(&workspace).map_err(|source| AppError::Io {
        operation: "create workspace directory",
        path: workspace.clone(),
        source,
    })?;

    let database = workspace.join(DATABASE_FILE);
    let created = !database.exists();
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&database)
        .map_err(|source| AppError::Io {
            operation: "create workspace database",
            path: database.clone(),
            source,
        })?
        .sync_all()
        .map_err(|source| AppError::Io {
            operation: "flush workspace database",
            path: database.clone(),
            source,
        })?;
    let mut store = Store::open(&database)?;
    store.migrate()?;
    store.ensure_workspace_id()?;

    Ok(InitResult {
        workspace,
        database,
        created,
    })
}

fn open_existing_store(explicit_workspace: Option<&Path>) -> Result<(PathBuf, Store), AppError> {
    let workspace = resolve_existing_workspace(explicit_workspace)?;
    let database = database_path(&workspace);
    let mut store = Store::open(&database)?;
    store.migrate()?;
    store.ensure_workspace_id()?;
    Ok((workspace, store))
}

fn add_issue(
    _workspace: &Path,
    store: &mut Store,
    args: AddArgs,
) -> Result<crate::domain::Issue, AppError> {
    let priority = Priority::new(args.priority)?;
    let created_at = now()?;
    let workspace_id = store.ensure_workspace_id()?;

    for nonce in 0..=u32::MAX {
        let id = IssueId::generate(
            &workspace_id,
            &args.title,
            &args.description,
            &created_at,
            nonce,
        );
        if store.get_issue(&id)?.is_none() {
            return Ok(store.insert_draft(
                id,
                crate::domain::IssueDraft {
                    title: args.title,
                    description: args.description,
                    priority,
                    kind: crate::domain::IssueKind::parse(&args.kind)?,
                    created_at,
                },
            )?);
        }
    }

    Err(AppError::IdCollisionExhausted)
}

fn list_issues(store: &Store, args: ListArgs) -> Result<Vec<crate::domain::Issue>, AppError> {
    let issues = store.list_issues()?;
    let Some(status) = args.status else {
        return Ok(issues);
    };
    let status = Status::parse(&status)?;
    Ok(issues
        .into_iter()
        .filter(|issue| issue.status == status)
        .collect())
}

fn show_issue(store: &Store, args: ShowArgs) -> Result<crate::domain::Issue, AppError> {
    let id = IssueId::new(args.id)?;
    store
        .get_issue(&id)?
        .ok_or_else(|| crate::store::StoreError::IssueNotFound { id: id.to_string() })
        .map_err(AppError::from)
}

fn update_issue(store: &mut Store, args: UpdateArgs) -> Result<crate::domain::Issue, AppError> {
    if args.title.is_none() && args.description.is_none() && args.priority.is_none() {
        return Err(AppError::InvalidArguments {
            message: "update requires at least one field".to_owned(),
        });
    }
    let id = IssueId::new(args.id)?;
    let priority = args.priority.map(Priority::new).transpose()?;
    Ok(store.update_issue(
        &id,
        IssuePatch {
            title: args.title,
            description: args.description,
            priority,
        },
        now()?,
        args.if_revision.map(Revision::new),
    )?)
}

fn close_issue(store: &mut Store, args: CloseArgs) -> Result<crate::domain::Issue, AppError> {
    let id = IssueId::new(args.id)?;
    Ok(store.close_issue(
        &id,
        args.reason,
        now()?,
        args.if_revision.map(Revision::new),
    )?)
}

fn claim_issue(store: &mut Store, args: ClaimArgs) -> Result<crate::domain::Issue, AppError> {
    let id = IssueId::new(args.id)?;
    let actor = Actor::new(args.actor)?;
    let current = now()?;
    match args.lease_seconds {
        Some(seconds) if seconds > 0 => {
            let until = after_seconds(&current, seconds)?;
            Ok(store.claim_issue_with_lease(&id, actor, &current, &until)?)
        }
        Some(_) => Err(AppError::InvalidArguments {
            message: "lease-seconds must be positive".to_owned(),
        }),
        None => Ok(store.claim_issue(&id, actor, &current, None)?),
    }
}

fn claim_next(store: &mut Store, args: ClaimNextArgs) -> Result<crate::domain::Issue, AppError> {
    let actor = Actor::new(args.actor)?;
    let current = now()?;
    match args.lease_seconds {
        Some(seconds) if seconds > 0 => {
            let until = after_seconds(&current, seconds)?;
            Ok(store.claim_next_with_lease(actor, &current, &until)?)
        }
        Some(_) => Err(AppError::InvalidArguments {
            message: "lease-seconds must be positive".to_owned(),
        }),
        None => Ok(store.claim_next(actor, &current)?),
    }
}

fn link_edge(
    store: &mut Store,
    args: crate::cli::LinkArgs,
) -> Result<crate::domain::Edge, AppError> {
    let source = IssueId::new(args.source)?;
    let (target, edge_type) = match (args.blocks, args.parent, args.related) {
        (Some(target), None, None) => (target, crate::domain::EdgeType::Blocks),
        (None, Some(target), None) => (target, crate::domain::EdgeType::ParentChild),
        (None, None, Some(target)) => (target, crate::domain::EdgeType::Related),
        (None, None, None) => {
            return Err(AppError::InvalidArguments {
                message: "link requires one of --blocks, --parent, or --related".to_owned(),
            });
        }
        _ => unreachable!("clap enforces mutually exclusive link options"),
    };
    let target = IssueId::new(target)?;
    Ok(store.insert_edge(crate::domain::Edge::new(source, target, edge_type, now()?)?)?)
}

fn unlink_edge(store: &mut Store, args: crate::cli::UnlinkArgs) -> Result<bool, AppError> {
    Ok(store.remove_edge(
        &IssueId::new(args.source)?,
        &IssueId::new(args.target)?,
        &now()?,
    )?)
}

fn read_events(store: &Store, args: EventsArgs, mode: OutputMode) -> Result<(), AppError> {
    if args.follow {
        return follow_events(store, args.since, args.limit);
    }

    let page = store.list_events(args.since, args.limit)?;
    mode.print_json_or_human(&page, format_event_page(&page)?)?;
    Ok(())
}

fn follow_events(store: &Store, mut since: u64, limit: usize) -> Result<(), AppError> {
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    loop {
        let page = store.list_events(since, limit)?;
        for event in page.events {
            serde_json::to_writer(&mut stdout, &event)?;
            stdout.write_all(b"\n").map_err(|source| AppError::Io {
                operation: "write event stream",
                path: PathBuf::from("-"),
                source,
            })?;
            stdout.flush().map_err(|source| AppError::Io {
                operation: "flush event stream",
                path: PathBuf::from("-"),
                source,
            })?;
            since = event.seq;
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn format_event_page(page: &crate::store::JournalPage) -> Result<String, AppError> {
    page.events
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .map(|events| events.join("\n"))
        .map_err(AppError::from)
}

#[derive(Debug, Serialize)]
struct ReplayResult {
    applied: usize,
    skipped: usize,
}

#[derive(Debug, Serialize)]
struct VerifyResult {
    valid: bool,
    issues: usize,
    edges: usize,
}

fn export_workspace(store: &Store) -> Result<(), AppError> {
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    for issue in store.list_issues()? {
        write_jsonl_record(&mut stdout, "issue", &issue)?;
    }
    for edge in store.list_edges()? {
        write_jsonl_record(&mut stdout, "edge", &edge)?;
    }
    stdout.flush().map_err(|source| AppError::Io {
        operation: "flush export stream",
        path: PathBuf::from("-"),
        source,
    })?;
    Ok(())
}

fn import_workspace(args: ImportArgs, mode: OutputMode) -> Result<(), AppError> {
    let initialized = initialize(Some(&args.into))?;
    let (_, mut store) = open_existing_store(Some(&initialized.workspace))?;
    let issues = snapshot_issues(&args.input)?;
    let edges = snapshot_edges(&args.input)?;
    let result = store.import_snapshot(issues, edges)?;
    mode.print_json_or_human(
        &result,
        format!(
            "imported {} issue(s) and {} edge(s)",
            result.issues, result.edges
        ),
    )?;
    Ok(())
}

fn snapshot_issues(
    path: &Path,
) -> Result<impl Iterator<Item = Result<crate::domain::Issue, crate::store::StoreError>>, AppError>
{
    let file = File::open(path).map_err(|source| AppError::Io {
        operation: "open snapshot input",
        path: path.to_path_buf(),
        source,
    })?;
    Ok(io::BufReader::new(file)
        .lines()
        .enumerate()
        .filter_map(|(index, line)| match line {
            Ok(line) if line.trim().is_empty() => None,
            Ok(line) => match snapshot_kind(index + 1, &line) {
                Ok(kind) if kind == "edge" => None,
                Ok(_) => Some(parse_snapshot_issue(index + 1, &line)),
                Err(error) => Some(Err(error)),
            },
            Err(error) => Some(Err(crate::store::StoreError::ReplayConflict {
                message: format!("snapshot line {}: {error}", index + 1),
            })),
        }))
}

fn snapshot_edges(
    path: &Path,
) -> Result<impl Iterator<Item = Result<crate::domain::Edge, crate::store::StoreError>>, AppError> {
    let file = File::open(path).map_err(|source| AppError::Io {
        operation: "open snapshot input",
        path: path.to_path_buf(),
        source,
    })?;
    Ok(io::BufReader::new(file)
        .lines()
        .enumerate()
        .filter_map(|(index, line)| match line {
            Ok(line) if line.trim().is_empty() => None,
            Ok(line) => match snapshot_kind(index + 1, &line) {
                Ok(kind) if kind == "issue" => None,
                Ok(_) => Some(parse_snapshot_edge(index + 1, &line)),
                Err(error) => Some(Err(error)),
            },
            Err(error) => Some(Err(crate::store::StoreError::ReplayConflict {
                message: format!("snapshot line {}: {error}", index + 1),
            })),
        }))
}

fn snapshot_kind(line_number: usize, line: &str) -> Result<String, crate::store::StoreError> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|error| crate::store::StoreError::ReplayConflict {
            message: format!("snapshot line {line_number}: {error}"),
        })?;
    value
        .get("record_kind")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| crate::store::StoreError::ReplayConflict {
            message: format!("snapshot line {line_number}: missing string field `record_kind`"),
        })
}

fn parse_snapshot_issue(
    line_number: usize,
    line: &str,
) -> Result<crate::domain::Issue, crate::store::StoreError> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|error| crate::store::StoreError::ReplayConflict {
            message: format!("snapshot line {line_number}: {error}"),
        })?;
    if value.get("record_kind").and_then(serde_json::Value::as_str) != Some("issue") {
        return Err(crate::store::StoreError::ReplayConflict {
            message: format!("snapshot line {line_number}: expected kind `issue`"),
        });
    }
    let mut object =
        value
            .as_object()
            .cloned()
            .ok_or_else(|| crate::store::StoreError::ReplayConflict {
                message: format!("snapshot line {line_number}: expected JSON object"),
            })?;
    object.remove("record_kind");
    serde_json::from_value(serde_json::Value::Object(object)).map_err(|error| {
        crate::store::StoreError::ReplayConflict {
            message: format!("snapshot line {line_number}: {error}"),
        }
    })
}

fn parse_snapshot_edge(
    line_number: usize,
    line: &str,
) -> Result<crate::domain::Edge, crate::store::StoreError> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|error| crate::store::StoreError::ReplayConflict {
            message: format!("snapshot line {line_number}: {error}"),
        })?;
    if value.get("record_kind").and_then(serde_json::Value::as_str) != Some("edge") {
        return Err(crate::store::StoreError::ReplayConflict {
            message: format!("snapshot line {line_number}: expected kind `edge`"),
        });
    }
    let mut object =
        value
            .as_object()
            .cloned()
            .ok_or_else(|| crate::store::StoreError::ReplayConflict {
                message: format!("snapshot line {line_number}: expected JSON object"),
            })?;
    object.remove("record_kind");
    serde_json::from_value(serde_json::Value::Object(object)).map_err(|error| {
        crate::store::StoreError::ReplayConflict {
            message: format!("snapshot line {line_number}: {error}"),
        }
    })
}

fn write_jsonl_record<T: Serialize>(
    writer: &mut impl Write,
    kind: &str,
    value: &T,
) -> Result<(), AppError> {
    let mut record = serde_json::to_value(value)?
        .as_object()
        .cloned()
        .ok_or_else(|| AppError::InvalidArguments {
            message: format!("{kind} export must be a JSON object"),
        })?;
    record.insert(
        "record_kind".to_owned(),
        serde_json::Value::String(kind.to_owned()),
    );
    serde_json::to_writer(&mut *writer, &record)?;
    writer.write_all(b"\n").map_err(|source| AppError::Io {
        operation: "write export stream",
        path: PathBuf::from("-"),
        source,
    })?;
    Ok(())
}

fn replay_workspace(args: ReplayArgs, mode: OutputMode) -> Result<(), AppError> {
    let initialized = initialize(Some(&args.into))?;
    let (_, mut store) = open_existing_store(Some(&initialized.workspace))?;
    let input = File::open(&args.input).map_err(|source| AppError::Io {
        operation: "open replay input",
        path: args.input.clone(),
        source,
    })?;
    let reader = io::BufReader::new(input);
    let mut applied = 0;
    let mut skipped = 0;

    for (index, line) in reader.lines().enumerate() {
        let line_number = index + 1;
        let line = line.map_err(|source| AppError::Io {
            operation: "read replay input",
            path: args.input.clone(),
            source,
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let event: crate::store::JournalEvent =
            serde_json::from_str(&line).map_err(|source| AppError::Replay {
                line: line_number,
                message: source.to_string(),
            })?;
        if store
            .replay_event(&event)
            .map_err(|source| AppError::Replay {
                line: line_number,
                message: source.to_string(),
            })?
        {
            applied += 1;
        } else {
            skipped += 1;
        }
    }

    let result = ReplayResult { applied, skipped };
    mode.print_json_or_human(
        &result,
        format!(
            "replayed {} event(s), skipped {} duplicate(s)",
            result.applied, result.skipped
        ),
    )?;
    Ok(())
}

fn verify_workspace(store: &Store) -> Result<VerifyResult, AppError> {
    let issues = store.list_issues()?;
    for issue in &issues {
        crate::domain::IssueId::new(issue.id.as_str())?;
        issue.validate()?;
    }

    let edges = store.list_edges()?;
    let mut accepted = Vec::new();
    for edge in &edges {
        if edge.edge_type.is_scheduling() {
            crate::graph::would_create_cycle(&edge.source_id, &edge.target_id, accepted.clone())?;
        }
        accepted.push(edge.clone());
    }

    Ok(VerifyResult {
        valid: true,
        issues: issues.len(),
        edges: edges.len(),
    })
}

fn format_verify(result: &VerifyResult) -> String {
    format!(
        "verified workspace: {} issue(s), {} edge(s)",
        result.issues, result.edges
    )
}

fn graph_edges(
    store: &Store,
    args: crate::cli::GraphArgs,
) -> Result<Vec<crate::domain::Edge>, AppError> {
    let edges = store.list_edges()?;
    let Some(id) = args.id else {
        return Ok(edges);
    };
    let id = IssueId::new(id)?;
    Ok(edges
        .into_iter()
        .filter(|edge| edge.source_id == id || edge.target_id == id)
        .collect())
}

fn resolve_existing_workspace(explicit_workspace: Option<&Path>) -> Result<PathBuf, AppError> {
    if let Some(path) = explicit_workspace {
        let workspace = normalize_workspace_path(path);
        if workspace.is_dir() && existing_database_path(&workspace).is_some() {
            return Ok(workspace);
        }
        return Err(AppError::WorkspaceNotInitialized);
    }

    let current = env::current_dir().map_err(|source| AppError::Io {
        operation: "read current directory",
        path: PathBuf::from("."),
        source,
    })?;
    for directory in current.ancestors() {
        let candidate = directory.join(WORKSPACE_DIR);
        if candidate.is_dir() && existing_database_path(&candidate).is_some() {
            return Ok(candidate);
        }
        let legacy_candidate = directory.join(LEGACY_WORKSPACE_DIR);
        if legacy_candidate.is_dir() && existing_database_path(&legacy_candidate).is_some() {
            return Ok(legacy_candidate);
        }
    }
    Err(AppError::WorkspaceNotInitialized)
}

fn now() -> Result<String, AppError> {
    Ok(OffsetDateTime::now_utc().format(&Rfc3339)?)
}

fn format_issue(issue: &crate::domain::Issue) -> String {
    let assignee = issue
        .assignee
        .as_ref()
        .map(|actor| format!(" @{}", actor.as_str()))
        .unwrap_or_default();
    format!(
        "{} [P{}] {}{} {}",
        issue.id,
        issue.priority.value(),
        issue.status.as_str(),
        assignee,
        issue.title
    )
}

fn format_issue_list(issues: &[crate::domain::Issue]) -> String {
    issues
        .iter()
        .map(format_issue)
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_epic_summary(summary: &crate::store::EpicSummary) -> String {
    format!(
        "{}: {}/{} children complete{}",
        summary.epic.title,
        summary.completed,
        summary.total,
        if summary.complete { " (complete)" } else { "" }
    )
}

fn format_edge(edge: &crate::domain::Edge) -> String {
    format!(
        "{} --{}--> {}",
        edge.source_id,
        edge.edge_type.as_str(),
        edge.target_id
    )
}

fn format_edge_list(edges: &[crate::domain::Edge]) -> String {
    edges.iter().map(format_edge).collect::<Vec<_>>().join("\n")
}

fn ready_issues(store: &Store, args: ReadyArgs) -> Result<Vec<crate::domain::Issue>, AppError> {
    let priority = args.priority.map(Priority::new).transpose()?;
    let mut issues = store.list_ready(&now()?)?;
    if let Some(priority) = priority {
        issues.retain(|issue| issue.priority == priority);
    }
    if let Some(limit) = args.limit {
        issues.truncate(limit);
    }
    Ok(issues)
}

fn remember_memory(
    store: &mut Store,
    args: RememberArgs,
) -> Result<crate::agent::memory::MemoryRecord, AppError> {
    let key = crate::agent::memory::resolve_key(args.key, &args.content)?;
    let memory = store.upsert_memory_with_governance(
        &key,
        &args.content,
        &args.scope,
        "agent",
        &now()?,
        crate::store::MemoryGovernance {
            importance: args.importance,
            confidence: args.confidence,
            expires_at: args.expires_at,
        },
    )?;
    Ok(memory.into())
}

fn format_memory(memory: &crate::store::Memory) -> String {
    format!("{} [{}] {}", memory.key, memory.scope, memory.content)
}

fn format_memory_record(memory: &crate::agent::memory::MemoryRecord) -> String {
    format!("{} [{}] {}", memory.key, memory.scope, memory.content)
}

fn heartbeat(store: &mut Store, args: HeartbeatArgs) -> Result<crate::store::Lease, AppError> {
    if args.lease_seconds <= 0 {
        return Err(AppError::InvalidArguments {
            message: "lease-seconds must be positive".to_owned(),
        });
    }
    let id = IssueId::new(args.id)?;
    let actor = Actor::new(args.actor)?;
    let current = now()?;
    let until = after_seconds(&current, args.lease_seconds)?;
    Ok(store.heartbeat(&id, &actor, &current, &until)?)
}

fn release_lease(
    store: &mut Store,
    args: LeaseOwnerArgs,
) -> Result<crate::domain::Issue, AppError> {
    Ok(store.release_lease(&IssueId::new(args.id)?, &Actor::new(args.actor)?, &now()?)?)
}

fn reclaim_lease(store: &mut Store, args: ReclaimArgs) -> Result<crate::domain::Issue, AppError> {
    Ok(store.reclaim_lease(
        &IssueId::new(args.id)?,
        &Actor::new(args.supervisor)?,
        &now()?,
    )?)
}

fn handoff(store: &mut Store, args: HandoffArgs) -> Result<crate::store::Handoff, AppError> {
    Ok(store.create_handoff(
        &IssueId::new(args.id)?,
        &Actor::new(args.from)?,
        args.to.map(Actor::new).transpose()?,
        &args.summary,
        &args.artifacts,
        &args.next,
        &now()?,
    )?)
}

fn format_lease(lease: &crate::store::Lease) -> String {
    format!(
        "{} lease until {} (heartbeat {})",
        lease.owner.as_str(),
        lease.until,
        lease.heartbeat_at
    )
}

fn format_handoff(handoff: &crate::store::Handoff) -> String {
    format!(
        "{} -> {}: {} (next: {})",
        handoff.from_actor.as_str(),
        handoff
            .to_actor
            .as_ref()
            .map(Actor::as_str)
            .unwrap_or("unassigned"),
        handoff.summary,
        handoff.next_step
    )
}

fn after_seconds(timestamp: &str, seconds: i64) -> Result<String, AppError> {
    if timestamp.trim().is_empty() || seconds <= 0 {
        return Err(AppError::InvalidArguments {
            message: "lease timestamp and duration must be valid".to_owned(),
        });
    }
    Ok((OffsetDateTime::now_utc() + time::Duration::seconds(seconds)).format(&Rfc3339)?)
}

fn run_agent_worker_once<W: AgentWorker>(
    store: &mut Store,
    actor: Actor,
    worker: W,
    now: &str,
    lease_until: &str,
    max_steps: u32,
) -> Result<AgentRunResult, AppError> {
    let mut runner = AgentRunner::new(worker, actor);
    Ok(runner.run_until_terminal(store, now, lease_until, max_steps)?)
}

fn issue_runner_token(store: &Store, args: TokenArgs) -> Result<String, AppError> {
    Ok(crate::runner::issue_token(
        store,
        &IssueId::new(args.id)?,
        &Actor::new(args.actor)?,
    )?)
}

fn run_runner_command(store: &Store, args: RunArgs) -> Result<crate::runner::RunResult, AppError> {
    Ok(crate::runner::run_command(
        store,
        &IssueId::new(args.id)?,
        &Actor::new(args.actor)?,
        &args.token,
        &args.command,
        &args.args,
    )?)
}

fn setup_agent(args: SetupArgs) -> Result<crate::agent::setup::SetupReport, AppError> {
    if args.target != "generic" && args.target != "codex" {
        return Err(AppError::InvalidArguments {
            message: "setup target must be `generic` or `codex`".to_owned(),
        });
    }
    let root = match args.root {
        Some(path) => path,
        None => env::current_dir().map_err(|source| AppError::Io {
            operation: "read current directory",
            path: PathBuf::from("."),
            source,
        })?,
    };
    let action = if args.remove {
        crate::agent::setup::SetupAction::Remove
    } else if args.check {
        crate::agent::setup::SetupAction::Check
    } else {
        crate::agent::setup::SetupAction::Install
    };
    crate::agent::setup::apply(&root, action).map_err(|error| AppError::InvalidArguments {
        message: error.to_string(),
    })
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CompactResult {
    Analyze {
        candidates: Vec<CompactionCandidate>,
        dry_run: bool,
    },
    Applied {
        issue: crate::domain::Issue,
    },
    Auto {
        results: Vec<AutoCompactResult>,
    },
}

#[derive(Debug, Serialize)]
struct AutoCompactResult {
    id: String,
    status: String,
    issue: Option<crate::domain::Issue>,
    error: Option<String>,
}

fn compact_issue(store: &mut Store, args: CompactArgs) -> Result<CompactResult, AppError> {
    let has_summary = args.summary.is_some();
    let mode_count = usize::from(args.analyze) + usize::from(args.apply) + usize::from(args.auto);
    if mode_count > 1 {
        return Err(AppError::InvalidArguments {
            message: "--analyze, --apply, and --auto are mutually exclusive".to_owned(),
        });
    }

    if args.analyze || (!has_summary && !args.apply && !args.auto) {
        if args.id.is_some() && !args.analyze {
            return Err(AppError::InvalidArguments {
                message: "compact requires --analyze, --apply, --auto, or --summary".to_owned(),
            });
        }
        let id = args.id.as_deref().map(IssueId::new).transpose()?;
        let candidates = store.list_compaction_candidates(id.as_ref(), args.min_bytes)?;
        return Ok(CompactResult::Analyze {
            candidates,
            dry_run: true,
        });
    }

    if args.apply || has_summary {
        if args.all || args.id.is_none() || args.summary.is_none() {
            return Err(AppError::InvalidArguments {
                message:
                    "--apply/--summary requires <id> and --summary; --all is only valid with --auto"
                        .to_owned(),
            });
        }
        let id = IssueId::new(args.id.expect("validated id"))?;
        let issue = store.compact_issue_with_metadata(
            &id,
            &args.summary.expect("validated summary"),
            &now()?,
            CompactionMetadata {
                actor: args.actor,
                model: args.model,
                provider: "manual".to_owned(),
                ..CompactionMetadata::default()
            },
        )?;
        return Ok(CompactResult::Applied { issue });
    }

    if args.all && args.id.is_some() {
        return Err(AppError::InvalidArguments {
            message: "--all and <id> cannot be used together".to_owned(),
        });
    }
    if !args.all && args.id.is_none() {
        return Err(AppError::InvalidArguments {
            message: "--auto requires <id> or --all".to_owned(),
        });
    }

    let candidates = store.list_compaction_candidates(
        args.id.as_deref().map(IssueId::new).transpose()?.as_ref(),
        args.min_bytes,
    )?;
    if args.dry_run {
        return Ok(CompactResult::Analyze {
            candidates,
            dry_run: true,
        });
    }

    let provider = args
        .provider
        .as_deref()
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if args.command.is_some() || std::env::var_os("AG_COMPACTION_COMMAND").is_some() {
                "external".to_owned()
            } else {
                "openai".to_owned()
            }
        });
    let compactor: Box<dyn crate::compaction::Summarizer> = match provider.as_str() {
        "external" => Box::new(
            crate::compaction::ExternalCompactor::with_options(
                args.command,
                args.command_args,
                args.timeout_seconds,
                args.retries,
            )
            .map_err(|error| AppError::InvalidArguments {
                message: error.to_string(),
            })?,
        ),
        "openai" => Box::new(
            crate::compaction::OpenAiCompactor::from_env(
                args.model.clone(),
                args.timeout_seconds,
                args.retries,
            )
            .map_err(|error| AppError::InvalidArguments {
                message: error.to_string(),
            })?,
        ),
        _ => {
            return Err(AppError::InvalidArguments {
                message: format!("unsupported compaction provider `{provider}`"),
            });
        }
    };
    let mut results = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let id = candidate.issue.id.clone();
        let result = match compactor.summarize(&candidate.issue) {
            Ok(summary) => store
                .compact_issue_with_metadata(
                    &id,
                    &summary.summary,
                    &now()?,
                    CompactionMetadata {
                        actor: args.actor.clone(),
                        model: if summary.provider == "external" {
                            args.model.clone()
                        } else {
                            summary.model
                        },
                        provider: summary.provider,
                        duration_ms: Some(summary.duration_ms),
                        input_tokens: summary.input_tokens,
                        output_tokens: summary.output_tokens,
                        ..CompactionMetadata::default()
                    },
                )
                .map(|issue| AutoCompactResult {
                    id: id.to_string(),
                    status: "compacted".to_owned(),
                    issue: Some(issue),
                    error: None,
                })
                .unwrap_or_else(|error| AutoCompactResult {
                    id: id.to_string(),
                    status: "skipped".to_owned(),
                    issue: None,
                    error: Some(error.to_string()),
                }),
            Err(error) => AutoCompactResult {
                id: id.to_string(),
                status: "skipped".to_owned(),
                issue: None,
                error: Some(error.to_string()),
            },
        };
        results.push(result);
    }
    Ok(CompactResult::Auto { results })
}

fn format_compact_result(result: &CompactResult) -> String {
    match result {
        CompactResult::Analyze { candidates, .. } => {
            format!("{} compaction candidate(s)", candidates.len())
        }
        CompactResult::Applied { issue } => format_issue(issue),
        CompactResult::Auto { results } => format!(
            "compacted {} issue(s), skipped {}",
            results
                .iter()
                .filter(|item| item.status == "compacted")
                .count(),
            results
                .iter()
                .filter(|item| item.status == "skipped")
                .count()
        ),
    }
}

fn restore_issue(store: &mut Store, args: RestoreArgs) -> Result<crate::domain::Issue, AppError> {
    Ok(store.restore_issue(&IssueId::new(args.id)?, &now()?)?)
}

#[derive(Debug, Serialize)]
struct TaskContext {
    issue: crate::domain::Issue,
    checkpoint: Option<crate::store::Checkpoint>,
    dependencies: Vec<crate::domain::Edge>,
    dependents: Vec<crate::domain::Edge>,
}

fn save_checkpoint(
    store: &mut Store,
    args: CheckpointArgs,
) -> Result<crate::store::Checkpoint, AppError> {
    let id = IssueId::new(args.id)?;
    let actor = Actor::new(args.actor)?;
    Ok(store.upsert_checkpoint(
        &id,
        actor,
        &args.summary,
        &args.completed,
        &args.next,
        &args.blockers,
        &args.evidence,
        &now()?,
    )?)
}

fn task_context(store: &Store, args: ContextArgs) -> Result<TaskContext, AppError> {
    let id = IssueId::new(args.id)?;
    let issue = store
        .get_issue(&id)?
        .ok_or_else(|| crate::store::StoreError::IssueNotFound { id: id.to_string() })?;
    let edges = store.list_edges()?;
    let dependencies = edges
        .iter()
        .filter(|edge| edge.source_id == id && edge.edge_type.is_scheduling())
        .cloned()
        .collect();
    let dependents = edges
        .into_iter()
        .filter(|edge| edge.target_id == id && edge.edge_type.is_scheduling())
        .collect();
    Ok(TaskContext {
        checkpoint: store.get_checkpoint(&id)?,
        issue,
        dependencies,
        dependents,
    })
}

fn format_checkpoint(checkpoint: &crate::store::Checkpoint) -> String {
    format!(
        "{} rev {}: {}\ncompleted: {}\nnext: {}\nblockers: {}\nevidence: {}",
        checkpoint.issue_id,
        checkpoint.revision.value(),
        checkpoint.summary,
        checkpoint.completed,
        checkpoint.next_step,
        if checkpoint.blockers.is_empty() {
            "none"
        } else {
            &checkpoint.blockers
        },
        if checkpoint.evidence.is_empty() {
            "none"
        } else {
            &checkpoint.evidence
        }
    )
}

fn format_task_context(context: &TaskContext) -> String {
    let dependencies = context
        .dependencies
        .iter()
        .map(format_edge)
        .collect::<Vec<_>>()
        .join("\n");
    let dependents = context
        .dependents
        .iter()
        .map(format_edge)
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{}\ncheckpoint:\n{}\ndependencies:\n{}\ndependents:\n{}",
        format_issue(&context.issue),
        context
            .checkpoint
            .as_ref()
            .map(format_checkpoint)
            .unwrap_or_else(|| "none".to_owned()),
        if dependencies.is_empty() {
            "none"
        } else {
            &dependencies
        },
        if dependents.is_empty() {
            "none"
        } else {
            &dependents
        },
    )
}

fn prime_context(
    store: &Store,
    workspace: &Path,
    args: PrimeArgs,
) -> Result<crate::agent::prime::PrimeContext, AppError> {
    Ok(crate::agent::prime::build(
        store,
        workspace,
        args.actor.as_deref(),
        args.max_memory_count,
        args.max_memory_chars,
    )?)
}

fn resolve_init_workspace(explicit_workspace: Option<&Path>) -> Result<PathBuf, AppError> {
    if let Some(path) = explicit_workspace {
        return Ok(normalize_workspace_path(path));
    }

    let current = env::current_dir().map_err(|source| AppError::Io {
        operation: "read current directory",
        path: PathBuf::from("."),
        source,
    })?;

    Ok(current.join(WORKSPACE_DIR))
}

#[cfg(test)]
fn resolve_workspace_from(explicit_workspace: Option<&Path>, current: &Path) -> Option<PathBuf> {
    if let Some(path) = explicit_workspace {
        return Some(normalize_workspace_path(path));
    }

    for directory in current.ancestors() {
        let candidate = directory.join(WORKSPACE_DIR);
        if candidate.is_dir() {
            return Some(candidate);
        }
        let legacy_candidate = directory.join(LEGACY_WORKSPACE_DIR);
        if legacy_candidate.is_dir() {
            return Some(legacy_candidate);
        }
    }

    None
}

fn normalize_workspace_path(path: &Path) -> PathBuf {
    if path
        .file_name()
        .is_some_and(|name| name == WORKSPACE_DIR || name == LEGACY_WORKSPACE_DIR)
    {
        path.to_path_buf()
    } else {
        path.join(WORKSPACE_DIR)
    }
}

fn database_path(workspace: &Path) -> PathBuf {
    existing_database_path(workspace).unwrap_or_else(|| workspace.join(DATABASE_FILE))
}

fn existing_database_path(workspace: &Path) -> Option<PathBuf> {
    [DATABASE_FILE, LEGACY_DATABASE_FILE]
        .into_iter()
        .map(|name| workspace.join(name))
        .find(|path| path.is_file())
}

fn format_init(result: &InitResult) -> String {
    if result.created {
        format!("initialized workspace at {}", result.workspace.display())
    } else {
        format!(
            "workspace already initialized at {}",
            result.workspace.display()
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("workspace is not initialized; run `ag init` first")]
    WorkspaceNotInitialized,

    #[error("memory `{key}` was not found")]
    MemoryNotFound { key: String },

    #[error("{message}")]
    InvalidArguments { message: String },

    #[error("could not generate a unique issue id")]
    IdCollisionExhausted,

    #[error("failed to {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to render output: {0}")]
    Output(#[from] serde_json::Error),

    #[error(transparent)]
    Storage(#[from] crate::store::StoreError),

    #[error(transparent)]
    Runner(#[from] crate::runner::RunnerError),

    #[error(transparent)]
    Plan(#[from] PlanError),

    #[error(transparent)]
    Runtime(#[from] crate::runtime::RuntimeError),

    #[error(transparent)]
    RuntimeToken(#[from] crate::runtime::token::RuntimeTokenError),

    #[error(transparent)]
    AgentRunner(#[from] crate::runtime::agent_worker::AgentRunnerError),

    #[error(transparent)]
    AgentWorker(#[from] crate::runtime::agent_worker::AgentWorkerError),

    #[error("ui server error: {message}")]
    Ui { message: String },

    #[error(transparent)]
    Domain(#[from] crate::domain::DomainError),

    #[error(transparent)]
    Time(#[from] time::error::Format),

    #[error("replay failed at line {line}: {message}")]
    Replay { line: usize, message: String },
}

impl AppError {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::WorkspaceNotInitialized => "workspace_not_initialized",
            Self::MemoryNotFound { .. } => "memory_not_found",
            Self::InvalidArguments { .. } => "invalid_arguments",
            Self::IdCollisionExhausted => "id_collision_exhausted",
            Self::Io { .. } => "io_error",
            Self::Output(_) => "output_error",
            Self::Storage(_) => "storage_error",
            Self::Runner(_) => "runner_error",
            Self::Plan(_) => "plan_error",
            Self::Runtime(_) => "runtime_error",
            Self::RuntimeToken(_) => "runtime_token_error",
            Self::AgentRunner(_) => "agent_runner_error",
            Self::AgentWorker(_) => "agent_worker_error",
            Self::Ui { .. } => "ui_error",
            Self::Domain(_) => "domain_error",
            Self::Time(_) => "time_error",
            Self::Replay { .. } => "replay_error",
        }
    }

    pub fn exit_code(&self) -> u8 {
        match self {
            Self::WorkspaceNotInitialized => 2,
            Self::MemoryNotFound { .. } => 1,
            Self::InvalidArguments { .. }
            | Self::IdCollisionExhausted
            | Self::Io { .. }
            | Self::Output(_)
            | Self::Storage(_)
            | Self::Runner(_)
            | Self::Domain(_)
            | Self::Time(_) => 1,
            Self::Plan(_) => 1,
            Self::Runtime(_) => 1,
            Self::RuntimeToken(_) => 1,
            Self::AgentRunner(_) => 1,
            Self::AgentWorker(_) => 1,
            Self::Ui { .. } => 1,
            Self::Replay { .. } => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{WORKSPACE_DIR, normalize_workspace_path, resolve_workspace_from};
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    #[test]
    fn explicit_workspace_appends_default_directory() {
        assert_eq!(
            normalize_workspace_path(Path::new("/tmp/project")),
            Path::new("/tmp/project").join(WORKSPACE_DIR)
        );
    }

    #[test]
    fn explicit_workspace_directory_is_preserved() {
        let path = Path::new("/tmp/project/.agent-stride");
        assert_eq!(normalize_workspace_path(path), path);
    }

    #[test]
    fn legacy_workspace_directory_is_preserved() {
        let path = Path::new("/tmp/project/.agent-graph");
        assert_eq!(normalize_workspace_path(path), path);
    }

    #[test]
    fn discovery_finds_workspace_in_an_ancestor() {
        let root = tempdir().expect("temporary root");
        let nested = root.path().join("src").join("module");
        std::fs::create_dir_all(&nested).expect("nested directory");
        let workspace = root.path().join(WORKSPACE_DIR);
        std::fs::create_dir_all(&workspace).expect("workspace directory");

        let discovered = resolve_workspace_from(None, &nested).expect("workspace should be found");

        assert_eq!(
            fs::canonicalize(discovered).expect("canonical discovered path"),
            fs::canonicalize(workspace).expect("canonical workspace path")
        );
    }
}
