#!/usr/bin/env bash
# Install and enable the daily contract-drift watch
# (docs/notes/claude-contract-probes.md §Maintenance → Scheduled watch) as a
# systemd user timer — the billing-canary installer's pattern. Idempotent:
# re-running restores drifted copies and modes, so rerun it after editing
# scripts/contract-drift-watch.sh or the units.

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
LIBEXEC_DIR="$HOME/.local/libexec/claude-print"
SYSTEMD_USER_DIR="${XDG_CONFIG_HOME:-"$HOME/.config"}/systemd/user"

if ! command -v systemctl >/dev/null 2>&1; then
    printf '[ERROR] systemctl is required to install the contract-drift watch\n' >&2
    exit 1
fi

# The drift alert channel is the filed bead: without the CLI the watch still
# exits red, but the §Maintenance hand-off would regress to a manual to-do —
# exactly the blind window this timer exists to close.
if ! command -v bead >/dev/null 2>&1; then
    printf '[ERROR] bead is required to install the contract-drift watch (drift files a follow-up bead)\n' >&2
    exit 1
fi

# claude absent is a warning, not a blocker: every run would record
# INDETERMINATE (exit 2) and fail the unit loudly until it is installed.
if ! command -v claude >/dev/null 2>&1; then
    printf '[WARN] claude is not on PATH; the watch will fail indeterminate until it is installed\n' >&2
    printf '[WARN] Install it: curl -fsSL https://claude.ai/install.sh | bash\n' >&2
fi

if command -v loginctl >/dev/null 2>&1; then
    linger=$(loginctl show-user "$(id -un)" -p Linger --value 2>/dev/null || true)
    if [ "$linger" != yes ]; then
        printf '[WARN] User lingering is disabled; the timer only runs while the user manager is active.\n' >&2
        printf '[WARN] Ask an administrator to run: loginctl enable-linger %s\n' "$(id -un)" >&2
    fi
fi

install -d -m 700 "$LIBEXEC_DIR"
install -m 755 "$SCRIPT_DIR/contract-drift-watch.sh" "$LIBEXEC_DIR/contract-drift-watch.sh"
install -d -m 755 "$SYSTEMD_USER_DIR"
install -m 644 "$SCRIPT_DIR/claude-print-contract-drift-watch.service" \
    "$SYSTEMD_USER_DIR/claude-print-contract-drift-watch.service"
install -m 644 "$SCRIPT_DIR/claude-print-contract-drift-watch.timer" \
    "$SYSTEMD_USER_DIR/claude-print-contract-drift-watch.timer"

systemctl --user daemon-reload
systemctl --user enable --now claude-print-contract-drift-watch.timer

printf '[INFO] Installed and enabled claude-print-contract-drift-watch.timer\n'
printf '[INFO] Detection reads the checkout pinned by CLAUDE_PRINT_CONTRACT_REPO in the .service unit\n'
printf '[INFO] Result: %s\n' \
    "${XDG_STATE_HOME:-"$HOME/.local/state"}/claude-print/contract-drift-watch/last-result"
printf '[INFO] Logs: journalctl --user -u claude-print-contract-drift-watch.service\n'
systemctl --user list-timers claude-print-contract-drift-watch.timer --no-pager
