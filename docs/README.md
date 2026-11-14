# Ryoko documentation

| Document | Owns |
| --- | --- |
| [RYOKO_2_MASTER_BLUEPRINT.md](RYOKO_2_MASTER_BLUEPRINT.md) | Product scope, milestones, critical path |
| [architecture/SYSTEM_ARCHITECTURE.md](architecture/SYSTEM_ARCHITECTURE.md) | Layers, boundaries, data flow |
| [architecture/CRATE_RESPONSIBILITIES.md](architecture/CRATE_RESPONSIBILITIES.md) | Crate ownership and dependency rules |
| [architecture/DESKTOP_ARCHITECTURE.md](architecture/DESKTOP_ARCHITECTURE.md) | Tauri shell, frontend, typed IPC |
| [architecture/EDITOR_ARCHITECTURE.md](architecture/EDITOR_ARCHITECTURE.md) | Monaco, documents, LSP, diagnostics |
| [architecture/AGENT_RUNTIME.md](architecture/AGENT_RUNTIME.md) | Agent lifecycle, execution, review |
| [architecture/CONTEXT_ENGINE.md](architecture/CONTEXT_ENGINE.md) | Indexing, retrieval, project memory |
| [protocols/IACP_V2.md](protocols/IACP_V2.md) | Inter-agent protocol evolution |
| [security/THREAT_MODEL.md](security/THREAT_MODEL.md) | Assets, trust boundaries, threats |
| [security/EXECUTION_SANDBOX.md](security/EXECUTION_SANDBOX.md) | Restricted project execution |
| [product/UX_SPECIFICATION.md](product/UX_SPECIFICATION.md) | Layout, modes, interaction |
| [releases/RELEASE_CRITERIA.md](releases/RELEASE_CRITERIA.md) | Gates and evidence |

Existing project documents:

| Document | Notes |
| --- | --- |
| [IACP.md](IACP.md) | Implemented IACP v1.0 contract |
| [ARCHITECTURE_SECURITY_AUDIT.md](ARCHITECTURE_SECURITY_AUDIT.md) | Audit and prioritized risks |
| [UX_UPGRADES.md](UX_UPGRADES.md) | Shipped terminal UX work |
| [GITHUB_APP.md](GITHUB_APP.md) | Experimental GitHub App broker |

Everything except the implemented contracts listed above describes intent. Pre-alpha
code must not be presented as satisfying a document just because the document
exists.
