// Ryoko Review findings, shared by agent runs, swarm tasks and the Changes panel.

export type Severity = 'blocker' | 'warning' | 'suggestion';
export interface ReviewFinding {
  path: string; line: number | null; severity: Severity; title: string; detail: string;
}
export interface ReviewReport {
  verdict: 'approve' | 'needs_changes'; summary: string; findings: ReviewFinding[];
}

const SEVERITY_LABEL: Record<Severity, string> = {
  blocker: 'Must fix',
  warning: 'Should fix',
  suggestion: 'Consider',
};

/** One-line headline for a report, exported for tests. */
export function reviewHeadline(report: ReviewReport): string {
  if (report.findings.length === 0) return 'Looks good · no issues found';
  const blockers = report.findings.filter((finding) => finding.severity === 'blocker').length;
  const count = `${report.findings.length} finding${report.findings.length === 1 ? '' : 's'}`;
  return blockers > 0 ? `Needs changes · ${count}, ${blockers} must fix` : `Mostly fine · ${count}`;
}

/** Renders a report. `open` is called when a finding is clicked. */
export function renderReview(
  report: ReviewReport,
  open: (finding: ReviewFinding) => void,
): HTMLElement {
  const root = document.createElement('section');
  root.className = 'ryoko-review ryoko-review-' + report.verdict;
  const head = document.createElement('strong');
  head.textContent = reviewHeadline(report);
  root.append(head);
  if (report.summary) {
    const summary = document.createElement('p');
    summary.className = 'ryoko-review-summary';
    summary.textContent = report.summary;
    root.append(summary);
  }
  const list = document.createElement('ul');
  list.className = 'ryoko-review-list';
  for (const finding of report.findings) {
    const item = document.createElement('li');
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'ryoko-finding ryoko-finding-' + finding.severity;
    const badge = document.createElement('span');
    badge.className = 'ryoko-finding-badge';
    badge.textContent = SEVERITY_LABEL[finding.severity];
    const title = document.createElement('span');
    title.className = 'ryoko-finding-title';
    title.textContent = finding.title;
    const where = document.createElement('span');
    where.className = 'ryoko-finding-where';
    where.textContent = finding.line ? `${finding.path}:${finding.line}` : finding.path;
    button.append(badge, title, where);
    if (finding.detail) button.title = finding.detail;
    button.addEventListener('click', () => open(finding));
    item.append(button);
    if (finding.detail) {
      const detail = document.createElement('p');
      detail.className = 'ryoko-finding-detail';
      detail.textContent = finding.detail;
      item.append(detail);
    }
    list.append(item);
  }
  if (report.findings.length > 0) root.append(list);
  return root;
}
