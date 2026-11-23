# Security Policy

Ryoko is a pre-alpha, experimental AI IDE. Its desktop preview installers are unsigned and the security model is still evolving. Do not assume that preview builds, agent-generated code, third-party integrations, or sandboxed execution are production-safe.

## Supported versions

Security fixes are considered on a best-effort basis for:

| Version | Security updates |
| --- | --- |
| Current `main` branch | Best effort |
| Latest published pre-release | Best effort, when a fix can be backported or a new preview released |
| Older pre-releases, unsupported branches, and forks | Not supported |

This table is not a guarantee of maintenance, response times, or a security update for every report.

## Privately report a vulnerability

**Do not disclose exploitable security vulnerabilities in public GitHub issues, pull requests, discussions, or community channels.**

Use GitHub's **Security → Report a vulnerability** workflow at [Ryoko's Security page](https://github.com/Flanxiyum001/Ryoko/security) if private vulnerability reporting is enabled. Otherwise, contact the maintainer directly at **[pourushnair6@gmail.com](mailto:pourushnair6@gmail.com)** with the subject `[Ryoko Security] Vulnerability report`.

Please provide, when possible:

- A clear description of the issue, potential impact, and affected components.
- An affected release/tag or commit SHA, operating system, and relevant configuration.
- Minimal reproduction steps or a proof of concept that does not access third-party data.
- Redacted logs, screenshots, or relevant source locations.
- Whether there is evidence of active exploitation or public disclosure.

**Never include credentials, real API keys, tokens, personal data, or other sensitive information in a report or proof of concept.** Coordinate a secure way to exchange anything sensitive before sending it.

## What is in scope?

Examples include vulnerabilities in Ryoko's CLI and desktop application, agent permissions and tool execution, worktree or file-system isolation, container/WASI sandbox boundaries, provider credential handling, the GitHub App integration, and official release packaging.

Issues in third-party LLM providers, extensions, model weights, operating systems, or other upstream dependencies should also be reported to their respective maintainers. Please report to Ryoko if its integration makes such an issue exploitable in Ryoko.

For ordinary bugs, feature requests, or questions without security impact, use [public issues](https://github.com/Flanxiyum001/Ryoko/issues).

## Coordinated handling and disclosure

The maintainer will review reports on a best-effort basis, attempt to reproduce and assess impact, and work on a mitigation where practical. Response or fix timelines cannot be guaranteed, particularly during pre-alpha development.

Please avoid public disclosure of an unpatched exploit until there has been an opportunity to coordinate a fix and an advisory. When appropriate, remediation details may be published through [GitHub Security Advisories](https://github.com/Flanxiyum001/Ryoko/security/advisories) or the [release notes](https://github.com/Flanxiyum001/Ryoko/releases).

Only test systems and data you own or have explicit permission to assess. Avoid destructive testing, service disruption, persistence, and accessing other users' information. This project does not currently promise a bug bounty or financial reward.
