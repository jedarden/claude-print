#!/usr/bin/env bash
# probe-version-guard.sh — the version-straddle guard every probe-*.sh sources
# (docs/notes/claude-contract-probes.md §Version guard; added 2026-09-26,
# claudepr-9fe76ef4).
#
# Why: on 2026-09-24 the dev host's auto-updater repointed claude
# 2.1.281 → 2.1.282 mid-re-pin, and the first same-day probe run had to be
# discarded whole — its evidence could no longer be trusted to name one
# version. The dangerous shape is the straddle that is NOT noticed: it mixes
# measurements from two Claude versions into one probe session, and a re-pin
# built on it would stamp one version while individual evidence numbers came
# from another — the **Measured against:** stamp and the active fixtures
# would "agree" (satisfying the one-pin invariant claudepr-b590e46d enforces)
# while being false. Detection after the fact cannot catch that, so the
# probes must refuse to produce mixable evidence in the first place.
#
# Two layered defenses:
#
#   PIN      probe_version_guard_begin resolves CLAUDE_BIN to its final
#            symlink target ONCE (~/.local/bin/claude is a symlink the
#            auto-updater repoints; the versions it leaves behind persist
#            under ~/.local/share/claude/versions/). Every later invocation
#            in the run execs that one concrete binary, so a mid-run repoint
#            cannot redirect a measurement — the 2026-09-24 shape becomes
#            unreachable. An operator may pre-set CLAUDE_BIN to pin a chosen
#            binary for a whole re-pin session (§Version guard, "pinning a
#            whole re-pin session"); the drift-window PATH shim achieves the
#            same through PATH.
#
#   BRACKET  probe_version_guard_end re-runs `--version` on the pinned
#            binary after the measurements and compares. This catches the
#            remaining straddle shape — the resolved path changing content
#            in place (an install swapping files under a stable path) — and
#            any mismatch aborts the run as failed: its evidence is a
#            mixture of two Claude versions and MUST NOT be pinned. A stable
#            run ends by stamping the machine-readable verdict line the
#            re-pin records beside the fixture write:
#
#   version-guard: verdict=single-version start=<v> end=<v> binary=<path>
#
# Guard exit codes (a straddled probe run therefore exits non-zero):
#   0  single-version run confirmed
#   1  STRADDLED — mid-run version change; the run's evidence is invalid
#   2  cannot determine (no claude on PATH, CLAUDE_BIN not executable, or an
#      unparsable --version) — fails closed before/after the measurements
#
# This file is a library, not a standalone probe: probe-*.sh sources it and
# calls begin right before resolving anything about claude and end as the
# very last line, so the guard's verdict is the run's exit status.

probe_version_guard_token() {
    # First x.y.z-looking token in the given text, empty if none — the same
    # parse scripts/check-claude-version-bump.sh uses.
    printf '%s' "$1" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true
}

probe_version_guard_begin() {
    if [ -z "${CLAUDE_BIN:-}" ]; then
        command -v claude >/dev/null 2>&1 || {
            echo "version-guard: FAIL — no claude on PATH and CLAUDE_BIN is unset; cannot start a pinned measurement run" >&2
            exit 2
        }
        CLAUDE_BIN="$(command -v claude)"
    fi
    # PIN: resolve symlinks once, so a later launcher repoint cannot redirect
    # mid-run invocations (readlink -f resolves through the drift-window PATH
    # shim exactly the same way).
    local resolved
    resolved="$(readlink -f "$CLAUDE_BIN" 2>/dev/null || true)"
    [ -n "$resolved" ] && CLAUDE_BIN="$resolved"
    [ -x "$CLAUDE_BIN" ] || {
        echo "version-guard: FAIL — CLAUDE_BIN '$CLAUDE_BIN' is not executable" >&2
        exit 2
    }
    PROBE_VERSION_START_LINE="$("$CLAUDE_BIN" --version 2>&1 | head -1)"
    PROBE_VERSION_START="$(probe_version_guard_token "${PROBE_VERSION_START_LINE:-}")"
    [ -n "$PROBE_VERSION_START" ] || {
        echo "version-guard: FAIL — unparsable claude --version output: '${PROBE_VERSION_START_LINE:-<empty>}'" >&2
        exit 2
    }
    PROBE_VERSION_BINARY="$CLAUDE_BIN"
    printf 'claude version (start): %s\n' "$PROBE_VERSION_START_LINE"
    printf 'version-guard: binary=%s (resolved once, pinned for this run)\n' "$PROBE_VERSION_BINARY"
}

probe_version_guard_end() {
    [ -n "${PROBE_VERSION_START:-}" ] || {
        echo "version-guard: FAIL — probe_version_guard_end without a preceding probe_version_guard_begin" >&2
        exit 2
    }
    local end_line end_version
    end_line="$("$CLAUDE_BIN" --version 2>&1 | head -1)"
    end_version="$(probe_version_guard_token "${end_line:-}")"
    printf 'claude version (end):   %s\n' "$end_line"
    [ -n "$end_version" ] || {
        printf 'version-guard: verdict=UNDETERMINABLE start=%s end=<unparsable> binary=%s\n' \
            "$PROBE_VERSION_START" "$PROBE_VERSION_BINARY"
        echo "version-guard: FAIL — the pinned binary's --version stopped parsing mid-run; treating the run as straddled" >&2
        exit 2
    }
    if [ "$end_version" != "$PROBE_VERSION_START" ]; then
        printf 'version-guard: verdict=STRADDLED start=%s end=%s binary=%s\n' \
            "$PROBE_VERSION_START" "$end_version" "$PROBE_VERSION_BINARY"
        echo "ERROR: claude changed version mid-run ($PROBE_VERSION_START -> $end_version) — this run's evidence mixes two Claude versions and MUST NOT be pinned; re-run the suite (docs/notes/claude-contract-probes.md §Version guard)" >&2
        exit 1
    fi
    printf 'version-guard: verdict=single-version start=%s end=%s binary=%s\n' \
        "$PROBE_VERSION_START" "$end_version" "$PROBE_VERSION_BINARY"
    # Informational: the PIN above protects this run's evidence, but if the
    # host launcher has moved on since begin, the operator should re-pin
    # knowingly — the 2026-09-24 shape, made visible instead of silent.
    if command -v claude >/dev/null 2>&1; then
        local path_line path_version
        path_line="$(claude --version 2>&1 | head -1)"
        path_version="$(probe_version_guard_token "${path_line:-}")"
        if [ -n "$path_version" ] && [ "$path_version" != "$PROBE_VERSION_START" ]; then
            printf 'version-guard: note=host-claude-moved-on path-now=%s — this run measured the pinned %s only, but the host binary drifted mid-run; re-pin knowingly (docs/notes/claude-contract-probes.md §Version guard)\n' \
                "$path_version" "$PROBE_VERSION_START"
        fi
    fi
}
