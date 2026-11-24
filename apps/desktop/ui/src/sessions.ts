/** Pure formatting for the read-only Agent Sessions panel. */

export type AgentSessionState = 'active' | 'published' | 'merged' | 'abandoned';

export interface AgentSession {
  id: string;
  agent: string;
  state: AgentSessionState;
  branch: string;
  baseBranch: string;
  files: string[];
  summary: string | null;
  updatedMs: number;
}

export function sessionAge(updatedMs: number, nowMs = Date.now()): string {
  if (!Number.isFinite(updatedMs) || updatedMs <= 0) return 'unknown';
  const seconds = Math.max(0, Math.floor((nowMs - updatedMs) / 1000));
  if (seconds < 60) return 'just now';
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}

export function sessionMeta(session: AgentSession): string {
  const files = session.files.length === 1 ? '1 file' : `${session.files.length} files`;
  return `${session.agent} · ${files} · ${sessionAge(session.updatedMs)}`;
}

export function sessionTitle(session: AgentSession): string {
  const summary = session.summary?.trim();
  if (!summary) return session.id;
  return summary.length > 140 ? `${summary.slice(0, 140)}…` : summary;
}

export type SessionChangeStatus = 'added' | 'modified' | 'deleted' | 'binary';

export interface SessionChange {
  path: string;
  status: SessionChangeStatus;
  additions: number;
  deletions: number;
}

export interface SessionFileDiff {
  path: string;
  status: SessionChangeStatus;
  original: string;
  modified: string;
}

export interface SessionActionResult {
  action: string;
  message: string;
  merge_commit?: string;
  passed?: boolean;
  summary?: string;
}

export function changeStatusLabel(status: SessionChangeStatus): string {
  switch (status) {
    case 'added': return 'added';
    case 'deleted': return 'deleted';
    case 'binary': return 'binary';
    case 'modified': return 'modified';
  }
}

export function changeCounts(change: SessionChange): string {
  if (change.status === 'binary') return 'binary';
  return `+${change.additions} −${change.deletions}`;
}
