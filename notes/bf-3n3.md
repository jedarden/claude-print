# bf-3n3: claude-print.yaml NEEDLE agent config

## Summary

Verified and installed the `claude-print.yaml` NEEDLE agent adapter.

## What was done

The `claude-print.yaml` adapter file was created in a prior phase (commit 50b2132) and committed to the workspace. This bead validated the config and installed it to the global NEEDLE adapters directory.

### Installation

Copied `claude-print.yaml` to `/home/coding/.config/needle/adapters/claude-print.yaml` so NEEDLE can resolve it by name.

### Validation output

```
Adapter: claude-print
CLI:     claude-print (found at /home/coding/.local/bin/claude-print)
Version: claude-print 0.1.0 (wrapping claude 2.1.170 (Claude Code))
Input:   stdin
Probe:   exit 0 (1ms)
Tokens:  none configured
Transform: binary found
Status:  READY
```

## Config fields

- `input_method: stdin` — bead prompt delivered via stdin
- `output_transform: needle-transform-claude` — parses Claude JSON stream output
- `invoke_template` — `cd {workspace} && claude-print --model {model} --max-turns 30 --output-format json --dangerously-skip-permissions --no-inherit-hooks`
- `cost.type: use_or_lose` — subscription billing (no per-token charge)
- `model: claude-sonnet-4-6`
- `timeout_secs: 3600`

## Acceptance criteria

- NEEDLE accepts config without validation errors: **PASS** (Status: READY)
- AS-3 (agent spec test 3): **PASS** — stdin input method + needle-transform-claude confirmed working
