use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "ag",
    version,
    about = "A local-first runtime for reliable long-running Agent work"
)]
pub struct Cli {
    /// Use a specific workspace instead of discovering one.
    #[arg(long, global = true, value_name = "PATH")]
    pub workspace: Option<PathBuf>,

    /// Render machine-readable JSON output.
    #[arg(long, global = true)]
    pub json: bool,

    /// Suppress non-essential human-readable output.
    #[arg(long, global = true)]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Initialize a workspace.
    Init(InitArgs),
    /// Create a work item.
    Add(AddArgs),
    /// List work items.
    List(ListArgs),
    /// Show one work item.
    Show(ShowArgs),
    /// Update a work item.
    Update(UpdateArgs),
    /// Close a work item.
    Close(CloseArgs),
    /// Add a dependency edge.
    Link(LinkArgs),
    /// Remove a dependency edge.
    Unlink(UnlinkArgs),
    /// Show graph edges.
    Graph(GraphArgs),
    /// Show the currently executable work.
    Ready(ReadyArgs),
    /// Show an epic and its direct child tasks.
    Epic(EpicArgs),
    /// Show work blocked by the current graph.
    Blocked(BlockedArgs),
    /// Claim one work item.
    Claim(ClaimArgs),
    /// Claim the highest-priority ready work item.
    ClaimNext(ClaimNextArgs),
    /// Read committed mutation events.
    Events(EventsArgs),
    /// Export the current workspace.
    Export(ExportArgs),
    /// Import a snapshot JSONL stream.
    Import(ImportArgs),
    /// Replay an event or snapshot stream.
    Replay(ReplayArgs),
    /// Verify workspace invariants.
    Verify(VerifyArgs),
    /// Store a durable project memory.
    Remember(RememberArgs),
    /// Read one durable project memory.
    Recall(RecallArgs),
    /// List durable project memories.
    Memories(MemoriesArgs),
    /// Mark memories as used by an agent context.
    TouchMemories(TouchMemoriesArgs),
    /// Delete one durable project memory.
    Forget(ForgetArgs),
    /// Apply memory lifecycle governance.
    GovernMemories(GovernMemoriesArgs),
    /// Save resumable progress for a claimed work item.
    Checkpoint(CheckpointArgs),
    /// Show the task context needed to resume work.
    Context(ContextArgs),
    /// Build the bounded context for a new or resumed agent session.
    Prime(PrimeArgs),
    /// Handle an agent session lifecycle hook.
    AgentHook(AgentHookArgs),
    /// Extend an active task lease.
    Heartbeat(HeartbeatArgs),
    /// Release a task lease and return the task to the ready frontier.
    Release(LeaseOwnerArgs),
    /// Reclaim an expired task lease.
    Reclaim(ReclaimArgs),
    /// Record a handoff for another agent.
    Handoff(HandoffArgs),
    /// Issue a capability token for an active lease.
    Token(TokenArgs),
    /// Run a policy-checked command under a capability token.
    Run(RunArgs),
    /// Install or check AgentStride integration files.
    Setup(SetupArgs),
    /// Replace issue content with a reversible summary.
    Compact(CompactArgs),
    /// Restore the latest issue snapshot.
    Restore(RestoreArgs),
    /// Run the MCP stdio server.
    Mcp(McpArgs),
    /// Send a message to another agent.
    Send(SendArgs),
    /// Read an actor's inbox.
    Inbox(InboxArgs),
    /// Read a complete message thread.
    Thread(ThreadArgs),
    /// Reply to a message thread.
    Reply(ReplyArgs),
    /// Manage durable runtime runs.
    Runtime(RuntimeArgs),
    /// Interact with durable runtime task leases.
    Worker(WorkerArgs),
    /// Reconcile a run against a committed plan revision.
    Reconcile(ReconcileArgs),
    /// Manage Git-backed plan revisions.
    Plan(PlanArgs),
    /// Open the local read-only observability UI.
    Ui(UiArgs),
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Do not print the initialized workspace path.
    #[arg(long)]
    pub quiet: bool,
}

#[derive(Debug, Args)]
pub struct UiArgs {
    /// Open this runtime run by default.
    #[arg(long)]
    pub run: Option<String>,

    /// Local HTTP port.
    #[arg(long, default_value_t = 8787)]
    pub port: u16,
}

#[derive(Debug, Args)]
pub struct AddArgs {
    /// Work item title.
    pub title: String,

    /// Optional work item description.
    #[arg(long, default_value = "")]
    pub description: String,

    /// Priority from 0 (highest) to 4 (lowest).
    #[arg(short, long, default_value_t = 2)]
    pub priority: u8,

    /// Issue kind: task or epic.
    #[arg(long, default_value = "task", value_parser = ["task", "epic"])]
    pub kind: String,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Filter by lifecycle status.
    #[arg(long)]
    pub status: Option<String>,

    /// Filter by issue kind.
    #[arg(long, value_parser = ["task", "epic"])]
    pub kind: Option<String>,
}

#[derive(Debug, Args)]
pub struct EpicArgs {
    /// Epic issue ID.
    pub id: String,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// Work item ID.
    pub id: String,
}

#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// Work item ID.
    pub id: String,

    /// Replace the title.
    #[arg(long)]
    pub title: Option<String>,

    /// Replace the description.
    #[arg(long)]
    pub description: Option<String>,

    /// Replace the priority.
    #[arg(short, long)]
    pub priority: Option<u8>,

    /// Require this revision before writing.
    #[arg(long)]
    pub if_revision: Option<u64>,
}

#[derive(Debug, Args)]
pub struct CloseArgs {
    /// Work item ID.
    pub id: String,

    /// Optional close reason.
    #[arg(long, default_value = "")]
    pub reason: String,

    /// Require this revision before writing.
    #[arg(long)]
    pub if_revision: Option<u64>,
}

#[derive(Debug, Args)]
pub struct LinkArgs {
    /// Dependent issue ID.
    pub source: String,

    /// Issue that blocks the source.
    #[arg(long, conflicts_with_all = ["parent", "related"])]
    pub blocks: Option<String>,

    /// Parent issue of the source.
    #[arg(long, conflicts_with_all = ["blocks", "related"])]
    pub parent: Option<String>,

    /// Informationally related issue.
    #[arg(long, conflicts_with_all = ["blocks", "parent"])]
    pub related: Option<String>,
}

#[derive(Debug, Args)]
pub struct UnlinkArgs {
    /// Dependent issue ID.
    pub source: String,

    /// Dependency target ID.
    pub target: String,
}

#[derive(Debug, Args)]
pub struct GraphArgs {
    /// Optional root issue ID. Without it, show all edges.
    pub id: Option<String>,
}

#[derive(Debug, Args)]
pub struct ReadyArgs {
    /// Only show this priority.
    #[arg(short, long)]
    pub priority: Option<u8>,

    /// Limit the number of results.
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Debug, Args)]
pub struct BlockedArgs {}

#[derive(Debug, Args)]
pub struct ClaimArgs {
    /// Work item ID.
    pub id: String,

    /// Worker identity.
    #[arg(long)]
    pub actor: String,

    /// Lease duration in seconds.
    #[arg(long)]
    pub lease_seconds: Option<i64>,
}

#[derive(Debug, Args)]
pub struct ClaimNextArgs {
    /// Worker identity.
    #[arg(long)]
    pub actor: String,

    /// Lease duration in seconds.
    #[arg(long)]
    pub lease_seconds: Option<i64>,
}

#[derive(Debug, Args)]
pub struct EventsArgs {
    /// Return events with sequence numbers greater than this cursor.
    #[arg(long, default_value_t = 0)]
    pub since: u64,

    /// Maximum number of events to return in one page.
    #[arg(long, default_value_t = 100)]
    pub limit: usize,

    /// Poll for new events until interrupted.
    #[arg(long)]
    pub follow: bool,
}

#[derive(Debug, Args)]
pub struct ExportArgs {}

#[derive(Debug, Args)]
pub struct PlanArgs {
    #[command(subcommand)]
    pub command: PlanCommand,
}

#[derive(Debug, Subcommand)]
pub enum PlanCommand {
    /// Export the SQLite plan into the project-local .tasks directory.
    Export(PlanExportArgs),
    /// Show the current Git revision and uncommitted plan files.
    Status(PlanStatusArgs),
    /// Show a Git diff restricted to the plan directory.
    Diff(PlanDiffArgs),
    /// Record a proposed plan change against a run.
    Propose(PlanProposeArgs),
    /// Approve a proposal without changing Git or runtime state.
    Approve(PlanApproveArgs),
    /// Apply an approved revision to a run through the Reconciler.
    Activate(PlanActivateArgs),
}

#[derive(Debug, Args)]
pub struct PlanExportArgs {}

#[derive(Debug, Args)]
pub struct PlanStatusArgs {}

#[derive(Debug, Args)]
pub struct PlanDiffArgs {
    /// Base Git revision.
    pub base: String,

    /// Optional head Git revision. Without it, compare base to the worktree.
    pub head: Option<String>,
}

#[derive(Debug, Args)]
pub struct PlanProposeArgs {
    /// Runtime run ID whose current revision is the proposal base.
    pub run_id: String,

    /// Base Git revision.
    #[arg(long)]
    pub base: String,

    /// Proposed Git revision.
    #[arg(long)]
    pub head: String,

    /// Proposal author.
    #[arg(long)]
    pub author: String,

    /// Why the plan should change.
    #[arg(long)]
    pub reason: String,

    /// Stable proposal identifier.
    #[arg(long)]
    pub id: String,
}

#[derive(Debug, Args)]
pub struct PlanApproveArgs {
    /// Proposal identifier.
    pub proposal_id: String,

    /// Human or agent approving the proposal.
    #[arg(long)]
    pub actor: String,
}

#[derive(Debug, Args)]
pub struct PlanActivateArgs {
    /// Runtime run ID.
    pub run_id: String,

    /// Approved Git revision.
    pub revision: String,
}

#[derive(Debug, Args)]
pub struct ReconcileArgs {
    /// Runtime run ID.
    pub run_id: String,

    /// Git commit containing the desired .tasks plan.
    pub revision: String,

    /// Human-readable reason for the reconciliation request.
    #[arg(long, default_value = "plan revision selected")]
    pub reason: String,

    /// Explicitly replace one task identity with another: OLD=NEW.
    #[arg(long = "supersede", value_name = "OLD=NEW")]
    pub supersede: Vec<String>,
}

#[derive(Debug, Args)]
pub struct RuntimeArgs {
    #[command(subcommand)]
    pub command: RuntimeCommand,
}

#[derive(Debug, Subcommand)]
pub enum RuntimeCommand {
    /// Create and start a durable run bound to a plan revision.
    Start(RuntimeStartArgs),
    /// Show the materialized runtime state.
    Show(RuntimeRunArgs),
    /// Read runtime history after a sequence cursor.
    Events(RuntimeEventsArgs),
    /// Recover expired task leases and schedule retries.
    Recover(RuntimeRecoverArgs),
    /// Fire a due persistent timer.
    Fire(RuntimeFireArgs),
}

#[derive(Debug, Args)]
pub struct RuntimeStartArgs {
    /// Runtime run ID.
    pub run_id: String,

    /// Git commit or other immutable plan revision.
    #[arg(long)]
    pub plan: String,
}

#[derive(Debug, Args)]
pub struct RuntimeRunArgs {
    /// Runtime run ID.
    pub run_id: String,
}

#[derive(Debug, Args)]
pub struct RuntimeEventsArgs {
    /// Runtime run ID.
    pub run_id: String,

    /// Return events with sequence numbers greater than this cursor.
    #[arg(long, default_value_t = 0)]
    pub since: u64,

    /// Maximum number of events to return.
    #[arg(long, default_value_t = 100)]
    pub limit: usize,
}

#[derive(Debug, Args)]
pub struct RuntimeRecoverArgs {
    /// Runtime run ID.
    pub run_id: String,

    /// Retry delay after an expired lease.
    #[arg(long, default_value_t = 30)]
    pub retry_delay: i64,
}

#[derive(Debug, Args)]
pub struct RuntimeFireArgs {
    /// Persistent timer identifier.
    pub timer_id: String,
}

#[derive(Debug, Args)]
pub struct WorkerArgs {
    #[command(subcommand)]
    pub command: WorkerCommand,
}

#[derive(Debug, Subcommand)]
pub enum WorkerCommand {
    /// Claim and execute one task with the deterministic built-in worker.
    Run(WorkerRunArgs),
    /// Atomically claim the next durable task.
    Poll(WorkerPollArgs),
    /// Start the attempt represented by a token.
    Start(WorkerTokenArgs),
    /// Extend a task lease.
    Heartbeat(WorkerHeartbeatArgs),
    /// Complete a task with a JSON result.
    Complete(WorkerCompleteArgs),
    /// Fail a task with an explicit error.
    Fail(WorkerFailArgs),
    /// Hand off a leased task to another Worker.
    Handoff(WorkerHandoffArgs),
}

#[derive(Debug, Args)]
pub struct WorkerRunArgs {
    /// Worker identity.
    #[arg(long)]
    pub worker: String,

    /// Agent provider: fake or openai.
    #[arg(long, default_value = "fake", value_parser = ["fake", "openai"])]
    pub provider: String,

    /// OpenAI model when --provider openai is selected.
    #[arg(long, default_value = "gpt-5-mini")]
    pub model: String,

    /// Provider request timeout in seconds.
    #[arg(long, default_value_t = 120)]
    pub timeout_seconds: u64,

    /// Lease duration in seconds.
    #[arg(long, default_value_t = 60)]
    pub lease_seconds: i64,

    /// Seed for deterministic worker output.
    #[arg(long, default_value_t = 0)]
    pub seed: u64,

    /// Number of attempts that fail before completion.
    #[arg(long, default_value_t = 0)]
    pub failures_before_success: u32,

    /// Number of deterministic steps before the worker checkpoints.
    #[arg(long, default_value_t = 0)]
    pub duration_steps: u32,

    /// Maximum execution steps within one durable attempt.
    #[arg(long, default_value_t = 100)]
    pub max_steps: u32,

    /// Simulate a process crash at this step.
    #[arg(long)]
    pub crash_at_step: Option<u32>,

    /// Emit a plan-waiting outcome with this proposal identifier.
    #[arg(long)]
    pub proposal_id: Option<String>,

    /// Hand the task to a specific Worker after the current durable step.
    #[arg(long)]
    pub handoff_to: Option<String>,
}

#[derive(Debug, Args)]
pub struct WorkerPollArgs {
    /// Worker identity.
    #[arg(long)]
    pub worker: String,

    /// Lease expiration in RFC3339 format.
    #[arg(long)]
    pub lease_until: String,
}

#[derive(Debug, Args)]
pub struct WorkerTokenArgs {
    /// Signed runtime task token.
    pub token: String,
}

#[derive(Debug, Args)]
pub struct WorkerHeartbeatArgs {
    /// Signed runtime task token.
    pub token: String,

    /// New lease expiration in RFC3339 format.
    #[arg(long)]
    pub lease_until: String,
}

#[derive(Debug, Args)]
pub struct WorkerCompleteArgs {
    /// Signed runtime task token.
    pub token: String,

    /// JSON result value.
    #[arg(long, default_value = "{}")]
    pub result: String,
}

#[derive(Debug, Args)]
pub struct WorkerFailArgs {
    /// Signed runtime task token.
    pub token: String,

    /// Failure message.
    #[arg(long)]
    pub error: String,
}

#[derive(Debug, Args)]
pub struct WorkerHandoffArgs {
    /// Signed runtime task token.
    pub token: String,

    /// Optional target Worker identity.
    #[arg(long)]
    pub to_worker: Option<String>,

    /// Durable context summary for the next Worker.
    #[arg(long)]
    pub summary: String,

    /// Artifact references produced before handoff.
    #[arg(long, default_value = "")]
    pub artifacts: String,

    /// Next action the receiving Worker should take.
    #[arg(long)]
    pub next_step: String,
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Snapshot JSONL input.
    pub input: PathBuf,

    /// Project directory or .agent-stride directory to receive the import.
    #[arg(long)]
    pub into: PathBuf,
}

#[derive(Debug, Args)]
pub struct ReplayArgs {
    /// JSONL event stream to replay.
    pub input: PathBuf,

    /// Project directory or .agent-stride directory to receive the replay.
    #[arg(long)]
    pub into: PathBuf,
}

#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// Optional project directory or .agent-stride directory.
    pub path: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct RememberArgs {
    /// Memory content.
    pub content: String,

    /// Explicit memory key. Without it, the key is derived from content.
    #[arg(long)]
    pub key: Option<String>,

    /// Memory scope.
    #[arg(long, default_value = "workspace")]
    pub scope: String,

    /// Importance from 1 (low) to 5 (critical).
    #[arg(long, default_value_t = 3)]
    pub importance: u8,

    /// Confidence from 1 (uncertain) to 5 (verified).
    #[arg(long, default_value_t = 3)]
    pub confidence: u8,

    /// Optional RFC3339 expiration timestamp.
    #[arg(long)]
    pub expires_at: Option<String>,
}

#[derive(Debug, Args)]
pub struct RecallArgs {
    /// Memory key.
    pub key: String,
}

#[derive(Debug, Args)]
pub struct MemoriesArgs {
    /// Case-insensitive search over keys and content.
    #[arg(long)]
    pub search: Option<String>,

    /// Filter by scope.
    #[arg(long)]
    pub scope: Option<String>,

    /// Include archived memories.
    #[arg(long)]
    pub include_archived: bool,
}

#[derive(Debug, Args)]
pub struct GovernMemoriesArgs {
    /// Archive expired or stale memories.
    #[arg(long)]
    pub archive_stale: bool,

    /// Do not write governance changes.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct TouchMemoriesArgs {
    /// Memory keys to mark as used.
    #[arg(required = true)]
    pub keys: Vec<String>,
}

#[derive(Debug, Args)]
pub struct ForgetArgs {
    /// Memory key.
    pub key: String,
}

#[derive(Debug, Args)]
pub struct CheckpointArgs {
    /// Work item ID.
    pub id: String,

    /// Current claiming actor.
    #[arg(long)]
    pub actor: String,

    /// Short progress summary.
    #[arg(long)]
    pub summary: String,

    /// Completed work since the previous checkpoint.
    #[arg(long, default_value = "")]
    pub completed: String,

    /// The next concrete action.
    #[arg(long)]
    pub next: String,

    /// Current blockers.
    #[arg(long, default_value = "")]
    pub blockers: String,

    /// Verification evidence.
    #[arg(long, default_value = "")]
    pub evidence: String,
}

#[derive(Debug, Args)]
pub struct ContextArgs {
    /// Work item ID.
    pub id: String,
}

#[derive(Debug, Args)]
pub struct PrimeArgs {
    /// Worker identity used to locate the current claim.
    #[arg(long)]
    pub actor: Option<String>,

    /// Maximum number of memories to inject.
    #[arg(long)]
    pub max_memory_count: Option<usize>,

    /// Maximum number of memory characters to inject.
    #[arg(long)]
    pub max_memory_chars: Option<usize>,
}

#[derive(Debug, Args)]
pub struct AgentHookArgs {
    /// Hook lifecycle event.
    #[arg(value_name = "EVENT")]
    pub event: String,

    /// Worker identity for context refresh.
    #[arg(long)]
    pub actor: Option<String>,

    /// Stable session identifier supplied by the host.
    #[arg(long, default_value = "default")]
    pub session: String,
}

#[derive(Debug, Args)]
pub struct HeartbeatArgs {
    pub id: String,

    #[arg(long)]
    pub actor: String,

    #[arg(long)]
    pub lease_seconds: i64,
}

#[derive(Debug, Args)]
pub struct LeaseOwnerArgs {
    pub id: String,

    #[arg(long)]
    pub actor: String,
}

#[derive(Debug, Args)]
pub struct ReclaimArgs {
    pub id: String,

    #[arg(long)]
    pub supervisor: String,
}

#[derive(Debug, Args)]
pub struct HandoffArgs {
    pub id: String,

    #[arg(long)]
    pub from: String,

    #[arg(long)]
    pub to: Option<String>,

    #[arg(long)]
    pub summary: String,

    #[arg(long, default_value = "")]
    pub artifacts: String,

    #[arg(long)]
    pub next: String,
}

#[derive(Debug, Args)]
pub struct TokenArgs {
    pub id: String,

    #[arg(long)]
    pub actor: String,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    pub id: String,

    #[arg(long)]
    pub actor: String,

    #[arg(long)]
    pub token: String,

    /// Program to execute.
    pub command: String,

    /// Arguments passed to the program.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

#[derive(Debug, Args)]
pub struct SetupArgs {
    /// `generic` and `codex` currently share project-local integration.
    pub target: String,

    /// Project root. Defaults to the current directory.
    #[arg(long)]
    pub root: Option<PathBuf>,

    /// Check for drift without changing files.
    #[arg(long)]
    pub check: bool,

    /// Remove only AgentStride managed content.
    #[arg(long)]
    pub remove: bool,
}

#[derive(Debug, Args)]
pub struct CompactArgs {
    pub id: Option<String>,

    #[arg(long)]
    pub summary: Option<String>,

    /// List closed issues eligible for compaction without changing data.
    #[arg(long, conflicts_with_all = ["apply", "auto", "summary"])]
    pub analyze: bool,

    /// Apply an externally generated summary.
    #[arg(long, conflicts_with_all = ["analyze", "auto"])]
    pub apply: bool,

    /// Generate a summary with an AI or external summarizer.
    #[arg(long, conflicts_with_all = ["analyze", "apply", "summary"])]
    pub auto: bool,

    /// Compact every eligible issue. Only valid with --auto.
    #[arg(long)]
    pub all: bool,

    /// Do not mutate; show what auto compaction would attempt.
    #[arg(long)]
    pub dry_run: bool,

    /// Actor recorded in compaction metadata.
    #[arg(long, default_value = "compactor")]
    pub actor: String,

    /// Summarizer provider: openai or external. If omitted, --command selects external;
    /// otherwise OpenAI is used.
    #[arg(long, value_parser = ["openai", "external"])]
    pub provider: Option<String>,

    /// Model used by the provider and recorded in compaction metadata.
    #[arg(long, default_value = "gpt-5-mini")]
    pub model: String,

    /// External summarizer executable. It receives a JSON issue on stdin and
    /// must print the summary on stdout. Defaults to AG_COMPACTION_COMMAND.
    #[arg(long)]
    pub command: Option<String>,

    /// Arguments passed to the external summarizer.
    #[arg(long = "command-arg")]
    pub command_args: Vec<String>,

    /// Minimum description size for analyze/auto candidate selection.
    #[arg(long, default_value_t = 1)]
    pub min_bytes: usize,

    /// Maximum time allowed for one summarizer attempt.
    #[arg(long, default_value_t = 120)]
    pub timeout_seconds: u64,

    /// Number of retries after a failed summarizer attempt.
    #[arg(long, default_value_t = 2)]
    pub retries: usize,
}

#[derive(Debug, Args)]
pub struct RestoreArgs {
    pub id: String,
}

#[derive(Debug, Args)]
pub struct McpArgs {}

#[derive(Debug, Args)]
pub struct SendArgs {
    #[arg(long)]
    pub from: String,
    #[arg(long)]
    pub to: String,
    #[arg(long, default_value = "")]
    pub subject: String,
    pub body: String,
    #[arg(long)]
    pub thread: Option<String>,
    #[arg(long)]
    pub parent: Option<String>,
    #[arg(long, default_value = "message")]
    pub kind: String,
}

#[derive(Debug, Args)]
pub struct InboxArgs {
    #[arg(long)]
    pub actor: String,
    #[arg(long)]
    pub thread: Option<String>,
    #[arg(long)]
    pub mark_read: bool,
}

#[derive(Debug, Args)]
pub struct ThreadArgs {
    pub id: String,
}

#[derive(Debug, Args)]
pub struct ReplyArgs {
    pub id: String,
    #[arg(long)]
    pub from: String,
    #[arg(long)]
    pub to: String,
    pub body: String,
}

#[derive(Debug, Args)]
pub struct UnsupportedArgs {
    /// Preserve the command's future positional arguments.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

impl Cli {
    pub fn parse_args() -> Self {
        Self::parse()
    }
}
