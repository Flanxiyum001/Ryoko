import { normalizeIssueRequest } from './pipeline.mjs';

const COMMAND = /^\/ryoko (run|retry|status)$/i;

/**
 * A command is actionable only if GitHub signed the delivery and the sender is
 * an explicitly configured operator. The webhook signature is verified by the
 * HTTP layer before this parser is invoked.
 */
export function issueBotCommand(event, payload, allowedActors = []) {
  if (event !== 'issue_comment' || payload?.action !== 'created'
    || payload.issue?.pull_request || payload.comment?.user?.type === 'Bot') return null;
  const body = payload.comment?.body;
  if (typeof body !== 'string' || body.length > 80) return null;
  const command = COMMAND.exec(body.trim())?.[1]?.toLowerCase();
  if (!command) return null;
  const actor = payload.sender?.login;
  if (typeof actor !== 'string' || !actor || typeof payload.comment?.user?.login !== 'string'
    || actor.toLowerCase() !== payload.comment.user.login.toLowerCase()
    || !allowedActors.some((login) => typeof login === 'string' && login.toLowerCase() === actor.toLowerCase())) {
    return null;
  }
  const request = normalizeIssueRequest(payload);
  if (!request || (command !== 'status' && payload.issue?.state === 'closed')) return null;
  return { ...request, command, actor };
}

/** A dedicated issues-write token cannot push code, create PRs or merge. */
export async function postBotComment(broker, request, message) {
  if (typeof message !== 'string' || !message || message.length > 2000) {
    throw new Error('invalid bot comment');
  }
  const token = await broker.installationToken(request.installationId, request.repoId, 'issues');
  const root = '/repos/' + encodeURIComponent(request.owner) + '/' + encodeURIComponent(request.name);
  return broker.request(root + '/issues/' + request.number + '/comments', token, {
    method: 'POST', body: { body: message },
  });
}

export function botStatus(request, job) {
  const issue = '#' + request.number;
  if (!job) {
    return `Ryoko has no recorded job for issue ${issue} in this broker session. To request work, an authorized maintainer can add the \`ryoko-fix\` label or comment \`/ryoko run\`.`;
  }
  if (job.status === 'running') {
    return `Ryoko is working on issue ${issue}. A draft pull request will be created only if the engineering run succeeds. No changes are merged automatically.`;
  }
  if (job.status === 'completed' && Number.isSafeInteger(job.number) && job.number > 0) {
    const link = `https://github.com/${request.fullName}/pull/${job.number}`;
    return `Ryoko completed issue ${issue} and opened [draft PR #${job.number}](${link}). Review the code, validation and CI checks before manually merging.`;
  }
  if (job.status === 'failed') {
    return `Ryoko could not finish issue ${issue}. No merge was performed. Check the broker logs and any partially pushed branch before an authorized maintainer sends \`/ryoko retry\`.`;
  }
  return `Ryoko has no completed result for issue ${issue}.`;
}
