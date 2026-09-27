#!/usr/bin/env bash
# check-release-provenance.sh — live verifier for the Forgejo source-of-truth
# release workflow (README §"Repository & contributions",
# docs/notes/release-runbook.md §"Provenance verification", bead
# claudepr-c7ca74a6). The hermetic pins (tests/release_runbook_docs.rs,
# tests/release_provenance.rs) bind the docs and the CI WorkflowTemplate to
# each other; four of the workflow's invariants live OUTSIDE this checkout
# and can only be checked against the real hosts — this script owns those,
# so verifying the publication path is one command before cutting a release
# or after any hosting-side change.
#
# Four checks (each skippable with --skip):
#
#   origin      The checkout has exactly one remote, `origin`, and both its
#               fetch and push URLs are the canonical Forgejo URL parsed from
#               the CI WorkflowTemplate's clone step — never a GitHub URL and
#               never a second remote. A second remote is a second push
#               target even when it points at Forgejo, so its mere existence
#               is the client-side dual-push the mirror rule prohibits.
#
#   mirror      The Forgejo side carries the outward direction only: the repo
#               is not itself a pull mirror (nothing flows GitHub → Forgejo),
#               and exactly one push mirror exists, aimed at the GitHub repo,
#               with sync-on-commit enabled (the transport behind the
#               tag-before-release invariant). Needs a Forgejo credential
#               (see below); hosts without one run --skip mirror.
#
#   tags        Every tag on the GitHub mirror exists on Forgejo pointing at
#               the identical commit — the mirror is a strict downstream copy
#               of the tag namespace. A tag on GitHub only, or a SHA
#               divergence, is the mirror-prune / direct-publish failure mode
#               and fails. Tags only on Forgejo are the healthy in-flight
#               direction (pushed to the source of truth, mirror sync
#               pending) and are reported, not failed.
#
#   publishing  The tracked tree carries no `.github/` surface (GitHub
#               Actions are disabled org-wide; the sanctioned CI surface is
#               the Argo WorkflowTemplate + sensor + eventsource trio), and
#               the WorkflowTemplate never moves refs to GitHub: every
#               `git push` and every `git clone` names Forgejo, so the
#               template's only GitHub writes are the `gh release` asset
#               calls.
#
# Exit codes (the check-bead-hygiene.sh convention):
#   0  pass — every enabled check held
#   1  DRIFT — an enforceable invariant is violated; the script names every
#      violation it found and repairs nothing
#   2  cannot determine — a required tool is missing, the WorkflowTemplate
#      could not be parsed unambiguously, no Forgejo credential was available
#      for an enabled mirror check, or an ls-remote/API call failed. Fail
#      closed.
#
# Credentials: the Forgejo token is read with `git credential fill` and
# reaches curl through a stdin config (`curl -K -`). It is never a
# command-line argument, never echoed, and never written to disk. GitHub is
# queried anonymously (the mirror is public).
#
# Safety: strictly read-only — `git remote`, `git ls-remote`, `git credential
# fill`, and GET-only curl. Nothing is fetched, written, or repaired.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TEMPLATE="$REPO_ROOT/claude-print-ci-workflowtemplate.yml"

log_info() { printf '[INFO] %s\n' "$1"; }
log_warn() { printf '[WARN] %s\n' "$1"; }
log_error() { printf '[ERROR] %s\n' "$1" >&2; }

usage() {
    printf 'Usage: %s [--skip origin|mirror|tags|publishing]...\n' "$0" >&2
}

SKIP_ORIGIN=0
SKIP_MIRROR=0
SKIP_TAGS=0
SKIP_PUBLISHING=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        -h|--help) usage; exit 0 ;;
        --skip)
            if [ "$#" -lt 2 ]; then
                log_error "--skip needs a check name: origin|mirror|tags|publishing"
                exit 2
            fi
            case "$2" in
                origin) SKIP_ORIGIN=1 ;;
                mirror) SKIP_MIRROR=1 ;;
                tags) SKIP_TAGS=1 ;;
                publishing) SKIP_PUBLISHING=1 ;;
                *)
                    log_error "unknown check '$2' (origin|mirror|tags|publishing)"
                    exit 2
                    ;;
            esac
            shift 2
            ;;
        *)
            usage
            exit 2
            ;;
    esac
done

for tool in git curl python3; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        log_error "$tool is required but not on PATH"
        exit 2
    fi
done

# ── Parse the canonical URLs out of the CI WorkflowTemplate ─────────────────
#
# The template is the one place that already names both hosts for what they
# are: its `git clone`/`git push` steps carry the Forgejo URL (the source of
# truth) and its `gh release --repo` flags carry the GitHub slug (the
# artifact host). Deriving both here keeps this script honest when the repo
# moves: edit the template and every check follows, with ambiguity failing
# closed.

if [ ! -f "$TEMPLATE" ]; then
    log_error "CI WorkflowTemplate not found: $TEMPLATE"
    exit 2
fi

# Every https URL ending in .git, with any credentials stripped so the tag
# push (https://x-token:${FORGEJO_TOKEN}@git.ardenone.com/…) and the clone
# steps normalize to the same canonical URL. Exactly one non-GitHub URL must
# remain — the Forgejo source of truth.
FORGEJO_URL="$(grep -oE 'https://[A-Za-z0-9_./:$@{}-]+\.git\b' "$TEMPLATE" \
    | sed 's#//[^/@]*@#//#' | grep -v 'github' | sort -u || true)"
if [ "$(printf '%s' "$FORGEJO_URL" | grep -c . || true)" -ne 1 ]; then
    log_error "could not derive exactly one Forgejo URL from the WorkflowTemplate:"
    log_error "$FORGEJO_URL"
    exit 2
fi

FORGEJO_HOST="$(printf '%s' "$FORGEJO_URL" | sed -n 's#^https://\([^/]*\)/.*$#\1#p')"
FORGEJO_SLUG="$(printf '%s' "$FORGEJO_URL" | sed -n 's#^https://[^/]*/\(.*\)\.git$#\1#p')"
if [ -z "$FORGEJO_HOST" ] || [ -z "$FORGEJO_SLUG" ]; then
    log_error "could not split the Forgejo URL into host and slug: $FORGEJO_URL"
    exit 2
fi

GH_SLUG="$(sed -n 's#.*--repo \([A-Za-z0-9_.-]*/[A-Za-z0-9_.-]*\).*#\1#p' "$TEMPLATE" | sort -u)"
if [ "$(printf '%s' "$GH_SLUG" | grep -c . || true)" -ne 1 ]; then
    log_error "could not derive exactly one GitHub slug from the WorkflowTemplate's --repo flags:"
    log_error "$GH_SLUG"
    exit 2
fi
GH_URL="https://github.com/${GH_SLUG}.git"

log_info "Forgejo source of truth: $FORGEJO_URL"
log_info "GitHub artifact host:    https://github.com/$GH_SLUG"

FAILURES=0

fail() {
    log_error "$1"
    FAILURES=$((FAILURES + 1))
}

# ── origin ──────────────────────────────────────────────────────────────────
if [ "$SKIP_ORIGIN" -eq 1 ]; then
    log_info "origin: SKIPPED"
else
    REMOTES="$(git -C "$REPO_ROOT" remote || true)"
    REMOTE_COUNT="$(printf '%s' "$REMOTES" | grep -c . || true)"
    if [ "$REMOTE_COUNT" -ne 1 ] || [ "$REMOTES" != "origin" ]; then
        fail "origin: expected exactly one remote named 'origin'; found ($REMOTE_COUNT): $REMOTES"
    else
        FETCH_URL="$(git -C "$REPO_ROOT" remote get-url origin)"
        PUSH_URL="$(git -C "$REPO_ROOT" remote get-url --push origin)"
        if [ "$FETCH_URL" != "$FORGEJO_URL" ]; then
            fail "origin: fetch URL is $FETCH_URL, expected the canonical $FORGEJO_URL"
        elif [ "$PUSH_URL" != "$FORGEJO_URL" ]; then
            fail "origin: push URL is $PUSH_URL, expected the canonical $FORGEJO_URL"
        else
            log_info "origin: sole remote 'origin' pushes and fetches the Forgejo source of truth"
        fi
    fi
fi

# ── mirror ──────────────────────────────────────────────────────────────────
if [ "$SKIP_MIRROR" -eq 1 ]; then
    log_info "mirror: SKIPPED"
else
    # The token moves git credential store → curl stdin config. It is never
    # an argument (argv lands in ps and shell history) and never echoed.
    TOKEN="$(printf 'protocol=https\nhost=%s\n\n' "$FORGEJO_HOST" \
        | git credential fill 2>/dev/null | sed -n 's/^password=//p' || true)"
    if [ -z "$TOKEN" ]; then
        log_error "mirror: no Forgejo credential for $FORGEJO_HOST — run --skip mirror on hosts without one"
        exit 2
    fi
    forgejo_api() {
        printf 'header = "Authorization: token %s"\n' "$TOKEN" \
            | curl -fsS --max-time 30 -K - "https://$FORGEJO_HOST/api/v1/repos/$FORGEJO_SLUG$1"
    }

    if ! REPO_JSON="$(forgejo_api "" )"; then
        log_error "mirror: Forgejo API unreachable at https://$FORGEJO_HOST"
        exit 2
    fi
    # The repo must not be a pull mirror: the only sanctioned direction is
    # the push mirror below, so nothing can flow GitHub → Forgejo.
    if ! printf '%s' "$REPO_JSON" | python3 -c '
import json, sys
repo = json.load(sys.stdin)
if repo.get("mirror"):
    print(f"mirror: the Forgejo repo is itself a pull mirror — a GitHub → Forgejo direction the workflow forbids")
    sys.exit(1)
'; then
        fail "mirror: pull-mirror direction detected"
    else
        log_info "mirror: Forgejo repo is not a pull mirror (no GitHub → Forgejo direction)"
    fi

    if ! MIRRORS_JSON="$(forgejo_api "/push_mirrors")"; then
        log_error "mirror: push_mirrors query failed at https://$FORGEJO_HOST"
        exit 2
    fi
    if ! printf '%s' "$MIRRORS_JSON" | GH_URL="$GH_URL" python3 -c '
import json, os, sys
mirrors = json.load(sys.stdin)
expected = os.environ["GH_URL"]
if len(mirrors) != 1:
    print(f"expected exactly one push mirror, found {len(mirrors)}")
    sys.exit(1)
m = mirrors[0]
address = m.get("remote_address", "")
if address != expected:
    print(f"push mirror aims at {address!r}, expected {expected!r}")
    sys.exit(1)
if not m.get("sync_on_commit"):
    print("push mirror does not sync on commit — the tag-before-release window is unbounded")
    sys.exit(1)
'; then
        fail "mirror: push-mirror configuration drifted"
    else
        log_info "mirror: exactly one push mirror → https://github.com/$GH_SLUG, sync on commit"
    fi
fi

# ── tags ────────────────────────────────────────────────────────────────────
if [ "$SKIP_TAGS" -eq 1 ]; then
    log_info "tags: SKIPPED"
else
    # ls-remote on `origin` rides the clone's own credential helper; the
    # GitHub side is anonymous (the mirror is public).
    if ! FORGEJO_TAGS="$(git -C "$REPO_ROOT" ls-remote --tags origin 2>/dev/null | sed 's/\t/ /' | sort)"; then
        log_error "tags: git ls-remote against origin failed — run from a clone that can reach Forgejo"
        exit 2
    fi
    if ! GITHUB_TAGS="$(git -C "$REPO_ROOT" ls-remote --tags "$GH_URL" 2>/dev/null | sed 's/\t/ /' | sort)"; then
        log_error "tags: git ls-remote against $GH_URL failed"
        exit 2
    fi
    if [ -z "$FORGEJO_TAGS" ]; then
        log_error "tags: Forgejo advertises no tags — refusing to pass a vacuous comparison"
        exit 2
    fi
    # The invariant is directional: GitHub (the downstream copy) may hold
    # nothing Forgejo does not, at the identical commit. comm -13 is exactly
    # the GitHub-only-or-divergent set.
    VIOLATIONS="$(comm -13 <(printf '%s\n' "$FORGEJO_TAGS") <(printf '%s\n' "$GITHUB_TAGS") | sed '/^$/d' || true)"
    if [ -n "$VIOLATIONS" ]; then
        fail "tags: refs live on GitHub only, or diverge from Forgejo (mirror prune / direct-publish failure mode):"
        printf '%s\n' "$VIOLATIONS" | while IFS= read -r line; do
            log_error "  $line"
        done
    else
        FORGEJO_ONLY="$(comm -23 <(printf '%s\n' "$FORGEJO_TAGS") <(printf '%s\n' "$GITHUB_TAGS") | sed '/^$/d' || true)"
        if [ -n "$FORGEJO_ONLY" ]; then
            log_warn "tags: $(printf '%s\n' "$FORGEJO_ONLY" | grep -c .) ref(s) on Forgejo not yet mirrored (healthy in-flight direction)"
        fi
        log_info "tags: every GitHub tag exists on Forgejo at the identical commit"
    fi
fi

# ── publishing ──────────────────────────────────────────────────────────────
if [ "$SKIP_PUBLISHING" -eq 1 ]; then
    log_info "publishing: SKIPPED"
else
    GH_TRACKED="$(git -C "$REPO_ROOT" ls-files | grep '^\.github/' || true)"
    if [ -n "$GH_TRACKED" ]; then
        fail "publishing: .github/ surface is tracked (GitHub Actions are disabled org-wide; CI is the Argo trio):"
        printf '%s\n' "$GH_TRACKED" | while IFS= read -r line; do
            log_error "  $line"
        done
    else
        log_info "publishing: no .github/ surface in the tracked tree"
    fi

    FORGEJO_HOST_RE="$FORGEJO_HOST"
    BAD_PUSH="$(grep 'git push' "$TEMPLATE" | grep 'https://' | grep -v "$FORGEJO_HOST_RE" || true)"
    if [ -n "$BAD_PUSH" ]; then
        fail "publishing: the WorkflowTemplate pushes refs somewhere other than Forgejo:"
        printf '%s\n' "$BAD_PUSH" | while IFS= read -r line; do
            log_error "  $line"
        done
    else
        log_info "publishing: every WorkflowTemplate ref push targets Forgejo"
    fi

    BAD_CLONE="$(grep -A1 'git clone' "$TEMPLATE" | grep 'https://' | grep -v "$FORGEJO_HOST_RE" || true)"
    if [ -n "$BAD_CLONE" ]; then
        fail "publishing: the WorkflowTemplate clones from somewhere other than Forgejo:"
        printf '%s\n' "$BAD_CLONE" | while IFS= read -r line; do
            log_error "  $line"
        done
    else
        log_info "publishing: every WorkflowTemplate clone comes from Forgejo"
    fi
fi

if [ "$FAILURES" -gt 0 ]; then
    log_error "release provenance: $FAILURES drift finding(s) — see the [ERROR] lines above"
    exit 1
fi
log_info "release provenance: all enabled checks held"
