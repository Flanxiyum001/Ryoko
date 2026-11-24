/** Connector catalog panel. File writes and validation stay in the Tauri shell. */
import { invoke } from '@tauri-apps/api/core';
import {
  catalogEntries,
  connectorBadges,
  credentialSummary,
  readyCount,
  toggleTool,
  transportSummary,
  type ConnectorSnapshot,
  type ConnectorView,
} from './connectors';

function byId<T extends HTMLElement>(id: string): T {
  const item = document.getElementById(id);
  if (!item) throw new Error('Missing connector element ' + id);
  return item as T;
}

export function mountConnectors(setStatus: (value: string) => void): { reload: () => void } {
  const count = byId<HTMLElement>('connectorCount');
  const path = byId<HTMLElement>('connectorPath');
  const refresh = byId<HTMLButtonElement>('connectorRefresh');
  const notice = byId<HTMLElement>('connectorNotice');
  const list = byId<HTMLElement>('connectorList');
  const inspect = byId<HTMLElement>('connectorInspect');

  function showNotice(text: string) {
    notice.textContent = text;
    notice.classList.toggle('hidden', text.length === 0);
  }

  function fail(error: unknown) {
    const message = error instanceof Error ? error.message : String(error);
    showNotice(message);
    setStatus('Connectors: ' + message);
  }

  function card(connector: ConnectorView): HTMLElement {
    const article = document.createElement('article');
    article.className = 'connector-card' + (connector.enabled ? ' active' : '');
    article.dataset.connectorId = connector.id;

    const head = document.createElement('div');
    head.className = 'connector-head';
    const title = document.createElement('strong');
    title.textContent = connector.name;
    const id = document.createElement('span');
    id.className = 'counter';
    id.textContent = connector.id + ' · ' + connector.category;
    head.append(title, id);

    const badges = document.createElement('div');
    badges.className = 'connector-badges';
    for (const badge of connectorBadges(connector)) {
      const item = document.createElement('span');
      item.className = 'connector-badge ' + badge.tone;
      item.textContent = badge.label;
      badges.append(item);
    }

    const transport = document.createElement('p');
    transport.className = 'connector-meta';
    transport.textContent = transportSummary(connector);
    transport.title = transport.textContent;

    const credentials = document.createElement('p');
    credentials.className = 'connector-meta' + (connector.missing_env.length ? ' missing' : '');
    credentials.textContent = credentialSummary(connector);

    const actions = document.createElement('div');
    actions.className = 'connector-actions';
    if (connector.source === 'preset') {
      const add = document.createElement('button');
      add.type = 'button';
      add.className = 'button ghost compact';
      add.textContent = 'Add';
      add.addEventListener('click', () => {
        void invoke<ConnectorSnapshot>('add_connector', { preset: connector.id })
          .then((snapshot) => {
            showNotice('');
            render(snapshot);
            setStatus(`Added connector ${connector.id}.`);
          })
          .catch(fail);
      });
      actions.append(add);
    } else {
      const toggle = document.createElement('button');
      toggle.type = 'button';
      toggle.className = 'button ' + (connector.enabled ? 'ghost' : 'primary') + ' compact';
      toggle.textContent = connector.enabled ? 'Disable' : 'Enable';
      toggle.addEventListener('click', () => {
        void invoke<ConnectorSnapshot>('set_connector_enabled', {
          id: connector.id,
          enabled: !connector.enabled,
        })
          .then((snapshot) => {
            showNotice('');
            render(snapshot);
            setStatus(`Connector ${connector.id} ${connector.enabled ? 'disabled' : 'enabled'}.`);
          })
          .catch(fail);
      });

      const test = document.createElement('button');
      test.type = 'button';
      test.className = 'button ghost compact';
      test.textContent = 'Test tools';
      test.disabled = connector.transport.kind !== 'stdio';
      test.addEventListener('click', () => {
        inspect.textContent = `Starting ${connector.id}…`;
        inspect.classList.remove('hidden');
        void invoke<{ tools: string[]; note: string }>('inspect_connector', { id: connector.id })
          .then((result) => {
            inspect.textContent =
              `${connector.id}: ${result.note}\n` +
              result.tools.map((tool) => (connector.write_tools.includes(tool) ? `! ${tool}` : `  ${tool}`)).join('\n');
          })
          .catch((error: unknown) => {
            inspect.textContent = connector.id + ': ' + (error instanceof Error ? error.message : String(error));
          });
      });
      actions.append(toggle, test);
    }

    article.append(head, badges, transport, credentials, actions);

    if (connector.tools.length > 0) {
      const details = document.createElement('details');
      details.className = 'connector-tools';
      const summary = document.createElement('summary');
      summary.textContent = `Tools (${connector.allowed_tools.length}/${connector.tools.length} allowed)`;
      const grid = document.createElement('div');
      grid.className = 'connector-tool-grid';
      for (const tool of connector.tools) {
        const label = document.createElement('label');
        label.className = 'connector-tool' + (tool.write ? ' write' : '');
        const input = document.createElement('input');
        input.type = 'checkbox';
        input.checked = tool.allowed;
        input.disabled = connector.source === 'preset';
        input.addEventListener('change', () => {
          const next = toggleTool({ ...connector, allowed_tools: connector.allowed_tools }, tool.name);
          void invoke<ConnectorSnapshot>('set_connector_tools', { id: connector.id, tools: next })
            .then((snapshot) => {
              showNotice('');
              render(snapshot);
            })
            .catch((error) => {
              input.checked = tool.allowed;
              fail(error);
            });
        });
        const text = document.createElement('span');
        text.textContent = tool.write ? tool.name + ' (writes)' : tool.name;
        label.append(input, text);
        grid.append(label);
      }
      details.append(summary, grid);
      article.append(details);
    }
    return article;
  }

  function render(snapshot: ConnectorSnapshot) {
    path.textContent = snapshot.path;
    if (snapshot.error) {
      showNotice(snapshot.error);
    }
    const configured = snapshot.connectors.length;
    const ready = readyCount(snapshot);
    count.textContent = configured === 0
      ? 'none configured'
      : `${ready}/${configured} ready`;
    list.replaceChildren();
    const entries = catalogEntries(snapshot);
    if (entries.length === 0) {
      const hint = document.createElement('p');
      hint.className = 'hint';
      hint.textContent = 'No connector presets found.';
      list.append(hint);
      return;
    }
    for (const connector of entries) {
      list.append(card(connector));
    }
  }

  function load(quiet = false) {
    refresh.disabled = true;
    void invoke<ConnectorSnapshot>('list_connectors')
      .then((snapshot) => {
        showNotice('');
        render(snapshot);
      })
      .catch((error: unknown) => {
        count.textContent = 'no folder';
        path.textContent = 'Open a folder to configure connectors.';
        // Before a workspace is open this always fails; that is not an error
        // worth a toast. Explicit refreshes still report the reason.
        if (!quiet) fail(error);
      })
      .finally(() => {
        refresh.disabled = false;
      });
  }

  refresh.addEventListener('click', () => load());
  load(true);
  return { reload: () => load(true) };
}
