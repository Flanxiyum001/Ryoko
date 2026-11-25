/** Read-only, reference-matched GitHub PR detail. No merge permission is implied. */
export interface PullDetail {
  author: string;
  agent: boolean;
  created_at: string;
  head: string;
  base: string;
  changed_files: number | null;
  additions: number | null;
  deletions: number | null;
  mergeable: boolean | null;
  mergeable_state: string;
  merged: boolean;
  files: { name: string; additions: number; deletions: number }[] | null;
  checks: { name: string; status: string; conclusion: string | null }[] | null;
  checks_complete: boolean;
  reviews: { name: string; state: string }[] | null;
}
interface Pull {
  title: string;
  number: number;
  state?: string;
  draft?: boolean;
  detail?: PullDetail | null;
}
const paths = {
  pull: '<circle cx="6" cy="4" r="2"/><circle cx="18" cy="20" r="2"/><path d="M6 6v14M18 18V9a5 5 0 0 0-5-5h-2"/>',
  branch: '<circle cx="6" cy="19" r="2"/><circle cx="18" cy="5" r="2"/><path d="M6 17V5m0 9c0-5 5-9 10-9"/>',
  file: '<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8zM14 2v6h6M10 12l-3 3 3 3m4-6 3 3-3 3"/>',
  good: '<circle cx="12" cy="12" r="10"/><path d="m8 12 3 3 5-6"/>',
  unknown: '<circle cx="12" cy="12" r="10"/><path d="M12 7v5m0 4h.01"/>',
  bad: '<circle cx="12" cy="12" r="10"/><path d="m8 8 8 8m0-8-8 8"/>',
  copy: '<rect x="8" y="8" width="13" height="13" rx="3"/><path d="M16 8V5a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h3"/>',
  external: '<path d="M13 4h7v7m0-7L10 14M9 5H5a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-4"/>',
  chevron: '<path d="m5 9 7 7 7-7"/>',
};
function node(tag: string, className = '', value = '') {
  const el = document.createElement(tag);
  el.className = className;
  el.textContent = value;
  return el;
}
function icon(name: keyof typeof paths) {
  const el = node('span', 'github-pr-icon github-pr-icon-' + name);
  el.setAttribute('aria-hidden', 'true');
  // Only static, locally-authored SVG paths; GitHub strings always use textContent.
  el.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.65" stroke-linecap="round" stroke-linejoin="round">' + paths[name] + '</svg>';
  return el;
}
export function mergePresentation(pull: Pull): { label: string; note: string; tone: 'good' | 'unknown' | 'bad' } {
  const d = pull.detail;
  if (d?.merged) return { label: 'Merged', note: '', tone: 'good' };
  if (pull.state === 'closed') return { label: 'Closed', note: '', tone: 'unknown' };
  if (pull.draft) return { label: 'Draft', note: 'Mark ready for review on GitHub', tone: 'unknown' };
  if (d?.mergeable === false) return { label: 'Merge blocked', note: 'GitHub reports conflicts', tone: 'bad' };
  if (d?.mergeable === true && d.mergeable_state === 'clean') return { label: 'Ready to merge', note: 'GitHub reports clean merge state', tone: 'good' };
  return { label: 'Merge status unconfirmed', note: 'Verify on GitHub', tone: 'unknown' };
}
export function checkPresentation(d?: PullDetail | null): { label: string; note: string; tone: 'good' | 'unknown' | 'bad' } {
  if (!d?.checks?.length) return { label: 'Checks unavailable', note: 'Required checks unverified', tone: 'unknown' };
  const passing = d.checks.filter(check => check.status === 'completed' && check.conclusion === 'success').length;
  const pending = d.checks.filter(check => check.status !== 'completed').length;
  const failed = d.checks.filter(check => check.status === 'completed' && ['failure', 'timed_out', 'action_required', 'cancelled', 'startup_failure', 'stale'].includes(check.conclusion || '')).length;
  const other = d.checks.length - passing - pending - failed;
  const note = passing + ' passing, ' + pending + ' pending' + (failed ? ', ' + failed + ' unsuccessful' : '') + (other ? ', ' + other + ' other' : '') + (d.checks_complete ? '' : ' · partial list');
  return { label: failed ? 'Checks need attention' : passing === d.checks.length && d.checks_complete ? 'Reported checks passed' : 'Checks pending or unverified', note, tone: failed ? 'bad' : passing === d.checks.length && d.checks_complete ? 'good' : 'unknown' };
}
function reviewPresentation(d?: PullDetail | null): { label: string; note: string; tone: 'good' | 'unknown' | 'bad' } {
  if (!d?.reviews) return { label: 'Review status unavailable', note: '', tone: 'unknown' };
  const changes = d.reviews.filter(r => r.state === 'CHANGES_REQUESTED');
  if (changes.length) return { label: 'Changes requested', note: changes.map(r => r.name).join(', '), tone: 'bad' };
  const approved = d.reviews.filter(r => r.state === 'APPROVED');
  if (approved.length) return { label: 'Approved', note: 'by ' + approved.map(r => r.name).join(', '), tone: 'good' };
  return { label: 'No approval recorded', note: '', tone: 'unknown' };
}
function age(value: string) {
  const time = Date.parse(value);
  if (!Number.isFinite(time)) return '';
  const minutes = Math.max(0, Math.floor((Date.now() - time) / 60_000));
  return minutes < 60 ? 'opened ' + minutes + ' min ago' : minutes < 1440 ? 'opened ' + Math.floor(minutes / 60) + ' hr ago' : 'opened ' + Math.floor(minutes / 1440) + ' d ago';
}
function statusRow(label: string, note: string, tone: keyof typeof paths) {
  const row = node('div', 'github-pr-row');
  const main = node('div', 'github-pr-row-main');
  main.append(icon(tone), node('strong', '', label), node('span', 'github-pr-muted', note));
  row.append(main);
  return row;
}
function expandable(label: string, note: string, tone: keyof typeof paths, entries: HTMLElement[], extra?: HTMLElement) {
  const details = node('details', 'github-pr-expand');
  const summary = node('summary', 'github-pr-row');
  const main = node('span', 'github-pr-row-main');
  main.append(icon(tone), node('strong', '', label), node('span', 'github-pr-muted', note));
  summary.append(main);
  if (extra) summary.append(extra);
  summary.append(icon('chevron'));
  const body = node('div', 'github-pr-expanded-body');
  body.append(...entries);
  details.append(summary, body);
  return details;
}
export function renderPullCard(pull: Pull, repository: string, url: string | null, notice = ''): HTMLElement {
  const d = pull.detail;
  const card = node('article', 'github-pr-card');
  const header = node('header', 'github-pr-header');
  const heading = node('div', 'github-pr-heading');
  heading.append(icon('pull'));
  const title = node('h2', '', pull.title);
  title.append(node('span', 'github-pr-muted', ' #' + pull.number));
  heading.append(title);
  if (url) {
    const copy = document.createElement('button');
    copy.className = 'github-pr-copy';
    copy.type = 'button';
    copy.setAttribute('aria-label', 'Copy pull request link');
    copy.title = 'Copy pull request link';
    copy.append(icon('copy'));
    copy.addEventListener('click', async () => {
      try { await navigator.clipboard.writeText(url); copy.title = 'Copied'; copy.setAttribute('aria-label', 'Link copied'); }
      catch { copy.title = 'Copy unavailable; open pull request instead'; copy.setAttribute('aria-label', copy.title); }
    });
    heading.append(copy);
  }
  const meta = node('div', 'github-pr-meta');
  meta.append(node('span', !pull.draft && pull.state !== 'closed' ? 'github-pr-positive' : '', d?.merged ? 'Merged' : pull.state === 'closed' ? 'Closed' : pull.draft ? 'Draft' : 'Open'), node('span', '', '·'), node('span', '', repository));
  if (d?.author) meta.append(node('span', '', '·'), node('span', '', d.author));
  if (d?.agent) meta.append(node('span', 'github-pr-agent', 'Agent'));
  if (d?.created_at) meta.append(node('span', '', '·'), node('span', '', age(d.created_at)));
  header.append(heading, meta);
  if (d?.head && d.base) {
    const branch = node('div', 'github-pr-branch');
    branch.append(icon('branch'), node('span', '', d.head + ' → ' + d.base));
    header.append(branch);
  }
  card.append(header);
  if (notice) card.append(node('p', 'github-pr-notice', notice));
  const fileEntries = (d?.files || []).map(file => {
    const row = node('div', 'github-pr-entry');
    row.append(node('span', '', file.name), node('span', 'github-pr-positive', '+' + file.additions), node('span', 'github-pr-negative', '−' + file.deletions));
    return row;
  });
  if (!d?.files) fileEntries.push(node('p', 'github-pr-muted', 'File details unavailable.'));
  else if (d.changed_files !== null && d.changed_files > d.files.length) fileEntries.push(node('p', 'github-pr-muted', 'Showing first ' + d.files.length + ' files; open GitHub for the full diff.'));
  else if (!fileEntries.length) fileEntries.push(node('p', 'github-pr-muted', 'No changed files.'));
  const diff = node('span', 'github-pr-diff');
  if (d && d.additions !== null && d.deletions !== null) {
    diff.append(node('span', 'github-pr-positive', '+' + d.additions), node('span', 'github-pr-negative', '−' + d.deletions));
    const blocks = node('span', 'github-pr-diff-blocks');
    blocks.setAttribute('aria-hidden', 'true');
    const total = d.additions + d.deletions;
    const green = total ? Math.round(5 * d.additions / total) : 0;
    for (let i = 0; i < 5 && total > 0; i++) blocks.append(node('i', i < green ? 'addition' : 'deletion'));
    diff.append(blocks);
  }
  card.append(expandable(d?.changed_files != null ? d.changed_files + (d.changed_files === 1 ? ' file changed' : ' files changed') : 'Files changed unavailable', '', 'file', fileEntries, diff));
  const checks = checkPresentation(d);
  const checkEntries = (d?.checks || []).map(check => {
    const row = node('div', 'github-pr-entry');
    row.append(node('span', '', check.name), node('span', 'github-pr-muted', check.conclusion || check.status));
    return row;
  });
  checkEntries.push(node('p', 'github-pr-muted', 'Required checks and legacy commit statuses must be verified on GitHub.'));
  card.append(expandable(checks.label, checks.note, checks.tone, checkEntries));
  const review = reviewPresentation(d);
  card.append(statusRow(review.label, review.note, review.tone));
  const merge = mergePresentation(pull);
  card.append(statusRow(merge.label, merge.note, merge.tone));
  const footer = node('footer', 'github-pr-footer');
  if (url) {
    const link = document.createElement('a');
    link.href = url;
    link.target = '_blank';
    link.rel = 'noopener noreferrer';
    link.append(icon('external'), node('span', '', 'Open pull request'));
    footer.append(link);
  }
  const mergeButton = document.createElement('button');
  mergeButton.type = 'button';
  mergeButton.className = 'github-pr-merge';
  mergeButton.disabled = true;
  mergeButton.title = 'Desktop pairing is read-only. Open the pull request to merge on GitHub.';
  mergeButton.append(icon('branch'), node('span', '', 'Squash and merge'));
  footer.append(mergeButton);
  card.append(footer, node('p', 'github-pr-readonly', 'Read-only connection · Merge on GitHub'));
  return card;
}
