//! The Cmd+K inline edit overlay: captures the active selection, asks the
//! backend for a model rewrite, highlights the affected range while the
//! replacement is validated against drift, and applies on demand.

import { invoke } from '@tauri-apps/api/core';
import { splitContext } from './inline_edit';

export interface InlineEditReply {
  protocol: number;
  replacement: string;
  model: string;
}

export interface InlineEditArgs {
  path: string;
  language: string;
  instructions: string;
  before: string;
  selected: string;
  after: string;
}

export interface InlineEditDeps {
  editor: import('monaco-editor').editor.IStandaloneCodeEditor;
  currentFile: () => { path: string; model: import('monaco-editor').editor.ITextModel } | null;
  languageId: () => string;
  setStatus: (message: string) => void;
  onSaveAfterApply: () => Promise<void>;
}

const PENDING_CLASS = 'inline-edit-pending';

export class InlineEditController {
  private contextText: string | null = null;
  private contextOffset = 0;
  private contextLength = 0;
  private replacement: string | null = null;
  private decorations: string[] = [];
  private decoratedFile: { path: string; model: import('monaco-editor').editor.ITextModel } | null = null;
  private busy = false;

  constructor(private readonly deps: InlineEditDeps) {}

  isActive(): boolean {
    return this.contextText !== null;
  }

  /** Opens the prompt. Reopening while a preview is pending discards it. */
  open(): void {
    if (this.busy) return;
    this.reject();
    const file = this.deps.currentFile();
    const editorModel = this.deps.editor.getModel();
    const selection = this.deps.editor.getSelection();
    if (!file || file.model !== editorModel || !selection) return;
    const offset = editorModel!.getOffsetAt(selection.getStartPosition());
    const length = editorModel!.getValueLengthInRange(selection);
    const text = editorModel!.getValue();
    if (text.length > 32_000) {
      this.deps.setStatus('Inline edits cover buffers of at most 32,000 characters.');
      return;
    }
    this.contextText = text;
    this.contextOffset = offset;
    this.contextLength = length;
    const overlay = document.getElementById('inlineEditOverlay');
    const input = document.getElementById('inlineEditInput') as HTMLInputElement | null;
    const hint = document.getElementById('inlineEditHint');
    if (!overlay || !input) return;
    if (hint) hint.textContent = `${file.path} - ${this.deps.languageId()}`;
    overlay.classList.remove('hidden');
    input.value = '';
    input.disabled = false;
    input.focus();
  }

  hidePrompt(): void {
    document.getElementById('inlineEditOverlay')?.classList.add('hidden');
    this.deps.editor.focus();
  }

  async submit(): Promise<void> {
    const input = document.getElementById('inlineEditInput') as HTMLInputElement | null;
    if (!input || this.contextText === null || this.busy) return;
    const instructions = input.value.trim();
    if (instructions.length === 0) {
      this.hidePrompt();
      return;
    }
    const file = this.deps.currentFile();
    if (!file) {
      this.deps.setStatus('Open a file before editing with the agent.');
      return;
    }
    this.busy = true;
    input.disabled = true;
    try {
      const context = splitContext(this.contextText, this.contextOffset, this.contextLength);
      const reply = await invoke<InlineEditReply>('inline_edit', {
        args: {
          path: file.path,
          language: this.deps.languageId(),
          instructions,
          before: context.before,
          selected: context.selected,
          after: context.after,
        } satisfies InlineEditArgs,
      });
      this.replacement = reply.replacement;
      this.decoratedFile = file;
      this.decorate(file);
      this.hidePrompt();
      this.deps.setStatus(
        'Inline edit ready - press Mod+Enter to accept, Escape to reject',
      );
    } catch (error) {
      this.deps.setStatus('Inline edit failed: ' + String(error));
      this.hidePrompt();
      this.reject('Inline edit discarded after a backend error.');
      return;
    } finally {
      this.busy = false;
      input.disabled = false;
    }
  }

  private decorate(
    file: { path: string; model: import('monaco-editor').editor.ITextModel },
  ): void {
    const model = file.model;
    const start = model.getPositionAt(this.contextOffset);
    const end = model.getPositionAt(this.contextOffset + this.contextLength);
    this.decorations = model.deltaDecorations([], [
      {
        range: {
          startLineNumber: start.lineNumber,
          startColumn: start.column,
          endLineNumber: end.lineNumber,
          endColumn: end.column,
        },
        options: {
          className: PENDING_CLASS,
          isWholeLine: false,
          overviewRuler: { color: '#2d7ff9', position: 4 },
        },
      },
    ]);
  }

  accept(): void {
    if (this.busy || this.replacement === null || this.contextText === null) return;
    const file = this.decoratedFile;
    if (!file) return;
    if (file.model.getValue() !== this.contextText) {
      this.reject('Buffer changed since the inline edit; discarded.');
      return;
    }
    const model = file.model;
    const start = model.getPositionAt(this.contextOffset);
    const end = model.getPositionAt(this.contextOffset + this.contextLength);
    model.pushEditOperations(
      [],
      [{
        range: {
          startLineNumber: start.lineNumber,
          startColumn: start.column,
          endLineNumber: end.lineNumber,
          endColumn: end.column,
        },
        text: this.replacement,
      }],
      () => null,
    );
    this.clearDecorations();
    this.contextText = null;
    this.replacement = null;
    this.decoratedFile = null;
    this.overlay()?.classList.add('hidden');
    this.deps.setStatus('Inline edit applied to the buffer.');
    void this.deps.onSaveAfterApply();
  }

  reject(message = 'Inline edit discarded.'): void {
    this.clearDecorations();
    this.contextText = null;
    this.replacement = null;
    this.decoratedFile = null;
    this.overlay()?.classList.add('hidden');
    this.deps.setStatus(message);
  }

  private overlay(): HTMLElement | null {
    return document.getElementById('inlineEditOverlay');
  }

  private clearDecorations(): void {
    const file = this.decoratedFile;
    if (file && this.decorations.length > 0) {
      file.model.deltaDecorations(this.decorations, []);
    }
    this.decorations = [];
  }
}
