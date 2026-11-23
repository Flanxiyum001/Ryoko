import { afterEach, describe, expect, it, vi } from 'vitest';
import { InlineEditController, type InlineEditDeps } from './inline_edit_view';
import { invoke } from '@tauri-apps/api/core';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
afterEach(() => { vi.unstubAllGlobals(); vi.clearAllMocks(); });

function fixture() {
  const input = { value: 'Rewrite', disabled: false, focus: vi.fn() };
  const overlay = { classList: { add: vi.fn(), remove: vi.fn() } };
  vi.stubGlobal('document', { getElementById: (id: string) =>
    id === 'inlineEditInput' ? input : id === 'inlineEditOverlay' ? overlay : null });
  const model = {
    isDisposed: () => false, getOffsetAt: () => 0, getValueLengthInRange: () => 3,
    getValue: () => 'old', getPositionAt: () => ({ lineNumber: 1, column: 1 }),
    deltaDecorations: vi.fn(() => ['decoration']), pushEditOperations: vi.fn(),
  };
  let current: unknown = { path: 'a.ts', model };
  const deps = {
    editor: { getModel: () => model, getSelection: () => ({ getStartPosition: () => ({}) }), focus: vi.fn() },
    currentFile: () => current, languageId: () => 'typescript',
    setStatus: vi.fn(), onSaveAfterApply: vi.fn(),
  };
  const controller = new InlineEditController(deps as unknown as InlineEditDeps);
  controller.open(); input.value = 'Rewrite';
  return { controller, model, deps, switchFile: () => { current = null; } };
}

describe('inline edit lifetime across editor tabs', () => {
  it('does not decorate a closed buffer when a cancelled request returns', async () => {
    const f = fixture();
    let resolve!: (reply: unknown) => void;
    vi.mocked(invoke).mockReturnValue(new Promise((done) => { resolve = done; }));
    const pending = f.controller.submit();
    f.controller.reject();
    resolve({ replacement: 'new' });
    await pending;
    expect(f.model.deltaDecorations).not.toHaveBeenCalled();
    f.controller.accept();
    expect(f.model.pushEditOperations).not.toHaveBeenCalled();
  });
  it('does not apply or save a background file after selection changes', async () => {
    const f = fixture();
    vi.mocked(invoke).mockResolvedValue({ replacement: 'new' });
    await f.controller.submit();
    f.switchFile();
    f.controller.accept();
    expect(f.model.pushEditOperations).not.toHaveBeenCalled();
    expect(f.deps.onSaveAfterApply).not.toHaveBeenCalled();
  });
});
