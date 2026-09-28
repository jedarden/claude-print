#!/usr/bin/env bash
# Validate the provenance and coverage claims in a recorded cargo test report.
#
# Exit contract:
#   0 = evidence valid
#   1 = invalid, misleading, or incomplete evidence
#   2 = usage error or malformed evidence
#
# Both axes are derived from captured output. Remote output has the
# submitting/workflow/streaming prelude and exactly one terminal PASSED,
# FAILED, or timed-out line; local fallback output has a fallback banner
# preceded by a wrapper reason line. A clean extraction has no wrapper output
# at all, so its ordinary green cargo result is the local provenance shape.
# The accepted vocabulary is `[cargo-remote] submitting`,
# `[cargo-remote] workflow:`, `streaming logs from`, `[cargo-remote] PASSED`,
# `[cargo-remote] FAILED`, and `[cargo-remote] falling back to local`, with local reasons
# `no git remote`, `uncommitted changes detected`, `push failed`, or `submit
# failed`. Complete coverage is exactly `--tests` plus `--doc`.

set -euo pipefail

fail() {
    local rule=$1
    shift
    printf 'FAIL %s: %s\n' "$rule" "$*" >&2
    exit 1
}

usage() {
    printf 'usage: %s EVIDENCE_FILE|-\n' "$0" >&2
    exit 2
}

malformed() {
    printf 'FAIL fence: %s\n' "$*" >&2
    exit 2
}

[[ $# -eq 1 ]] || usage

tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/claude-print-verification.XXXXXX")
trap 'rm -rf "$tmp_dir"' EXIT
input=$tmp_dir/evidence.txt
prose=$tmp_dir/prose.txt
verified=$tmp_dir/verified.txt
output=$tmp_dir/output.txt

if [[ $1 == - ]]; then
    cat >"$input" || {
        printf 'usage: could not read evidence from stdin\n' >&2
        exit 2
    }
else
    [[ -f $1 && -r $1 ]] || usage
    cp -- "$1" "$input" || usage
fi

# Split the markdown-shaped recording and enforce its deliberately small
# grammar: prose plus exactly one verified command fence and one captured
# output fence.
if ! awk -v prose="$prose" -v verified="$verified" -v output="$output" '
BEGIN { region = "prose"; verified_open = 0; output_open = 0; closes = 0 }
{
    if ($0 == "```verified:") {
        if (region != "prose" || ++verified_open > 1) exit 10
        region = "verified"
        next
    }
    if ($0 == "```cargo-output") {
        if (region != "prose" || ++output_open > 1) exit 11
        region = "output"
        next
    }
    if ($0 == "```") {
        if (region == "prose") exit 12
        region = "prose"
        closes++
        next
    }
    if ($0 ~ /^```/) exit 13

    if (region == "prose") print > prose
    else if (region == "verified") print > verified
    else print > output
}
END {
    if (region != "prose" || verified_open != 1 || output_open != 1 || closes != 2)
        exit 20
}
' "$input"; then
    malformed 'expected exactly one ```verified: fence and one ```cargo-output fence'
fi

# Executed lines are shell input, not a place to record result or mode.
if grep -Eq '(^|[[:space:]])exit=[0-9]+|\((remote|local|complete|targeted)(,|\))' "$verified"; then
    if grep -Eq '(^|[[:space:]])exit=[1-9][0-9]*([[:space:]]|$)' "$verified"; then
        fail leg-outcome 'an executed command records a nonzero exit status'
    fi
    fail annotation-on-executed-line 'mode or exit annotations belong in prose, not the verified command block'
fi

# Derive the execution site from the wrapper's output vocabulary. Remote
# requires the complete prelude and exactly one terminal outcome; local
# fallback requires the fallback line and one wrapper reason line. A clean
# git-archive extraction never invokes cargo-remote, and is therefore local
# only when the captured cargo output contains a green test result. A partial
# or contradictory wrapper transcript is never guessed into either class.
tell_file=$tmp_dir/tells
awk '
BEGIN { submit=workflow=stream=passed=failed=timed=fall=reason=reason_before_fall=wrapper=0 }
{ if ($0 ~ /\[cargo-remote\]/) wrapper=1 }
/\[cargo-remote\][[:space:]]+submitting([[:space:]]|$)/ { submit=1 }
/\[cargo-remote\][[:space:]]+workflow:/ { workflow=1 }
/\[cargo-remote\][[:space:]]+streaming logs from/ { stream=1 }
/\[cargo-remote\][[:space:]]+PASSED([[:space:]]|$)/ { passed++ }
/\[cargo-remote\][[:space:]]+FAILED([[:space:]]|$)/ { failed++ }
/\[cargo-remote\][[:space:]]+timed([ -])out([[:space:]]|$)/ { timed++ }
/\[cargo-remote\][[:space:]]+falling back to local([[:space:]]|$)/ { fall=1 }
/\[cargo-remote\][[:space:]]+(no git remote|uncommitted changes detected|push failed|submit failed)([[:space:]:]|$)/ {
    reason=1
    if (!fall) reason_before_fall=1
}
END {
    terminal = passed + failed + timed
    remote_prelude = submit && workflow && stream
    remote_tells = submit || workflow || stream || terminal
    remote = remote_prelude && terminal == 1
    local = fall && reason && reason_before_fall
    contradictory = (terminal > 1) || (local && remote_tells)
    if (contradictory) site="ambiguous"
    else if (remote) site="remote"
    else if (local) site="local"
    else if (!wrapper) site="extraction"
    else site="unknown"
    printf "site=%s\nremote_failed=%d\nextraction=%d\n", site, (failed || timed), (!wrapper)
}' "$output" >"$tell_file"

site=$(sed -n 's/^site=//p' "$tell_file")
remote_failed=$(sed -n 's/^remote_failed=//p' "$tell_file")
case $site in
    remote|local) ;;
    extraction)
        grep -Eiq 'test result:[[:space:]]+ok([[:space:].]|$)|test result:.*[[:space:]]+[0-9]+ passed;[[:space:]]+0 failed' "$output" ||
            fail output-tells 'a no-banner capture is not a green cargo extraction transcript'
        site=local
        ;;
    ambiguous) fail output-tells 'the cargo-output fence contains contradictory remote and local provenance tells' ;;
    *) fail output-tells 'the cargo-output fence has no unambiguous remote, fallback, or extraction provenance' ;;
esac
[[ $remote_failed == 0 ]] || fail remote-outcome 'the captured run ended in failure or timed out'

# Extract the test commands from the verified fence. --tests and --doc are
# the complete split; --test NAME and --lib are targeted selectors. Other
# verification commands (fmt, clippy, build) do not affect this axis. The
# command region is executable shell, so a commented-out cargo test is not a
# leg and is called out explicitly instead of silently disappearing.
selectors=$tmp_dir/selectors

: >"$selectors"
while IFS= read -r command; do
    if [[ $command =~ ^[[:space:]]*#[[:space:]]*cargo[[:space:]]+test([[:space:]]|$) ]]; then
        printf '%s\n' '--commented' >>"$selectors"
    elif [[ $command =~ ^cargo[[:space:]]+test([[:space:]]|$) ]]; then
        if [[ $command =~ ^cargo[[:space:]]+test[[:space:]]+--tests[[:space:]]*$ ]]; then
            printf '%s\n' '--tests' >>"$selectors"
        elif [[ $command =~ ^cargo[[:space:]]+test[[:space:]]+--doc[[:space:]]*$ ]]; then
            printf '%s\n' '--doc' >>"$selectors"
        elif [[ $command =~ ^cargo[[:space:]]+test[[:space:]]+--lib[[:space:]]*$ ]]; then
            printf '%s\n' '--lib' >>"$selectors"
        elif [[ $command =~ ^cargo[[:space:]]+test[[:space:]]+--test(=|[[:space:]])([^[:space:]]+)[[:space:]]*$ ]]; then
            printf '%s\n' "--test ${BASH_REMATCH[2]}" >>"$selectors"
        else
            printf '%s\n' '--unnamed' >>"$selectors"
        fi
    fi
done <"$verified"

tests_count=0
doc_count=0
has_unnamed=0
has_commented=0
test_command_count=0
while IFS= read -r selector; do
    case $selector in
        --tests) ((tests_count += 1)); ((test_command_count += 1)) ;;
        --doc) ((doc_count += 1)); ((test_command_count += 1)) ;;
        --lib|--test\ *) ((test_command_count += 1)) ;;
        --unnamed) has_unnamed=1 ;;
        --commented) has_commented=1 ;;
    esac
done <"$selectors"
(( has_commented == 0 )) ||
    fail coverage-commented 'a commented-out cargo test is not an executed verification leg'
[[ $has_unnamed == 0 ]] || fail selector-unnamed 'a cargo test command has no named selector'

if (( tests_count > 1 || doc_count > 1 )); then
    fail coverage-duplicate 'the complete --tests and --doc legs may each appear only once'
fi

duplicate_selector=$(awk '/^--(tests|doc|lib)$|^--test / { if (++seen[$0] == 2) { print $0; exit } }' "$selectors")
[[ -z $duplicate_selector ]] ||
    fail coverage-duplicate "the verification selector ${duplicate_selector#--} is recorded more than once"

if (( tests_count == 1 && doc_count == 1 && test_command_count == 2 )); then
    coverage=complete
    required_results=2
elif (( tests_count == 0 && doc_count == 0 && test_command_count > 0 )); then
    coverage=targeted
    required_results=$test_command_count
else
    if (( tests_count || doc_count )); then
        fail coverage-mismatch 'complete coverage requires exactly one cargo test --tests leg and one cargo test --doc leg'
    fi
    fail coverage-mismatch 'the verified fence contains no cargo test selector'
fi

if [[ $coverage == targeted ]]; then
    while IFS= read -r selector; do
        if [[ $selector == --lib ]]; then
            [[ $(grep -Eic '(^|[^[:alnum:]_-])--lib([^[:alnum:]_-]|$)' "$prose") -gt 0 ]] ||
                fail selector-unnamed 'the targeted --lib selector is absent from the prose'
        else
            name=${selector#--test }
            [[ $name =~ ^[[:alnum:]_.-]+$ ]] ||
                fail selector-invalid "the targeted --test selector name ${name@Q} is not a cargo target name"
            grep -Eiq -- "(^|[^[:alnum:]_.-])--test(=|[[:space:]])${name}([^[:alnum:]_.-]|$)" "$prose" ||
                fail selector-unnamed "the targeted --test ${name} selector is absent from the prose"
        fi
    done < <(grep -E '^--lib$|^--test ' "$selectors")
fi

# Local captures have no remote terminal line, so a green cargo result must be
# visible. This also catches a failed local leg without a wrapper banner.
if grep -Eq '(^|[^[:alnum:]])(FAILED|timed out)([^[:alnum:]]|$)|test result:.*[1-9][0-9]* failed' "$output"; then
    fail remote-outcome 'the captured cargo output is not green'
fi
grep -Eiq 'test result:[[:space:]]+ok|test result:.*[[:space:]]+[0-9]+ passed' "$output" ||
    fail leg-outcome 'the captured cargo output contains no green test result'

# A green wrapper terminal is not enough to prove every recorded leg ran to
# completion. Cargo emits one `test result: ok` line per test command (and may
# emit additional lines for the targets behind --tests), so require at least
# one result for each command in the verified fence.
green_results=$(grep -Eic 'test result:[[:space:]]+ok([[:space:].]|$)' "$output" || true)
(( green_results >= required_results )) ||
    fail leg-outcome "the captured cargo output has ${green_results} green test result(s) for ${required_results} recorded test command(s)"

# Count claims in prose, excluding "no git remote", which is a fallback
# reason rather than a remote execution claim.
claims=$tmp_dir/claims
awk '
function count_word(text, word,    n, pos, rest, left, right) {
    n=0; rest=text
    while ((pos = index(tolower(rest), word)) != 0) {
        left = (pos == 1 ? " " : substr(rest, pos - 1, 1))
        right = (pos + length(word) > length(rest) ? " " : substr(rest, pos + length(word), 1))
        if (left !~ /[[:alnum:]_]/ && right !~ /[[:alnum:]_]/) n++
        rest = substr(rest, pos + length(word))
    }
    return n
}
{
    line=$0
    gsub(/[Nn][Oo][[:space:]]+[Gg][Ii][Tt][[:space:]]+[Rr][Ee][Mm][Oo][Tt][Ee]/, "", line)
    text = text " " line
}
END {
    print "remote=" count_word(text, "remote")
    print "local=" count_word(text, "local")
    print "complete=" count_word(text, "complete")
    print "targeted=" count_word(text, "targeted")
}' "$prose" >"$claims"

remote_claim=$(sed -n 's/^remote=//p' "$claims")
local_claim=$(sed -n 's/^local=//p' "$claims")
complete_claim=$(sed -n 's/^complete=//p' "$claims")
targeted_claim=$(sed -n 's/^targeted=//p' "$claims")

if (( remote_claim + local_claim == 0 )); then
    fail axis-unstated 'the prose does not state remote or local execution'
elif (( remote_claim + local_claim != 1 )); then
    fail site-mismatch 'the prose states remote/local execution more than once or both ways'
elif [[ $site == remote && $remote_claim != 1 ]] || [[ $site == local && $local_claim != 1 ]]; then
    fail site-mismatch "the prose claims a site different from the captured $site run"
fi

if (( complete_claim + targeted_claim == 0 )); then
    fail axis-unstated 'the prose does not state complete or targeted coverage'
elif (( complete_claim + targeted_claim != 1 )); then
    fail coverage-mismatch 'the prose states complete and targeted coverage ambiguously'
elif [[ $coverage == complete && $complete_claim != 1 ]] || [[ $coverage == targeted && $targeted_claim != 1 ]]; then
    fail coverage-mismatch "the prose claims $complete_claim/$targeted_claim coverage for a $coverage run"
fi

printf 'evidence valid: %s, %s\n' "$site" "$coverage"
