import { test, expect, type Page } from '@playwright/test';

// Exercise the real renderer and Monaco with a deterministic IPC boundary.
// These tests do not claim to run native dialogs, disk IO or language servers.
test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const files: Record<string, string> = {
      'README.md': '# Demo workspace\n',
      'main.ts': 'const greeting = "Hello Ryoko";\nconsole.log(greeting);\n',
      'other.ts': 'export const value = 42;\n',
    };
    let reads = 0;
    Object.assign(window, {
      __TAURI_EVENT_PLUGIN_INTERNALS__: { unregisterListener() {} },
      __TAURI_INTERNALS__: {
        transformCallback: () => 1,
        unregisterCallback() {},
        async invoke(command: string, args: Record<string, string> = {}) {
          if (command === 'plugin:dialog|open') return '/demo/workspace';
          if (command === 'open_workspace') return { root: '/demo/workspace', name: 'workspace' };
          if (command === 'list_files') return Object.keys(files);
          if (command === 'read_file') { reads++; document.body.dataset.reads = String(reads); return files[args.path]; }
          if (command === 'write_file') {
            if (files[args.path] !== args.expectedContent) throw new Error('File changed on disk');
            files[args.path] = args.content;
            document.body.dataset.saved = args.content;
            return;
          }
          if (command === 'git_changes') return { isRepository: true, entries: [] };
          if (command === 'run_status') return { running: false, paused: false, exitCode: null, eventLog: null, startedMs: null, logTail: '' };
          if (command === 'lsp_registry') return { registry: { servers: [] } };
          if (command === 'lsp_start') throw new Error('Language servers are outside renderer testing');
          if (command === 'marketplace_catalog' || command.includes('list')) return [];
          return null;
        },
      },
    });
  });
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'Ryoko', exact: true })).toBeVisible();
});

async function openProject(page: Page) {
  await page.locator('#openFolder').click();
  await expect(page.locator('#workspaceName')).toHaveText('workspace');
  await expect(page.locator('#lspStatus')).not.toHaveText('ryoko-lsp starting');
}
async function openFile(page: Page, name: string) {
  await page.locator('.file-link').filter({ hasText: name }).click();
  await expect(page.getByRole('tab', { name, exact: true })).toHaveAttribute('aria-selected', 'true');
}
async function edit(page: Page) {
  await page.locator('#editor .inputarea').first().focus();
  await page.keyboard.press('Control+Home');
  await page.keyboard.type('// Unsaved edit\n');
  await expect(page.getByRole('tab', { name: 'main.ts, unsaved changes' })).toBeVisible();
}

test('tabs preserve edits, save through IPC, and confirm discard', async ({ page }, info) => {
  await page.screenshot({ path: info.outputPath('welcome.png') });
  await openProject(page);
  await openFile(page, 'main.ts');
  await edit(page);
  await openFile(page, 'other.ts');
  const reads = await page.locator('body').getAttribute('data-reads');
  await page.getByRole('tab', { name: 'main.ts, unsaved changes' }).click();
  await expect(page.locator('body')).toHaveAttribute('data-reads', reads!);
  await expect(page.locator('#editor')).toContainText('Unsaved edit');
  page.once('dialog', (dialog) => dialog.dismiss());
  await page.getByRole('button', { name: 'Close main.ts', exact: true }).click();
  await expect(page.getByRole('tab')).toHaveCount(2);
  await page.locator('#saveFile').click();
  await expect(page.locator('body')).toHaveAttribute('data-saved', /Unsaved edit/);
  await expect(page.getByRole('tab', { name: 'main.ts', exact: true })).toBeVisible();
  await edit(page);
  await page.screenshot({ path: info.outputPath('editor.png') });
  page.once('dialog', (dialog) => dialog.accept());
  await page.getByRole('button', { name: 'Close main.ts', exact: true }).click();
  await expect(page.getByRole('tab', { name: 'other.ts' })).toHaveAttribute('aria-selected', 'true');
  await page.getByRole('button', { name: 'Close other.ts', exact: true }).click();
  await expect(page.getByRole('tab')).toHaveCount(0);
  await expect(page.locator('#emptyEditor')).toBeVisible();
});

test('keyboard navigation, background close and workspace cancellation', async ({ page }) => {
  await openProject(page);
  await openFile(page, 'main.ts');
  await edit(page);
  await openFile(page, 'other.ts');
  await page.getByRole('tab', { name: 'other.ts' }).press('ArrowLeft');
  await expect(page.getByRole('tab', { name: 'main.ts, unsaved changes' })).toBeFocused();
  await page.getByRole('button', { name: 'Close other.ts', exact: true }).click();
  await expect(page.getByRole('tab', { name: 'main.ts, unsaved changes' })).toHaveAttribute('aria-selected', 'true');
  page.once('dialog', (dialog) => dialog.dismiss());
  await page.locator('#openFolder').click();
  await expect(page.getByRole('tab', { name: 'main.ts, unsaved changes' })).toBeVisible();
  page.once('dialog', (dialog) => dialog.accept());
  await page.locator('#openFolder').click();
  await expect(page.getByRole('tab')).toHaveCount(0);
  await expect(page.locator('#emptyEditor')).toBeVisible();
});

test('navigation exposes search and browse-only extensions', async ({ page }) => {
  await page.getByRole('button', { name: 'Search workspace', exact: true }).click();
  await expect(page.locator('#searchInput')).toBeVisible();
  await page.getByRole('button', { name: 'Browse extensions', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Marketplace preview' })).toBeVisible();
  await expect(page.getByText('Installing third-party code is disabled during alpha testing.', { exact: false })).toBeVisible();
  await page.getByRole('button', { name: 'Close', exact: true }).last().click();
  // Quick open intentionally requires an active workspace; verify the native
  // folder flow before expecting the palette to appear.
  await openProject(page);
  await page.locator('#quickOpenButton').click();
  await expect(page.getByRole('dialog', { name: 'Quick open files' })).toBeVisible();
});

test('narrow layout opens files from the explorer without clipping', async ({ page }, info) => {
  await page.setViewportSize({ width: 680, height: 780 });
  await openProject(page);
  await openFile(page, 'README.md');
  await expect(page.locator('#editor')).toBeVisible();
  await expect(page.locator('#explorerPane')).not.toBeVisible();
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
  expect(overflow).toBe(false);
  await page.screenshot({ path: info.outputPath('narrow.png') });
});
