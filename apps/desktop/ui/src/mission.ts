/** Pure event-log parsing, reduction and layout for the Mission Control panel. */

export interface MissionEvent {
  type: string;
  task_id?: string;
  role?: unknown;
  description?: string;
  dependencies?: string[];
  agent?: string;
  attempt?: number;
  phase?: string;
  stage?: string;
  at_ms?: number;
  message?: string;
  requests?: number;
  input_tokens?: number;
  output_tokens?: number;
  ledger_cost_usd?: number;
  passed?: boolean;
  summary?: string;
  logs?: string[];
  duration_ms?: number;
  files?: string[];
  output?: string;
  error?: string;
  retrying?: boolean;
  reason?: string;
  attempts?: number;
  run_id?: string;
  task_count?: number;
  pr_id?: string;
  branch?: string;
  succeeded?: number;
  failed?: number;
  skipped?: number;
}

export type MissionStatus =
  | 'scheduled'
  | 'running'
  | 'retrying'
  | 'validating'
  | 'succeeded'
  | 'failed'
  | 'skipped';

export type MissionPhase = 'thinking' | 'coding' | 'testing' | 'waiting';

export interface MissionActivity {
  atMs: number;
  attempt?: number;
  kind: 'phase' | 'progress' | 'validation' | 'pull-request' | 'result';
  label: string;
  detail?: string;
}

export interface MissionAttempt {
  number: number;
  agent?: string;
  status: MissionStatus;
  phase?: MissionPhase;
  progress?: string;
  validation?: {
    passed: boolean;
    summary: string;
    logs: string[];
    durationMs: number;
  };
  error?: string;
  output?: string;
}

export interface MissionTask {
  id: string;
  role: string;
  description: string;
  dependencies: string[];
  status: MissionStatus;
  agent?: string;
  attempt?: number;
  phase?: MissionPhase;
  failureStage?: string;
  progress?: string;
  history?: MissionAttempt[];
  activity?: MissionActivity[];
  usage?: {
    requests: number;
    inputTokens: number;
    outputTokens: number;
    ledgerCostUsd: number;
  };
  validation?: {
    passed: boolean;
    summary: string;
    logs: string[];
    durationMs: number;
  };
  pullRequest?: { id: string; branch: string; files: string[]; summary: string };
  output?: string;
  error?: string;
  attempts?: number;
}

export interface MissionSnapshot {
  runId?: string;
  taskCount: number;
  tasks: MissionTask[];
  totals: { requests: number; inputTokens: number; outputTokens: number; ledgerCostUsd: number };
  finished?: { succeeded: number; failed: number; skipped: number; durationMs: number };
}

export type LiveStatus =
  | 'Waiting'
  | 'Thinking'
  | 'Coding'
  | 'Testing'
  | 'Succeeded'
  | 'Failed'
  | 'Skipped';

export function liveStatus(task: MissionTask): LiveStatus {
  if (task.status === 'succeeded') return 'Succeeded';
  if (task.status === 'failed') return 'Failed';
  if (task.status === 'skipped') return 'Skipped';
  if (task.status === 'scheduled') return 'Waiting';
  if (task.status === 'validating' || task.phase === 'testing') return 'Testing';
  if (task.status === 'retrying') return 'Thinking';
  if (task.phase === 'coding') return 'Coding';
  if (task.phase === 'waiting') return 'Waiting';
  return 'Thinking';
}

export function formatTokens(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return '0';
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}m`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(1)}k`;
  return String(Math.round(value));
}

export function formatLedgerCost(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return '$0.0000';
  return `$${value.toFixed(4)}`;
}

export function parseMissionLog(text: string): MissionEvent[] {
  const events: MissionEvent[] = [];
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    try {
      const value: unknown = JSON.parse(trimmed);
      if (
        typeof value === 'object' &&
        value !== null &&
        typeof (value as { type?: unknown }).type === 'string'
      ) {
        events.push(value as MissionEvent);
      }
    } catch {
      // The writer flushes line by line; a trailing partial line is expected.
    }
  }
  return events;
}

export function roleLabel(role: unknown): string {
  if (typeof role === 'string') return role.replaceAll('_', '-');
  if (typeof role === 'object' && role !== null) {
    const custom = (role as { custom?: unknown }).custom;
    if (typeof custom === 'string') return custom;
  }
  return 'agent';
}

function upsert(tasks: Map<string, MissionTask>, event: MissionEvent): MissionTask | null {
  if (!event.task_id) return null;
  let task = tasks.get(event.task_id);
  if (!task) {
    task = {
      id: event.task_id,
      role: roleLabel(event.role),
      description: event.description ?? 'Task',
      dependencies: event.dependencies ?? [],
      status: 'scheduled',
    };
    tasks.set(task.id, task);
  }
  if (event.role !== undefined) task.role = roleLabel(event.role);
  if (event.description) task.description = event.description;
  if (event.dependencies) task.dependencies = event.dependencies;
  return task;
}

function recordAttempt(task: MissionTask, number?: number): MissionAttempt {
  const target = number ?? task.attempt ?? task.attempts ?? 1;
  task.history ??= [];
  let attempt = task.history.find((item) => item.number === target);
  if (!attempt) {
    attempt = { number: target, status: 'scheduled' };
    task.history.push(attempt);
  }
  return attempt;
}

function recordActivity(
  task: MissionTask,
  event: MissionEvent,
  kind: MissionActivity['kind'],
  label: string,
  detail?: string,
): void {
  task.activity ??= [];
  task.activity.push({
    atMs: typeof event.at_ms === 'number' ? event.at_ms : 0,
    attempt: event.attempt,
    kind,
    label,
    detail: detail ? detail.slice(0, 500) : undefined,
  });
  if (task.activity.length > 80) task.activity.splice(0, task.activity.length - 80);
}

export function reduceMissionEvents(events: MissionEvent[]): MissionSnapshot {
  const tasks = new Map<string, MissionTask>();
  let runId: string | undefined;
  let taskCount = 0;
  let finished: MissionSnapshot['finished'];
  const totals = { requests: 0, inputTokens: 0, outputTokens: 0, ledgerCostUsd: 0 };

  for (const event of events) {
    switch (event.type) {
      case 'run_started':
        tasks.clear();
        runId = event.run_id;
        taskCount = event.task_count ?? 0;
        finished = undefined;
        break;
      case 'task_scheduled':
        upsert(tasks, event);
        break;
      case 'task_started': {
        const task = upsert(tasks, event);
        if (task) {
          task.status = 'running';
          task.agent = event.agent;
          task.attempt = event.attempt;
          const attempt = recordAttempt(task, event.attempt);
          attempt.status = 'running';
          attempt.agent = event.agent;
        }
        break;
      }
      case 'task_phase': {
        const task = tasks.get(event.task_id ?? '');
        if (task) {
          const phase = event.phase;
          if (phase === 'thinking' || phase === 'coding' || phase === 'testing' || phase === 'waiting') {
            task.phase = phase;
            recordAttempt(task).phase = phase;
          }
          recordActivity(task, event, 'phase', event.phase ?? 'phase', event.message);
        }
        break;
      }
      case 'task_progress': {
        const task = tasks.get(event.task_id ?? '');
        if (task) {
          task.progress = event.message;
          recordAttempt(task).progress = event.message;
          recordActivity(task, event, 'progress', 'Progress', event.message);
        }
        break;
      }
      case 'task_usage': {
        const task = tasks.get(event.task_id ?? '');
        if (task) {
          task.usage = {
            requests: event.requests ?? 0,
            inputTokens: event.input_tokens ?? 0,
            outputTokens: event.output_tokens ?? 0,
            ledgerCostUsd: event.ledger_cost_usd ?? 0,
          };
        }
        break;
      }
      case 'validation_started': {
        const task = tasks.get(event.task_id ?? '');
        if (task) {
          task.status = 'validating';
          task.phase = 'testing';
          recordAttempt(task).status = 'validating';
          recordActivity(task, event, 'phase', 'Testing');
        }
        break;
      }
      case 'validation_finished': {
        const task = tasks.get(event.task_id ?? '');
        if (task) {
          task.validation = {
            passed: Boolean(event.passed),
            summary: event.summary ?? '',
            logs: (event.logs ?? []).slice(0, 100),
            durationMs: event.duration_ms ?? 0,
          };
          recordAttempt(task).validation = task.validation;
          recordActivity(
            task,
            event,
            'validation',
            event.passed ? 'Validation passed' : 'Validation failed',
            event.summary,
          );
        }
        break;
      }
      case 'pull_request_opened': {
        const task = tasks.get(event.task_id ?? '');
        if (task) {
          task.pullRequest = {
            id: event.pr_id ?? '',
            branch: event.branch ?? '',
            files: event.files ?? [],
            summary: event.summary ?? '',
          };
          task.phase = 'waiting';
          recordActivity(task, event, 'pull-request', 'Pull request opened', event.summary);
        }
        break;
      }
      case 'task_succeeded': {
        const task = upsert(tasks, event);
        if (task) {
          task.status = 'succeeded';
          task.output = event.output;
          task.attempts = event.attempts;
          const attempt = recordAttempt(task);
          attempt.status = 'succeeded';
          attempt.output = event.output;
          recordActivity(task, event, 'result', 'Succeeded', event.output);
        }
        break;
      }
      case 'task_failed': {
        const task = upsert(tasks, event);
        if (task) {
          task.status = event.retrying ? 'retrying' : 'failed';
          task.error = event.error;
          task.attempts = event.attempts;
          task.failureStage = event.stage;
          const attempt = recordAttempt(task);
          attempt.status = task.status;
          attempt.error = event.error;
          recordActivity(task, event, 'result', event.retrying ? 'Retrying' : 'Failed', event.error);
        }
        break;
      }
      case 'task_skipped': {
        const task = upsert(tasks, event);
        if (task) {
          task.status = 'skipped';
          task.error = event.reason;
          recordActivity(task, event, 'result', 'Skipped', event.reason);
          // No attempt was dispatched when an upstream dependency skipped this task.
        }
        break;
      }
      case 'run_finished':
        finished = {
          succeeded: event.succeeded ?? 0,
          failed: event.failed ?? 0,
          skipped: event.skipped ?? 0,
          durationMs: event.duration_ms ?? 0,
        };
        break;
      default:
        break;
    }
  }

  for (const task of tasks.values()) {
    if (!task.usage) continue;
    totals.requests += task.usage.requests;
    totals.inputTokens += task.usage.inputTokens;
    totals.outputTokens += task.usage.outputTokens;
    totals.ledgerCostUsd += task.usage.ledgerCostUsd;
  }

  return {
    runId,
    taskCount: taskCount || tasks.size,
    tasks: [...tasks.values()],
    totals,
    finished,
  };
}

/** Show the selected node, its complete prerequisite chain, and its direct consumers. */
export function focusMissionTasks(tasks: MissionTask[], selectedId: string | null): MissionTask[] {
  if (!selectedId || !tasks.some((task) => task.id === selectedId)) return tasks;
  const byId = new Map(tasks.map((task) => [task.id, task]));
  const included = new Set([selectedId]);
  const visitDependencies = (id: string): void => {
    for (const dependency of byId.get(id)?.dependencies ?? []) {
      if (byId.has(dependency) && !included.has(dependency)) {
        included.add(dependency);
        visitDependencies(dependency);
      }
    }
  };
  visitDependencies(selectedId);
  for (const task of tasks) {
    if (task.dependencies.includes(selectedId)) included.add(task.id);
  }
  return tasks.filter((task) => included.has(task.id));
}

export interface PositionedTask {
  task: MissionTask;
  x: number;
  y: number;
  depth: number;
}

export interface MissionLayout {
  width: number;
  height: number;
  nodes: PositionedTask[];
  edges: { from: PositionedTask; to: PositionedTask }[];
}

const NODE_WIDTH = 154;
const NODE_HEIGHT = 56;
const X_GAP = 42;
const Y_GAP = 20;
const PADDING = 18;

/** Deterministic layered layout: a task sits one column right of its deps. */
export function layoutMission(tasks: MissionTask[]): MissionLayout {
  const byId = new Map(tasks.map((task) => [task.id, task]));
  const depths = new Map<string, number>();

  const depthOf = (id: string, visiting: Set<string>): number => {
    const cached = depths.get(id);
    if (cached !== undefined) return cached;
    if (visiting.has(id)) return 0;
    visiting.add(id);
    const task = byId.get(id);
    let depth = 0;
    for (const dependency of task?.dependencies ?? []) {
      if (byId.has(dependency)) depth = Math.max(depth, depthOf(dependency, visiting) + 1);
    }
    visiting.delete(id);
    depths.set(id, depth);
    return depth;
  };

  const groups = new Map<number, MissionTask[]>();
  for (const task of tasks) {
    const depth = depthOf(task.id, new Set());
    const group = groups.get(depth) ?? [];
    group.push(task);
    groups.set(depth, group);
  }

  const nodes: PositionedTask[] = [];
  for (const [depth, group] of [...groups.entries()].sort(([left], [right]) => left - right)) {
    group.forEach((task, index) => {
      nodes.push({
        task,
        depth,
        x: PADDING + depth * (NODE_WIDTH + X_GAP),
        y: PADDING + index * (NODE_HEIGHT + Y_GAP),
      });
    });
  }

  const positions = new Map(nodes.map((node) => [node.task.id, node]));
  const edges: MissionLayout['edges'] = [];
  for (const node of nodes) {
    for (const dependency of node.task.dependencies) {
      const from = positions.get(dependency);
      if (from) edges.push({ from, to: node });
    }
  }

  const columns = groups.size === 0 ? 1 : Math.max(...groups.keys()) + 1;
  const rows = Math.max(1, ...[...groups.values()].map((group) => group.length));
  return {
    width: PADDING * 2 + columns * NODE_WIDTH + (columns - 1) * X_GAP,
    height: PADDING * 2 + rows * NODE_HEIGHT + (rows - 1) * Y_GAP,
    nodes,
    edges,
  };
}

export const missionNodeSize = { width: NODE_WIDTH, height: NODE_HEIGHT };
