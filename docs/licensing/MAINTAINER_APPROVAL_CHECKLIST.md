# Maintainer approval checklist — licensing transition

> **Current state: NOT APPROVED.** Checking boxes in this draft or merging the proposal documentation does not itself change Ryoko's license.

Use this checklist to record a deliberate decision before a separate implementation PR is opened.

## A. Strategy approval

- [ ] Approve AGPL-3.0-only as the license for a specifically identified future community release.
- [ ] Confirm the commercial model: official hosted services, support/SLAs, and separately licensed commercial capabilities where legally separable.
- [ ] Acknowledge that AGPL permits commercial use and does not prohibit competitors from offering services if they comply with the license.
- [ ] Decide whether to use the exact SPDX identifier `AGPL-3.0-only` consistently.

## B. Rights and provenance

- [ ] Review the complete repository history, including branches, tags, imported commits, squashed PRs, and release artifacts.
- [ ] Verify ownership or permission for all source code, documentation, designs, fonts, icons, screenshots, and other assets.
- [ ] Investigate any copied, generated, vendored, or externally sourced code.
- [ ] Identify any contributions from employers, contractors, collaborators, or other rights holders.
- [ ] Obtain explicit permission where the project does not hold sufficient rights for the intended licensing.

## C. Third-party dependencies

- [ ] Generate a versioned SBOM/dependency inventory for Cargo, npm, Tauri, native libraries, binaries, and bundled assets.
- [ ] Verify every direct and transitive dependency license and applicable notice obligation.
- [ ] Resolve unknown or potentially incompatible licenses.
- [ ] Prepare and review a consolidated third-party notices file if required.

## D. Contributor agreement

- [ ] Obtain legal review of the CLA draft.
- [ ] Name the correct legal Project Recipient.
- [ ] Approve copyright, patent, sublicensing, and commercial relicensing grants.
- [ ] Approve contributor representations and employer/third-party ownership handling.
- [ ] Implement and test durable electronic acceptance and record retention.
- [ ] Announce the effective date before requiring new contributions to use the CLA.
- [ ] Decide how to handle already-open pull requests and previously accepted contributions.

## E. Release and documentation

- [ ] Choose the exact first release/version/commit to be AGPL-3.0-only.
- [ ] Keep existing Apache-2.0 releases and their notices available and accurately labeled.
- [ ] Prepare the official AGPL-3.0 license text and appropriate SPDX/copyright notices.
- [ ] Update README, CONTRIBUTING, release notes, package metadata, and distribution notices consistently.
- [ ] Review commercial/community boundaries with counsel, including linking, plugins, APIs, IPC, and hosted deployment.
- [ ] Confirm that no documentation implies that AGPL prohibits all commercial competition.

## F. Explicit maintainer authorization

- [ ] I have reviewed the transition proposal and understand that existing Apache-2.0 grants are not revoked.
- [ ] I authorize a separate implementation PR to change licensing for future releases only.
- [ ] I authorize adoption of the finalized contributor agreement and acceptance process.

**Maintainer name:** ______________________________

**Date (YYYY-MM-DD):** ___________________________

**Approved first AGPL release/version:** ___________________________

**Link to legal review / decision record:** _________________________

Until this section is explicitly completed and the implementation PR is separately approved, the root `LICENSE` remains Apache-2.0 and the current contribution terms remain in force.
