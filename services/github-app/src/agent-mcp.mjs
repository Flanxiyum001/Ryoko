import { positiveId, validateProposal } from './security.mjs';

const TOOL = 'queue_github_proposal';
const OPERATIONS = ['create_branch', 'commit_file', 'open_pull_request', 'review_comment'];
const MAX_FRAME = 64 * 1024;

export const GITHUB_AGENT_TOOL = Object.freeze({
  name: TOOL,
  description: 'Queue a GitHub repository change for human approval in Ryoko Mission Control. This does NOT apply, push, merge or execute the change. The host has already selected the repository; you cannot choose a different one.',
  inputSchema: {
    type: 'object',
    properties: {
      operation: { type: 'string', enum: OPERATIONS },
      params: {
        type: 'object',
        description: 'Operation arguments. create_branch: {branch}; commit_file: {branch,path,content,message,expectedSha?}; open_pull_request: {head,title,body}; review_comment: {number,comment}. Branches must start with ryoko/.',
      },
    },
    required: ['operation', 'params'],
    additionalProperties: false,
  },
});

function brokerEndpoint(value) {
  const url = new URL(value || 'http://127.0.0.1:4173');
  if (!['https:', 'http:'].includes(url.protocol)
    || (url.protocol === 'http:' && !['localhost', '127.0.0.1', '[::1]'].includes(url.hostname))
    || url.username || url.password || url.search || url.hash
    || url.pathname !== '/' || url.port === '0') {
    throw new Error('GitHub broker requires an HTTPS URL or an exact loopback origin');
  }
  return new URL('/api/agent/proposals', url).toString();
}

function toolError(message) {
  return { content: [{ type: 'text', text: message }], isError: true };
}

function reply(id, result) {
  return { jsonrpc: '2.0', id, result };
}
function fault(id, code, message) {
  return { jsonrpc: '2.0', id, error: { code, message } };
}

/** Host-owned MCP tool. Its installation/repository IDs never come from the model. */
export function createAgentMcpHandler({
  env = process.env,
  fetchImpl = fetch,
} = {}) {
  const install = positiveId(env.RYOKO_GITHUB_INSTALLATION_ID, 'installation');
  const repository = positiveId(env.RYOKO_GITHUB_REPO_ID, 'repository');
  const token = env.RYOKO_AGENT_BROKER_TOKEN || '';
  if (typeof token !== 'string' || token.length < 32 || token.length > 512) {
    throw new Error('missing trusted GitHub agent broker credential');
  }
  const endpoint = brokerEndpoint(env.RYOKO_GITHUB_BROKER_URL);

  return async (message) => {
    if (!message || typeof message !== 'object' || Array.isArray(message)
      || message.jsonrpc !== '2.0') {
      return fault(null, -32600, 'invalid JSON-RPC request');
    }
    if (!Object.hasOwn(message, 'id')) return null; // notifications are never approvals
    const id = message.id;
    if (!(typeof id === 'number' && Number.isSafeInteger(id))
      && !(typeof id === 'string' && id.length <= 80)) {
      return fault(null, -32600, 'invalid JSON-RPC id');
    }
    switch (message.method) {
      case 'initialize':
        return reply(id, {
          protocolVersion: '2025-06-18',
          capabilities: { tools: { listChanged: false } },
          serverInfo: { name: 'ryoko-github-approvals', version: '1.0.0' },
        });
      case 'tools/list':
        return reply(id, { tools: [GITHUB_AGENT_TOOL] });
      case 'tools/call': {
        if (message.params?.name !== TOOL || !message.params.arguments
          || typeof message.params.arguments !== 'object'
          || Array.isArray(message.params.arguments)) {
          return reply(id, toolError('Unknown tool or invalid tool arguments'));
        }
        const args = message.params.arguments;
        if (Object.keys(args).some((name) => !['operation', 'params'].includes(name))
          || !OPERATIONS.includes(args.operation)) {
          return reply(id, toolError('Operation not permitted for this agent'));
        }
        let proposal;
        try {
          proposal = validateProposal({
            installationId: install, repoId: repository,
            operation: args.operation, params: args.params,
          });
        } catch {
          return reply(id, toolError('Proposal rejected: invalid branch, path or required operation fields'));
        }
        if (Buffer.byteLength(JSON.stringify(proposal)) >= MAX_FRAME) {
          return reply(id, toolError('Proposal exceeds MCP request size limit'));
        }
        try {
          const response = await fetchImpl(endpoint, {
            method: 'POST',
            redirect: 'error',
            signal: AbortSignal.timeout(15_000),
            headers: {
              Authorization: 'Bearer ' + token,
              Accept: 'application/json',
              'Content-Type': 'application/json',
            },
            body: JSON.stringify(proposal),
          });
          if (!response.ok) return reply(id, toolError('Broker rejected the proposal; no GitHub write was executed'));
          const result = await response.json();
          if (!/^[0-9a-f-]{36}$/.test(String(result?.id)) || result.status !== 'pending') {
            return reply(id, toolError('Broker did not acknowledge a pending approval'));
          }
          return reply(id, {
            content: [{ type: 'text', text: JSON.stringify({
              proposalId: result.id, status: 'pending_human_approval',
              note: 'Queued only. A human must independently review and approve in Ryoko.',
            }) }],
          });
        } catch {
          return reply(id, toolError('GitHub broker unavailable; execution not confirmed'));
        }
      }
      default:
        return fault(id, -32601, 'method not found');
    }
  };
}
