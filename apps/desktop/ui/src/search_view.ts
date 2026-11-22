//! The workspace search/replace panel mounted at the bottom of the editor.
//! Pure grouping logic lives in `search_panel.ts`; this file owns the DOM and
//! the typed `workbench` IPC commands.

import { invoke } from '@tauri-apps/api/core';
import { annotatePreview, groupHits, summarizeMatches, type SearchHit } from './search_panel';

export interface SearchReply {
  protocol: number;
  hits: SearchHit[];
  truncated: boolean;
}

export interface ReplacePreview {
  path: string;
  matchCount: number;
  firstPreview: string;
  contentHash: string;
}

export interface ReplaceReply {
  protocol: number;
  previews: ReplacePreview[];
  totalMatches: number;
}

export interface ReplaceResult {
  protocol: number;
  appliedFiles: number;
  appliedMatches: number;
  skipped: string[];
}

export interface SearchPanelDeps {
  openFile: (path: string) => Promise<void>;
  setStatus: (message: string) => void;
}

const PREVIEW_FILE_LIMIT = 200;

export function mountSearchPanel(deps: SearchPanelDeps): void {
  const searchPanel = document.getElementById('searchPanel');
  if (!searchPanel) return;

  const input = searchPanel.querySelector<HTMLInputElement>('#searchInput');
  const caseToggle = searchPanel.querySelector<HTMLInputElement>('#searchCaseSensitive');
  const replaceInput = searchPanel.querySelector<HTMLInputElement>('#searchReplace');
  const searchButton = searchPanel.querySelector<HTMLButtonElement>('#searchRun');
  const replaceButton = searchPanel.querySelector<HTMLButtonElement>('#searchReplaceRun');
  const output = searchPanel.querySelector<HTMLDivElement>('#searchResults');
  if (!input || !searchButton || !output || !replaceButton || !replaceInput) return;
  const queryBox = input;
  const runButton = searchButton;
  const applyButton = replaceButton;
  const replacementBox = replaceInput;
  const results = output;
  void input; void searchButton; void output; void replaceButton; void replaceInput;

  let lastPreviews: ReplacePreview[] = [];

  async function run(previewOnly: boolean): Promise<void> {
    const query = queryBox.value;
    if (query.length === 0) return;
    const caseSensitive = caseToggle?.checked ?? false;
    runButton.disabled = true;
    applyButton.disabled = true;
    results.replaceChildren(runRow('Searching\u2026'));
    try {
      if (previewOnly) {
        const reply = await invoke<ReplaceReply>('workspace_replace_preview', {
          query,
          replacement: replacementBox.value,
          caseSensitive,
        });
        lastPreviews = reply.previews;
        renderReplace(reply, results);
      } else {
        const reply = await invoke<SearchReply>('workspace_search', { query, caseSensitive });
        lastPreviews = [];
        renderSearch(reply, results, deps);
      }
    } catch (error) {
      results.replaceChildren(runRow(String(error)));
    } finally {
      runButton.disabled = false;
      applyButton.disabled = replacementBox.value.length === 0;
    }
  }

  async function apply(): Promise<void> {
    if (lastPreviews.length === 0) {
      await run(true);
      return;
    }
    applyButton.disabled = true;
    try {
      const result = await invoke<ReplaceResult>('workspace_replace_apply', {
        query: queryBox.value,
        replacement: replacementBox.value,
        caseSensitive: caseToggle?.checked ?? false,
        expected: lastPreviews,
      });
      const skipped = result.skipped.length > 0 ? `, ${result.skipped.length} skipped` : '';
      results.replaceChildren(
        runRow(`Replaced ${result.appliedMatches} match(es) in ${result.appliedFiles} file(s)${skipped}`),
      );
      for (const skip of result.skipped.slice(0, 20)) {
        results.append(runRow(skip));
      }
      lastPreviews = [];
      deps.setStatus(`Workspace replace: ${result.appliedFiles} file(s) updated${skipped}`);
    } catch (error) {
      results.replaceChildren(runRow(String(error)));
    } finally {
      applyButton.disabled = false;
    }
  }

  runButton.addEventListener('click', () => void run(false));
  applyButton.addEventListener('click', () => void apply());
  queryBox.addEventListener('keydown', (event) => {
    if (event.key === 'Enter') {
      event.preventDefault();
      void run(false);
    }
  });
  const invalidatePreview = (): void => {
    if (lastPreviews.length > 0) {
      lastPreviews = [];
      results.replaceChildren(runRow('Search or replacement changed. Preview again before applying.'));
    }
  };
  queryBox.addEventListener('input', invalidatePreview);
  caseToggle?.addEventListener('change', invalidatePreview);
  replacementBox.addEventListener('input', () => {
    invalidatePreview();
    applyButton.disabled = replacementBox.value.length === 0;
  });
}

function renderSearch(reply: SearchReply, output: HTMLElement, deps: SearchPanelDeps): void {
  output.replaceChildren(runRow(summarizeMatches(reply.hits.length, reply.truncated)));
  const fragment = document.createDocumentFragment();
  for (const group of groupHits(reply.hits).slice(0, PREVIEW_FILE_LIMIT)) {
    const head = document.createElement('button');
    head.type = 'button';
    head.className = 'search-group-head';
    head.textContent = `${group.path} (${group.hits.length})`;
    head.title = group.path;
    head.addEventListener('click', () => {
      void deps.openFile(group.path);
    });
    fragment.append(head);
    for (const hit of group.hits.slice(0, 40)) {
      const row = document.createElement('button');
      row.type = 'button';
      row.className = 'search-hit';
      const line = document.createElement('span');
      line.className = 'search-hit-line';
      line.textContent = String(hit.line);
      const preview = document.createElement('span');
      preview.className = 'search-hit-preview';
      for (const segment of annotatePreview(hit)) {
        const span = document.createElement('span');
        if (segment.match) span.className = 'search-hit-match';
        span.textContent = segment.text;
        preview.append(span);
      }
      row.append(line, preview);
      row.addEventListener('click', () => {
        void deps.openFile(group.path).then(() => {
          deps.setStatus(`${group.path}:${hit.line}`);
        });
      });
      fragment.append(row);
    }
  }
  output.append(fragment);
}

function renderReplace(reply: ReplaceReply, output: HTMLElement): void {
  output.replaceChildren(
    runRow(
      reply.previews.length === 0
        ? 'Nothing to replace.'
        : `Would change ${reply.totalMatches} match(es) in ${reply.previews.length} file(s) \u2014 press Apply to continue.`,
    ),
  );
  for (const preview of reply.previews.slice(0, PREVIEW_FILE_LIMIT)) {
    output.append(runRow(`${preview.path}: ${preview.matchCount} match(es)`));
  }
}

function runRow(text: string): HTMLElement {
  const row = document.createElement('div');
  row.className = 'search-status';
  row.textContent = text;
  return row;
}
