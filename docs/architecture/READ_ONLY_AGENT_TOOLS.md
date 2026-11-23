# Read-only agent toolbox (phase 1)

The PR #16 archive proposed a unified agent execution engine but exposed shell commands through string pattern filtering, and several modules referenced unavailable types. Shipping it as a complete security implementation would be misleading.

This first replacement introduces a **standalone, tested read-only toolbox** with typed `ReadFile`, `ListFiles`, and `SearchText` requests. It only reads supported source extensions under a canonical workspace root, blocks parent traversal and out-of-workspace symlinks, skips hidden/build directories, and caps search results and file reads. It does **not** execute arbitrary code, write files, run commands, call MCP services, or bypass operator approval.

A later PR must integrate this library with the agent runtime and add explicit workspace-lease binding and permissions before it is offered to models. Further modules for sandboxed execution, approval persistence, and MCP calls require separate threat-model tests.
