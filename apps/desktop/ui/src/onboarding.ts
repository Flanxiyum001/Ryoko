/** First-run setup and the welcome page. Secrets are sent only to the native
 * keychain command, and never enter localStorage or provider catalog JSON. */
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';

export interface SetupState {
  provider: string;
  model: string;
  configured: boolean;
  recentFolders: string[];
}

export const MODELS: Record<string, string> = {
  openai: 'gpt-4.1-mini',
  anthropic: 'claude-sonnet-4-5',
  gemini: 'gemini-2.5-flash',
  ollama: 'qwen2.5-coder:7b',
};

export function folderName(path: string): string {
  return path.replace(/[\\/]+$/, '').split(/[\\/]/).at(-1) || path;
}

export function validModelId(model: string): boolean {
  return /^[A-Za-z0-9:._/-]{1,128}$/.test(model.trim());
}

interface OnboardingDeps {
  openFolder: () => Promise<void>;
  openFolderPath: (path: string) => Promise<void>;
  openMission: () => void;
  hasWorkspace: () => boolean;
  prepareTask: (goal: string) => void;
  notify: (message: string) => void;
}

const FIRST_GOAL = 'Review this repository and identify one small, low-risk improvement. Explain the proposed change, run relevant checks, and present all edits for my review. Do not commit, push, or merge anything.';

export function mountOnboarding(deps: OnboardingDeps): {
  workspaceOpened: (path: string) => void;
  refresh: () => Promise<void>;
} {
  const recent = document.getElementById('welcomeRecent');
  const launch = document.getElementById('welcomeOnboarding');
  const clone = document.getElementById('welcomeClone');
  let state: SetupState | null = null;
  let step = 0;

  const dialog = document.createElement('dialog');
  dialog.id = 'firstRunDialog';
  dialog.className = 'setup-dialog';
  dialog.setAttribute('aria-label', 'Set up Ryoko');
  dialog.innerHTML = `
    <div class="setup-header">
      <span class="setup-eyebrow">RYOKO / INITIAL SETUP</span>
      <h2>Make the workspace yours.</h2>
      <p>Choose your model, open a project, and try your first agent task.</p>
      <div id="setupProgress" class="setup-progress" aria-label="Setup progress"></div>
    </div>
    <div id="setupContent" class="setup-content"></div>
    <div class="setup-footer">
      <button id="setupDismiss" class="button ghost" type="button">Not now</button>
      <span id="setupError" class="setup-error" role="alert"></span>
      <button id="setupNext" class="button primary" type="button">Continue</button>
    </div>`;
  document.body.append(dialog);
  const content = dialog.querySelector<HTMLElement>('#setupContent')!;
  const error = dialog.querySelector<HTMLElement>('#setupError')!;
  const next = dialog.querySelector<HTMLButtonElement>('#setupNext')!;
  const dismiss = dialog.querySelector<HTMLButtonElement>('#setupDismiss')!;
  const progress = dialog.querySelector<HTMLElement>('#setupProgress')!;

  function renderRecent(): void {
    if (!recent) return;
    recent.replaceChildren();
    if (!state?.recentFolders?.length) {
      const empty = document.createElement('p');
      empty.className = 'welcome-recent-empty';
      empty.textContent = 'No recent projects. Open a folder or clone a repository to begin.';
      recent.append(empty);
      return;
    }
    for (const path of state.recentFolders) {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'recent-folder';
      const name = document.createElement('strong');
      name.textContent = folderName(path);
      const location = document.createElement('small');
      location.textContent = path;
      button.append(name, location);
      button.title = path;
      button.addEventListener('click', () => {
        void deps.openFolderPath(path);
      });
      recent.append(button);
    }
  }

  async function cloneRepository(): Promise<void> {
    const value = await new Promise<string | null>((resolve) => {
      const picker = document.createElement('dialog');
      picker.className = 'setup-dialog clone-dialog';
      picker.setAttribute('aria-label', 'Clone repository');
      picker.innerHTML = `
        <form method="dialog" class="clone-form">
          <h2>Clone a repository</h2>
          <p>Paste an HTTPS Git URL. Ryoko will ask where to place the clone.</p>
          <label for="cloneUrl">Repository URL</label>
          <input id="cloneUrl" type="url" required placeholder="https://github.com/owner/repository.git" spellcheck="false" autocomplete="url">
          <div class="clone-actions">
            <button type="button" class="button ghost" id="cloneCancel">Cancel</button>
            <button type="submit" class="button primary">Choose destination…</button>
          </div>
        </form>`;
      document.body.append(picker);
      const input = picker.querySelector<HTMLInputElement>('#cloneUrl')!;
      picker.querySelector<HTMLButtonElement>('#cloneCancel')!.addEventListener('click', () => picker.close());
      picker.querySelector<HTMLFormElement>('.clone-form')!.addEventListener('submit', (event) => {
        event.preventDefault();
        if (input.checkValidity()) picker.close(input.value.trim());
        else input.reportValidity();
      });
      picker.addEventListener('close', () => {
        const choice = picker.returnValue || null;
        picker.remove();
        resolve(choice);
      }, { once: true });
      picker.showModal();
      input.focus();
    });
    if (!value) return;
    const parent = await open({ directory: true, multiple: false, title: 'Choose where to clone the repository' });
    if (typeof parent !== 'string') return;
    try {
      deps.notify('Cloning repository…');
      const path = await invoke<string>('clone_repository', { url: value.trim(), parent });
      await deps.openFolderPath(path);
      deps.notify('Repository cloned and opened.');
    } catch (err) {
      showError(err);
    }
  }

  function showError(err: unknown): void {
    const text = err instanceof Error ? err.message : String(err);
    error.textContent = text;
    deps.notify(text);
  }

  function renderStep(): void {
    error.textContent = '';
    progress.replaceChildren();
    ['Provider', 'Workspace', 'First task'].forEach((label, index) => {
      const item = document.createElement('span');
      item.className = 'setup-step' + (index <= step ? ' is-active' : '');
      item.textContent = String(index + 1) + '  ' + label;
      progress.append(item);
    });
    if (step === 0) {
      content.innerHTML = `
        <label for="setupProvider">AI provider</label>
        <select id="setupProvider">
          <option value="openai">OpenAI</option>
          <option value="anthropic">Anthropic</option>
          <option value="gemini">Google Gemini</option>
          <option value="ollama">Ollama (local, no key)</option>
        </select>
        <label for="setupModel">Model ID</label>
        <input id="setupModel" type="text" autocapitalize="off" autocomplete="off" spellcheck="false" maxlength="128">
        <label for="setupKey">API key (stored in your OS keychain)</label>
        <input id="setupKey" type="password" autocomplete="off" spellcheck="false" placeholder="Your API key">
        <p class="hint" id="setupSecretNote">Keys never go into project files or browser storage. Local Ollama requires no key.</p>`;
      const provider = content.querySelector<HTMLSelectElement>('#setupProvider')!;
      const model = content.querySelector<HTMLInputElement>('#setupModel')!;
      const key = content.querySelector<HTMLInputElement>('#setupKey')!;
      provider.value = state?.provider && MODELS[state.provider] ? state.provider : 'openai';
      model.value = state?.model || MODELS[provider.value];
      provider.addEventListener('change', () => {
        model.value = MODELS[provider.value];
        key.disabled = provider.value === 'ollama';
        key.placeholder = key.disabled ? 'No key required' : 'Your API key';
      });
      key.disabled = provider.value === 'ollama';
      next.textContent = 'Save & continue';
    } else if (step === 1) {
      content.innerHTML = `
        <div class="setup-summary"><strong>Provider configured</strong><span>Keys are kept in your OS credential vault.</span></div>
        <p>Choose a project folder or clone a Git repository. You can switch workspaces anytime.</p>
        <div class="setup-actions">
          <button type="button" id="setupOpenFolder" class="button primary">Open a folder…</button>
          <button type="button" id="setupClone" class="button">Clone a repository…</button>
        </div>`;
      content.querySelector('#setupOpenFolder')!.addEventListener('click', () => void deps.openFolder());
      content.querySelector('#setupClone')!.addEventListener('click', () => void cloneRepository());
      next.textContent = deps.hasWorkspace() ? 'Continue' : 'Choose a folder first';
      next.disabled = !deps.hasWorkspace();
    } else {
      content.innerHTML = `
        <div class="setup-summary"><strong>Your first agent task</strong><span>Ryoko will stage its proposed changes for review.</span></div>
        <p class="setup-goal"></p>
        <p class="hint">This prepares the goal in Mission Control. You explicitly choose Run to start work.</p>`;
      content.querySelector<HTMLElement>('.setup-goal')!.textContent = FIRST_GOAL;
      next.disabled = false;
      next.textContent = 'Prepare my first task';
    }
    if (step === 0) next.disabled = false;
  }

  next.addEventListener('click', async () => {
    if (step === 0) {
      const provider = content.querySelector<HTMLSelectElement>('#setupProvider')!.value;
      const model = content.querySelector<HTMLInputElement>('#setupModel')!.value.trim();
      const key = content.querySelector<HTMLInputElement>('#setupKey')!;
      if (!validModelId(model)) { error.textContent = 'Enter a valid model ID.'; return; }
      if (provider !== 'ollama' && !key.value.trim()) { error.textContent = 'Enter an API key to continue.'; return; }
      next.disabled = true;
      try {
        state = await invoke<SetupState>('onboarding_configure', { provider, model, apiKey: provider === 'ollama' ? null : key.value });
        key.value = '';
        step = 1;
        renderStep();
        deps.notify('Model configured securely.');
      } catch (err) {
        key.value = '';
        showError(err);
        next.disabled = false;
      }
    } else if (step === 1) {
      if (!deps.hasWorkspace()) return;
      step = 2;
      renderStep();
    } else {
      dialog.close();
      deps.prepareTask(FIRST_GOAL);
      deps.openMission();
      deps.notify('Review the starter goal, then choose Run when ready.');
    }
  });
  dismiss.addEventListener('click', () => dialog.close());
  dialog.addEventListener('cancel', () => { error.textContent = ''; });
  launch?.addEventListener('click', () => { step = 0; renderStep(); dialog.showModal(); });
  clone?.addEventListener('click', () => void cloneRepository());

  async function refresh(): Promise<void> {
    try {
      const result = await invoke<SetupState | null>('onboarding_state');
      if (!result || !Array.isArray(result.recentFolders)) return; // browser preview / mocked bridge
      state = result;
      renderRecent();
      if (!result.configured && !dialog.open) {
        step = 0;
        renderStep();
        dialog.showModal();
      }
    } catch {
      // Renderer-only previews run without native onboarding commands.
      renderRecent();
    }
  }

  function workspaceOpened(path: string): void {
    if (state) {
      state.recentFolders = [path, ...state.recentFolders.filter(item => item !== path)].slice(0, 8);
      renderRecent();
    }
    if (dialog.open && step === 1) {
      step = 2;
      renderStep();
    }
  }

  renderRecent();
  void refresh();
  return { workspaceOpened, refresh };
}
