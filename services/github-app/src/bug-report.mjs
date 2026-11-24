const bad = (message, status = 400) => Object.assign(new Error(message), { status });

const MAX_TITLE = 120;
const MAX_TEXT = 8_000;
const MAX_REPORTS_PER_WINDOW = 5;
const WINDOW_MS = 60 * 60 * 1000;

function cleanText(value, field, limit = MAX_TEXT, required = false) {
  if (typeof value !== 'string') {
    if (required) throw bad('invalid ' + field);
    return '';
  }
  const text = value.replace(/\0/g, '').trim();
  if (required && !text) throw bad(field + ' is required');
  if (text.length > limit) throw bad(field + ' is too long');
  return text;
}

function safeIp(req) {
  // Do not trust forwarded headers unless the service is deployed behind a
  // separately configured trusted proxy. Socket address is the safe default.
  return String(req.socket?.remoteAddress || 'unknown').slice(0, 80);
}

export function createBugReportHandler({ token = '', repository = 'Flanxiyum001/Ryoko', fetchImpl = fetch } = {}) {
  const buckets = new Map();

  return async function submitBugReport(req, body) {
    if (!token || !/^[-\w.]+\/[-\w.]+$/.test(repository)) throw bad('bug reporting is not configured', 503);
    const now = Date.now();
    const ip = safeIp(req);
    let bucket = buckets.get(ip);
    if (!bucket || now - bucket.started >= WINDOW_MS) bucket = { started: now, count: 0 };
    if (bucket.count >= MAX_REPORTS_PER_WINDOW) throw bad('report limit reached; try again later', 429);
    bucket.count += 1;
    buckets.set(ip, bucket);
    if (buckets.size > 10_000) {
      for (const [key, value] of buckets) if (now - value.started >= WINDOW_MS) buckets.delete(key);
    }

    const title = cleanText(body?.title, 'title', MAX_TITLE, true);
    const description = cleanText(body?.description, 'description', MAX_TEXT, true);
    const expected = cleanText(body?.expected, 'expected behavior');
    const actual = cleanText(body?.actual, 'actual behavior');
    const steps = cleanText(body?.steps, 'reproduction steps');
    const severity = ['low', 'medium', 'high', 'critical'].includes(body?.severity) ? body.severity : 'medium';
    const version = cleanText(body?.version, 'version', 80);
    const os = cleanText(body?.os, 'operating system', 120);
    const diagnostics = cleanText(body?.diagnostics, 'diagnostics', 2_000);

    const sections = [
      '## Description\n' + description,
      expected ? '## Expected behavior\n' + expected : '',
      actual ? '## Actual behavior\n' + actual : '',
      steps ? '## Steps to reproduce\n' + steps : '',
      '## Severity\n' + severity,
      '## Environment\n- Ryoko version: ' + (version || 'Unknown') + '\n- OS: ' + (os || 'Unknown'),
      diagnostics ? '## Diagnostics (user-provided)\n\n```text\n' + diagnostics + '\n```' : '',
      '\n---\nSubmitted through Ryoko’s in-app bug reporter.',
    ].filter(Boolean);
    const response = await fetchImpl('https://api.github.com/repos/' + repository + '/issues', {
      method: 'POST',
      redirect: 'error',
      signal: AbortSignal.timeout(15_000),
      headers: {
        Accept: 'application/vnd.github+json',
        Authorization: 'Bearer ' + token,
        'Content-Type': 'application/json',
        'User-Agent': 'Ryoko-Bug-Reporter',
        'X-GitHub-Api-Version': '2022-11-28',
      },
      body: JSON.stringify({ title, body: sections.join('\n\n'), labels: ['bug'] }),
    });
    let data = {};
    try { data = await response.json(); } catch {}
    if (!response.ok || typeof data.html_url !== 'string' || !Number.isInteger(data.number)) {
      const error = bad('GitHub issue creation failed (' + response.status + ')', response.status === 403 ? 403 : 502);
      throw error;
    }
    return { number: data.number, url: data.html_url, title: data.title || title };
  };
}
