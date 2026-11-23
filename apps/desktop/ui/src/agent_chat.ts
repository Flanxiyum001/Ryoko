/** Streaming, read-only coding chat in the inspector rail.
 * Every suggestion is text until the operator explicitly opens a review.
 */
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

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

interface ChatTurn { role: 'user' | 'assistant'; text: string }
interface ChatPayload { requestId: string; text: string }
interface ChatDone { requestId: string; cancelled: boolean; inputTokens: number; outputTokens: number; estimatedCostUsd: number }

export interface AgentChatDeps {
  files: () => string[];
  selection: () => EditorSelectionContext | null;
  review: (suggestion: CodeSuggestion) => void;
  onTokens: (input: number, output: number, estimatedCostUsd: number) => void;
  setStatus: (message: string) => void;
}

export function mentionedFiles(message: string, known: string[]): string[] {
  const allowed = new Set(known);
  const paths: string[] = [];
  for (const match of message.matchAll(/(?:^|\s)@([^\s]+)/g)) {
    const path = match[1];
    if (path !== 'selection' && allowed.has(path) && !paths.includes(path)) paths.push(path);
  }
  return paths.slice(0, 4);
}

export function firstCodeFence(text: string): string | null {
  const found = /(?:^|\n)```[^\n`]*\r?\n([\s\S]*?)\r?\n?```/.exec(text);
  return found ? found[1] : null;
}

export function mountAgentChat(deps: AgentChatDeps): { focus: () => void; reset: () => void } {
  const historyNode = document.getElementById('chatHistory')!;
  const input = document.getElementById('chatInput') as HTMLTextAreaElement;
  const send = document.getElementById('chatSend') as HTMLButtonElement;
  const stop = document.getElementById('chatStop') as HTMLButtonElement;
  const fileInput = document.getElementById('chatFilePath') as HTMLInputElement;
  const attachButton = document.getElementById('chatAttachFile') as HTMLButtonElement;
  const selectionButton = document.getElementById('chatAttachSelection') as HTMLButtonElement;
  const fileChoices = document.getElementById('chatFileChoices') as HTMLDataListElement;
  const attachmentsNode = document.getElementById('chatAttachments')!;
  const note = document.getElementById('chatNote')!;
  let turns: ChatTurn[] = [];
  let attached: string[] = [];
  let includeSelection = false;
  let activeRequest: string | null = null;
  let output = '';
  let outputNode: HTMLElement | null = null;
  let submission: EditorSelectionContext | null = null;
  let requestSequence = 0;

  function showAttachments(): void {
    attachmentsNode.replaceChildren();
    for (const path of attached) {
      const tag = document.createElement('button');
      tag.type = 'button';
      tag.className = 'chat-chip';
      tag.textContent = '@' + path + ' ×';
      tag.title = 'Remove file attachment';
      tag.addEventListener('click', () => {
        attached = attached.filter((name) => name !== path);
        showAttachments();
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
        showAttachments();
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

  async function submit(): Promise<void> {
    if (activeRequest) return;
    const message = input.value.trim();
    if (!message) return;
    const known = deps.files();
    const files = [...new Set([...attached, ...mentionedFiles(message, known)])].slice(0, 4);
    if (files.some((path) => !known.includes(path))) {
      note.textContent = 'One of the attached files is no longer in this workspace.';
      return;
    }
    const selection = deps.selection();
    const requestedSelection = includeSelection || /(?:^|\s)@selection(?:\s|$)/.test(message);
    if (requestedSelection && (!selection || !selection.text)) {
      note.textContent = 'Select code in the active editor before attaching @selection.';
      return;
    }
    const previous = turns.slice(-12);
    turns.push({ role: 'user', text: message.slice(0, 8000) });
    appendTurn('user', message);
    input.value = '';
    attached = [];
    includeSelection = false;
    showAttachments();
    outputNode = appendTurn('assistant', '');
    output = '';
    submission = requestedSelection ? selection : null;
    const requestId = 'chat-' + (++requestSequence) + '-' + Date.now();
    activeRequest = requestId;
    send.disabled = true;
    stop.disabled = false;
    note.textContent = 'Streaming from the configured model…';
    try {
      await invoke('agent_chat', {
        args: {
          requestId, message, files,
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
  attachButton.addEventListener('click', () => {
    const path = fileInput.value.trim();
    if (!deps.files().includes(path)) {
      note.textContent = 'Choose a known workspace file.';
      return;
    }
    if (!attached.includes(path) && attached.length < 4) attached.push(path);
    fileInput.value = '';
    showAttachments();
  });
  fileInput.addEventListener('focus', () => {
    fileChoices.replaceChildren();
    for (const path of deps.files().slice(0, 5000)) {
      const option = document.createElement('option');
      option.value = path;
      fileChoices.append(option);
    }
  });
  selectionButton.addEventListener('click', () => {
    if (!deps.selection()?.text) {
      note.textContent = 'First select a block of code in the editor.';
      return;
    }
    includeSelection = true;
    showAttachments();
  });
  return {
    focus: () => input.focus(),
    reset: () => {
      void cancel();
      turns = [];
      attached = [];
      includeSelection = false;
      output = '';
      historyNode.replaceChildren();
      showAttachments();
      note.textContent = 'Attach a workspace file or select code, then ask a question.';
    },
  };
}
