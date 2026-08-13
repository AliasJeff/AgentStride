-- Consolidated fresh-install schema for AgentStride.
CREATE TABLE IF NOT EXISTS workspace_meta (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS issues (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL CHECK (status IN ('open', 'in_progress', 'closed')),
    priority INTEGER NOT NULL CHECK (priority BETWEEN 0 AND 4),
    assignee TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    defer_until TEXT,
    close_reason TEXT NOT NULL DEFAULT '',
    revision INTEGER NOT NULL,
    lease_owner TEXT,
    lease_started_at TEXT,
    lease_until TEXT,
    heartbeat_at TEXT,
    kind TEXT NOT NULL DEFAULT 'task' CHECK (kind IN ('task', 'epic'))
);

CREATE TABLE IF NOT EXISTS edges (
    source_id TEXT NOT NULL,
    target_id TEXT NOT NULL,
    edge_type TEXT NOT NULL CHECK (edge_type IN ('blocks', 'parent_child', 'related')),
    created_at TEXT NOT NULL,
    PRIMARY KEY (source_id, target_id),
    FOREIGN KEY (source_id) REFERENCES issues(id) ON DELETE CASCADE,
    FOREIGN KEY (target_id) REFERENCES issues(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS journal (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    operation TEXT NOT NULL,
    issue_id TEXT,
    actor TEXT,
    snapshot TEXT,
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS memories (
    key TEXT PRIMARY KEY NOT NULL,
    content TEXT NOT NULL,
    scope TEXT NOT NULL,
    source TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    revision INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    importance INTEGER NOT NULL DEFAULT 3 CHECK (importance BETWEEN 1 AND 5),
    confidence INTEGER NOT NULL DEFAULT 3 CHECK (confidence BETWEEN 1 AND 5),
    expires_at TEXT,
    last_used_at TEXT
);

CREATE TABLE IF NOT EXISTS session_checkpoints (
    issue_id TEXT PRIMARY KEY NOT NULL,
    actor TEXT NOT NULL,
    summary TEXT NOT NULL,
    completed TEXT NOT NULL DEFAULT '',
    next_step TEXT NOT NULL,
    blockers TEXT NOT NULL DEFAULT '',
    evidence TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    revision INTEGER NOT NULL,
    FOREIGN KEY (issue_id) REFERENCES issues(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS handoffs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    issue_id TEXT NOT NULL,
    from_actor TEXT NOT NULL,
    to_actor TEXT,
    summary TEXT NOT NULL,
    artifacts TEXT NOT NULL DEFAULT '',
    next_step TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (issue_id) REFERENCES issues(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS issue_snapshots (
    issue_id TEXT NOT NULL,
    compaction_level INTEGER NOT NULL,
    content_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    restored_at TEXT,
    PRIMARY KEY (issue_id, compaction_level),
    FOREIGN KEY (issue_id) REFERENCES issues(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY NOT NULL,
    thread_id TEXT NOT NULL,
    parent_id TEXT,
    from_actor TEXT NOT NULL,
    to_actor TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'message',
    subject TEXT NOT NULL DEFAULT '',
    body TEXT NOT NULL,
    created_at TEXT NOT NULL,
    read_at TEXT
);

CREATE TABLE IF NOT EXISTS runtime_runs (
    run_id TEXT PRIMARY KEY NOT NULL,
    plan_revision TEXT NOT NULL,
    status TEXT NOT NULL,
    history_version INTEGER NOT NULL,
    state_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS execution_events (
    run_id TEXT NOT NULL,
    event_seq INTEGER NOT NULL,
    event_id TEXT NOT NULL UNIQUE,
    event_type TEXT NOT NULL,
    plan_revision TEXT NOT NULL,
    task_id TEXT,
    attempt_id TEXT,
    actor TEXT,
    occurred_at TEXT NOT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY (run_id, event_seq),
    FOREIGN KEY (run_id) REFERENCES runtime_runs(run_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS runtime_tasks (
    durable_task_id TEXT PRIMARY KEY NOT NULL,
    run_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    attempt_id TEXT,
    task_kind TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('ready', 'leased', 'completed', 'failed')),
    available_at TEXT NOT NULL,
    lease_owner TEXT,
    lease_until TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    preferred_worker TEXT,
    UNIQUE (run_id, task_id, attempt_id),
    FOREIGN KEY (run_id) REFERENCES runtime_runs(run_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS runtime_timers (
    timer_id TEXT PRIMARY KEY NOT NULL,
    run_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    fire_at TEXT NOT NULL,
    timer_kind TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'fired')),
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL,
    fired_at TEXT,
    FOREIGN KEY (run_id) REFERENCES runtime_runs(run_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS plan_proposals (
    proposal_id TEXT PRIMARY KEY NOT NULL,
    run_id TEXT NOT NULL,
    base_revision TEXT NOT NULL,
    head_revision TEXT NOT NULL,
    author TEXT NOT NULL,
    reason TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('proposed', 'approved', 'rejected')),
    created_at TEXT NOT NULL,
    approved_by TEXT,
    approved_at TEXT,
    FOREIGN KEY (run_id) REFERENCES runtime_runs(run_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_issues_status_priority_created ON issues (status, priority, created_at, id);
CREATE INDEX IF NOT EXISTS idx_issues_lease_until ON issues (lease_until);
CREATE INDEX IF NOT EXISTS idx_edges_source_type ON edges (source_id, edge_type);
CREATE INDEX IF NOT EXISTS idx_edges_target_type ON edges (target_id, edge_type);
CREATE INDEX IF NOT EXISTS idx_journal_issue_seq ON journal (issue_id, seq);
CREATE INDEX IF NOT EXISTS idx_memories_scope_updated ON memories (scope, updated_at, key);
CREATE INDEX IF NOT EXISTS idx_memories_governance ON memories (status, scope, importance, updated_at);
CREATE INDEX IF NOT EXISTS idx_checkpoints_actor ON session_checkpoints (actor, created_at);
CREATE INDEX IF NOT EXISTS idx_handoffs_issue_created ON handoffs (issue_id, created_at);
CREATE INDEX IF NOT EXISTS idx_messages_inbox ON messages (to_actor, read_at, created_at);
CREATE INDEX IF NOT EXISTS idx_messages_thread ON messages (thread_id, created_at);
CREATE INDEX IF NOT EXISTS idx_execution_events_run_seq ON execution_events (run_id, event_seq);
CREATE INDEX IF NOT EXISTS idx_execution_events_task ON execution_events (run_id, task_id, event_seq);
CREATE INDEX IF NOT EXISTS idx_runtime_tasks_poll ON runtime_tasks (status, available_at, durable_task_id);
CREATE INDEX IF NOT EXISTS idx_runtime_tasks_run_task ON runtime_tasks (run_id, task_id, status);
CREATE INDEX IF NOT EXISTS idx_runtime_tasks_worker_poll ON runtime_tasks (status, preferred_worker, available_at, durable_task_id);
CREATE INDEX IF NOT EXISTS idx_runtime_timers_due ON runtime_timers (status, fire_at, timer_id);
CREATE INDEX IF NOT EXISTS idx_runtime_timers_run ON runtime_timers (run_id, status, fire_at);
CREATE INDEX IF NOT EXISTS idx_plan_proposals_run_revision ON plan_proposals (run_id, head_revision, status);
