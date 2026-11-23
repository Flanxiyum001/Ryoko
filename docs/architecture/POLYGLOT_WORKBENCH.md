# Polyglot editor and native task execution

Status: **pre-alpha first increment**, pending CI and manual hardware tests.

## Delivered capabilities

Ryoko recognizes dozens of common language extensions, Dockerfile/Makefile and other special filenames, and scripts with known shebangs. Unknown extensions stay editable as plain text. The language selector can manually override any registered Monaco mode. Semantic completion, hover and go-to-definition remain conditional on an installed and user-authorized LSP server.

The desktop now offers **Run file** with a curated direct-argv recipe for languages with a suitable single-file launcher, plus **Output → Executable/Arguments → Run task** for arbitrary installed build systems, compilers and runtimes. For example, use Python with arguments -u {file}; Go with run {file}; or a Rust project with Cargo and arguments run --locked. C/C++ users can call make all or cmake --build build; .NET users can call dotnet run --project MyApp. TypeScript's default single-file launcher requires Node 22+ type stripping; unsupported TS features and JSX components need a project task.

Run Task executes the selected executable with a separately parsed argv, without shell evaluation. Quoted argument groups are supported. Exact argument tokens {file} and {workspace} expand to native, validated paths. The task must refer to a regular file inside the open workspace. Symlink escapes are rejected and each run shows an **OS-native approval prompt**. The UI displays bounded stdout/stderr, exit status and a Stop control. A workspace switch or desktop shutdown kills the directly managed process.

**No compilers, interpreters, SDKs, language servers or debugging adapters are bundled.** Developers must install the toolchains they intend to use. Language recognition alone is not guaranteed syntax highlighting or semantic support; some modes have no Monaco tokenizer. React/HTML/library files are intentionally not assumed to be standalone executables. Build commands may modify files, use the network or spawn subprocesses. Native Run Task is **not a sandbox** and runs with the user's local account privileges; only run trusted project code. For untrusted code, use Ryoko's separate container validation mechanism. Interactive CLI apps belong in the integrated terminal, not the non-interactive task output.

Language servers are configured through Settings → Language servers. Each language has one preferred server, and external servers only start with explicit approval. Debugging remains in the separate user-chosen DAP adapter panel.

## Verification before release

- Frontend: cd apps/desktop; npm ci; npm test; npm run build.
- Tauri: cargo fmt --manifest-path apps/desktop/src-tauri/Cargo.toml -- --check; cargo check --manifest-path apps/desktop/src-tauri/Cargo.toml --locked; cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml --locked.
- Workspace Rust formatting, clippy and tests per AGENTS.md.
- Manual Windows/macOS/Linux smoke: Python and JavaScript, one compiled project, unknown extension manual mode, LSP routing, missing executable, native approval cancellation, nonzero exit, output truncation, path traversal and symlink escape, run stop and workspace switch.

Known gaps: toolchain discovery, project-level task persistence, multistep compile-and-run pipelines, interactive input to the native runner, automatic language-server installation, and full subprocess-tree termination across platforms. Do not claim universal runtime/debugger parity or production-readiness from a green build alone.
