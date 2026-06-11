# bf-4eb: Starvation Alert — Beads Invisible to Worker

## Diagnosis

The starvation alert was triggered because both open beads were blocked by dependencies:

- **bf-52c** (Binary-level E2E tests) — depends on bf-40i (Wire main())
- **bf-4r6** (Write AGENTS.md) — depends on bf-40i (Wire main())

Since bf-40i is in_progress, `br ready` returned nothing, so the worker had nothing to claim.

## Root Cause

The dependency of bf-4r6 on bf-40i was overly conservative. Writing AGENTS.md (documentation of build commands, module map, repo purpose) does not require main() to be fully wired — it can be done independently. The dependency was likely added reflexively because both beads were created at the same time as "Phase 9/10 deliverables."

bf-52c's dependency on bf-40i IS correct — binary E2E tests require a working binary, which requires main() to be wired.

## Fix

Removed the dependency: `br dep remove bf-4r6 bf-40i`

After the fix, `br ready` shows bf-4r6 as claimable. A worker can now proceed with writing AGENTS.md while bf-40i is still being worked on.

## Not a Configuration Error

This was not a misconfiguration of exclude_labels, workspace paths, or filter settings — it was an overly conservative dependency creating a false bottleneck.
