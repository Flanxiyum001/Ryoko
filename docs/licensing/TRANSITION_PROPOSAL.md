# Proposed Apache-2.0 to AGPL-3.0 transition

> **Decision status: awaiting maintainer approval.**
>
> This document is a proposal. It does not relicense the current repository, change the root `LICENSE`, amend prior releases, or bind contributors.

## 1. Recommendation

Adopt **AGPL-3.0-only for a clearly identified future Ryoko Community Edition release**, subject to the approval gates in [MAINTAINER_APPROVAL_CHECKLIST.md](MAINTAINER_APPROVAL_CHECKLIST.md).

Offer a separate commercial license for eligible customers who need contractual terms or use rights incompatible with AGPL-3.0. The commercial offering may include hosted infrastructure, support, SLAs, organization controls, and separately developed proprietary capabilities, subject to a careful review of how those components interact with AGPL-covered code.

AGPL-3.0 is still an open-source license. It does not prohibit commercial use or prevent another party from offering a competing service. Its network-use provisions require operators of modified covered versions to offer the corresponding source to users interacting with those versions over a network, under the license's conditions.

## 2. Repository baseline found during this audit

The audit of the repository's default branch found:

- The root `LICENSE` contains the complete Apache License, Version 2.0.
- `README.md` states that currently published source is Apache-2.0 and describes commercial use as permitted.
- `CONTRIBUTING.md` says contributions are accepted under Apache-2.0, authors retain copyright, and no CLA or copyright assignment is currently required.
- The first 100 commits returned by the repository history endpoint identify one commit author: Pourush Nair. This is useful evidence about the visible commit history, but it is **not proof of complete copyright ownership**. Squashed commits, imported code, copied snippets, generated files, assets, and third-party dependencies require separate review.
- No root `NOTICE` file was present in the repository tree at the time of this audit.
- The repository contains a Rust workspace, a Tauri desktop application, a JavaScript/TypeScript desktop frontend, and a GitHub App service. Each dependency ecosystem and bundled asset set needs its own license review.

This is a documentation and policy audit, not a complete software bill-of-materials or legal chain-of-title opinion.

## 3. What changes — and what does not

### Existing Apache-2.0 releases

The proposed change must not claim to revoke or replace rights already granted under Apache-2.0. Recipients of existing Apache-2.0 releases retain the permissions granted to them under that license, subject to its terms. Existing copies and forks may continue to rely on the Apache-2.0 license accompanying those copies.

### Future releases

If approved, the project should select and announce a specific release boundary. New Ryoko-authored material first distributed after that boundary could be offered under AGPL-3.0-only, provided the project has the necessary rights to every included contribution and component.

The transition release should include:
- The complete, unmodified AGPL-3.0 license text at the repository root.
- Updated SPDX identifiers and copyright/license notices where appropriate.
- A clear README statement identifying the exact version/date from which AGPL-3.0 applies.
- A historical licensing note explaining that earlier releases remain available under Apache-2.0.
- Updated contribution terms and a contributor-rights process that has been reviewed and adopted before accepting contributions under it.

Do not label the entire Git history or all previously published versions as AGPL-3.0.

## 4. Dual licensing and commercial plans

A dual-license model can let the rights holder offer eligible Ryoko code under AGPL-3.0 to the community and under a separate commercial agreement to customers who need different terms.

This requires sufficient rights to the relevant code. Copyright ownership of a contribution normally remains with its author unless separately assigned; a CLA can instead grant the project owner broad licensing and sublicensing rights while the contributor retains copyright.

A CLA is not a magic cure for third-party code. It cannot grant rights the contributor does not own, override dependency licenses, or make incompatible components relicensable. Keep third-party components and their notices intact, and review their compatibility before distribution.

A commercial edition should be designed deliberately:
- Keep the community codebase and commercial-only components clearly identified.
- Do not assume that putting code in a separate crate, process, repository, or service automatically makes it independent of AGPL obligations.
- Obtain a specific legal review of linking, combined works, APIs, plugins, IPC, and deployment architecture.
- Consider selling hosted operations, support, compliance services, and SLAs independently of any proprietary code licensing.
- Publish accurate feature and license boundaries; do not imply that AGPL prevents all commercial competition.

## 5. Contribution policy required before transition

Before accepting contributions under a policy intended to support dual licensing:

1. Finalize a contributor license agreement with qualified legal review.
2. Identify the legal person or entity receiving the grant (for example, the maintainer personally today, or a company if one is later formed). Do not leave the recipient ambiguous.
3. State that contributors retain copyright; the grant is a license, not a copyright assignment.
4. Cover copyright and patent permissions, the right to sublicense and offer the contribution under alternative commercial terms, and contributor authority/originality representations.
5. Define how the agreement is accepted and recorded. A pull-request checkbox or comment alone should not be treated as a robust process without counsel's approval.
6. Require separate permission for contributions made by employers or other rights holders.
7. Keep a durable record of the accepted agreement and the exact version in force.

Until the CLA and contribution workflow are separately approved and operational, the current Apache-2.0 contribution terms remain in force. This proposal does not silently change the terms for open pull requests or future submissions.

## 6. Dependency and asset audit gates

Before a release under AGPL-3.0, generate and review a dependency inventory for at least:

- Cargo workspace dependencies and transitive dependencies.
- npm packages and lockfiles used by the desktop frontend and GitHub App.
- Tauri plugins, native libraries, platform-specific binaries, and installer contents.
- Fonts, icons, screenshots, sample projects, templates, and other bundled assets.
- Copied, adapted, vendored, generated, or imported source files.

Record each component's name, version, source, license identifier, whether it is modified, distribution location, and required notices. Resolve unknown, proprietary, source-available, or potentially incompatible terms before shipping. Preserve all required third-party notices.

## 7. Rollout sequence

1. Maintainer reviews and approves this proposal.
2. Obtain legal review of the transition plan and CLA.
3. Complete source provenance and dependency/asset audit.
4. Finalize the contribution agreement and operational acceptance process.
5. Make a separate implementation PR that changes the root license and all applicable notices in one coordinated change.
6. Update release notes and documentation with the exact effective release boundary.
7. Publish the first AGPL-3.0 release only after all checks and approvals are complete.

## 8. Important limitations

This proposal is not legal advice. License interpretation, compatibility, relicensing authority, and the treatment of combined works depend on the actual facts and applicable law. Obtain advice from qualified open-source/software counsel before adopting the transition or signing a CLA.

## 9. Approval record

No approval is implied by merging this documentation proposal. The maintainer should complete [MAINTAINER_APPROVAL_CHECKLIST.md](MAINTAINER_APPROVAL_CHECKLIST.md) and explicitly authorize a separate license implementation change.
