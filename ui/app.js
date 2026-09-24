(() => {
  "use strict";

  const SVG_NS = "http://www.w3.org/2000/svg";
  const state = {
    runId: window.__AG_DEFAULT_RUN__ || new URLSearchParams(window.location.search).get("run"),
    runs: [],
    overview: null,
    plan: null,
    events: new Map(),
    selectedEvent: null,
    selectedTask: null,
    cursor: 0,
    controller: null,
    timer: null,
    zoom: 1,
    pan: { x: 0, y: 0 },
    dragging: null,
  };

  const $ = (selector) => document.querySelector(selector);
  const runSelect = $("#run-select");
  const taskSelect = $("#task-select");
  const dashboard = $("#dashboard");
  const emptyState = $("#empty-state");
  const errorState = $("#error-state");
  const connectionState = $("#connection-state");
  const flowViewport = $("#flow-viewport");
  const flowCanvas = $("#flow-canvas");
  const flowWorld = $("#flow-world");

  function escapeHtml(value) {
    return String(value ?? "")
      .replaceAll("&", "&amp;")
      .replaceAll("<", "&lt;")
      .replaceAll(">", "&gt;")
      .replaceAll('"', "&quot;")
      .replaceAll("'", "&#039;");
  }

  function statusLabel(value) {
    return String(value || "unknown").replaceAll("_", " ");
  }

  function eventName(event) {
    return statusLabel(event.kind?.type || "unknown");
  }

  function eventTask(event) {
    return event.kind?.task_id || null;
  }

  function eventWorker(event) {
    const worker = event.kind?.worker || event.kind?.to_worker;
    return typeof worker === "string" ? worker : worker?.[0] || "";
  }

  function setConnection(status, text) {
    connectionState.dataset.state = status;
    connectionState.textContent = text;
  }

  function showError(error) {
    dashboard.hidden = true;
    emptyState.hidden = true;
    errorState.hidden = false;
    $("#error-title").textContent = "The Runtime could not be read.";
    $("#error-message").textContent = error.message || String(error);
    setConnection("offline", "Disconnected");
  }

  function clearError() {
    errorState.hidden = true;
  }

  async function requestJson(path, signal) {
    const response = await fetch(path, { signal, headers: { Accept: "application/json" } });
    const payload = await response.json();
    if (!response.ok || payload.error) {
      throw new Error(payload.error?.message || `Request failed (${response.status})`);
    }
    return payload.data;
  }

  async function loadRuns() {
    const controller = new AbortController();
    try {
      const runs = await requestJson("/api/runs", controller.signal);
      state.runs = runs;
      runSelect.innerHTML = "";
      for (const run of runs) {
        const option = document.createElement("option");
        option.value = run.run_id;
        option.textContent = `${run.run_id} / ${statusLabel(run.status)}`;
        runSelect.append(option);
      }

      if (!state.runId && runs[0]) {
        state.runId = runs[0].run_id;
      }
      if (state.runId && runs.some((run) => run.run_id === state.runId)) {
        runSelect.value = state.runId;
        emptyState.hidden = true;
        await refresh(true);
      } else {
        dashboard.hidden = true;
        emptyState.hidden = false;
        setConnection("live", "Live");
      }
    } catch (error) {
      showError(error);
    }
  }

  async function refresh(reset = false) {
    if (!state.runId) {
      return;
    }
    if (state.controller) {
      state.controller.abort();
    }
    state.controller = new AbortController();
    const { signal } = state.controller;
    try {
      const overview = await requestJson(
        `/api/runs/${encodeURIComponent(state.runId)}/overview`,
        signal,
      );
      if (reset) {
        state.events.clear();
        state.cursor = 0;
        state.plan = null;
        state.selectedEvent = null;
        state.selectedTask = null;
        resetView();
      }
      state.overview = overview;
      syncTaskSelect();
      const page = await requestJson(
        `/api/runs/${encodeURIComponent(state.runId)}/events?since=${state.cursor}&limit=100`,
        signal,
      );
      for (const event of page.events) {
        state.events.set(event.seq, event);
        state.cursor = Math.max(state.cursor, event.seq);
      }
      if (!state.plan || reset) {
        try {
          state.plan = await requestJson(
            `/api/runs/${encodeURIComponent(state.runId)}/plan`,
            signal,
          );
        } catch {
          state.plan = null;
        }
      }
      render();
      setConnection("live", "Live");
      $("#last-updated").textContent = `updated ${new Date().toLocaleTimeString()}`;
      $("#history-cursor").textContent = `history ${overview.summary.history_version}`;
      clearError();
    } catch (error) {
      if (error.name !== "AbortError") {
        showError(error);
      }
    }
  }

  function render() {
    if (!state.overview) {
      return;
    }
    dashboard.hidden = false;
    const { summary } = state.overview;
    $("#run-title").textContent = summary.run_id;
    $("#run-status").textContent = statusLabel(summary.status);
    $("#run-status").dataset.status = String(summary.status).toLowerCase();
    $("#plan-revision").textContent = summary.plan_revision;
    const selectedTask = state.selectedTask ? taskMap().get(state.selectedTask) : null;
    $("#flow-title").textContent = selectedTask
      ? `Execution: ${selectedTask.title}`
      : "Plan to durable attempt";
    $("#metric-grid").innerHTML = [
      metric(summary.task_count, "tasks"),
      metric(summary.succeeded_tasks, "succeeded"),
      metric(summary.active_tasks, "active"),
      metric(summary.waiting_tasks, "waiting"),
      metric(summary.failed_tasks, "failed"),
      metric(state.overview.recovery_events, "recoveries"),
    ].join("");
    renderFlow();
    renderInspector();
    renderEvents();
  }

  function metric(value, label) {
    return `<div class="metric"><strong>${escapeHtml(value)}</strong><span>${escapeHtml(label)}</span></div>`;
  }

  function taskMap() {
    const tasks = new Map();
    for (const task of state.overview?.tasks || []) {
      tasks.set(task.task_id, {
        id: task.task_id,
        title: task.task_id,
        status: task.status,
        attempt: task.attempt,
        worker: task.worker,
        checkpoint: task.checkpoint,
        result: task.result,
        failure: task.failure,
      });
    }
    for (const issue of state.plan?.issues || []) {
      const task = tasks.get(issue.id);
      if (task) {
        task.title = issue.title;
        task.priority = issue.priority;
      } else if (issue.runtime_status) {
        tasks.set(issue.id, {
          id: issue.id,
          title: issue.title,
          status: issue.runtime_status,
          attempt: issue.attempt || 0,
          worker: issue.worker,
          priority: issue.priority,
        });
      }
    }
    return tasks;
  }

  function syncTaskSelect() {
    const tasks = [...taskMap().values()];
    const current = state.selectedTask;
    taskSelect.innerHTML = "";
    for (const task of tasks) {
      const option = document.createElement("option");
      option.value = task.id;
      option.textContent = `${task.title} / ${statusLabel(task.status)}`;
      taskSelect.append(option);
    }
    if (!tasks.length) {
      taskSelect.disabled = true;
      taskSelect.innerHTML = "<option>No tasks in this Run</option>";
      return;
    }
    taskSelect.disabled = false;
    state.selectedTask = tasks.some((task) => task.id === current) ? current : tasks[0].id;
    taskSelect.value = state.selectedTask;
  }

  function buildLayout() {
    const tasks = [...taskMap().values()];
    const compact = window.innerWidth < 820;
    const edges = (state.plan?.edges || []).filter(
      (edge) => tasks.some((task) => task.id === edge.source_id) && tasks.some((task) => task.id === edge.target_id),
    );
    const layers = new Map(tasks.map((task) => [task.id, 0]));
    const independent = !compact && edges.length === 0;
    for (let pass = 0; pass < tasks.length; pass += 1) {
      for (const edge of edges) {
        layers.set(edge.target_id, Math.max(layers.get(edge.target_id) || 0, (layers.get(edge.source_id) || 0) + 1));
      }
    }
    const columns = new Map();
    for (const [index, task] of tasks.entries()) {
      const layer = independent ? index : layers.get(task.id) || 0;
      if (!columns.has(layer)) {
        columns.set(layer, []);
      }
      columns.get(layer).push(task);
    }
    const nodeWidth = compact ? 250 : 250;
    const nodeHeight = 126;
    const gapX = compact ? 0 : 86;
    const gapY = 44;
    const positions = new Map();
    let maxRows = 1;
    for (const [layer, column] of [...columns.entries()].sort((a, b) => a[0] - b[0])) {
      maxRows = Math.max(maxRows, column.length);
      const totalHeight = column.length * nodeHeight + (column.length - 1) * gapY;
      column.forEach((task, index) => {
        positions.set(task.id, {
          x: compact ? 75 : 70 + layer * (nodeWidth + gapX),
          y: compact
            ? 70 + positions.size * (nodeHeight + gapY)
            : Math.max(70, (560 - totalHeight) / 2 + index * (nodeHeight + gapY)),
        });
      });
    }
    return {
      tasks,
      edges,
      positions,
      compact,
      width: compact ? 400 : Math.max(1100, Math.max(1, columns.size) * (nodeWidth + gapX) + 50),
      height: compact
        ? Math.max(560, tasks.length * (nodeHeight + gapY) + 100)
        : Math.max(560, maxRows * (nodeHeight + gapY) + 110),
    };
  }

  function svgElement(name, attributes = {}) {
    const element = document.createElementNS(SVG_NS, name);
    for (const [key, value] of Object.entries(attributes)) {
      element.setAttribute(key, value);
    }
    return element;
  }

  function svgText(parent, value, x, y, className) {
    const text = svgElement("text", { x, y, class: className });
    text.textContent = value;
    parent.append(text);
    return text;
  }

  function renderFlow() {
    if (state.selectedTask) {
      renderTaskLifecycle();
      return;
    }
    const layout = buildLayout();
    $("#flow-empty").hidden = layout.tasks.length > 0;
    flowCanvas.setAttribute("viewBox", `0 0 ${layout.width} ${layout.height}`);
    flowWorld.replaceChildren();
    if (!layout.tasks.length) {
      applyTransform();
      return;
    }

    for (const edge of layout.edges) {
      const source = layout.positions.get(edge.source_id);
      const target = layout.positions.get(edge.target_id);
      const startX = source.x + 250;
      const startY = source.y + 63;
      const endX = target.x;
      const endY = target.y + 63;
      const curve = layout.compact ? 52 : Math.max(50, (endX - startX) / 2);
      const path = svgElement("path", {
        class: "flow-edge",
        d: layout.compact
          ? `M ${source.x + 125} ${source.y + 126} C ${source.x + 125 + curve} ${source.y + 140}, ${target.x + 125 + curve} ${target.y - 14}, ${target.x + 125} ${target.y}`
          : `M ${startX} ${startY} C ${startX + curve} ${startY}, ${endX - curve} ${endY}, ${endX} ${endY}`,
      });
      flowWorld.append(path);
      const edgeLabel = svgText(flowWorld, statusLabel(edge.edge_type), (startX + endX) / 2 - 22, (startY + endY) / 2 - 8, "flow-edge-label");
      edgeLabel.setAttribute("text-anchor", "middle");
    }

    for (const task of layout.tasks) {
      const position = layout.positions.get(task.id);
      const node = svgElement("g", {
        class: `flow-node ${statusClass(task.status)}${state.selectedTask === task.id ? " is-selected" : ""}`,
        "data-task-id": task.id,
        role: "button",
        tabindex: "0",
        "aria-label": `${task.title}, ${statusLabel(task.status)}`,
      });
      node.append(
        svgElement("rect", {
          class: "flow-node-surface",
          x: position.x,
          y: position.y,
          width: 250,
          height: 126,
          rx: 6,
        }),
        svgElement("circle", {
          class: "flow-node-dot",
          cx: position.x + 20,
          cy: position.y + 23,
          r: 6,
        }),
      );
      svgText(node, task.title, position.x + 36, position.y + 28, "flow-node-title");
      svgText(node, task.id, position.x + 16, position.y + 56, "flow-node-id");
      svgText(node, statusLabel(task.status), position.x + 16, position.y + 82, "flow-node-status");
      svgText(node, `attempt ${task.attempt}${task.worker ? ` · ${task.worker}` : ""}`, position.x + 16, position.y + 105, "flow-node-meta");
      node.addEventListener("click", () => selectTask(task.id));
      node.addEventListener("keydown", (event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          selectTask(task.id);
        }
      });
      flowWorld.append(node);
    }
    applyTransform();
  }

  function renderTaskLifecycle() {
    const task = taskMap().get(state.selectedTask);
    const events = [...state.events.values()]
      .filter((event) => eventTask(event) === state.selectedTask)
      .sort((left, right) => left.seq - right.seq);
    const steps = events.length
      ? events.map((event) => ({
        id: `event-${event.seq}`,
        event,
        title: eventName(event),
        status: eventStatus(event),
        meta: [eventTask(event), eventWorker(event)].filter(Boolean).join(" · "),
      }))
      : [{
        id: "current",
        event: null,
        title: statusLabel(task?.status || "unknown"),
        status: task?.status || "waiting",
        meta: task ? `attempt ${task.attempt}` : "no task selected",
      }];
    const compact = window.innerWidth < 820;
    const nodeWidth = 250;
    const nodeHeight = 110;
    const gap = compact ? 34 : 70;
    const positions = new Map(
      steps.map((step, index) => [
        step.id,
        compact
          ? { x: 75, y: 58 + index * (nodeHeight + gap) }
          : { x: 44 + index * (nodeWidth + gap), y: 215 },
      ]),
    );
    const width = compact ? 400 : Math.max(1100, steps.length * (nodeWidth + gap) + 50);
    const height = compact ? Math.max(560, steps.length * (nodeHeight + gap) + 105) : 560;
    $("#flow-empty").hidden = true;
    flowCanvas.setAttribute("viewBox", `0 0 ${width} ${height}`);
    flowWorld.replaceChildren();

    steps.slice(1).forEach((step, index) => {
      const source = positions.get(steps[index].id);
      const target = positions.get(step.id);
      const path = svgElement("path", {
        class: "flow-edge",
        d: compact
          ? `M ${source.x + 125} ${source.y + nodeHeight} C ${source.x + 125} ${source.y + nodeHeight + 20}, ${target.x + 125} ${target.y - 20}, ${target.x + 125} ${target.y}`
          : `M ${source.x + nodeWidth} ${source.y + 55} C ${source.x + nodeWidth + 35} ${source.y + 55}, ${target.x - 35} ${target.y + 55}, ${target.x} ${target.y + 55}`,
      });
      flowWorld.append(path);
    });

    for (const step of steps) {
      const position = positions.get(step.id);
      const selected = step.event && state.selectedEvent === step.event.seq;
      const node = svgElement("g", {
        class: `flow-node ${statusClass(step.status)}${selected ? " is-selected" : ""}`,
        "data-event-seq": step.event?.seq || "",
        role: "button",
        tabindex: "0",
        "aria-label": `${step.title}, ${step.meta}`,
      });
      node.append(
        svgElement("rect", {
          class: "flow-node-surface",
          x: position.x,
          y: position.y,
          width: nodeWidth,
          height: nodeHeight,
          rx: 6,
        }),
        svgElement("circle", {
          class: "flow-node-dot",
          cx: position.x + 20,
          cy: position.y + 23,
          r: 6,
        }),
      );
      svgText(node, step.title, position.x + 36, position.y + 28, "flow-node-title");
      svgText(node, step.event ? `seq ${step.event.seq}` : state.selectedTask, position.x + 16, position.y + 55, "flow-node-id");
      svgText(node, statusLabel(step.status), position.x + 16, position.y + 80, "flow-node-status");
      svgText(node, step.meta || "run", position.x + 16, position.y + 101, "flow-node-meta");
      if (step.event) {
        node.addEventListener("click", () => selectEvent(step.event.seq));
        node.addEventListener("keydown", (event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            selectEvent(step.event.seq);
          }
        });
      }
      flowWorld.append(node);
    }
    $("#flow-note").textContent = `${steps.length} lifecycle step${steps.length === 1 ? "" : "s"} for this task. Click a step to inspect the event.`;
    applyTransform();
  }

  function eventStatus(event) {
    const type = event.kind?.type || "";
    if (type.includes("succeeded")) return "succeeded";
    if (type.includes("timed_out") || type.includes("failed")) return "failed";
    if (type.includes("claimed") || type.includes("started")) return "running";
    if (type.includes("ready") || type.includes("scheduled") || type.includes("created")) return "waiting";
    return "waiting";
  }

  function statusClass(status) {
    if (status === "succeeded" || status === "completed") return "is-succeeded";
    if (status === "running" || status === "claimed" || status === "scheduled") return "is-active";
    if (status === "failed") return "is-failed";
    return "is-waiting";
  }

  function selectTask(taskId) {
    state.selectedTask = taskId;
    state.selectedEvent = null;
    renderFlow();
    renderInspector();
    renderEvents();
  }

  function selectEvent(seq) {
    const event = state.events.get(seq);
    state.selectedEvent = seq;
    state.selectedTask = eventTask(event);
    renderFlow();
    renderInspector();
    renderEvents();
  }

  function renderInspector() {
    const task = state.selectedTask ? taskMap().get(state.selectedTask) : null;
    const event = state.events.get(state.selectedEvent);
    const name = $("#inspector-name");
    const summary = $("#inspector-summary");
    const content = $("#inspector-content");
    if (task) {
      content.innerHTML = `
        <p class="inspector-kicker">Selected task</p>
        <strong id="inspector-name">${escapeHtml(task.title)}</strong>
        <dl class="inspector-facts">
          <div><dt>Status</dt><dd>${escapeHtml(statusLabel(task.status))}</dd></div>
          <div><dt>Task ID</dt><dd>${escapeHtml(task.id)}</dd></div>
          <div><dt>Attempt</dt><dd>${escapeHtml(task.attempt)}</dd></div>
          <div><dt>Worker</dt><dd>${escapeHtml(task.worker || "unassigned")}</dd></div>
        </dl>`;
      $("#detail-seq").textContent = event ? `seq ${event.seq}` : "task";
      $("#event-detail").textContent = event
        ? JSON.stringify(event, null, 2)
        : task.failure || task.result
          ? JSON.stringify(task.failure || task.result, null, 2)
          : "No event selected for this task.";
      return;
    }
    name.textContent = state.overview?.summary.run_id || "Select a task or event";
    summary.textContent = "The map is the primary view. Select a node or event to inspect durable state.";
    $("#detail-seq").textContent = "run";
    $("#event-detail").textContent = event ? JSON.stringify(event, null, 2) : "No event selected.";
  }

  function renderEvents() {
    const selectedTask = state.selectedTask;
    const events = [...state.events.values()]
      .sort((left, right) => right.seq - left.seq)
      .filter((event) => !selectedTask || eventTask(event) === selectedTask);
    $("#event-count").textContent = `${events.length} visible`;
    $("#event-list").innerHTML = events.length
      ? events.slice(0, 18).map(eventMarkup).join("")
      : '<p class="panel-note">No matching execution events.</p>';
    $("#event-list").querySelectorAll("[data-seq]").forEach((item) => {
      item.addEventListener("click", () => selectEvent(Number(item.dataset.seq)));
      item.addEventListener("keydown", (event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          selectEvent(Number(item.dataset.seq));
        }
      });
    });
  }

  function eventMarkup(event) {
    const selected = state.selectedEvent === event.seq;
    const task = eventTask(event);
    const worker = eventWorker(event);
    return `<button class="event-item${selected ? " is-selected" : ""}" type="button" data-seq="${event.seq}">
      <span class="event-seq">${event.seq}</span>
      <span class="event-main"><strong>${escapeHtml(eventName(event))}</strong><small>${escapeHtml([task, worker].filter(Boolean).join(" · ") || "run")}</small></span>
      <time>${escapeHtml(formatTime(event.occurred_at))}</time>
    </button>`;
  }

  function formatTime(value) {
    const date = new Date(value);
    return Number.isNaN(date.getTime())
      ? value
      : date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  }

  function applyTransform() {
    flowWorld.setAttribute(
      "transform",
      `translate(${state.pan.x} ${state.pan.y}) scale(${state.zoom})`,
    );
    $("#flow-zoom-label").textContent = `${Math.round(state.zoom * 100)}%`;
  }

  function setZoom(value) {
    state.zoom = Math.min(1.8, Math.max(0.65, value));
    applyTransform();
  }

  function resetView() {
    state.zoom = 1;
    state.pan = { x: 0, y: 0 };
    applyTransform();
  }

  function bindFlowControls() {
    $("#zoom-in").addEventListener("click", () => setZoom(state.zoom + 0.1));
    $("#zoom-out").addEventListener("click", () => setZoom(state.zoom - 0.1));
    $("#zoom-reset").addEventListener("click", resetView);
    flowViewport.addEventListener("wheel", (event) => {
      event.preventDefault();
      setZoom(state.zoom + (event.deltaY < 0 ? 0.08 : -0.08));
    }, { passive: false });
    flowViewport.addEventListener("pointerdown", (event) => {
      if (event.target.closest(".flow-node")) return;
      state.dragging = { x: event.clientX, y: event.clientY, panX: state.pan.x, panY: state.pan.y };
      flowViewport.setPointerCapture(event.pointerId);
      flowViewport.classList.add("is-dragging");
    });
    flowViewport.addEventListener("pointermove", (event) => {
      if (!state.dragging) return;
      state.pan.x = state.dragging.panX + event.clientX - state.dragging.x;
      state.pan.y = state.dragging.panY + event.clientY - state.dragging.y;
      applyTransform();
    });
    flowViewport.addEventListener("pointerup", () => {
      state.dragging = null;
      flowViewport.classList.remove("is-dragging");
    });
    flowViewport.addEventListener("pointerleave", () => {
      state.dragging = null;
      flowViewport.classList.remove("is-dragging");
    });
  }

  runSelect.addEventListener("change", () => {
    state.runId = runSelect.value;
    refresh(true);
  });
  taskSelect.addEventListener("change", () => {
    state.selectedTask = taskSelect.value || null;
    state.selectedEvent = null;
    render();
  });
  $("#refresh-button").addEventListener("click", () => refresh(true));
  $("#retry-button").addEventListener("click", () => {
    clearError();
    loadRuns();
  });

  bindFlowControls();
  loadRuns();
  state.timer = window.setInterval(() => refresh(false), 1500);
})();
