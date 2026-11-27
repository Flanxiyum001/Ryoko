/** Streaming, read-only coding chat in the inspector rail.
 * Every suggestion is text until the operator explicitly opens a review.
 * Prompt context is pinned explicitly with @file, @symbol, @docs, @git-diff
 * and @selection; the native shell re-validates and bounds every pin.
 */
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import {
  MAX_CONTEXT_MENTIONS,
  addMention,
  formatMention,
  mentionKey,
  mentionLabel,
  parseMentions,
  type ContextMention,
} from './context_mentions';

export interface EditorSelectionContext {
  path: string;
  text: string;
  fullText: string;
  startOffset: number;
  endOffset: number;
}

export interface CodeSuggestion {
  context: EditorSelectionContext;
  replacement: string;
}

export interface SymbolSuggestion {
  name: string;
  path: string;
  kind: string;
}

export interface MentionPreview {
  label: string;
  preview: string;
  bytes: number;
}

interface ChatTurn { role: 'user' | 'assistant'; text: string }
interface ChatPayload { requestId: string; text: string }
interface ChatDone { requestId: string; cancelled: boolean; inputTokens: number; outputTokens: number; estimatedCostUsd: number }

export interface AgentChatDeps {
  /** When true, messages go to the agent harness instead of read-only chat. */
  agentMode?: () => boolean;
  /** Starts an agent run for a message already shown as the user's turn. */
  runAgent?: (message: string) => void;
  files: () => string[];
  selection: () => EditorSelectionContext | null;
  review: (suggestion: CodeSuggestion) => void;
  onTokens: (input: number, output: number, estimatedCostUsd: number) => void;
  setStatus: (message: string) => void;
  /** Optional symbol lookup against the workspace index. */
  searchSymbols?: (query: string) => Promise<SymbolSuggestion[]>;
  /** Optional native preview; a pin that cannot resolve is never sent. */
  resolveMention?: (mention: ContextMention) => Promise<MentionPreview>;
}

export function mentionedFiles(message: string, known: string[]): string[] {
  return parseMentions(message, known)
    .mentions
    .filter((mention) => mention.kind === 'file')
    .map((mention) => mention.value)
    .slice(0, MAX_CONTEXT_MENTIONS);
}

export function firstCodeFence(text: string): string | null {
  const found = /(?:^|\n)```[^\n`]*\r?\n([\s\S]*?)\r?\n?```/.exec(text);
  return found ? found[1] : null;
}

type PinKind = 'file' | 'symbol' | 'docs';

export function mountAgentChat(deps: AgentChatDeps): { focus: () => void; reset: () => void } {
  const historyNode = document.getElementById('chatHistory')!;
  const input = document.getElementById('chatInput') as HTMLTextAreaElement;
  const send = document.getElementById('chatSend') as HTMLButtonElement;
  const stop = document.getElementById('chatStop') as HTMLButtonElement;
  const kindSelect = document.getElementById('chatMentionKind') as HTMLSelectElement;
  const valueInput = document.getElementById('chatMentionValue') as HTMLInputElement;
  const choices = document.getElementById('chatMentionChoices') as HTMLDataListElement;
  const pinButton = document.getElementById('chatPinMention') as HTMLButtonElement;
  const pinDiffButton = document.getElementById('chatPinDiff') as HTMLButtonElement;
  const selectionButton = document.getElementById('chatAttachSelection') as HTMLButtonElement;
  const attachmentsNode = document.getElementById('chatAttachments')!;
  const note = document.getElementById('chatNote')!;
  let turns: ChatTurn[] = [];
  let pinned: ContextMention[] = [];
  let includeSelection = false;
  let activeRequest: string | null = null;
  let output = '';
  let outputNode: HTMLElement | null = null;
  let submission: EditorSelectionContext | null = null;
  let requestSequence = 0;
  let symbolTimer: number | undefined;
  let symbolTicket = 0;

  function renderChips(): void {
    attachmentsNode.replaceChildren();
    for (const mention of pinned) {
      const tag = document.createElement('button');
      tag.type = 'button';
      tag.className = 'chat-chip';
      tag.textContent = `${formatMention(mention)} ×`;
      tag.title = `Remove ${mentionLabel(mention)}`;
      tag.addEventListener('click', () => {
        pinned = pinned.filter((entry) => mentionKey(entry) !== mentionKey(mention));
        renderChips();
      });
      attachmentsNode.append(tag);
    }
    if (includeSelection) {
      const tag = document.createElement('button');
      tag.type = 'button';
      tag.className = 'chat-chip';
      tag.textContent = '@selection ×';
      tag.addEventListener('click', () => {
        includeSelection = false;
        renderChips();
      });
      attachmentsNode.append(tag);
    }
  }

  function appendTurn(role: ChatTurn['role'], text: string): HTMLElement {
    const article = document.createElement('article');
    article.className = 'chat-turn chat-' + role;
    const heading = document.createElement('strong');
    heading.textContent = role === 'assistant' ? 'Ryoko agent' : 'You';
    const content = document.createElement('pre');
    content.className = 'chat-turn-body';
    content.textContent = text;
    article.append(heading, content);
    historyNode.append(article);
    historyNode.scrollTop = historyNode.scrollHeight;
    return article;
  }

  function renderReviewAction(article: HTMLElement, text: string): void {
    if (!submission) return;
    const replacement = firstCodeFence(text);
    if (replacement === null) return;
    const captured = submission;
    const review = document.createElement('button');
    review.type = 'button';
    review.className = 'button primary compact';
    review.textContent = 'Review suggested edit';
    review.title = 'Preview the proposed code in a diff. Nothing is written automatically.';
    review.addEventListener('click', () => deps.review({ context: captured, replacement }));
    article.append(review);
  }

  function complete(error?: string, usage?: ChatDone): void {
    if (!activeRequest) return;
    if (error) {
      const message = document.createElement('p');
      message.className = 'chat-error';
      message.textContent = error;
      outputNode?.append(message);
      deps.setStatus('Agent chat: ' + error);
    } else {
      if (outputNode) renderReviewAction(outputNode, output);
      if (output.trim()) turns.push({ role: 'assistant', text: output.slice(0, 8000) });
    }
    if (usage) deps.onTokens(usage.inputTokens, usage.outputTokens, usage.estimatedCostUsd);
    activeRequest = null;
    submission = null;
    outputNode = null;
    send.disabled = false;
    stop.disabled = true;
    note.textContent = error ? 'Request failed — your files were not changed.' : 'Read-only answer. Review edits before applying.';
  }

  async function pin(mention: ContextMention): Promise<boolean> {
    if (pinned.some((entry) => mentionKey(entry) === mentionKey(mention))) {
      note.textContent = 'That context pin is already attached.';
      return false;
    }
    if (pinned.length >= MAX_CONTEXT_MENTIONS) {
      note.textContent = `At most ${MAX_CONTEXT_MENTIONS} context pins can be attached.`;
      return false;
    }
    if (deps.resolveMention) {
      try {
        const preview = await deps.resolveMention(mention);
        note.textContent = `Pinned ${preview.label} · ${Math.ceil(preview.bytes / 1024)} KiB`;
      } catch (error) {
        note.textContent = 'Could not pin context: '
          + (error instanceof Error ? error.message : String(error));
        return false;
      }
    }
    if (pinned.length >= MAX_CONTEXT_MENTIONS) {
      note.textContent = `At most ${MAX_CONTEXT_MENTIONS} context pins can be attached.`;
      return false;
    }
    pinned = addMention(pinned, mention);
    renderChips();
    return true;
  }

  function pinCurrent(): void {
    const value = valueInput.value.trim();
    const kind = kindSelect.value as PinKind;
    let mention: ContextMention;
    if (kind === 'file') {
      if (!deps.files().includes(value)) {
        note.textContent = 'Choose a known workspace file to pin.';
        return;
      }
      mention = { kind, value };
    } else if (kind === 'symbol') {
      const parsed = parseMentions(`@symbol:${value}`, deps.files());
      if (parsed.invalid.length > 0 || parsed.mentions.length === 0) {
        note.textContent = 'Symbol pins must be a plain identifier, for example `run`.';
        return;
      }
      mention = { kind, value };
    } else {
      if (!value) {
        mention = { kind: 'docs', value: '' };
      } else {
        const parsed = parseMentions(`@docs:${value}`, deps.files());
        if (parsed.invalid.length > 0 || parsed.mentions.length === 0) {
          note.textContent = 'Docs pins must be a short word or path, for example `security`.';
          return;
        }
        mention = { kind: 'docs', value };
      }
    }
    void pin(mention).then((added) => {
      if (added) valueInput.value = '';
    });
  }

  async function pinGitDiff(): Promise<void> {
    await pin({ kind: 'git-diff', value: '' });
  }

  async function refreshSymbolChoices(query: string): Promise<void> {
    if (!deps.searchSymbols) return;
    const ticket = ++symbolTicket;
    try {
      const suggestions = await deps.searchSymbols(query);
      if (ticket !== symbolTicket) return;
      choices.replaceChildren();
      const seen = new Set<string>();
      for (const suggestion of suggestions) {
        if (seen.has(suggestion.name)) continue;
        seen.add(suggestion.name);
        const option = document.createElement('option');
        option.value = suggestion.name;
        option.label = `${suggestion.kind} · ${suggestion.path}`;
        choices.append(option);
      }
    } catch {
      if (ticket === symbolTicket) choices.replaceChildren();
    }
  }

  function refreshValueChoices(): void {
    const kind = kindSelect.value as PinKind;
    if (kind === 'file') {
      symbolTicket += 1;
      choices.replaceChildren();
      for (const path of deps.files().slice(0, 5000)) {
        const option = document.createElement('option');
        option.value = path;
        choices.append(option);
      }
    } else if (kind === 'symbol') {
      void refreshSymbolChoices(valueInput.value.trim());
    } else {
      symbolTicket += 1;
      choices.replaceChildren();
    }
    valueInput.placeholder = kind === 'file'
      ? 'File path…'
      : kind === 'symbol'
        ? 'Symbol name…'
        : 'Docs query (empty = project memory)…';
  }

  async function submit(): Promise<void> {
    if (activeRequest) return;
    const message = input.value.trim();
    if (!message) {
      input.focus();
      return;
    }
    if (Array.from(message).length > 8000) {
      note.textContent = 'The prompt exceeds 8000 characters; remove some text before sending.';
      return;
    }
    if (deps.agentMode?.() && deps.runAgent) {
      appendTurn('user', message);
      input.value = '';
      deps.runAgent(message);
      return;
    }
    const known = deps.files();
    const parsed = parseMentions(message, known);
    if (parsed.invalid.length > 0) {
      note.textContent = 'Unknown or malformed context pins: ' + parsed.invalid.slice(0, 4).join(' ');
      return;
    }
    if (parsed.overflow > 0) {
      note.textContent = `At most ${MAX_CONTEXT_MENTIONS} context pins can be sent at once.`;
      return;
    }
    const knownKeys = new Set(pinned.map(mentionKey));
    const extra = parsed.mentions.filter((mention) => {
      const key = mentionKey(mention);
      if (knownKeys.has(key)) return false;
      knownKeys.add(key);
      return true;
    }).length;
    if (pinned.length + extra > MAX_CONTEXT_MENTIONS) {
      note.textContent = `At most ${MAX_CONTEXT_MENTIONS} context pins can be sent at once.`;
      return;
    }
    let mentions = [...pinned];
    for (const mention of parsed.mentions) mentions = addMention(mentions, mention);
    const requestedSelection = includeSelection
      || mentions.some((mention) => mention.kind === 'selection')
      || /(?:^|\s)@selection(?:\s|$)/.test(message);
    const sent = mentions.filter((mention) => mention.kind !== 'selection');
    const selection = deps.selection();
    if (requestedSelection && (!selection || !selection.text)) {
      note.textContent = 'Select code in the active editor before attaching @selection.';
      return;
    }
    const previous = turns.slice(-12);
    turns.push({ role: 'user', text: message.slice(0, 8000) });
    appendTurn('user', message);
    input.value = '';
    pinned = [];
    includeSelection = false;
    renderChips();
    outputNode = appendTurn('assistant', '');
    output = '';
    submission = requestedSelection ? selection : null;
    const requestId = 'chat-' + (++requestSequence) + '-' + Date.now();
    activeRequest = requestId;
    send.disabled = true;
    stop.disabled = false;
    note.textContent = sent.length > 0
      ? `Streaming with ${sent.length} context pin${sent.length === 1 ? '' : 's'}…`
      : 'Streaming from the configured model…';
    try {
      await invoke('agent_chat', {
        args: {
          requestId, message, mentions: sent,
          selection: requestedSelection ? selection?.text : undefined,
          activePath: requestedSelection ? selection?.path : undefined,
          history: previous,
        },
      });
    } catch (error) {
      complete(error instanceof Error ? error.message : String(error));
    }
  }

  async function cancel(): Promise<void> {
    if (!activeRequest) return;
    // Ignore any late chunks, and do not allow another send until native cancellation resolves.
    if (outputNode) {
      const body = outputNode.querySelector('.chat-turn-body');
      if (body) body.textContent = output + '\n[Stopped]';
    }
    activeRequest = null;
    submission = null;
    outputNode = null;
    send.disabled = true;
    stop.disabled = true;
    note.textContent = 'Stopping the current response…';
    try {
      await invoke('agent_chat_cancel');
      note.textContent = 'Stopped. No code was applied.';
    } catch (error) {
      note.textContent = 'Could not confirm cancellation: ' + String(error);
    } finally {
      send.disabled = false;
    }
  }

  void listen<ChatPayload>('ryoko-chat-chunk', ({ payload }) => {
    if (payload.requestId !== activeRequest || !outputNode) return;
    output += payload.text;
    const body = outputNode.querySelector('.chat-turn-body');
    if (body) body.textContent = output;
    historyNode.scrollTop = historyNode.scrollHeight;
  });
  void listen<ChatPayload>('ryoko-chat-error', ({ payload }) => {
    if (payload.requestId === activeRequest) complete(payload.text);
  });
  void listen<ChatDone>('ryoko-chat-done', ({ payload }) => {
    if (payload.requestId === activeRequest) complete(undefined, payload);
  });
  send.addEventListener('click', () => void submit());
  stop.addEventListener('click', () => void cancel());
  input.addEventListener('keydown', (event) => {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      void submit();
    }
  });
  pinButton.addEventListener('click', pinCurrent);
  pinDiffButton.addEventListener('click', () => void pinGitDiff());
  kindSelect.addEventListener('change', refreshValueChoices);
  valueInput.addEventListener('focus', refreshValueChoices);
  valueInput.addEventListener('input', () => {
    if (kindSelect.value !== 'symbol') return;
    window.clearTimeout(symbolTimer);
    symbolTimer = window.setTimeout(() => void refreshSymbolChoices(valueInput.value.trim()), 250);
  });
  valueInput.addEventListener('keydown', (event) => {
    if (event.key === 'Enter') {
      event.preventDefault();
      pinCurrent();
    }
  });
  selectionButton.addEventListener('click', () => {
    if (!deps.selection()?.text) {
      note.textContent = 'First select a block of code in the editor.';
      return;
    }
    includeSelection = true;
    renderChips();
  });
  return {
    focus: () => input.focus(),
    reset: () => {
      void cancel();
      turns = [];
      pinned = [];
      includeSelection = false;
      output = '';
      historyNode.replaceChildren();
      renderChips();
      note.textContent = 'Pin @file, @symbol, @docs or @git-diff, then ask a question.';
    },
  };
}
