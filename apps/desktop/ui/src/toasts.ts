export function isToastWarning(text: string): boolean {
  const isPath = /^(?:[A-Za-z]:[\\/]|[\w./\\:-]+(?:\.[\w-]+)+$)/.test(text);
  return !isPath && /(?:failed|\berror\b|invalid|cannot |could not|refus|missing|required|denied)/i.test(text);
}

/** Accessible, non-blocking app notifications. Transient events do not overwrite
 * the permanent language/cursor and LSP status indicators. */
export function createNotifier(doc: Document = document): (message: string) => void {
  const host = doc.createElement('div');
  host.className = 'toast-stack';
  host.setAttribute('role', 'region');
  host.setAttribute('aria-label', 'Ryoko notifications');
  doc.body.append(host);
  let lastMessage = '';
  let lastTime = 0;

  return (message: string): void => {
    const text = message.trim();
    if (!text) return;
    const now = Date.now();
    if (text === lastMessage && now - lastTime < 1800) return;
    lastMessage = text;
    lastTime = now;
    const warning = isToastWarning(text);
    const toast = doc.createElement('div');
    toast.className = 'toast-item' + (warning ? ' toast-error' : '');
    toast.setAttribute('role', warning ? 'alert' : 'status');
    const body = doc.createElement('span');
    body.textContent = text;
    const close = doc.createElement('button');
    close.type = 'button';
    close.className = 'toast-close';
    close.setAttribute('aria-label', 'Dismiss notification');
    close.textContent = '×';
    close.addEventListener('click', () => toast.remove());
    toast.append(body, close);
    host.prepend(toast);
    while (host.childElementCount > 4) host.lastElementChild?.remove();
    const timer = window.setTimeout(() => toast.remove(), warning ? 9000 : 4800);
    close.addEventListener('click', () => window.clearTimeout(timer), { once: true });
  };
}
