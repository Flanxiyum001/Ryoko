import { describe, expect, it } from 'vitest';
import {
  changeCounts,
  changeStatusLabel,
  sessionAge,
  sessionMeta,
  sessionTitle,
  type AgentSession,
} from './sessions';

const session: AgentSession = {
  id: 'task-1',
  agent: 'backend',
  state: 'published',
  branch: 'ryoko/backend/task-1',
  baseBranch: 'main',
  files: ['src/lib.rs', 'src/main.rs'],
  summary: 'Add a bounded parser',
  updatedMs: Date.UTC(2026, 10, 16, 12, 0, 0),
};

describe('agent session formatting', () => {
  it('formats recent and older timestamps without claiming precision it lacks', () => {
    expect(sessionAge(session.updatedMs, session.updatedMs + 30_000)).toBe('just now');
    expect(sessionAge(session.updatedMs, session.updatedMs + 5 * 60_000)).toBe('5m ago');
    expect(sessionAge(session.updatedMs, session.updatedMs + 3 * 3_600_000)).toBe('3h ago');
    expect(sessionAge(session.updatedMs, session.updatedMs + 2 * 86_400_000)).toBe('2d ago');
    expect(sessionAge(0)).toBe('unknown');
  });

  it('summarises the agent and file count', () => {
    expect(sessionMeta(session)).toContain('backend · 2 files');
    expect(sessionMeta({ ...session, files: ['only.rs'] })).toContain('1 file');
  });

  it('falls back to the task id when no summary was recorded', () => {
    expect(sessionTitle(session)).toBe('Add a bounded parser');
    expect(sessionTitle({ ...session, summary: null })).toBe('task-1');
    expect(sessionTitle({ ...session, summary: '   ' })).toBe('task-1');
  });
});

describe('session change formatting', () => {
  it('labels statuses and counts', () => {
    expect(changeStatusLabel('added')).toBe('added');
    expect(changeStatusLabel('binary')).toBe('binary');
    expect(changeCounts({ path: 'a.rs', status: 'modified', additions: 3, deletions: 2 }))
      .toBe('+3 −2');
    expect(changeCounts({ path: 'a.png', status: 'binary', additions: 0, deletions: 0 }))
      .toBe('binary');
  });
});
