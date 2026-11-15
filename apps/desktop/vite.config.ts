import { defineConfig } from 'vitest/config';

// The frontend lives in `ui/` so the pre-existing browser workbench files at
// the desktop root keep working while the native shell gains its own app.
export default defineConfig({
  root: 'ui',
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: '127.0.0.1',
  },
  build: {
    target: 'es2022',
    outDir: '../dist',
    emptyOutDir: true,
  },
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts'],
  },
});
