# Specification links and benchmark gates

<!-- ryoko:implements
  crates/ryoko-core/src/spec_link.rs
  crates/ryoko-cli/src/perf.rs
  crates/ryoko-cli/src/check.rs
-->

Two operator-owned checks connect a run to the things a diff can silently
break: the specifications that describe the intended behaviour, and the
measurements that describe how fast it is.

## 1. Living specifications

A markdown spec declares what it governs with a comment directive:

```markdown
<!-- ryoko:implements src/api/routes.rs src/api/schema.rs -->
```

Long lists can span lines, and a directive inside a fenced code block is
ignored, so a spec can document the syntax without linking its examples. Paths
are validated like any other change path, so a spec cannot link to `..`, an
absolute path, `.git/` or `.ryoko/`.

```sh
ryoko spec                 # report drift; non-zero exit when anything moved
ryoko spec --record        # snapshot the spec and file digests
ryoko spec --json          # machine-readable drift
```

Recording stores a content digest of each spec and of every file it links, in
`.ryoko/state/specs.json`. Drift is reported per spec, on either side:

| Change | Reported as |
| --- | --- |
| An implementation file changed | `changed: <files>` |
| A linked file was deleted | `missing: <files>` |
| The spec gained a link | `newly linked: <files>` |
| The spec dropped a link | `unlinked: <files>` |
| The spec text changed in any other way | `spec changed` |
| The spec has never been recorded | `no recorded baseline` |

The same index answers the reverse question, which is what the orchestrator
uses to attribute work: `SpecIndex::specs_for("src/api/routes.rs")` returns every
spec claiming that file. `drift_tasks` turns each drifted spec into a task for a
role, carrying `ryoko.spec`, `ryoko.files` and `ryoko.kind=spec-drift` metadata so
the plan and the review UI can trace it back.

Digests are FNV-1a over file bytes: drift detection needs a cheap, stable
comparison, not a cryptographic one, and the hashes never leave local state. The
walk is bounded (4 000 entries, depth 12, 512 KiB per spec) and skips `.git`,
`.ryoko`, `node_modules`, `target`, `dist`, `build` and `vendor`.

## 2. Benchmark and size gates

`--check-command` proves the code still builds and passes its tests. A perf gate
adds a measurement on top, run in the same isolated sandbox and only after the
check and the security audit pass:

```sh
ryoko run --repo . \
  --perf-command "cargo bench --bench parse -- --output-format bencher" \
  --perf-metric ns_per_iter \
  --perf-max-regression-pct 5 \
  "Speed up the argument parser"
```

The command must print `metric=number` on stdout — for example `latency_ms=12.5`
or `bundle_bytes=482113` — where the metric name matches `--perf-metric`. The
last matching line wins, so a benchmark that prints several iterations reports
its final result.

| Outcome | Result |
| --- | --- |
| Within the threshold | validation passes; both numbers are logged |
| Over the threshold | validation fails with `performance gate failed for <metric>` |
| No `metric=number` printed | fails: the command printed no measurement |
| Benchmark command failed or timed out | fails, with the exit code or timeout |
| No recorded baseline | fails with the exact command that records one |
| Baseline file unreadable or malformed | fails; never treated as "no regression" |

Baselines live in `.ryoko/perf-baseline.json` by default, one number per metric:

```json
{
  "latency_ms": 12.5,
  "bundle_bytes": 482113
}
```

Record or refresh them on your own branch with `--perf-update-baseline`; in that
mode the current measurement is written and the gate passes. A PR run never
records a baseline, so a regression cannot launder itself into the reference
value.

An improvement always passes, including improvements beyond the threshold; the
limit applies to increases only. A zero baseline blocks any increase.

## What is not covered

Both checks compare against state that the operator controls. Neither one can be
influenced by model output: the spec links come from committed markdown, the
metric and threshold from the command line, and the baseline from a file that a
PR run only reads.
