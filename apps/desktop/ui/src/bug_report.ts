import { getVersion } from '@tauri-apps/api/app';

const STORAGE_KEY = 'ryoko-bug-report-endpoint';
const REPORT_FIELDS = [
  ['description', 'What happened?', 'Describe the problem you encountered.', true],
  ['expected', 'What did you expect to happen?', 'Expected behavior', false],
  ['actual', 'What happened instead?', 'Actual behavior', false],
  ['steps', 'Steps to reproduce', '1.\n2.\n3.', false],
];

function element(tag, className, text) {
  const item = document.createElement(tag);
  if (className) item.className = className;
  if (text !== undefined) item.textContent = text;
  return item;
}

function field(id, labelText, placeholder, required = false) {
  const wrapper = element('div', 'bug-report-field');
  const label = element('label', '', labelText) as HTMLLabelElement;
  label.htmlFor = id;
  const input = element('textarea', 'bug-report-input') as HTMLTextAreaElement;
  input.id = id;
  input.placeholder = placeholder;
  input.rows = id === 'description' ? 4 : 3;
  input.required = required;
  input.maxLength = 8000;
  wrapper.append(label, input);
  return { wrapper, input };
}

function safeEndpoint(value) {
  try {
    const url = new URL(value.trim());
    if (url.protocol !== 'https:' || url.username || url.password || url.hash) return null;
    return url.toString().replace(/\/$/, '');
  } catch {
    return null;
  }
}

export function mountBugReporter({ setStatus }) {
  const dialog = element('dialog', 'bug-report-dialog') as HTMLDialogElement;
  dialog.setAttribute('aria-labelledby', 'bug-report-title');
  const form = element('form', 'bug-report-form') as HTMLFormElement;
  form.method = 'dialog';
  const header = element('header', 'bug-report-header');
  const heading = element('div');
  const reportHeading = element('h2', '', 'Report a bug');\n  reportHeading.id = 'bug-report-title';\n  heading.append(reportHeading, element('p', 'hint', 'Tell us what went wrong. Review your report before sending.'));
  const close = element('button', 'button ghost compact', 'Close') as HTMLButtonElement;
  close.type = 'button';
  close.addEventListener('click', () => dialog.close());
  header.append(heading, close);

  const endpointField = element('div', 'bug-report-field');
  const endpointLabel = element('label', '', 'Bug-report server endpoint') as HTMLLabelElement;
  endpointLabel.htmlFor = 'bug-report-endpoint';
  const endpoint = element('input', 'bug-report-input') as HTMLInputElement;
  endpoint.id = 'bug-report-endpoint';
  endpoint.type = 'url';
  endpoint.placeholder = 'https://your-ryoko-server.example/api/bug-report';
  endpoint.required = true;
  endpoint.value = localStorage.getItem(STORAGE_KEY) || '';
  endpointField.append(endpointLabel, endpoint);
  const endpointHint = element('p', 'hint', 'The server endpoint must be HTTPS. Your report is sent only when you submit.');

  const titleField = element('div', 'bug-report-field');
  const titleLabel = element('label', '', 'Title') as HTMLLabelElement;
  titleLabel.htmlFor = 'bug-report-title-input';
  const title = element('input', 'bug-report-input') as HTMLInputElement;
  title.id = 'bug-report-title-input';
  title.type = 'text';
  title.maxLength = 120;
  title.required = true;
  title.placeholder = 'Short summary of the problem';
  titleField.append(titleLabel, title);

  const controls = REPORT_FIELDS.map(([id, labelText, placeholder, required]) => {
    const result = field('bug-report-' + id, labelText, placeholder, required);
    result.input.id = 'bug-report-' + id;
    return result;
  });
  const severityField = element('div', 'bug-report-field');
  const severityLabel = element('label', '', 'Severity') as HTMLLabelElement;
  severityLabel.htmlFor = 'bug-report-severity';
  const severity = element('select', 'bug-report-input') as HTMLSelectElement;
  severity.id = 'bug-report-severity';
  for (const value of ['low', 'medium', 'high', 'critical']) {
    const option = element('option', '', value[0].toUpperCase() + value.slice(1)) as HTMLOptionElement;
    option.value = value;
    if (value === 'medium') option.selected = true;
    severity.append(option);
  }
  severityField.append(severityLabel, severity);

  const diagnostics = element('p', 'bug-report-note', 'Automatically includes Ryoko version and operating system. No project files, logs, or secrets are attached.');
  const actions = element('div', 'bug-report-actions');
  const cancel = element('button', 'button ghost', 'Cancel') as HTMLButtonElement;
  cancel.type = 'button';
  cancel.addEventListener('click', () => dialog.close());
  const submit = element('button', 'button primary', 'Submit report') as HTMLButtonElement;
  submit.type = 'submit';
  actions.append(cancel, submit);
  const result = element('div', 'bug-report-result');
  result.setAttribute('aria-live', 'polite');
  form.append(header, endpointField, endpointHint, titleField, ...controls.map(item => item.wrapper), severityField, diagnostics, actions, result);
  dialog.append(form);
  document.body.append(dialog);

  form.addEventListener('submit', async (event) => {
    event.preventDefault();
    if (submit.disabled) return;
    const target = safeEndpoint(endpoint.value);
    if (!target) {
      endpoint.setCustomValidity('Enter a valid HTTPS bug-report endpoint.');
      endpoint.reportValidity();
      return;
    }
    endpoint.setCustomValidity('');
    localStorage.setItem(STORAGE_KEY, target);
    submit.disabled = true;
    submit.textContent = 'Submitting…';
    result.textContent = '';
    try {
      const version = await getVersion().catch(() => 'unknown');
      const response = await fetch(target, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
        body: JSON.stringify({
          title: title.value.trim(),
          description: controls[0].input.value.trim(),
          expected: controls[1].input.value.trim(),
          actual: controls[2].input.value.trim(),
          steps: controls[3].input.value.trim(),
          severity: severity.value,
          version,
          os: navigator.userAgent.slice(0, 120),
        }),
      });
      const data = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(data.error || 'Submission failed (' + response.status + ')');
      if (typeof data.url !== 'string' || !/^https:\/\/github\.com\/[\w.-]+\/[\w.-]+\/issues\/\d+$/.test(data.url)) {
        throw new Error('The server returned an invalid issue link.');
      }
      result.replaceChildren(element('strong', 'bug-report-success', 'Report submitted — Issue #' + data.number));
      const urlText = element('p', 'bug-report-url', data.url);
      const copy = element('button', 'button compact ghost', 'Copy issue link') as HTMLButtonElement;
      copy.type = 'button';
      copy.addEventListener('click', async () => {
        await navigator.clipboard.writeText(data.url);
        setStatus('Issue link copied.');
      });
      result.append(urlText, copy);
      setStatus('Bug report submitted as issue #' + data.number);
    } catch (error) {
      result.textContent = error instanceof Error ? error.message : 'Bug report failed';
      setStatus('Bug report failed: ' + result.textContent);
    } finally {
      submit.disabled = false;
      submit.textContent = 'Submit report';
    }
  });
  return { open: () => { form.reset(); endpoint.value = localStorage.getItem(STORAGE_KEY) || ''; severity.value = 'medium'; result.replaceChildren(); dialog.showModal(); title.focus(); } };
}
