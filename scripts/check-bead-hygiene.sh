#!/usr/bin/env bash
# check-bead-hygiene.sh — executable owner for the bead-workflow rules in
# AGENTS.md §"Bead workflow" (and the root workspace CLAUDE.md) that until
# now lived only in prose: every repository change must be covered by an
# owning bead, and the durable checkpoint must be refreshed (`bead sync
# flush-only`) after bead mutations before the work is committed or pushed.
# Nothing enforced either rule — this check does, as a safe pre-commit /
# pre-push / CI gate. Pinned by tests/bead_hygiene.rs.
#
# Three checks, each skippable with --skip (repeatable):
#
#   backend     The backend declared in .needle.yaml (`bead_cli: backend:`)
#               must agree with the on-disk tells. bead-rs = .beads/config.json
#               and/or .beads/checkpoint/ and no bf-shaped files; bf =
#               .beads/config.yaml and/or flat .beads/issues.jsonl and no
#               config.json. A repository with no bead store at all passes
#               (the rules are scoped to stores), but a declaration without a
#               store, a store without a declaration, or a declaration
#               contradicted by the tells is a repo inconsistency and fails
#               closed — the "stop and re-check the backend declaration
#               before attempting any repair" posture AGENTS.md demands,
#               because running the wrong CLI against a store does not fail
#               cleanly (the 2026-08-14 SEAM incident).
#
#   ownership   Every commit in the range (default @{upstream}..HEAD) must
#               reference a bead of this workspace — the repo's
#               `<type>(claudepr-xxxx): …` subject convention — and every
#               referenced ID must resolve in the live store (`bead show`).
#               Uncommitted non-.beads changes are attributed the same way a
#               pre-commit caller names them: --worktree-bead <id> (resolved
#               like any reference), or a notes/<prefix>-<id>.md journal (the
#               filename IS the attribution); otherwise they are a violation.
#               The bead prefix is parsed from .beads/config.json, never
#               hard-coded, so the check follows the workspace identity.
#
#   checkpoint  Two halves. First, the gitleaks half: .beads/checkpoint/
#               files staged for commit (or already changed inside the range)
#               fail the check — committing the checkpoint in THIS repo is
#               rejected by the Forgejo pre-receive gitleaks hook
#               (generic-api-key entropy false positive on immutable
#               closed-bead close-reason prose, verified 2026-09-18 during
#               claudepr-2069ca6e), and once landed it poisons every later
#               push from the clone. The repo policy is therefore: flush-only,
#               then LEAVE the checkpoint drift uncommitted. Second, the
#               freshness half: the on-disk checkpoint must be current with
#               the live beads.db — probed non-destructively by copying
#               .beads/ into a throwaway temp workspace, running
#               `bead sync flush-only` THERE, and comparing checkpoint content
#               before/after. A fresh store syncs content-stable ("Checkpoint
#               already current", verified 2026-09-27); a divergence means
#               auto-publish was suppressed or failed and `bead sync
#               flush-only` must be run for real.
#
# Exit codes (the check-claude-version-bump.sh convention):
#   0  pass — every enabled check held
#   1  DRIFT — an enforceable rule is violated (stale checkpoint, change with
#      no owning bead, unresolvable bead reference, checkpoint files swept
#      into a commit). Remediation is mechanical; the script names it and
#      never applies it.
#   2  cannot determine / repo inconsistency — ambiguous backend evidence,
#      missing .needle.yaml declaration, missing git on PATH, no bead CLI for
#      a check that needs it, no upstream to diff against (pass --range), a
#      bad --range, or the freshness probe itself failed. Fail closed.
#
# Safety: strictly read-only on the real workspace. The ONLY place anything
# is written is the mktemp -d probe copy (removed on exit via trap), so
# detection never publishes, repairs, re-initializes, imports, or deletes
# anything in the store it is checking — the opposite of the bf-shaped
# "recovery" that destroyed SEAM's live data. `bead` is invoked for exactly
# two subcommands: `show <id>` (a read; output discarded so store content
# never lands in logs) and `sync flush-only` (on the copy only). bf-shaped
# stores are detected but not probed for freshness (out of scope; this repo
# is bead-rs, and a bf store here is precisely the inconsistency the backend
# check reports).

set -u

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NEEDLE_YAML="$REPO_ROOT/.needle.yaml"
BEADS_DIR="$REPO_ROOT/.beads"
CHECKPOINT_DIR="$BEADS_DIR/checkpoint"
CONFIG_JSON="$BEADS_DIR/config.json"

SKIP_BACKEND=0
SKIP_OWNERSHIP=0
SKIP_CHECKPOINT=0
RANGE=""
WORKTREE_BEAD=""

PROBE_DIR=""
cleanup() {
    if [ -n "$PROBE_DIR" ] && [ -d "$PROBE_DIR" ]; then
        rm -rf "$PROBE_DIR"
    fi
}
trap cleanup EXIT

fail_drift() {
    printf 'bead-hygiene: DRIFT: %s\n' "$1" >&2
    exit 1
}

fail_indeterminate() {
    printf 'bead-hygiene: INDETERMINATE: %s\n' "$1" >&2
    exit 2
}

note() {
    printf 'bead-hygiene: %s\n' "$1"
}

usage() {
    sed -n '2,71p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
}

while [ $# -gt 0 ]; do
    case "$1" in
        --range)
            [ $# -ge 2 ] || usage
            RANGE="$2"
            shift 2
            ;;
        --worktree-bead)
            [ $# -ge 2 ] || usage
            WORKTREE_BEAD="$2"
            shift 2
            ;;
        --skip)
            [ $# -ge 2 ] || usage
            case "$2" in
                backend) SKIP_BACKEND=1 ;;
                ownership) SKIP_OWNERSHIP=1 ;;
                checkpoint) SKIP_CHECKPOINT=1 ;;
                *) usage ;;
            esac
            shift 2
            ;;
        --help|-h)
            usage
            ;;
        *)
            printf 'unknown argument: %s\n' "$1" >&2
            usage
            ;;
    esac
done

for tool in git grep sed sort comm head sha256sum mktemp cp rm find; do
    command -v "$tool" >/dev/null 2>&1 ||
        fail_indeterminate "required tool '$tool' is not on PATH"
done

# ── shared helpers ───────────────────────────────────────────────────────────

# The workspace's bead-ID prefix (config.json: {"prefix":"claudepr",…}), or
# nothing when the store does not carry one.
bead_prefix() {
    [ -f "$CONFIG_JSON" ] || return 1
    local raw
    raw="$(grep -oE '"prefix"[[:space:]]*:[[:space:]]*"[^"]*"' "$CONFIG_JSON" 2>/dev/null | head -n 1)" || true
    [ -n "$raw" ] || return 1
    printf '%s\n' "$raw" | sed -E 's/.*:[[:space:]]*"//; s/"[[:space:]]*$//'
}

# Every <prefix>-<id> reference in the given (possibly multi-line) text, one
# per line. Liberal on the ID body ([0-9a-z]{4,}): bf-era IDs were base36 and
# bead-rs IDs are 8 hex, and an over-greedy match is not a false pass — it
# fails `bead show` resolution. A sentinel space is prepended to each line so
# every match carries exactly one leading boundary character, which is then
# stripped — a ref at line start and one after `(` extract identically, and
# `xclaudepr-…` is not a reference.
bead_refs_in() {
    printf '%s\n' "$1" |
        sed 's/^/ /' |
        grep -oE '[^0-9a-z-]'"$(bead_prefix)"'-[0-9a-z]{4,}' |
        sed -E 's/^.//'
}

# Does the ID resolve in the live store? Read-only; output discarded so bead
# titles/descriptions never land in logs. The caller checks for a bead CLI
# first so a missing binary is never mistaken for an unknown ID.
bead_resolves() {
    bead show "$1" >/dev/null 2>&1
}

# The commit range both ownership and checkpoint scope to: --range, or the
# current branch's upstream..HEAD.
ensure_range() {
    [ -n "$RANGE" ] && return 0
    local upstream
    upstream="$(git -C "$REPO_ROOT" rev-parse --abbrev-ref --symbolic-full-name '@{upstream}' 2>/dev/null || true)"
    if [ -z "$upstream" ]; then
        fail_indeterminate "no upstream is configured for the current branch — pass --range <base>..<head>"
    fi
    RANGE="$upstream..HEAD"
}

validate_range() {
    if ! git -C "$REPO_ROOT" rev-list --count "$RANGE" >/dev/null 2>&1; then
        fail_indeterminate "--range '$RANGE' is not a resolvable commit range"
    fi
}

# ── backend ──────────────────────────────────────────────────────────────────

declared_backend() {
    [ -f "$NEEDLE_YAML" ] || return 1
    # The file is flat; take the first `backend:` key, strip a trailing
    # comment, keep the bare value.
    grep -E '^[[:space:]]*backend:' "$NEEDLE_YAML" 2>/dev/null |
        head -n 1 |
        sed -E 's/#.*$//; s/^[[:space:]]*backend:[[:space:]]*//; s/[[:space:]]+$//'
}

check_backend() {
    local declared tells_rs tells_bf on_disk
    declared="$(declared_backend || true)"
    tells_rs=0
    tells_bf=0
    [ -f "$CONFIG_JSON" ] && tells_rs=1
    [ -d "$CHECKPOINT_DIR" ] && tells_rs=1
    if [ -f "$BEADS_DIR/config.yaml" ] || [ -f "$BEADS_DIR/issues.jsonl" ]; then
        tells_bf=1
    fi

    if [ ! -d "$BEADS_DIR" ] && [ "$tells_rs" -eq 0 ] && [ "$tells_bf" -eq 0 ]; then
        if [ -n "$declared" ]; then
            fail_indeterminate ".needle.yaml declares bead backend '$declared' but there is no .beads/ store — a declaration without a store is a repo inconsistency"
        fi
        note "backend: no bead store in this repository — bead-workflow rules do not apply"
        return 0
    fi

    if [ "$tells_rs" -eq 1 ] && [ "$tells_bf" -eq 1 ]; then
        fail_indeterminate "on-disk tells are ambiguous: bead-rs shapes (.beads/config.json / .beads/checkpoint/) AND bf shapes (.beads/config.yaml / .beads/issues.jsonl) are both present — stop and re-check the backend before running either CLI"
    fi
    if [ "$tells_rs" -eq 0 ] && [ "$tells_bf" -eq 0 ]; then
        fail_indeterminate "a .beads/ store directory exists but carries neither the bead-rs shapes (.beads/config.json, .beads/checkpoint/) nor the bf shapes (.beads/config.yaml, .beads/issues.jsonl)"
    fi

    if [ "$tells_rs" -eq 1 ]; then on_disk="bead-rs"; else on_disk="bf"; fi

    if [ -z "$declared" ]; then
        fail_indeterminate "on-disk store is $on_disk-shaped but .needle.yaml declares no 'backend:' — the declaration is authoritative (AGENTS.md) and its absence is a repo inconsistency"
    fi
    if [ "$declared" != "$on_disk" ]; then
        fail_indeterminate ".needle.yaml declares backend '$declared' but the on-disk tells say '$on_disk' — do not run either CLI against this store until the mismatch is resolved"
    fi
    note "backend: declared '$declared', on-disk tells agree"
}

# ── ownership ────────────────────────────────────────────────────────────────

# Uncommitted changes that need an owning bead: everything `git status`
# reports EXCEPT .beads/** (checkpoint drift and other bead-workflow state is
# expected to sit uncommitted in this repo) and notes/<prefix>-<id>.md
# journals (self-attributing by filename). $1 = the workspace bead prefix.
unowned_worktree_paths() {
    local line path
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        path="${line:3}"
        case "$path" in
            *' -> '*) path="${path##* -> }" ;;
        esac
        case "$path" in
            .beads/*) continue ;;
        esac
        case "$path" in
            notes/"$1"-*.md) continue ;;
        esac
        printf '%s\n' "$path"
        # --untracked-files=all: a freshly added notes/ journal would
        # otherwise collapse into `?? notes/` and dodge the exemption.
    done < <(git -C "$REPO_ROOT" status --porcelain --untracked-files=all 2>/dev/null)
}

check_ownership() {
    if [ ! -d "$BEADS_DIR" ]; then
        note "ownership: no bead store in this repository — nothing to check"
        return 0
    fi
    command -v bead >/dev/null 2>&1 ||
        fail_indeterminate "no 'bead' CLI on PATH — cannot resolve bead references"

    local prefix
    prefix="$(bead_prefix || true)"
    if [ -z "$prefix" ]; then
        fail_indeterminate "cannot parse the bead prefix from $CONFIG_JSON — cannot tell which <prefix>-<id> references are this workspace's beads"
    fi

    # Uncommitted changes: named via --worktree-bead, self-attributed
    # notes/ journals, or a violation.
    local unowned
    unowned="$(unowned_worktree_paths "$prefix")"
    if [ -n "$unowned" ] && [ -z "$WORKTREE_BEAD" ]; then
        fail_drift "uncommitted change(s) with no owning bead — commit them with the owning bead's ID in the subject, or pass --worktree-bead <id>:
$unowned"
    fi
    if [ -n "$WORKTREE_BEAD" ]; then
        case "$WORKTREE_BEAD" in
            "$prefix"-[0-9a-z]*) ;;
            *)
                fail_drift "--worktree-bead '$WORKTREE_BEAD' is not a $prefix-* bead ID"
                ;;
        esac
        if ! bead_resolves "$WORKTREE_BEAD"; then
            fail_drift "--worktree-bead '$WORKTREE_BEAD' does not resolve in the live store — check the ID (and that the store is current)"
        fi
        if [ -n "$unowned" ]; then
            note "ownership: uncommitted path(s) attributed to $WORKTREE_BEAD via --worktree-bead"
        fi
    fi

    ensure_range
    validate_range

    local subjects count unreferenced=""
    subjects="$(git -C "$REPO_ROOT" log --no-merges --format='%s' "$RANGE" 2>/dev/null || true)"
    count="$(printf '%s\n' "$subjects" | grep -c . || true)"
    if [ "$count" -eq 0 ]; then
        note "ownership: no commits in $RANGE"
        return 0
    fi

    while IFS= read -r subject; do
        [ -n "$subject" ] || continue
        if [ -z "$(bead_refs_in "$subject")" ]; then
            unreferenced+="  $subject"$'\n'
        fi
    done <<< "$subjects"
    if [ -n "$unreferenced" ]; then
        fail_drift "commit(s) in $RANGE reference no $prefix-* bead in their subject — every repository change must be covered by an owning bead (AGENTS.md):
$unreferenced"
    fi

    local ref unknown=""
    while IFS= read -r ref; do
        [ -n "$ref" ] || continue
        if ! bead_resolves "$ref"; then
            unknown+="  $ref"$'\n'
        fi
    done <<< "$(bead_refs_in "$subjects" | sort -u)"
    if [ -n "$unknown" ]; then
        fail_drift "bead reference(s) in $RANGE commit subjects do not resolve in the live store — check the ID, and run bead sync flush-only if the commit predates its checkpoint:
$unknown"
    fi
    note "ownership: $count commit(s) in $RANGE, every subject references a resolving bead"
}

# ── checkpoint ───────────────────────────────────────────────────────────────

check_checkpoint() {
    if [ ! -d "$BEADS_DIR" ]; then
        note "checkpoint: no bead store in this repository — nothing to check"
        return 0
    fi

    # Gitleaks half: the checkpoint must not be swept into a commit.
    local staged in_range
    staged="$(git -C "$REPO_ROOT" diff --cached --name-only -- .beads/checkpoint/ 2>/dev/null || true)"
    if [ -n "$staged" ]; then
        fail_drift ".beads/checkpoint/ file(s) are staged for commit — the Forgejo pre-receive gitleaks hook rejects checkpoint commits in this repo (generic-api-key false positive on closed-bead close-reason prose, 2026-09-18), and once landed they poison every later push. Unstage them (git restore --staged .beads/checkpoint) and leave the drift uncommitted:
$staged"
    fi

    ensure_range
    validate_range
    in_range="$(git -C "$REPO_ROOT" diff --name-only "$RANGE" -- .beads/checkpoint/ 2>/dev/null || true)"
    if [ -n "$in_range" ]; then
        fail_drift "commit(s) in $RANGE change .beads/checkpoint/ — checkpoint commits are rejected by the Forgejo gitleaks hook in this repo and poison later pushes; mixed-reset the unpushed commit (never force-push) and leave the drift uncommitted:
$in_range"
    fi

    # Freshness half: probe a throwaway COPY, never the live store.
    local declared
    declared="$(declared_backend || true)"
    if [ "$declared" != "bead-rs" ]; then
        note "checkpoint: freshness probe is bead-rs-specific and the store is not bead-rs — skipped"
        return 0
    fi
    command -v bead >/dev/null 2>&1 ||
        fail_indeterminate "no 'bead' CLI on PATH — cannot probe checkpoint freshness"
    if [ ! -d "$CHECKPOINT_DIR" ]; then
        fail_indeterminate "$CHECKPOINT_DIR is missing for a bead-rs store"
    fi

    # Content snapshot of the checkpoint tree: relative paths in stable order,
    # each with its content hash, so before/after compare bytes not mtimes.
    snapshot() {
        (cd "$1" && find .beads/checkpoint -type f -print 2>/dev/null | sort |
            while IFS= read -r f; do
                sha256sum "$f" 2>/dev/null || printf 'UNREADABLE %s\n' "$f"
            done)
    }

    PROBE_DIR="$(mktemp -d)" ||
        fail_indeterminate "mktemp -d failed — cannot build the freshness probe copy"
    cp -a "$BEADS_DIR" "$PROBE_DIR/.beads" ||
        fail_indeterminate "copying .beads/ into the probe workspace failed"
    [ -f "$NEEDLE_YAML" ] && cp -a "$NEEDLE_YAML" "$PROBE_DIR/.needle.yaml"

    local before after
    before="$(snapshot "$PROBE_DIR")"
    if ! (cd "$PROBE_DIR" && bead sync flush-only >/dev/null 2>&1); then
        fail_indeterminate "the probe's 'bead sync flush-only' failed inside the copy — a live .beads/ may have been mid-write; rerun, and treat a persistent failure as cannot-determine"
    fi
    after="$(snapshot "$PROBE_DIR")"

    if [ "$before" != "$after" ]; then
        fail_drift "the durable checkpoint is stale: 'bead sync flush-only' on a throwaway copy of .beads/ would change
$(diff <(printf '%s\n' "$before") <(printf '%s\n' "$after") | sed -n 's/^[<>] //p' | sed -n 's/^[^ ]*  //p' | sort -u | sed 's/^/  /')
Run: bead sync flush-only   (idempotent, db -> checkpoint; never commit the result in this repo)"
    fi
    note "checkpoint: probe sync of a throwaway copy is content-stable — the durable checkpoint is current; no checkpoint files staged or committed in range"
}

# ── main ─────────────────────────────────────────────────────────────────────

[ -d "$REPO_ROOT/.git" ] ||
    fail_indeterminate "$REPO_ROOT is not a git repository (no .git/) — this check is a repository check"

if [ "$SKIP_BACKEND" -eq 0 ]; then
    check_backend
fi
if [ "$SKIP_OWNERSHIP" -eq 0 ]; then
    check_ownership
fi
if [ "$SKIP_CHECKPOINT" -eq 0 ]; then
    check_checkpoint
fi

note "OK"
exit 0
