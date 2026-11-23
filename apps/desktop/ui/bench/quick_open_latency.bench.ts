/**
 * Repeatable key-to-results latency harness for the quick-open palette.
 *
 * Run with `npm run bench:quick-open`. It is deliberately **not** part of
 * `npm test` and it asserts no timing threshold: until a stable budget has been
 * agreed, the numbers are evidence, not a gate. The functional invariants it
 * does check (determinism, no cross-query state, bounded result count) are the
 * parts a refactor can break silently.
 *
 * Measured work is exactly what one keystroke performs: `rankFiles` over the
 * workspace path list the desktop already holds. No filesystem scan, no
 * network, no model call and no DOM paint are included, so the numbers are
 * comparable across machines but they are not end-to-end input latency.
 */
import { describe, expect, it } from 'vitest';
import { rankFiles } from '../src/quick_open_model';
import { BENCH_QUERIES, buildWorkspacePaths, recentPaths } from './quick_open_fixture';

const WARM_SAMPLES = 200;
const COLD_ROUNDS = 25;

function percentile(sorted: readonly number[], fraction: number): number {
  if (!sorted.length) return 0;
  const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil(fraction * sorted.length) - 1));
  return sorted[index];
}

function sample(run: () => void, samples: number): number[] {
  const times: number[] = [];
  for (let index = 0; index < samples; index += 1) {
    const started = performance.now();
    run();
    times.push(performance.now() - started);
  }
  return times.sort((left, right) => left - right);
}

function fixed(value: number, digits = 3): string {
  return value.toFixed(digits);
}

describe('quick-open 10k latency harness', () => {
  it('reports cold and warm p50/p95 for every query shape', () => {
    const paths = buildWorkspacePaths();
    const recents = recentPaths(paths);
    expect(paths).toHaveLength(10_000);
    expect(new Set(paths).size).toBe(10_000);

    const rows: string[] = [];
    let worstP95 = 0;
    for (const { query, label } of BENCH_QUERIES) {
      // Cold: a fresh path list and no rank has ever run for this query, which
      // is what the first keystroke after opening a workspace looks like.
      const cold: number[] = [];
      for (let round = 0; round < COLD_ROUNDS; round += 1) {
        const fresh = [...paths];
        const started = performance.now();
        rankFiles(fresh, query, recents);
        cold.push(performance.now() - started);
      }
      cold.sort((left, right) => left - right);

      // Warm: the same query repeated against the same list.
      rankFiles(paths, query, recents);
      const warm = sample(() => rankFiles(paths, query, recents), WARM_SAMPLES);
      const p95 = percentile(warm, 0.95);
      worstP95 = Math.max(worstP95, p95);
      rows.push(
        '| ' + label + ' | `' + query + '` | '
        + fixed(percentile(cold, 0.5)) + ' | ' + fixed(percentile(cold, 0.95)) + ' | '
        + fixed(percentile(warm, 0.5)) + ' | ' + fixed(p95) + ' |',
      );
    }

    const table = [
      '| Query | Input | cold p50 ms | cold p95 ms | warm p50 ms | warm p95 ms |',
      '| --- | --- | --- | --- | --- | --- |',
      ...rows,
    ].join('\n');
    // Printed so a run leaves its evidence in the terminal log.
    console.log('\nquick-open 10k key-to-results latency (' + String(WARM_SAMPLES)
      + ' warm samples, ' + String(COLD_ROUNDS) + ' cold rounds)\n' + table + '\n');

    // Invariants only. A timing assertion here would be a flaky CI gate.
    expect(worstP95).toBeGreaterThan(0);
    expect(rankFiles(paths, 'main', recents)).toEqual(rankFiles(paths, 'main', recents));
    expect(rankFiles(paths, 'main', recents).length).toBeLessThanOrEqual(40);
    expect(rankFiles(paths, 'zzznomatch', recents)).toEqual([]);
    expect(rankFiles(paths, '', recents)[0]?.path).toBe(recents[0]);
  });
});
