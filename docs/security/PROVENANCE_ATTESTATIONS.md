# Merge provenance attestations

A merge can carry a signed statement of what produced it: which models wrote
the code, which sandbox validated it, which humans approved it, and exactly
which files changed. The statement is stored in Git notes, so it travels with
the repository and needs no running service to check.

## Format

The statement follows the in-toto v0.1 statement shape:

```json
{
  "_type": "https://in-toto.io/Statement/v0.1",
  "subject": [
    { "name": "git+commit:<sha>", "digest": { "gitCommit": "<sha>" } }
  ],
  "predicateType": "https://ryoko.dev/attestation/merge/v1",
  "predicate": {
    "commit": "<sha>",
    "branch": "main",
    "models": [{ "model": "local/qwen2.5-coder", "role": "senior-developer" }],
    "sandbox": { "backend": "oci", "image": "sha256:…" },
    "validation": { "passed": true, "summary": "pre-flight passed; security audit: clean" },
    "approvers": [
      { "name": "…", "email": "…", "approved_at": "2026-11-17T10:00:00Z" }
    ],
    "files": [{ "name": "src/api.rs", "digest": { "sha256": "…" } }]
  }
}
```

The statement is the DSSE payload, base64 encoded, in an envelope:

```json
{
  "payloadType": "application/vnd.in-toto+json",
  "payload": "<base64 statement>",
  "signatures": [{ "keyid": "SHA256:…", "sig": "<base64 armored SSHSIG>" }]
}
```

Notes live under `refs/notes/ryoko-attestations`, one per commit. They are not
on a branch, so pushing them is a deliberate choice:
`git push origin refs/notes/ryoko-attestations`.

## Signing and verifying

Signing uses `ssh-keygen -Y sign` in the `ryoko-attestation` namespace, so a
verifier needs nothing but the signer's public key:

```sh
ryoko attest create --repo . --commit <sha> --branch main \
  --model senior-developer=local/qwen2.5-coder \
  --sandbox-backend oci --sandbox-image sha256:… --validation-passed \
  --validation-summary "pre-flight passed; security audit: clean" \
  --approver "Pourush Nair <pourushnair6@gmail.com>" \
  --signing-key ~/.ssh/ryoko_attest

ryoko attest verify --repo . --commit <sha> \
  --allowed-signers ~/.ssh/allowed_signers --identity pourush

ryoko attest show --repo . --commit <sha>
```

Trust is never implied. Verification requires either an `allowed_signers` file
or a single public key; there is no "trust anything" mode.

The signature proves possession of a key, and the payload binds that signature
to one commit. Verification therefore does three things, not one:

1. the SSHSIG verifies against the trusted key;
2. `predicate.commit` equals the commit being verified; and
3. the statement subject carries that same `gitCommit` digest.

So copying a valid note onto another commit, or editing the approver name in the
payload, fails — both are covered by tests.

### What `--identity` means

With `--allowed-signers`, the identity must match a principal line in that file;
`ssh-keygen -Y verify` checks it, so a signature under another principal is
rejected.

With `--public-key`, `--identity` only **names the key you are choosing to
trust**. An SSHSIG does not sign a principal, so no file can make that name
cryptographic. When the human identity behind an approval matters, use
`allowed_signers` and put the real principals in it.

## Bounds

Everything in a statement is validated before it is trusted: the statement and
predicate types must match this document, the commit must be a full 40 or 64
character hex id, at most 32 models, 32 approvers and 512 files are accepted,
and every text field must be non-empty, printable and at most 512 characters. A
commit touching more files than that is refused rather than truncated, because a
partial file list would misrepresent what the merge did.

Approvers are mandatory: an attestation with no human approval is rejected by
the library, not just by the CLI.

Only the whole-worktree and selective-hunk merges in `ryoko review` are signed.
A merge performed by another tool, or by hand, needs an explicit
`ryoko attest create`.

## Automatic attestation when a review is accepted

`ryoko review` signs an accepted merge itself when a key is configured:

```sh
ryoko review --repo . --attest-key ~/.ssh/ryoko_attest
# or, equivalently
RYOKO_ATTEST_KEY=~/.ssh/ryoko_attest ryoko review --repo .
```

Without a key, nothing is signed and no claim is made. With one, both merge
paths (whole-worktree accept and the selective hunk merge) sign the merge commit
after Git has created it. Every field comes from the session that produced the
merge, never from model output:

| Field | Source |
| --- | --- |
| `commit`, `branch` | the merge receipt Git just produced |
| `models[].model`, `models[].role` | the configured model for the review and `--agent` |
| `sandbox.backend` | the isolation flags: `docker`, `podman`, `oci-auto` or `host` |
| `sandbox.image` | `--sandbox-image` |
| `validation` | the pre-flight report that gated the merge |
| `approvers` | `git config user.name` / `user.email` of the human who accepted |

An empty model list is recorded when no model id can be resolved, rather than a
placeholder name. A missing Git identity is an error, because an attestation
must name a real approver.

Attestation happens after the merge commit exists, so its failure cannot undo a
merge that already happened. It is reported as a warning on stderr and the
merge stands:

```
warning: merge succeeded but attestation failed: git failed: …
```

## What is not implemented

- **No SLSA v1 Predicate.** This is an in-toto statement with a Ryoko-specific
  predicate, not a SLSA provenance predicate; the SLSA fields (build type,
  builder identity, materials) are not populated.
- **Not verified in CI.** Publishing `refs/notes/ryoko-attestations` and running
  `ryoko attest verify` in a workflow is an operator step; no workflow is
  included.
