# Recorded Verification-Evidence Fixture Contract

| | |
|---|---|
| **Contract version** | v1 |
| **Machine-readable inventory** | [`tests/fixtures/verification_evidence_cases_v1.json`](../../tests/fixtures/verification_evidence_cases_v1.json) |
| **Literal recordings** | [`tests/fixtures/verification_evidence_*.txt`](../../tests/fixtures/) |
| **Current pin** | `tests/verification_evidence.rs` |
| **Provenance** | bead claudepr-e105aa87 (2026-09-28) |

This note defines the input contract for recorded verification evidence. The
text fixtures are complete recordings: each one contains the evidence prose,
the executable command block, and the captured cargo wrapper transcript. The
JSON inventory gives every recording an id, its expected result, and the
rationale for that result. A parser or validator consumes the literal `.txt`
file and uses the inventory as the test oracle; it must not infer semantics
from a filename or invent a different meaning for a missing field.

## Recording format

Each recording is UTF-8 Markdown-shaped text with these regions in this exact
order:

1. **Prose** comes first and states the two evidence axes exactly once:
   execution site (`remote` or `local`) and coverage (`complete` or
   `targeted`). For a targeted run, prose also names every targeted selector.
2. One exact ```` ```verified: ```` opening line starts the command region.
   It ends at one exact ```` ``` ```` line. Every non-empty line in this
   region is an executable command, without `exit=...` or parenthesized mode
   annotations.
3. One exact ```` ```cargo-output ```` opening line starts the captured
   transcript. It ends at one exact ```` ``` ```` line. This is the source of
   truth for execution site and run outcome.

There is exactly one `verified` region and exactly one `cargo-output` region.
The two regions must not be nested, reordered, duplicated, or replaced by a
generic code fence. Missing or malformed fences are a shape error, not a
semantic validation failure.

The literal fixture files are the normative examples of both prose and
transcript. The `prose_claim` and `cargo_output_shape` fields in the manifest
are labels for review and test selection; they do not replace the bytes in
the recording.

## Derived semantics

The validator derives the axes from the recording as follows:

- **Remote** requires all three prelude tells — `submitting`, `workflow:`,
  and `streaming logs from` — plus a terminal `PASSED`, `FAILED`, or `timed
  out` tell. A terminal failure or timeout rejects the evidence even if prose
  claims success.
- **Local** requires `falling back to local` and one wrapper reason: `no git
  remote`, `uncommitted changes detected`, `push failed`, or `submit failed`.
  The phrase `no git remote` is a reason, not a second site claim.
- A capture with no `[cargo-remote]` output is the extraction-local shape: it
  is accepted only when the ordinary cargo output contains a green `test
  result`. A partial wrapper transcript, contradictory site tells, or a
  no-banner capture without a green cargo result is rejected. Output tells
  always outrank prose or the caller's intent.
- **Complete** means the verified region contains exactly one bare command
  line for each of `cargo test --tests` and `cargo test --doc`, and the
  captured output contains at least one green cargo test result for each
  recorded leg. `--tests` alone is not complete because it skips doctests.
- **Targeted** means the verified region contains one or more unique
  `cargo test --test NAME` or `cargo test --lib` selectors and neither
  complete leg. Every such selector must be named in prose. A missing
  complete leg, duplicate leg or selector, commented-out test command, mixed
  complete/targeted selection, or bare `cargo test` command is rejected.
  Non-test commands such as fmt, clippy, and build do not change the coverage
  axis.
- Accepted evidence is green, has output-backed axes matching prose, and has
  no unnamed targeted selector. Semantic, misleading, incomplete, or
  contradictory evidence is rejected with exit 1. A malformed recording or
  usage error is rejected with exit 2.

## Fixture inventory

The four positive fixtures are the required remote/local × complete/targeted
corners:

| Case | Fixture | Captured run | Prose claim | Expected |
|---|---|---|---|---|
| `valid-remote-complete` | [`verification_evidence_valid_remote_complete.txt`](../../tests/fixtures/verification_evidence_valid_remote_complete.txt) | remote, green | remote + complete | accept, exit 0 |
| `valid-remote-targeted` | [`verification_evidence_valid_remote_targeted.txt`](../../tests/fixtures/verification_evidence_valid_remote_targeted.txt) | remote, green | remote + targeted (`docs_build_commands`, `lib`) | accept, exit 0 |
| `valid-local-complete` | [`verification_evidence_valid_local_complete.txt`](../../tests/fixtures/verification_evidence_valid_local_complete.txt) | local, green | local + complete | accept, exit 0 |
| `valid-local-targeted` | [`verification_evidence_valid_local_targeted.txt`](../../tests/fixtures/verification_evidence_valid_local_targeted.txt) | local, green | local + targeted (`watchdog`) | accept, exit 0 |

The negative fixtures deliberately exercise independent rejection rules:

| Case | Fixture | Defect represented | Expected |
|---|---|---|---|
| `reject-failed-remote` | [`verification_evidence_misleading_failed_remote_run.txt`](../../tests/fixtures/verification_evidence_misleading_failed_remote_run.txt) | failed remote run | reject, `remote-outcome`, exit 1 |
| `reject-site-claims-local` | [`verification_evidence_misleading_site_claims_local.txt`](../../tests/fixtures/verification_evidence_misleading_site_claims_local.txt) | prose says local, output says remote | reject, `site-mismatch`, exit 1 |
| `reject-site-claims-remote` | [`verification_evidence_misleading_site_claims_remote.txt`](../../tests/fixtures/verification_evidence_misleading_site_claims_remote.txt) | prose says remote, output says local | reject, `site-mismatch`, exit 1 |
| `reject-axes-unstated` | [`verification_evidence_misleading_axes_unstated.txt`](../../tests/fixtures/verification_evidence_misleading_axes_unstated.txt) | missing site and coverage axes | reject, `axis-unstated`, exit 1 |
| `reject-targeted-selector-unnamed` | [`verification_evidence_misleading_unnamed_selector.txt`](../../tests/fixtures/verification_evidence_misleading_unnamed_selector.txt) | targeted selector not named in prose | reject, `selector-unnamed`, exit 1 |
| `reject-annotation-on-executed-line` | [`verification_evidence_misleading_annotated_verified_line.txt`](../../tests/fixtures/verification_evidence_misleading_annotated_verified_line.txt) | annotation on an executable verified line | reject, `annotation-on-executed-line`, exit 1 |
| `reject-complete-one-leg` | [`verification_evidence_misleading_complete_one_leg.txt`](../../tests/fixtures/verification_evidence_misleading_complete_one_leg.txt) | prose says complete but only one leg ran | reject, `coverage-mismatch`, exit 1 |
| `reject-duplicate-complete-leg` | [`verification_evidence_misleading_duplicate_complete.txt`](../../tests/fixtures/verification_evidence_misleading_duplicate_complete.txt) | duplicate `--tests` leg | reject, `coverage-duplicate`, exit 1 |
| `reject-commented-complete-leg` | [`verification_evidence_misleading_commented_leg.txt`](../../tests/fixtures/verification_evidence_misleading_commented_leg.txt) | commented-out `--doc` leg | reject, `coverage-commented`, exit 1 |
| `reject-insufficient-leg-results` | [`verification_evidence_misleading_insufficient_results.txt`](../../tests/fixtures/verification_evidence_misleading_insufficient_results.txt) | one green result cannot prove two legs | reject, `leg-outcome`, exit 1 |
| `reject-output-tells-unbacked` | [`verification_evidence_misleading_mode_unbacked.txt`](../../tests/fixtures/verification_evidence_misleading_mode_unbacked.txt) | no wrapper tells and no green extraction result | reject, `output-tells`, exit 1 |
| `reject-missing-output-fence` | [`verification_evidence_malformed_missing_output_fence.txt`](../../tests/fixtures/verification_evidence_malformed_missing_output_fence.txt) | missing captured-output fence | reject, `fence`, exit 2 |

The manifest is the exhaustive list. Adding, removing, or renaming a
`verification_evidence_*.txt` fixture requires updating the manifest and its
contract test in the same change.
