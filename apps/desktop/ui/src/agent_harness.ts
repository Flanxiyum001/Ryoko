// Agent mode for the chat panel, backed by the native `ryoko-harness`.
//
// The agent works through tools, and every file edit is staged. When it
// finishes, the chat shows the changed files. The user can review each one as
// a diff, then apply or discard them. Commands only run after the user allows
// each one in the chat.

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { renderReview, type ReviewReport } from './ryoko_review';

type PlanStatus = 'pending' | 'in_progress' | 'done';
interface PlanItem { task: string; status: PlanStatus }

type HarnessEvent =
  | { event: 'turn_started'; depth: number; turn: number }
  | { event: 'assistant_text'; depth: number; text: string }
  | { event: 'tool_started'; depth: number; name: string }
  | { event: 'tool_finished'; depth: number; name: string; ok: boolean }
  | { event: 'plan_updated'; items: PlanItem[] }
  | { event: 'change_staged'; path: string }
  | { event: 'memory_saved'; note: string }
  | { event: 'command_denied'; program: string }
  | { event: 'context_compacted'; results_elided: number }
  | { event: 'sub_agent_started'; task: string }
  | { event: 'sub_agent_finished'; summary: string };

interface EventPayload { requestId: string; event: HarnessEvent }
interface ApprovalPayload {
  requestId: string; approvalId: string; program: string; args: string[]; reason: string;
}
export interface StagedChange { path: string; original: string | null; content: string }
interface DonePayload {
  requestId: string;
  finalText: string;
  stop: { cause: 'completed' | 'turn_limit' | 'cancelled' | 'model_error'; detail?: string };
  turns: number;
  plan: PlanItem[];
  changes: StagedChange[];
  estimatedCostUsd: number;
}

export interface AgentHarnessDeps {
  setStatus: (message: string) => void;
  showChange: (change: StagedChange, line?: number) => void;
  onApplied: (changes: StagedChange[]) => void;
  onCost: (estimatedCostUsd: number) => void;
  /** Runs the goal as a swarm of parallel tasks, one branch each. */
  runSwarm?: (goal: string) => Promise<void>;
}

const MODE_KEY = 'ryoko.chatMode';

const TOOL_LABELS: Record<string, string> = {
  read_file: 'Read a file',
  list_files: 'Looked through the project',
  search_text: 'Searched the code',
  write_file: 'Wrote a file',
  edit_file: 'Edited a file',
  update_plan: 'Updated the plan',
  remember: 'Saved a note for next time',
  load_skill: 'Loaded a skill',
  run_command: 'Ran a command',
  delegate: 'Handed off a sub-task',
};

/** Plain-language label for a tool call, exported for tests. */
export function toolLabel(name: string): string {
  return TOOL_LABELS[name] ?? `Used ${name}`;
}

/** Line counts for a change summary, exported for tests. */
export function changeStats(change: StagedChange): { added: number; removed: number } {
  const before = change.original === null ? [] : change.original.split('\n');
  const after = change.content.split('\n');
  const counts = new Map<string, number>();
  for (const line of before) counts.set(line, (counts.get(line) ?? 0) + 1);
  let kept = 0;
  for (const line of after) {
    const left = counts.get(line) ?? 0;
    if (left > 0) { counts.set(line, left - 1); kept += 1; }
  }
  return { added: after.length - kept, removed: before.length - kept };
}

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K, className?: string, text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

export function mountAgentHarness(deps: AgentHarnessDeps): {
  agentMode: () => boolean;
  run: (goal: string) => void;
  swarmFinished: () => void;
} {
  const history = document.getElementById('chatHistory')!;
  const input = document.getElementById('chatInput') as HTMLTextAreaElement;
  const send = document.getElementById('chatSend') as HTMLButtonElement;
  const stop = document.getElementById('chatStop') as HTMLButtonElement;
  const note = document.getElementById('chatNote')!;
  const askButton = document.getElementById('chatModeAsk') as HTMLButtonElement;
  const agentButton = document.getElementById('chatModeAgent') as HTMLButtonElement;
  const options = document.getElementById('chatAgentOptions')!;
  const allowCommands = document.getElementById('chatAllowCommands') as HTMLInputElement;
  const modeHint = document.getElementById('chatModeHint')!;

  let mode: 'ask' | 'agent' = localStorage.getItem(MODE_KEY) === 'agent' ? 'agent' : 'ask';
  let active: string | null = null;
  let sequence = 0;
  let view: {
    root: HTMLElement; plan: HTMLOListElement; steps: HTMLUListElement;
    text: HTMLElement; status: HTMLElement; stepCount: number;
  } | null = null;

  function setMode(next: 'ask' | 'agent'): void {
    mode = next;
    localStorage.setItem(MODE_KEY, next);
    askButton.setAttribute('aria-checked', String(next === 'ask'));
    agentButton.setAttribute('aria-checked', String(next === 'agent'));
    options.classList.toggle('hidden', next !== 'agent');
    modeHint.textContent = next === 'agent' ? 'Changes wait for you' : 'Answers only';
    input.placeholder = next === 'agent'
      ? 'Describe a change, for example: add input validation to the signup form'
      : 'Ask anything about your code…';
    if (!active) {
      note.textContent = next === 'agent'
        ? 'The agent can read and edit code. You review every change before it is saved.'
        : 'No changes are applied without your approval.';
    }
  }

  function scroll(): void { history.scrollTop = history.scrollHeight; }

  function startView(): void {
    const root = el('article', 'chat-turn chat-assistant agent-run');
    const head = el('div', 'agent-run-head');
    head.append(el('strong', undefined, 'Ryoko agent'));
    const status = el('span', 'agent-run-status', 'Starting…');
    head.append(status);
    const plan = el('ol', 'agent-plan');
    plan.hidden = true;
    const steps = el('ul', 'agent-steps');
    const text = el('div', 'agent-text');
    root.append(head, plan, steps, text);
    history.append(root);
    view = { root, plan, steps, text, status, stepCount: 0 };
    scroll();
  }

  function addStep(label: string, kind: 'tool' | 'info' | 'warn' = 'tool'): HTMLLIElement {
    const item = el('li', 'agent-step agent-step-' + kind, label);
    if (view) {
      view.steps.append(item);
      view.stepCount += 1;
      // Keep the list short; older steps fold into a counter.
      while (view.steps.children.length > 6) view.steps.firstElementChild?.remove();
      view.status.textContent = `Working · ${view.stepCount} step${view.stepCount === 1 ? '' : 's'}`;
    }
    scroll();
    return item;
  }

  function renderPlan(items: PlanItem[]): void {
    if (!view) return;
    view.plan.replaceChildren(...items.map((item) => {
      const li = el('li', 'agent-plan-item agent-plan-' + item.status, item.task);
      if (item.status === 'in_progress') li.setAttribute('aria-current', 'step');
      return li;
    }));
    view.plan.hidden = items.length === 0;
  }

  function onEvent(event: HarnessEvent): void {
    if (!view) return;
    switch (event.event) {
      case 'tool_started':
        addStep((event.depth > 0 ? 'Sub-agent: ' : '') + toolLabel(event.name));
        break;
      case 'tool_finished':
        if (!event.ok) view.steps.lastElementChild?.classList.add('agent-step-failed');
        break;
      case 'assistant_text':
        if (event.depth === 0) view.text.textContent = event.text;
        break;
      case 'plan_updated':
        renderPlan(event.items);
        break;
      case 'change_staged':
        addStep(`Staged ${event.path}`, 'info');
        break;
      case 'memory_saved':
        addStep(`Will remember: ${event.note}`, 'info');
        break;
      case 'command_denied':
        addStep(`Did not run ${event.program}`, 'warn');
        break;
      case 'sub_agent_started':
        addStep(`Started a sub-agent: ${event.task.slice(0, 120)}`, 'info');
        break;
      default:
        break;
    }
  }

  function askApproval(payload: ApprovalPayload): void {
    if (!view) return;
    const card = el('div', 'agent-approval');
    card.append(el('strong', undefined, 'Allow this command?'));
    card.append(el('code', 'agent-approval-command', [payload.program, ...payload.args].join(' ')));
    if (payload.reason) card.append(el('p', 'hint', payload.reason));
    const actions = el('div', 'agent-actions');
    const allow = el('button', 'button primary compact', 'Run it');
    const deny = el('button', 'button ghost compact', 'Don’t run');
    allow.type = deny.type = 'button';
    const answer = async (yes: boolean) => {
      allow.disabled = deny.disabled = true;
      try {
        await invoke('harness_answer_approval', { approvalId: payload.approvalId, allow: yes });
        card.replaceChildren(el('p', 'hint', yes ? `Allowed: ${payload.program}` : `Declined: ${payload.program}`));
      } catch (error) {
        card.replaceChildren(el('p', 'hint', String(error)));
      }
    };
    allow.addEventListener('click', () => void answer(true));
    deny.addEventListener('click', () => void answer(false));
    actions.append(allow, deny);
    card.append(actions);
    view.root.append(card);
    allow.focus();
    scroll();
  }

  function renderChanges(requestId: string, changes: StagedChange[]): void {
    if (!view || changes.length === 0) return;
    const card = el('section', 'agent-changes');
    const title = el('strong', undefined,
      `${changes.length} file${changes.length === 1 ? '' : 's'} changed · not saved yet`);
    const list = el('ul', 'agent-change-list');
    for (const change of changes) {
      const { added, removed } = changeStats(change);
      const row = el('li', 'agent-change');
      const open = el('button', 'agent-change-open');
      open.type = 'button';
      open.title = 'Review this change';
      open.append(
        el('span', 'agent-change-path', change.path),
        el('span', 'agent-change-tag', change.original === null ? 'new' : ''),
        el('span', 'agent-change-added', `+${added}`),
        el('span', 'agent-change-removed', `−${removed}`),
      );
      open.addEventListener('click', () => deps.showChange(change));
      row.append(open);
      list.append(row);
    }
    const actions = el('div', 'agent-actions');
    const apply = el('button', 'button primary compact', 'Apply all');
    const discard = el('button', 'button ghost compact', 'Discard');
    const reviewButton = el('button', 'button ghost compact', 'Review with Ryoko');
    apply.type = discard.type = reviewButton.type = 'button';
    reviewButton.addEventListener('click', async () => {
      reviewButton.disabled = true;
      reviewButton.textContent = 'Reviewing…';
      try {
        const report = await invoke<ReviewReport>('ryoko_review', {
          source: { source: 'agent', requestId },
        });
        card.insertBefore(renderReview(report, (finding) => {
          const change = changes.find((entry) => entry.path === finding.path);
          if (change) deps.showChange(change, finding.line ?? undefined);
        }), actions);
        reviewButton.remove();
      } catch (error) {
        reviewButton.disabled = false;
        reviewButton.textContent = 'Review with Ryoko';
        deps.setStatus('Review failed: ' + String(error));
      }
    });
    apply.addEventListener('click', async () => {
      apply.disabled = discard.disabled = true;
      try {
        const report = await invoke<{ applied: string[] }>('harness_apply', {
          requestId, paths: changes.map((change) => change.path),
        });
        deps.onApplied(changes.filter((change) => report.applied.includes(change.path)));
        title.textContent = `Applied ${report.applied.length} file${report.applied.length === 1 ? '' : 's'}`;
        actions.remove();
        deps.setStatus('Agent changes applied. Use Changes to review or undo them with Git.');
      } catch (error) {
        apply.disabled = discard.disabled = false;
        deps.setStatus(String(error));
        note.textContent = String(error);
      }
    });
    discard.addEventListener('click', async () => {
      await invoke('harness_discard', { requestId });
      title.textContent = 'Changes discarded';
      list.remove();
      actions.remove();
    });
    actions.append(apply, reviewButton, discard);
    card.append(title, list, actions);
    view.root.append(card);
    scroll();
  }

  function finish(payload: DonePayload): void {
    if (!view) return;
    const labels: Record<DonePayload['stop']['cause'], string> = {
      completed: 'Done',
      turn_limit: 'Stopped: reached the step limit',
      cancelled: 'Stopped',
      model_error: 'Stopped: the model returned an error',
    };
    view.status.textContent = `${labels[payload.stop.cause]} · ${view.stepCount} steps`;
    view.root.classList.add('agent-run-finished');
    if (payload.finalText) view.text.textContent = payload.finalText;
    if (payload.stop.cause === 'model_error' && payload.stop.detail) {
      view.root.append(el('p', 'agent-error', payload.stop.detail));
    }
    renderChanges(payload.requestId, payload.changes);
    deps.onCost(payload.estimatedCostUsd);
    active = null;
    send.disabled = false;
    stop.disabled = true;
    note.textContent = payload.changes.length > 0
      ? 'Review the changes above, then apply or discard them.'
      : 'The agent finished without changing any files.';
    view = null;
  }

  async function run(goal: string): Promise<void> {
    if (active) return;
    const swarm = document.getElementById('chatSwarm') as HTMLInputElement | null;
    if (swarm?.checked && deps.runSwarm) {
      send.disabled = true;
      note.textContent = 'Agents are working in parallel, each on its own branch.';
      await deps.runSwarm(goal);
      return;
    }
    const requestId = `agent-${++sequence}-${Date.now()}`;
    active = requestId;
    startView();
    send.disabled = true;
    stop.disabled = false;
    note.textContent = 'The agent is working. Edits stay staged until you apply them.';
    try {
      await invoke('harness_start', {
        args: { requestId, goal, allowCommands: allowCommands.checked },
      });
    } catch (error) {
      if (view) {
        view.status.textContent = 'Could not start';
        view.root.append(el('p', 'agent-error', String(error)));
      }
      active = null;
      view = null;
      send.disabled = false;
      stop.disabled = true;
      note.textContent = String(error);
    }
  }

  void listen<EventPayload>('ryoko-harness-event', ({ payload }) => {
    if (payload.requestId === active) onEvent(payload.event);
  });
  void listen<ApprovalPayload>('ryoko-harness-approval', ({ payload }) => {
    if (payload.requestId === active) askApproval(payload);
  });
  void listen<DonePayload>('ryoko-harness-done', ({ payload }) => {
    if (payload.requestId === active) finish(payload);
  });
  stop.addEventListener('click', () => {
    if (!active) return;
    if (view) view.status.textContent = 'Stopping…';
    void invoke('harness_cancel');
  });
  askButton.addEventListener('click', () => setMode('ask'));
  agentButton.addEventListener('click', () => setMode('agent'));
  setMode(mode);

  return {
    agentMode: () => mode === 'agent',
    run: (goal) => void run(goal),
    swarmFinished: () => {
      send.disabled = false;
      note.textContent = 'Swarm finished. Push a branch to open its pull request.';
    },
  };
}
