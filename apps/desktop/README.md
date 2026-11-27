# Ryoko Desktop

This directory contains two distinct frontends with separate responsibilities:

| Path | Role | Entry point |
| --- | --- | --- |
| `ui/` + `src-tauri/` | **Active native desktop IDE** (Tauri + Vite + Monaco + xterm.js + `ryoko-lsp`) | `npm run tauri dev` / `npm run build` |
| `index.html`, `app.js`, `styles.css` | **Browser GitHub App workbench** served by `services/github-app` (experimental) | `services/github-app/src/index.mjs` |

## Developing the native desktop IDE

Edit files under `ui/src/` and `src-tauri/src/`. Run the native shell (not bare `npm run dev` in a browser) when testing local filesystem access:

```sh
npm ci
npm run sidecar
npm run tauri dev
```

`npm run sidecar` builds and stages both `ryoko-lsp` and `wasm-guardrail-worker` into `src-tauri/binaries/` for the host target triple.
