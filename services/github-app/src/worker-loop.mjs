import { randomUUID } from 'node:crypto';
import { runIssuePipeline } from './pipeline.mjs';
import { postBotComment, botStatus } from './bot.mjs';

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * A separate worker process claims jobs atomically across a shared SQLite WAL
 * database. A job that loses its lease is marked interrupted; it is NEVER
 * automatically replayed, because a push or PR creation may have succeeded.
 */
export function startGithubWorker({
  store, broker, settings, runPipeline = runIssuePipeline,
  logger = console, pollMs = 500, leaseMs = 120_000,
} = {}) {
  if (!store || !broker || !settings) throw new Error('worker requires store and broker');
  const workerId = randomUUID();
  let active = true;
  let current = null;
  let nextMaintenance = 0;

  async function notify(request, message) {
    if (request.kind === 'goal') return;
    try { await postBotComment(broker, request, message); }
    catch { logger.warn?.('Could not post GitHub issue worker update'); }
  }

  async function work(job) {
    const started = Date.now();
    const heartbeat = setInterval(() => {
      try {
        if (!store.heartbeat(job, leaseMs)) {
          logger.warn?.('Ryoko job lease was lost; output must be reconciled');
        }
      } catch {
        logger.error?.('Ryoko could not renew a GitHub job lease');
      }
    }, Math.min(leaseMs / 3, 15_000));
    heartbeat.unref?.();
    try {
      await notify(job.request, `Ryoko is running the engineering team for #${job.request.number}. A successful run opens a draft PR only.`);
      const result = await runPipeline({
        broker, request: job.request, settings,
      });
      store.finishJob(job, 'completed', result);
      await notify(job.request, botStatus(job.request, {
        status: 'completed', ...result, startedAt: started, finishedAt: Date.now(),
      }));
    } catch (error) {
      try { store.finishJob(job, 'failed', {}); }
      catch { logger.error?.('Job outcome uncertain after lost lease; remote reconciliation required'); }
      await notify(job.request, botStatus(job.request, { status: 'failed' }));
      logger.error?.('Ryoko GitHub worker failed job ' + job.id
        + ': ' + (error?.status || 'internal error'));
    } finally {
      clearInterval(heartbeat);
    }
  }

  const stopped = (async () => {
    while (active) {
      try {
        store.recoverStale();
        if (Date.now() >= nextMaintenance) {
          store.prune();
          store.pruneJobs();
          nextMaintenance = Date.now() + 60_000;
        }
        const job = store.claimJob(workerId, leaseMs);
        if (job) {
          current = job;
          await work(job);
          current = null;
          continue;
        }
      } catch {
        logger.error?.('GitHub worker could not claim a job; verify database health');
      }
      if (active) await sleep(pollMs);
    }
  })();

  return {
    workerId,
    async stop() { active = false; await stopped; },
    get currentJob() { return current?.id || null; },
    get stopped() { return stopped; },
  };
}
