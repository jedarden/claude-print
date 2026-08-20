#!/bin/bash
# Install and enable the daily AS-4 canary as a systemd user timer.

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
LIBEXEC_DIR="$HOME/.local/libexec/claude-print"
SYSTEMD_USER_DIR="${XDG_CONFIG_HOME:-"$HOME/.config"}/systemd/user"

if ! command -v systemctl >/dev/null 2>&1; then
    printf '[ERROR] systemctl is required to install the billing canary\n' >&2
    exit 1
fi

if ! command -v claude-print >/dev/null 2>&1; then
    printf '[ERROR] claude-print is not installed on PATH\n' >&2
    exit 1
fi

if command -v loginctl >/dev/null 2>&1; then
    linger=$(loginctl show-user "$(id -un)" -p Linger --value 2>/dev/null || true)
    if [ "$linger" != yes ]; then
        printf '[WARN] User lingering is disabled; the timer only runs while the user manager is active.\n' >&2
        printf '[WARN] Ask an administrator to run: loginctl enable-linger %s\n' "$(id -un)" >&2
    fi
fi

install -d -m 700 "$LIBEXEC_DIR"
install -m 755 "$SCRIPT_DIR/billing-canary.sh" "$LIBEXEC_DIR/billing-canary.sh"
install -m 755 "$SCRIPT_DIR/check-billing.sh" "$LIBEXEC_DIR/check-billing.sh"
install -d -m 755 "$SYSTEMD_USER_DIR"
install -m 644 "$SCRIPT_DIR/claude-print-billing-canary.service" \
    "$SYSTEMD_USER_DIR/claude-print-billing-canary.service"
install -m 644 "$SCRIPT_DIR/claude-print-billing-canary.timer" \
    "$SYSTEMD_USER_DIR/claude-print-billing-canary.timer"

systemctl --user daemon-reload
systemctl --user enable --now claude-print-billing-canary.timer

printf '[INFO] Installed and enabled claude-print-billing-canary.timer\n'
printf '[INFO] Result: %s\n' \
    "${XDG_STATE_HOME:-"$HOME/.local/state"}/claude-print/billing-canary/last-result"
printf '[INFO] Logs: journalctl --user -u claude-print-billing-canary.service\n'
systemctl --user list-timers claude-print-billing-canary.timer --no-pager
