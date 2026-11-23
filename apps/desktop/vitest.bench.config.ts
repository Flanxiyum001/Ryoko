import { defineConfig } from 'vitest/config';

// Benchmark harnesses run through `npm run bench:quick-open`. They are kept out
// of `vitest.config.ts`/`npm test` so the unit suite stays fast and a benchmark
// can never become an accidental CI threshold.
export default defineConfig({
  root: 'ui',
  test: {
    environment: 'node',
    include: ['bench/**/*.bench.ts'],
    testTimeout: 300_000,
  },
});
