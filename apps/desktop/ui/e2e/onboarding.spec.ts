import { expect, test } from '@playwright/test';

test('first launch configures a local provider, opens a folder and prepares a review-first goal', async ({ page }) => {
  await page.addInitScript(() => {
    const setup = { provider: '', model: '', configured: false, recentFolders: [] as string[] };
    Object.assign(window, {
      __TAURI_EVENT_PLUGIN_INTERNALS__: { unregisterListener() {} },
      __TAURI_INTERNALS__: {
        transformCallback: () => 1,
        unregisterCallback() {},
        async invoke(command: string, args: Record<string, unknown> = {}) {
          if (command === 'onboarding_state') return setup;
          if (command === 'onboarding_configure') {
            setup.provider = String(args.provider);
            setup.model = String(args.model);
            setup.configured = true;
            return { ...setup };
          }
          if (command === 'onboarding_remember_folder') {
            setup.recentFolders.unshift(String(args.path));
            return { ...setup };
          }
          if (command === 'pick_workspace_folder') return '/tmp/example-project';
          if (command === 'open_workspace') return { root: '/tmp/example-project', name: 'example-project' };
          if (command === 'list_files') return ['README.md'];
          if (command === 'git_changes') return { isRepository: false, entries: [] };
          if (command === 'run_status') return { running: false, paused: false, exitCode: null, eventLog: null, startedMs: null, logTail: '' };
          if (command === 'lsp_registry') return { registry: { servers: [] } };
          if (command === 'lsp_start') throw new Error('LSP disabled in browser acceptance tests');
          if (command === 'marketplace_catalog' || command.includes('list')) return [];
          return null;
        },
      },
    });
  });
  await page.goto('/');
  const wizard = page.getByRole('dialog', { name: 'Set up Ryoko' });
  await expect(wizard).toBeVisible();
  await wizard.getByLabel('AI provider').selectOption('ollama');
  await expect(wizard.getByLabel('Model ID')).toHaveValue('qwen2.5-coder:7b');
  await expect(wizard.getByLabel(/API key/)).toBeDisabled();
  await wizard.getByRole('button', { name: 'Save & continue' }).click();
  await expect(wizard.getByText('Provider configured')).toBeVisible();
  await wizard.getByRole('button', { name: 'Open a folder…' }).click();
  await expect(wizard.getByText('Your first agent task', { exact: true })).toBeVisible();
  await wizard.getByRole('button', { name: 'Prepare my first task' }).click();
  await expect(wizard).not.toBeVisible();
  await expect(page.locator('#runGoal')).toHaveValue(/Review this repository/);
  await expect(page.locator('#workspaceName')).toHaveText('example-project');
});

test('hosted provider secrets are never rendered on the welcome page after setup', async ({ page }) => {
  await page.addInitScript(() => {
    Object.assign(window, {
      __TAURI_EVENT_PLUGIN_INTERNALS__: { unregisterListener() {} },
      __TAURI_INTERNALS__: {
        transformCallback: () => 1,
        unregisterCallback() {},
        async invoke(command: string, args: Record<string, string>) {
          if (command === 'onboarding_state') return { provider: '', model: '', configured: false, recentFolders: [] };
          if (command === 'onboarding_configure') {
            document.body.dataset.keyReceivedByNative = String(Boolean(args.apiKey));
            return { provider: args.provider, model: args.model, configured: true, recentFolders: [] };
          }
          if (command === 'lsp_registry') return { registry: { servers: [] } };
          if (command === 'lsp_start') throw new Error('LSP disabled');
          if (command === 'marketplace_catalog' || command.includes('list')) return [];
          return null;
        },
      },
    });
  });
  await page.goto('/');
  const wizard = page.getByRole('dialog', { name: 'Set up Ryoko' });
  await wizard.getByLabel('AI provider').selectOption('openai');
  await wizard.getByLabel(/API key/).fill('test-credential-never-log-this');
  await wizard.getByRole('button', { name: 'Save & continue' }).click();
  await expect(page.locator('body')).toHaveAttribute('data-key-received-by-native', 'true');
  await expect(wizard.getByText('Provider configured')).toBeVisible();
  await expect(page.locator('body')).not.toContainText('test-credential-never-log-this');
});
