import { describe, expect, it, vi } from 'vitest';
import { CommandRegistry, commandLabel, formatChord, fuzzyScore, rankCommands, type WorkbenchCommand } from './commands';

const make = (id: string, category: string, title: string, run = () => {}): WorkbenchCommand => ({ id, category, title, run });

describe('fuzzyScore', () => {
  it('matches subsequences and rejects missing characters', () => {
    expect(fuzzyScore('View: Toggle Minimap', 'tgmm')).not.toBeNull();
    expect(fuzzyScore('View: Toggle Minimap', 'xyz')).toBeNull();
  });
  it('reports matched positions', () => {
    expect(fuzzyScore('File: Save', 'sav')?.positions).toEqual([6, 7, 8]);
  });
  it('prefers contiguous word-start matches', () => {
    const contiguous = fuzzyScore('View: Toggle Panel', 'panel')!.score;
    const scattered = fuzzyScore('Run: Debug Panel Layout', 'pnl')!.score;
    expect(contiguous).toBeGreaterThan(scattered);
  });
  it('treats an empty query as a match', () => {
    expect(fuzzyScore('Anything', '   ')).toEqual({ score: 0, positions: [] });
  });
});

describe('rankCommands', () => {
  const commands = [
    make('view.togglePanel', 'View', 'Toggle Panel'),
    make('view.toggleMinimap', 'View', 'Toggle Minimap'),
    make('editor.save', 'File', 'Save'),
  ];
  it('filters and orders by score', () => {
    const ranked = rankCommands(commands, 'minimap').map((match) => match.command.id);
    expect(ranked).toEqual(['view.toggleMinimap']);
  });
  it('puts recently used commands first for an empty query', () => {
    expect(rankCommands(commands, '', ['editor.save'])[0].command.id).toBe('editor.save');
  });
  it('can find theme commands with a category prefix', () => {
    const themes = [make('theme.light', 'Theme', 'Light'), make('theme.dark', 'Theme', 'Dark'), ...commands];
    const ranked = rankCommands(themes, 'Theme: light');
    expect(ranked[0].command.id).toBe('theme.light');
  });
});

describe('CommandRegistry', () => {
  it('rejects invalid and duplicate ids', () => {
    const registry = new CommandRegistry();
    expect(() => registry.register(make('Bad Id', 'X', 'y'))).toThrow();
    registry.register(make('view.a', 'View', 'A'));
    expect(() => registry.register(make('view.a', 'View', 'A again'))).toThrow();
  });
  it('executes only registered, enabled commands', async () => {
    const registry = new CommandRegistry();
    const run = vi.fn();
    registry.register({ ...make('view.a', 'View', 'A', run) });
    registry.register({ ...make('view.b', 'View', 'B', run), enabled: () => false });
    expect(await registry.execute('view.a')).toBe(true);
    expect(await registry.execute('view.b')).toBe(false);
    expect(await registry.execute('view.unknown')).toBe(false);
    expect(await registry.execute({ id: 'view.a' })).toBe(false);
    expect(await registry.execute('__proto__')).toBe(false);
    expect(run).toHaveBeenCalledTimes(1);
  });
  it('labels commands with their category', () => {
    expect(commandLabel(make('view.a', 'View', 'Toggle'))).toBe('View: Toggle');
  });
});

describe('formatChord', () => {
  it('formats for Windows and Linux', () => {
    expect(formatChord('MOD+SHIFT+P', false)).toBe('Ctrl+Shift+P');
    expect(formatChord('MOD+,', false)).toBe('Ctrl+,');
  });
  it('formats for macOS', () => {
    expect(formatChord('MOD+SHIFT+P', true)).toBe('⇧⌘P');
    expect(formatChord('ALT+MOD+F', true)).toBe('⌥⌘F');
  });
});
