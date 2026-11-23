//! Persisted workbench layout: side bar, inspector and bottom panel sizes,
//! which panes are collapsed, and whether the minimap is shown. Values read
//! from storage are untrusted and are clamped before use.

export const LAYOUT_STORAGE = 'ryoko.layout.v1';

export interface LayoutState {
  sidebarWidth: number;
  inspectorWidth: number;
  panelHeight: number;
  sidebarCollapsed: boolean;
  minimap: boolean;
}

export const LAYOUT_LIMITS = {
  sidebarWidth: { min: 170, max: 640 },
  inspectorWidth: { min: 240, max: 720 },
  panelHeight: { min: 120, max: 900 },
} as const;

export const DEFAULT_LAYOUT: Readonly<LayoutState> = {
  sidebarWidth: 260,
  inspectorWidth: 340,
  panelHeight: 280,
  sidebarCollapsed: false,
  minimap: true,
};

/** Dragging a side bar below this width collapses it, like VS Code. */
export const COLLAPSE_THRESHOLD = 110;

export function clampSize(key: keyof typeof LAYOUT_LIMITS, value: unknown): number {
  const { min, max } = LAYOUT_LIMITS[key];
  if (typeof value !== 'number' || !Number.isFinite(value)) return DEFAULT_LAYOUT[key];
  return Math.round(Math.min(max, Math.max(min, value)));
}

export function sanitizeLayout(raw: unknown): LayoutState {
  const value = (typeof raw === 'object' && raw !== null ? raw : {}) as Record<string, unknown>;
  return {
    sidebarWidth: clampSize('sidebarWidth', value.sidebarWidth),
    inspectorWidth: clampSize('inspectorWidth', value.inspectorWidth),
    panelHeight: clampSize('panelHeight', value.panelHeight),
    sidebarCollapsed: value.sidebarCollapsed === true,
    minimap: typeof value.minimap === 'boolean' ? value.minimap : DEFAULT_LAYOUT.minimap,
  };
}

export interface LayoutStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

export function loadLayout(storage: LayoutStorage): LayoutState {
  try {
    return sanitizeLayout(JSON.parse(storage.getItem(LAYOUT_STORAGE) ?? '{}'));
  } catch {
    return { ...DEFAULT_LAYOUT };
  }
}

export function saveLayout(storage: LayoutStorage, layout: LayoutState): void {
  try {
    storage.setItem(LAYOUT_STORAGE, JSON.stringify(sanitizeLayout(layout)));
  } catch {
    // Storage is optional: the layout still applies for this session.
  }
}

/**
 * Size after a drag. `delta` is pointer movement in the direction that grows
 * the pane. Returns `collapse: true` when a collapsible pane is dragged small.
 */
export function resizeBy(
  key: keyof typeof LAYOUT_LIMITS,
  start: number,
  delta: number,
  collapsible = false,
): { size: number; collapse: boolean } {
  const raw = start + delta;
  if (collapsible && raw < COLLAPSE_THRESHOLD) return { size: start, collapse: true };
  return { size: clampSize(key, raw), collapse: false };
}
