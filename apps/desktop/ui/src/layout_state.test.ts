import { describe, expect, it } from 'vitest';
import { DEFAULT_LAYOUT, LAYOUT_LIMITS, LAYOUT_STORAGE, loadLayout, resizeBy, sanitizeLayout, saveLayout } from './layout_state';

function memory(initial: Record<string, string> = {}) {
  const data = new Map(Object.entries(initial));
  return { getItem: (key: string) => data.get(key) ?? null, setItem: (key: string, value: string) => { data.set(key, value); } };
}

describe('layout state', () => {
  it('falls back to defaults for missing or corrupt storage', () => {
    expect(loadLayout(memory())).toEqual(DEFAULT_LAYOUT);
    expect(loadLayout(memory({ [LAYOUT_STORAGE]: '{not json' }))).toEqual(DEFAULT_LAYOUT);
  });
  it('clamps untrusted stored sizes', () => {
    const layout = sanitizeLayout({ sidebarWidth: 99999, inspectorWidth: -5, panelHeight: 'tall', minimap: 'yes' });
    expect(layout.sidebarWidth).toBe(LAYOUT_LIMITS.sidebarWidth.max);
    expect(layout.inspectorWidth).toBe(LAYOUT_LIMITS.inspectorWidth.min);
    expect(layout.panelHeight).toBe(DEFAULT_LAYOUT.panelHeight);
    expect(layout.minimap).toBe(DEFAULT_LAYOUT.minimap);
  });
  it('round-trips through storage', () => {
    const storage = memory();
    saveLayout(storage, { ...DEFAULT_LAYOUT, sidebarWidth: 333, sidebarCollapsed: true, minimap: false });
    expect(loadLayout(storage)).toEqual({ ...DEFAULT_LAYOUT, sidebarWidth: 333, sidebarCollapsed: true, minimap: false });
  });
  it('collapses a collapsible pane dragged below the threshold', () => {
    expect(resizeBy('sidebarWidth', 260, -200, true)).toEqual({ size: 260, collapse: true });
    expect(resizeBy('sidebarWidth', 260, -200, false).size).toBe(LAYOUT_LIMITS.sidebarWidth.min);
    expect(resizeBy('sidebarWidth', 260, 40, true)).toEqual({ size: 300, collapse: false });
  });
});
