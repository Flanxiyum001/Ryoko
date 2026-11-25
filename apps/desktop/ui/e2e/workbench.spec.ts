import { test, expect, type Page } from '@playwright/test';

// Exercise the real renderer and Monaco with a deterministic IPC boundary.
// These tests do not claim to run native dialogs, disk IO or language servers.
test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const files: Record<string, string> = {
      'README.md': '# Demo workspace\n',
      'main.ts': 'const greeting = "Hello Ryoko";\nconsole.log(greeting);\n',
      'other.ts': 'export const value = 42;\n',
      'src/notes.unknown': 'example',
    };
    let reads = 0;
    Object.assign(window, {
      __TAURI_EVENT_PLUGIN_INTERNALS__: { unregisterListener() {} },
      __TAURI_INTERNALS__: {
        transformCallback: () => 1,
        unregisterCallback() {},
        async invoke(command: string, args: Record<string, string> = {}) {
          if (command === 'pick_workspace_folder') return '/demo/workspace';
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
  await expect(page.getByText('remote installation and automatic policy changes stay disabled', { exact: false })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Install local extension…' })).toBeVisible();
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

test('command palette lists commands with shortcuts and runs them', async ({ page }) => {
  await page.goto('/');
  await page.keyboard.press('Control+Shift+P');
  const palette = page.getByRole('dialog', { name: 'Command palette' });
  await expect(palette).toBeVisible();
  await page.keyboard.type('toggle side bar');
  const option = palette.getByRole('option').first();
  await expect(option).toContainText('View: Toggle Primary Side Bar');
  await expect(option).toContainText('Ctrl+B');
  await page.keyboard.press('Enter');
  await expect(palette).toBeHidden();
  await expect(page.locator('#explorerPane')).toBeHidden();
  await page.keyboard.press('Control+B');
  await expect(page.locator('#explorerPane')).toBeVisible();
});

test('menu bar opens with the keyboard and runs actions', async ({ page }) => {
  await page.goto('/');
  await page.locator('#menu-view').click();
  const viewMenu = page.getByRole('menu', { name: 'View' });
  await expect(viewMenu).toBeVisible();
  await expect(viewMenu.getByRole('menuitem', { name: /Command Palette/ })).toContainText('Ctrl+Shift+P');
  await page.keyboard.press('ArrowRight');
  await expect(page.getByRole('menu', { name: 'Run' })).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('menu', { name: 'Run' })).toBeHidden();
  await page.locator('#menu-file').click();
  await page.getByRole('menuitem', { name: /Open Folder/ }).click();
  await expect(page.locator('#workspaceName')).toHaveText('workspace');
});

test('theme and layout choices persist across reloads', async ({ page }) => {
  await page.goto('/');
  await page.keyboard.press('Control+Shift+P');
  await page.keyboard.type('theme light');
  await page.keyboard.press('Enter');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  const sash = page.locator('#sidebarSash');
  const box = (await sash.boundingBox())!;
  await page.mouse.move(box.x + 2, box.y + 200);
  await page.mouse.down();
  await page.mouse.move(box.x + 82, box.y + 200, { steps: 4 });
  await page.mouse.up();
  // The workbench now animates panel resizing; measure only the settled width.
  await page.locator('.layout').evaluate(async (layout) => {
    await Promise.all(layout.getAnimations().map((animation) => animation.finished.catch(() => {})));
  });
  const width = await page.locator('#explorerPane').evaluate((pane) => pane.getBoundingClientRect().width);
  expect(width).toBeGreaterThan(320);
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await expect.poll(async () => {
    const restored = await page.locator('#explorerPane').evaluate((pane) => pane.getBoundingClientRect().width);
    return Math.abs(restored - width);
  }).toBeLessThan(2);
});

test('Vim keymap mounts and persists across reloads', async ({ page }) => {
  await page.goto('/');
  await page.locator('#openSettings').click();
  await page.locator('#settingsKeymap').selectOption('vim');
  await expect(page.locator('#vimStatus')).toBeVisible();
  await page.reload();
  await expect(page.locator('#vimStatus')).toBeVisible();
  await page.locator('#openSettings').click();
  await expect(page.locator('#settingsKeymap')).toHaveValue('vim');
});

test('Emacs keymap applies without an unavailable fallback', async ({ page }) => {
  await page.goto('/');
  await page.locator('#openSettings').click();
  await page.locator('#settingsKeymap').selectOption('emacs');
  await expect(page.locator('.toast-item').filter({ hasText: 'Editor keymap: Emacs' })).toBeVisible();
  await expect(page.locator('.toast-error').filter({ hasText: 'Emacs keymap unavailable' })).toHaveCount(0);
});

test('explorer shows file-type icons and breadcrumbs for the active file', async ({ page }) => {
  await page.goto('/');
  await page.locator('#openFolder').click();
  await expect(page.locator('.file-link').filter({ hasText: 'main.ts' }).locator('.file-icon')).toHaveText('TS');
  await page.locator('.file-link').filter({ hasText: 'main.ts' }).click();
  await expect(page.getByRole('tab', { name: 'main.ts', exact: true }).locator('.file-icon')).toHaveText('TS');
  await expect(page.getByRole('navigation', { name: 'Breadcrumbs' })).toContainText('main.ts');
});

test('an available update is announced and installs only when the user asks', async ({ page }) => {
  await page.addInitScript(() => {
    const internals = (window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, a?: unknown) => Promise<unknown> } }).__TAURI_INTERNALS__;
    const original = internals.invoke;
    internals.invoke = async (command: string, args?: unknown) => {
      if (command === 'update_status') return { configured: true, currentVersion: '0.1.0-alpha.8' };
      if (command === 'update_check') {
        return { version: '0.1.0-alpha.9', currentVersion: '0.1.0-alpha.8', notes: '<b>New</b> command palette', date: null };
      }
      if (command === 'update_install') { document.body.dataset.installed = 'yes'; return null; }
      return original(command, args);
    };
  });
  await page.goto('/');
  await expect(page.locator('body')).not.toHaveAttribute('data-installed', 'yes');
  await page.locator('#menu-help').click();
  await page.getByRole('menuitem', { name: /Check for Updates/ }).click();
  const notice = page.getByRole('dialog', { name: 'A new update is available' });
  await expect(notice).toContainText('Ryoko 0.1.0-alpha.9 is ready to install. You have 0.1.0-alpha.8.');
  await expect(page.locator('#updateStatus')).toHaveText('⬆ Update 0.1.0-alpha.9');
  await notice.getByRole('button', { name: 'Release notes' }).click();
  await expect(notice.locator('.update-notes')).toHaveText('<b>New</b> command palette');
  await expect(page.locator('body')).not.toHaveAttribute('data-installed', 'yes');
  await notice.getByRole('button', { name: 'Download and install' }).click();
  await expect(page.locator('body')).toHaveAttribute('data-installed', 'yes');
});


test('workbench uses distinct accessible local SVG icons', async ({ page }) => {
  const rail = page.getByRole('navigation', { name: 'Workbench views' });
  const icons = rail.locator('button > svg.rail-icon');
  await expect(icons).toHaveCount(9);
  const names = await icons.evaluateAll(nodes => nodes.map(node => node.getAttribute('data-icon')));
  expect(new Set(names).size).toBe(9);
  for (const icon of await icons.all()) {
    await expect(icon).toHaveAttribute('aria-hidden', 'true');
    await expect(icon).toHaveAttribute('focusable', 'false');
  }
  await expect(page.locator('#quickOpenButton svg')).toHaveAttribute('data-icon', 'search');
  await expect(page.locator('#refreshFiles svg')).toHaveAttribute('data-icon', 'refresh-cw');
  await openProject(page);
  const folder = page.locator('.tree-dir').filter({ hasText: 'src' });
  await expect(folder.locator('svg')).toHaveAttribute('data-icon', 'folder');
  await folder.click();
  await expect(folder.locator('svg')).toHaveAttribute('data-icon', 'folder-open');
  await expect(page.locator('.file-link').filter({ hasText: 'notes.unknown' }).locator('svg')).toHaveAttribute('data-icon', 'file');
  await expect(page.locator('.file-link').filter({ hasText: 'main.ts' }).locator('.file-icon')).toHaveText('TS');
});


test('action icons retain labels and editor tab close behavior', async ({ page }) => {
  for (const [id, name] of Object.entries({
    openFolder: 'folder-open', saveFile: 'save', runActiveFile: 'play',
    toggleTerminal: 'terminal', toggleDebug: 'bug', chatSend: 'send',
    debugNext: 'step-forward', debugStepIn: 'arrow-down-to-line',
    debugStepOut: 'arrow-up-from-line', runPause: 'pause',
    proposalApply: 'check', settingsClose: 'x',
  })) {
    await expect(page.locator('#' + id + ' > svg')).toHaveAttribute('data-icon', name);
    await expect(page.locator('#' + id + ' > svg')).toHaveAttribute('aria-hidden', 'true');
    expect((await page.locator('#' + id).textContent())?.trim()).not.toBe('');
  }
  await openProject(page);
  await openFile(page, 'main.ts');
  const close = page.getByRole('button', { name: 'Close main.ts', exact: true });
  await expect(close.locator('svg')).toHaveAttribute('data-icon', 'x');
  await close.click();
  await expect(page.getByRole('tab')).toHaveCount(0);
});
