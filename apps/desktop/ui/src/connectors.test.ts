import { describe, expect, it } from 'vitest';
import {
  applyConnector,
  catalogEntries,
  connectorBadges,
  credentialSummary,
  isConfigured,
  readyCount,
  toggleTool,
  transportSummary,
  type ConnectorSnapshot,
  type ConnectorView,
} from './connectors';

function view(overrides: Partial<ConnectorView> = {}): ConnectorView {
  return {
    id: 'github',
    name: 'GitHub',
    category: 'Source Control',
    transport: {
      kind: 'stdio',
      command: 'npx',
      args: ['-y', '@modelcontextprotocol/server-github'],
    },
    required_env: ['GITHUB_PERSONAL_ACCESS_TOKEN'],
    allowed_tools: ['list_issues'],
    write_tools: ['create_issue'],
    enabled: true,
    status: 'ready',
    missing_env: [],
    source: 'configured',
    tools: [
      { name: 'list_issues', write: false, allowed: true },
      { name: 'create_issue', write: true, allowed: false },
    ],
    ...overrides,
  };
}

function snapshot(connectors: ConnectorView[], presets: ConnectorView[] = []): ConnectorSnapshot {
  return { path: '.ryoko/connectors.json', connectors, presets, error: null };
}

describe('connector badges', () => {
  it('reports a ready connector', () => {
    expect(connectorBadges(view())).toEqual([{ label: 'Ready', tone: 'ok' }]);
  });

  it('names every missing credential variable', () => {
    const badges = connectorBadges(
      view({ status: 'missing_env', missing_env: ['GITHUB_PERSONAL_ACCESS_TOKEN'] }),
    );
    expect(badges[0]).toEqual({
      label: 'Missing GITHUB_PERSONAL_ACCESS_TOKEN',
      tone: 'danger',
    });
  });

  it('marks disabled connectors and warns about allowed write tools', () => {
    const disabled = connectorBadges(view({ enabled: false, status: 'disabled' }));
    expect(disabled).toEqual([{ label: 'Disabled', tone: 'muted' }]);

    const writes = connectorBadges(view({ allowed_tools: ['list_issues', 'create_issue'] }));
    expect(writes).toContainEqual({ label: '1 write tool allowed', tone: 'warn' });
  });

  it('flags a connector with no allowed tools', () => {
    const badges = connectorBadges(view({ status: 'no_tools', allowed_tools: [] }));
    expect(badges[0]).toEqual({ label: 'No tools allowed', tone: 'warn' });
  });

  it('warns that a network transport is not mounted yet', () => {
    const badges = connectorBadges(
      view({ transport: { kind: 'sse', url: 'https://mcp.example.com/sse' } }),
    );
    expect(badges).toContainEqual({ label: 'Not mounted by the CLI yet', tone: 'warn' });
  });
});

describe('connector descriptors', () => {
  it('summarises the stdio command and the credential state', () => {
    const connector = view();
    expect(transportSummary(connector)).toBe(
      'npx -y @modelcontextprotocol/server-github',
    );
    expect(credentialSummary(connector)).toBe(
      'GITHUB_PERSONAL_ACCESS_TOKEN: set',
    );
    expect(
      credentialSummary(view({ missing_env: ['GITHUB_PERSONAL_ACCESS_TOKEN'] })),
    ).toBe('GITHUB_PERSONAL_ACCESS_TOKEN: missing');
    expect(
      credentialSummary(view({ required_env: [], missing_env: [] })),
    ).toBe('No credential variable needed.');
  });

  it('merges presets without duplicating a configured connector', () => {
    const configured = view();
    const presets = [view(), view({ id: 'cloudflare', name: 'Cloudflare', source: 'preset' })];
    const entries = catalogEntries(snapshot([configured], presets));
    expect(entries.map((entry) => entry.id)).toEqual(['github', 'cloudflare']);
    expect(isConfigured(entries[0])).toBe(true);
    expect(isConfigured(entries[1])).toBe(false);
  });

  it('counts only ready connectors', () => {
    expect(
      readyCount(
        snapshot([
          view(),
          view({ id: 'b', status: 'missing_env' }),
          view({ id: 'c', status: 'disabled' }),
        ]),
      ),
    ).toBe(1);
  });
});

describe('connector editing', () => {
  it('toggles a tool in catalog order and refuses unknown names', () => {
    const connector = view();
    expect(toggleTool(connector, 'create_issue')).toEqual(['list_issues', 'create_issue']);
    expect(toggleTool(connector, 'list_issues')).toEqual([]);
    expect(toggleTool(connector, 'drop_database')).toEqual(['list_issues']);
    expect(
      toggleTool(view({ allowed_tools: ['list_issues', 'create_issue'] }), 'list_issues'),
    ).toEqual(['create_issue']);
  });

  it('replaces one connector without reordering the catalog', () => {
    const first = view();
    const second = view({ id: 'cloudflare', name: 'Cloudflare' });
    const updated = view({ enabled: false, status: 'disabled' });
    const result = applyConnector(snapshot([first, second]), updated);
    expect(result.connectors.map((entry) => entry.id)).toEqual(['github', 'cloudflare']);
    expect(result.connectors[0].enabled).toBe(false);
  });
});
