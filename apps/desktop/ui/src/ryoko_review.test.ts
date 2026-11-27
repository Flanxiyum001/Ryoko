import { describe, expect, it } from 'vitest';
import { reviewHeadline } from './ryoko_review';
import { taskStatusText } from './swarm_view';

describe('Ryoko Review and swarm labels', () => {
  it('summarises a review in one line', () => {
    expect(reviewHeadline({ verdict: 'approve', summary: '', findings: [] })).toBe('Looks good · no issues found');
    expect(reviewHeadline({
      verdict: 'needs_changes',
      summary: '',
      findings: [
        { path: 'a', line: 1, severity: 'blocker', title: 'x', detail: '' },
        { path: 'a', line: 2, severity: 'warning', title: 'y', detail: '' },
      ],
    })).toBe('Needs changes · 2 findings, 1 must fix');
  });

  it('describes swarm task states', () => {
    expect(taskStatusText({ state: 'ready' })).toBe('Ready for a pull request');
    expect(taskStatusText({ state: 'failed', detail: 'boom' })).toBe('Failed: boom');
  });
});
