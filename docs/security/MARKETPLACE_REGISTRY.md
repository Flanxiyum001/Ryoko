# Ryoko marketplace registry: transport and artifact boundaries

The marketplace registry returns untrusted metadata. Registry records are **not** publisher
signatures, and a checksum supplied by the same registry only verifies the bytes against that
registry record. It does **not** independently authenticate the publisher.

This change enforces:

- HTTPS for public registry and artifact URLs; cleartext HTTP is restricted to exact loopback
  hosts for local testing (not `localhost.evil.example` or `127.0.0.1.evil.example`).
- Reject redirects that downgrade HTTPS to HTTP before the HTTP request is followed by the
  built-in marketplace client.
- Bound registry index responses to 4 MiB even if the server omits Content-Length.
- Require a correctly formatted lowercase SHA-256 checksum before downloading artifacts.
- Preserve existing per-chunk artifact size checks and clean up partial downloads after
  stream, size, or rename failures.

**Remaining requirements before an untrusted public marketplace launch:** independently verified
publisher identities or signed manifests, a reviewed registry policy, permission prompts,
extension/skill sandbox execution tests, and cross-platform installation/update tests. Avoid
installing code from a registry you do not trust.

The originally proposed large foundation implementation from PR #16 was archived on
`archive/pr16-foundation-work-in-progress` so it can be rebuilt as separate,
testable indexing, storage, agent-tool and desktop-marketplace PRs.
