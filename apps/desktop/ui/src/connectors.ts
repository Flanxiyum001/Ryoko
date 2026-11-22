/**
 * Pure connector-catalog presentation helpers for the desktop shell.
 *
 * The Tauri commands own parsing, validation and file writes; this module only
 * turns a snapshot into labels, badges and tool selections so it can be tested
 * without a window or a workspace.
 */

export type ConnectorStatus = 'ready' | 'disabled' | 'missing_env' | 'no_tools';

export interface ConnectorTransport {
  kind: 'stdio' | 'sse' | 'streamable_http';
  command?: string | null;
  args?: string[];
  url?: string | null;
}

export interface ConnectorTool {
  name: string;
  write: boolean;
  allowed: boolean;
}

export interface ConnectorEntry {
  id: string;
  name: string;
  category: string;
  description?: string;
  transport: ConnectorTransport;
  required_env: string[];
  allowed_tools: string[];
  write_tools: string[];
  enabled: boolean;
  max_calls_per_run?: number | null;
}

export interface ConnectorView extends ConnectorEntry {
  status: ConnectorStatus;
  missing_env: string[];
  tools: ConnectorTool[];
  source: 'configured' | 'preset';
}

export interface ConnectorSnapshot {
  path: string;
  connectors: ConnectorView[];
  presets: ConnectorView[];
  error: string | null;
}

export interface ConnectorBadge {
  label: string;
  tone: 'ok' | 'warn' | 'muted' | 'danger';
}

export function connectorBadges(connector: ConnectorView): ConnectorBadge[] {
  const badges: ConnectorBadge[] = [];
  if (!connector.enabled) {
    badges.push({ label: 'Disabled', tone: 'muted' });
  } else if (connector.missing_env.length > 0) {
    badges.push({
      label: `Missing ${connector.missing_env.join(', ')}`,
      tone: 'danger',
    });
  } else if (connector.allowed_tools.length === 0) {
    badges.push({ label: 'No tools allowed', tone: 'warn' });
  } else {
    badges.push({ label: 'Ready', tone: 'ok' });
  }
  if (connector.transport.kind !== 'stdio') {
    badges.push({ label: 'Not mounted by the CLI yet', tone: 'warn' });
  }
  const writes = connector.allowed_tools.filter((tool) =>
    connector.write_tools.includes(tool),
  );
  if (writes.length > 0) {
    badges.push({
      label: `${writes.length} write tool${writes.length === 1 ? '' : 's'} allowed`,
      tone: 'warn',
    });
  }
  return badges;
}

/** One line describing how the shell reaches the server. */
export function transportSummary(connector: ConnectorView): string {
  const transport = connector.transport;
  if (transport.kind === 'stdio') {
    const parts = [transport.command ?? '', ...(transport.args ?? [])].filter(Boolean);
    return parts.length > 0 ? parts.join(' ') : 'stdio';
  }
  return `${transport.kind} ${transport.url ?? ''}`.trim();
}

export function credentialSummary(connector: ConnectorView): string {
  if (connector.required_env.length === 0) {
    return 'No credential variable needed.';
  }
  return connector.required_env
    .map((name) => `${name}: ${connector.missing_env.includes(name) ? 'missing' : 'set'}`)
    .join(' · ');
}

/** Merges the configured entries and the built-in presets for the catalog. */
export function catalogEntries(snapshot: ConnectorSnapshot): ConnectorView[] {
  const configured = new Set(snapshot.connectors.map((connector) => connector.id));
  return [
    ...snapshot.connectors,
    ...snapshot.presets.filter((preset) => !configured.has(preset.id)),
  ];
}

/** The tool list with the requested tool toggled, preserving catalog order. */
export function toggleTool(connector: ConnectorView, tool: string): string[] {
  const enabled = new Set(connector.allowed_tools);
  if (enabled.has(tool)) {
    enabled.delete(tool);
  } else if (connector.tools.some((entry) => entry.name === tool)) {
    enabled.add(tool);
  }
  return connector.tools
    .map((entry) => entry.name)
    .filter((name) => enabled.has(name));
}

/** Replaces one connector in place so the rest of the catalog keeps its order. */
export function applyConnector(
  snapshot: ConnectorSnapshot,
  updated: ConnectorView,
): ConnectorSnapshot {
  return {
    ...snapshot,
    connectors: snapshot.connectors.map((connector) =>
      connector.id === updated.id ? updated : connector,
    ),
  };
}

export function readyCount(snapshot: ConnectorSnapshot): number {
  return snapshot.connectors.filter((connector) => connector.status === 'ready').length;
}

/** Presets can be added; configured connectors can be toggled or inspected. */
export function isConfigured(connector: ConnectorView): boolean {
  return connector.source === 'configured';
}
