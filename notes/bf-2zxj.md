# bf-2zxj: Lock cli.verbose to Tracer wiring with unit coverage and reconcile --verbose docs

## Task Completed

Both acceptance criteria have been verified as already complete:

### AC1: Unit Coverage ✅ COMPLETE

**Comprehensive unit tests already written and passing:**

1. **tests/cli.rs** (lines 187-251) - 3 integration tests:
   - `cli_verbose_propagates_to_launch_options`: Verifies CLI `--verbose` flag propagates to `LaunchOptions::verbose`
   - `cli_verbose_default_creates_disabled_tracer`: Verifies default (no flag) creates disabled Tracer
   - `cli_verbose_flag_creates_enabled_tracer`: Verifies `--verbose` flag creates enabled Tracer

2. **src/session.rs** (lines 1278-1313) - 2 unit tests:
   - `launch_options_verbose_true_creates_enabled_tracer`: Verifies `LaunchOptions::verbose=true` → enabled Tracer
   - `launch_options_verbose_false_creates_disabled_tracer`: Verifies `LaunchOptions::verbose=false` → disabled Tracer

**All 6 tests PASS** - verified with:
- `cargo test cli_verbose --test cli` → 4 tests passed
- `cargo test launch_options_verbose --lib` → 2 tests passed

**Wiring path verified end-to-end:**
```
CLI --verbose flag → cli.verbose:bool → main.rs:228 (LaunchOptions::verbose) → session.rs:323 (Tracer::new) → Tracer enabled/disabled
```

### AC2: Doc Reconciliation ✅ COMPLETE

**plan.md References:**
- ✅ plan.md EXISTS at `docs/plan/plan.md` (not missing)
- ✅ All bf-1bg4 bead body references resolve correctly:
  - plan.md section `--verbose Trace Points` (line 345)
  - Trace point specification (line 347)
  - AS-6 pass criteria (line 127)

**README.md Accuracy:**

README.md line 169 documents these trace points:
1. temp dir created
2. PTY opened  
3. child forked (with PID)
4. phase transitions (waiting → trust-dismissed → prompt-injected)
5. FIFO opened
6. prompt injected
7. Stop received (with session_id)
8. transcript retry count
9. cleanup reason

**All 9 trace points match implementation:**
1. `session.rs:327` → `"temp dir created at {}"`
2. `session.rs:423` → `"pty opened"`
3. `session.rs:424` → `"child forked pid={}"`
4. `session.rs:574-578` → `"phase transition: {} -> {}"`
5. `session.rs:488` → `"fifo opened"`
6. `session.rs:583` → `"prompt injected"`
7. `session.rs:705-708` → `"stop received session_id={}"`
8. `transcript.rs:230` → `"transcript read on attempt {}"`
9. `session.rs:643` → `"cleanup reason: timeout ({})"`

**README.md other --verbose references:**
- Line 107: Help text "Write timing traces to stderr" ✅
- Line 165: "Check `--verbose` output for 'Stop received'" ✅
- Line 167: "Run with `--verbose` to see retry attempts" ✅

All documentation claims match actual behavior. No stale or over-claimed trace points found.

## Conclusion

This task was to verify the completion of parent bead bf-1bg4 (which wired --verbose to the Tracer implementation). Both ACs are satisfied:
- Unit tests provide executable proof of the cli.verbose → Tracer path
- User-facing docs accurately describe implemented behavior
- plan.md references all resolve correctly

No code changes were needed - this was a verification and documentation task.
