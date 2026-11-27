// Agent swarm in the chat: one card, one row per parallel task. Each task
// gets its own branch and Ryoko Review. Pushing a branch and opening its pull
// request happens only when the user clicks.

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { renderReview, reviewHeadline, type ReviewReport } from './ryoko_review';

interface SwarmTask { title: string; goal: string }
type TaskStatus = { state: 'ready' } | { state: 'no_changes' } | { state: 'failed'; detail: string };
interface SwarmTaskResult {
  task: SwarmTask;
  status: TaskStatus;
  branch: string | null;
  base_branch: string | null;
  files: string[];
  summary: string;
  review: ReviewReport | null;
  pr_title: string;
  pr_body: string;
}
type SwarmEvent =
  | { kind: 'planned'; tasks: SwarmTask[] }
  | { kind: 'task_started'; index: number; branch: string }
  | { kind: 'task_progress'; index: number; event: { event: string; name?: string; path?: string } }
  | { kind: 'task_reviewing'; index: number }
  | { kind: 'task_finished'; index: number; result: SwarmTaskResult };

export interface SwarmDeps {
  setStatus: (message: string) => void;
  openFile: (path: string, line?: number) => void;
  onFinished: () => void;
}

/** Short status text for a task row, exported for tests. */
export function taskStatusText(status: TaskStatus): string {
  switch (status.state) {
    case 'ready': return 'Ready for a pull request';
    case 'no_changes': return 'No changes needed';
    default: return 'Failed: ' + status.detail;
  }
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, className?: string, text?: string) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

export function mountSwarm(deps: SwarmDeps): { run: (goal: string) => Promise<void> } {
  const history = document.getElementById('chatHistory')!;
  let active: string | null = null;
  let rows: { root: HTMLElement; state: HTMLElement; body: HTMLElement; steps: number }[] = [];
  let card: HTMLElement | null = null;
  let status: HTMLElement | null = null;

  function scroll(): void { history.scrollTop = history.scrollHeight; }

  function onEvent(event: SwarmEvent): void {
    if (!card || !status) return;
    if (event.kind === 'planned') {
      status.textContent = `${event.tasks.length} task${event.tasks.length === 1 ? '' : 's'} in parallel`;
      const list = el('ol', 'swarm-tasks');
      rows = event.tasks.map((task) => {
        const root = el('li', 'swarm-task');
        const head = el('div', 'swarm-task-head');
        head.append(el('strong', undefined, task.title));
        const state = el('span', 'swarm-task-state', 'Waiting');
        head.append(state);
        const body = el('div', 'swarm-task-body');
        root.append(head, body);
        list.append(root);
        return { root, state, body, steps: 0 };
      });
      card.append(list);
    } else {
      const row = rows[event.index];
      if (!row) return;
      if (event.kind === 'task_started') {
        row.state.textContent = 'Working';
        row.body.replaceChildren(el('code', 'swarm-branch', event.branch));
      } else if (event.kind === 'task_progress') {
        if (event.event.event === 'tool_started') {
          row.steps += 1;
          row.state.textContent = `Working · ${row.steps} steps`;
        }
      } else if (event.kind === 'task_reviewing') {
        row.state.textContent = 'Reviewing';
      } else if (event.kind === 'task_finished') {
        renderResult(row, event.result);
      }
    }
    scroll();
  }

  function renderResult(row: (typeof rows)[number], result: SwarmTaskResult): void {
    row.root.classList.add('swarm-task-' + result.status.state);
    row.state.textContent = taskStatusText(result.status);
    row.body.replaceChildren();
    if (result.branch) row.body.append(el('code', 'swarm-branch', result.branch));
    if (result.files.length) {
      row.body.append(el('p', 'hint', `${result.files.length} file${result.files.length === 1 ? '' : 's'}: ${result.files.slice(0, 4).join(', ')}${result.files.length > 4 ? '…' : ''}`));
    }
    if (result.review) {
      const details = el('details', 'swarm-review');
      details.append(el('summary', undefined, reviewHeadline(result.review)));
      details.append(renderReview(result.review, (finding) => deps.setStatus(
        `${finding.path}${finding.line ? ':' + finding.line : ''} is on branch ${result.branch}. Review the branch in Running agents.`,
      )));
      row.body.append(details);
    }
    if (result.status.state === 'ready' && result.branch && result.base_branch) {
      const actions = el('div', 'agent-actions');
      const push = el('button', 'button primary compact', 'Push & open PR');
      push.type = 'button';
      push.addEventListener('click', async () => {
        push.disabled = true;
        push.textContent = 'Pushing…';
        try {
          const outcome = await invoke<{ pullRequestUrl: string | null }>('swarm_push', {
            branch: result.branch, base: result.base_branch, title: result.pr_title, body: result.pr_body,
          });
          actions.replaceChildren();
          if (outcome.pullRequestUrl) {
            const link = el('a', 'swarm-pr-link', 'Open the pull request');
            link.href = outcome.pullRequestUrl;
            link.target = '_blank';
            link.rel = 'noopener noreferrer';
            const copy = el('button', 'button ghost compact', 'Copy link');
            copy.type = 'button';
            copy.addEventListener('click', () => {
              void navigator.clipboard.writeText(outcome.pullRequestUrl!);
              copy.textContent = 'Copied';
            });
            actions.append(link, copy);
          } else {
            actions.append(el('span', 'hint', 'Pushed. Open the pull request on your Git host.'));
          }
        } catch (error) {
          push.disabled = false;
          push.textContent = 'Push & open PR';
          deps.setStatus(String(error));
        }
      });
      actions.append(push);
      row.body.append(actions);
    }
  }

  async function run(goal: string): Promise<void> {
    if (active) return;
    const requestId = `swarm-${Date.now()}`;
    active = requestId;
    card = el('article', 'chat-turn chat-assistant agent-run swarm-run');
    const head = el('div', 'agent-run-head');
    head.append(el('strong', undefined, 'Ryoko swarm'));
    status = el('span', 'agent-run-status', 'Planning…');
    head.append(status);
    card.append(head, el('p', 'hint', 'Each task works on its own branch. Your files are not changed.'));
    history.append(card);
    scroll();
    try {
      await invoke('swarm_start', { requestId, goal });
    } catch (error) {
      status.textContent = 'Could not start';
      card.append(el('p', 'agent-error', String(error)));
      active = null;
      deps.onFinished();
    }
  }

  void listen<{ requestId: string; event: SwarmEvent }>('ryoko-swarm-event', ({ payload }) => {
    if (payload.requestId === active) onEvent(payload.event);
  });
  void listen<{ requestId: string; report: { tasks: SwarmTaskResult[] } }>('ryoko-swarm-done', ({ payload }) => {
    if (payload.requestId !== active || !card || !status) return;
    const ready = payload.report.tasks.filter((task) => task.status.state === 'ready').length;
    status.textContent = `Done · ${ready} branch${ready === 1 ? '' : 'es'} ready`;
    card.classList.add('agent-run-finished');
    active = null;
    deps.onFinished();
  });

  return { run };
}
