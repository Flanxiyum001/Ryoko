//! The multi-file review drawer: the working tree's Git changes listed with
//! status badges, each opening the shared diff review surface. Formatting is
//! pure and unit-tested; this file owns the drawer DOM and IPC.

import { invoke } from '@tauri-apps/api/core';

export interface ChangeEntry {
  path: string;
  kind: string;
  untracked: boolean;
}

export interface ChangesSnapshot {
  protocol: number;
  entries: ChangeEntry[];
  isRepository: boolean;
}

const KIND_CLASS: Record<string, string> = {
  added: 'change-added',
  modified: 'change-modified',
  deleted: 'change-deleted',
  renamed: 'change-renamed',
  other: 'change-other',
};

export function kindBadge(kind: string, untracked: boolean): string {
  if (untracked) return '??';
  const letter = kind.charAt(0).toUpperCase();
  return letter.length > 0 ? letter : '~';
}

export function kindClass(kind: string): string {
  return KIND_CLASS[kind] ?? 'change-other';
}

export function summarizeChanges(entries: readonly ChangeEntry[]): string {
  if (entries.length === 0) return 'No uncommitted changes.';
  const counts = new Map<string, number>();
  for (const entry of entries) counts.set(entry.kind, (counts.get(entry.kind) ?? 0) + 1);
  const order = ['added', 'modified', 'deleted', 'renamed', 'other'];
  return order
    .filter((kind) => counts.has(kind))
    .map((kind) => `${counts.get(kind)} ${kind}`)
    .join(', ');
}

export interface ChangesPanelDeps {
  openDiff: (path: string) => Promise<void>;
  setStatus: (message: string) => void;
}

/** Fetches the snapshot and renders it into #changesList. */
export async function refreshChanges(
  host: HTMLElement,
  summary: HTMLElement,
  deps: ChangesPanelDeps,
): Promise<void> {
  try {
    const snapshot = await invoke<ChangesSnapshot>('git_changes');
    summary.textContent = summarizeChanges(snapshot.entries);
    renderChanges(snapshot.entries, snapshot.isRepository, host, deps);
  } catch (error) {
    summary.textContent = '';
    host.replaceChildren();
    const note = document.createElement('p');
    note.className = 'hint';
    note.textContent = String(error);
    host.append(note);
  }
}

export function renderChanges(
  entries: readonly ChangeEntry[],
  isRepository: boolean,
  host: HTMLElement,
  deps: ChangesPanelDeps,
): void {
  host.replaceChildren();
  if (!isRepository) {
    host.append(kindNote('This workspace is not a Git repository.'));
    return;
  }
  if (entries.length === 0) {
    host.append(kindNote('Working tree is clean.'));
    return;
  }
  const fragment = document.createDocumentFragment();
  for (const entry of entries.slice(0, 500)) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'change-row';
    const badge = document.createElement('span');
    badge.className = 'change-badge ' + kindClass(entry.kind);
    badge.textContent = kindBadge(entry.kind, entry.untracked);
    const name = document.createElement('span');
    name.className = 'change-path';
    name.textContent = entry.path;
    name.title = entry.path;
    button.append(badge, name);
    if (entry.kind === 'deleted') {
      button.disabled = true;
      button.title = 'Deleted; restore via Git hunk review.';
    } else {
      button.addEventListener('click', () => {
        void deps.openDiff(entry.path);
      });
    }
    fragment.append(button);
  }
  host.append(fragment);
}

function kindNote(text: string): HTMLElement {
  const note = document.createElement('p');
  note.className = 'hint';
  note.textContent = text;
  return note;
}
