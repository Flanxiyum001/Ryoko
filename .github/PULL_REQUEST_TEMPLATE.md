## Summary

<!-- Explain what changed and why. Link related issues. -->

## Change type

- [ ] Bug fix
- [ ] Feature
- [ ] Documentation / community standards
- [ ] Refactor / maintenance
- [ ] Security fix (do not publish exploit details)

## Scope and verification

<!-- Explain acceptance criteria and which behaviors changed. -->

### Checks run

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo test --workspace --locked`
- [ ] Desktop checks (`npm test`, `npm run build`) if applicable
- [ ] GitHub App checks (`node --check`, `node --test`) if applicable
- [ ] Manual verification (if applicable)
- [ ] Not applicable / not run; explained below

**Results and skipped checks:**

<!-- State exactly which checks passed, failed, or were not run. Do not claim CI is green before it runs. -->

## Security, privacy, and compatibility

- [ ] No secrets, real credentials, or personal information included
- [ ] Untrusted inputs, agent permissions, and sandbox boundaries reviewed if relevant
- [ ] Regression tests added for security-sensitive or behavioral fixes when relevant
- [ ] Breaking changes, migrations, and compatibility risks documented if applicable
- [ ] Applicable documentation updated

**Risks / mitigations / rollback:**

<!-- For a security vulnerability, coordinate privately under SECURITY.md; do not disclose exploit instructions here. -->

## UI evidence (if applicable)

<!-- Add screenshots/recordings of changed user-visible flows, or explain why not applicable. -->

## Reviewer notes

<!-- List limitations, follow-ups, relevant links, and outstanding decisions. Human review is required before merge. -->
