import { positiveId, safeBranch } from './security.mjs';

const bad = (message) => Object.assign(new Error(message), { status: 409 });
const validSha = (value) => typeof value === 'string' && /^[a-f0-9]{40}$/i.test(value);

/**
 * GitHub check runs and legacy commit statuses for exactly the PR head SHA.
 * A missing, pending, failing or incomplete result can never pass the gate.
 * Branch protections and required-review settings remain GitHub's authority.
 */
export async function inspectDraftChecks({ broker, root, repoId, number, readToken }) {
  const n = positiveId(number, 'pull request');
  const pr = await broker.request(root + '/pulls/' + n, readToken);
  const sha = pr?.head?.sha;
  const valid = pr?.state === 'open' && pr?.draft === true
    && Number(pr?.head?.repo?.id) === repoId
    && validSha(sha)
    && typeof pr?.head?.ref === 'string';
  if (!valid) return { ready: false, sha: validSha(sha) ? sha : null, reason: 'PR must be an open draft from this repository' };
  try {
    safeBranch(pr.head.ref);
  } catch {
    return { ready: false, sha, reason: 'Only ryoko/ branches can be promoted' };
  }
  const encoded = encodeURIComponent(sha);
  const [checks, legacy] = await Promise.all([
    broker.request(root + '/commits/' + encoded + '/check-runs?per_page=100', readToken),
    broker.request(root + '/commits/' + encoded + '/status?per_page=100', readToken),
  ]);
  if (!Array.isArray(checks?.check_runs) || !Array.isArray(legacy?.statuses)
    || Number(checks?.total_count) !== checks.check_runs.length
    || checks.check_runs.length > 100 || legacy.statuses.length >= 100) {
    return { ready: false, sha, reason: 'CI results are missing, incomplete or exceed the supported page size' };
  }
  const latest = new Map();
  for (const status of legacy.statuses) {
    if (typeof status.context !== 'string' || !status.context) {
      return { ready: false, sha, reason: 'CI status is malformed' };
    }
    // GitHub returns legacy statuses newest-first; keep only the latest per context.
    if (!latest.has(status.context)) latest.set(status.context, status.state);
  }
  const runs = checks.check_runs;
  const statuses = [...latest.values()];
  const passingRuns = runs.every((run) => run.status === 'completed'
    && ['success', 'neutral', 'skipped'].includes(run.conclusion));
  const passingStatuses = statuses.every((state) => state === 'success');
  const anySuccessful = runs.some((run) => run.status === 'completed' && run.conclusion === 'success')
    || statuses.some((state) => state === 'success');
  return {
    ready: (runs.length > 0 || statuses.length > 0) && passingRuns && passingStatuses && anySuccessful,
    sha,
    checks: runs.length,
    statuses: statuses.length,
    reason: !anySuccessful ? 'No successful CI check recorded on this commit'
      : !passingRuns || !passingStatuses ? 'CI is pending, failing or cancelled'
        : 'All visible CI checks are successful',
  };
}

/** Verify the PR HEAD has not changed between CI inspection and mutation. */
export async function assertReadyForReview({ broker, root, repoId, number, readToken }) {
  const status = await inspectDraftChecks({ broker, root, repoId, number, readToken });
  if (!status.ready) throw bad(status.reason);
  const latest = await broker.request(root + '/pulls/' + positiveId(number, 'pull request'), readToken);
  if (latest?.head?.sha !== status.sha || latest?.draft !== true || latest?.state !== 'open'
    || Number(latest?.head?.repo?.id) !== repoId) {
    throw bad('pull request changed while verifying CI; refresh and try again');
  }
  return latest;
}
