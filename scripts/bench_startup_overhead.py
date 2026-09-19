#!/usr/bin/env python3
"""Startup-overhead benchmark harness (ADR-005 / plan §"Benchmark Contract").

Measures wall-clock overhead from process start to prompt injection — the
arrival of the `prompt injected` line among the `--verbose` traces — for the
stateless (cold) path and the warm-pool path, under one controlled
environment: the same box, the same mock-claude backend, a throwaway HOME and
XDG_CONFIG_HOME, sequential (non-concurrent) samples, no network, no
credentials, no real `claude` install.

Why the timestamp is taken outside the process: the plan's contract defines
overhead as "process start → bracketed-paste write", logged at the
PROMPT_INJECTED transition. The stateless session's tracer is anchored at
process start, but the pooled session's tracer re-anchors at `run_pooled`
entry — after pool acquisition — so its internal `<ms>` values exclude the
acquisition cost. Timestamping the same stderr line from outside gives both
paths one clock and keeps the comparison honest. The process-internal trace
values are still captured per sample as diagnostic sub-metrics.

Scope: startup/prompt-injection overhead ONLY. This harness measures nothing
about model latency — mock-claude answers from a canned response with none of
real Claude Code's startup (MCP init, JS runtime warm-up) or inference cost —
so these numbers do not establish model-latency savings and do not estimate
production wall-clock savings in absolute terms. See
docs/notes/startup-overhead-benchmark.md for the recorded evidence and the
full scope statement.

Prerequisite (deliberately NOT run by this script — `cargo` on some hosts is
a CI-submitting wrapper):

    cargo build          # produces target/debug/claude-print and mock-claude

Usage:

    scripts/bench_startup_overhead.py [--bin-dir DIR] [--samples N]
        [--warmup N] [--pool-size N] [--timeout SECS] [--mode cold|warm|both]
        [--output FILE] [--self-check]

Defaults: bin-dir=target/debug, samples=10, warmup=3, pool-size=1,
timeout=60, mode=both, output=- (stdout).
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone

# The ADR-005 canonical trivial prompt.
PROMPT = "Reply with exactly one word: pong"

# A client sample must finish well inside this budget; the harness kills and
# fails the run otherwise (a wedged sample is evidence of a bug, not an
# outlier to wait out).
SAMPLE_BUDGET_SECS = 120

# Ceiling for the daemon to reach `settled and ready` (same value as the pool
# e2e tests: mock-claude settles in well under a second; the ceiling only
# absorbs a loaded box).
WARMUP_BUDGET_SECS = 90

# Ceiling for a daemon SIGTERM shutdown to produce an exit (per-worker SIGTERM
# grace is 2 s before SIGKILL).
SHUTDOWN_BUDGET_SECS = 20

# `[claude-print <ms>ms] <message>` — the --verbose trace format.
TRACE_RE = re.compile(r"^\[claude-print (\d+)ms\] (.*)$")


def percentile(sorted_values, pct):
    """Linear-interpolated percentile of an already-sorted list (numpy-style)."""
    if not sorted_values:
        raise ValueError("percentile of empty list")
    if len(sorted_values) == 1:
        return sorted_values[0]
    rank = (len(sorted_values) - 1) * pct / 100.0
    lo = int(rank)
    hi = min(lo + 1, len(sorted_values) - 1)
    frac = rank - lo
    return sorted_values[lo] * (1.0 - frac) + sorted_values[hi] * frac


def parse_trace_line(line):
    """Return (internal_ms, message) for a trace line, else None."""
    m = TRACE_RE.match(line.strip())
    if not m:
        return None
    return int(m.group(1)), m.group(2)


class LineTee:
    """Reads a pipe line-by-line, timestamping each line on arrival."""

    def __init__(self, pipe):
        self.pipe = pipe
        self.lines: list[tuple[float, str]] = []
        self.thread = None

    def start(self):
        import threading

        def reader():
            for raw in iter(self.pipe.readline, b""):
                text = raw.decode("utf-8", errors="replace").rstrip("\n")
                self.lines.append((time.monotonic(), text))

        self.thread = threading.Thread(target=reader, daemon=True)
        self.thread.start()
        return self

    def wait_for(self, needle, deadline_secs):
        """Block until a line containing `needle` arrives; return its arrival
        time (monotonic). Raises TimeoutError past the deadline."""
        deadline = time.monotonic() + deadline_secs
        seen = 0
        while time.monotonic() < deadline:
            while seen < len(self.lines):
                _, text = self.lines[seen]
                seen += 1
                if needle in text:
                    return self.lines[seen - 1][0]
            time.sleep(0.002)
        raise TimeoutError(f"no line containing {needle!r} within {deadline_secs}s")


def build_env(home, xdg_config):
    env = dict(os.environ)
    env["HOME"] = home
    env["XDG_CONFIG_HOME"] = xdg_config
    return env


def run_sample(claude_print, mock, pooled_socket, timeout_secs, env, label):
    """One timed invocation. Returns a sample dict; raises on any invalid run."""
    cmd = [claude_print, "--claude-binary", mock, "--verbose",
           "--output-format", "json", "--timeout", str(timeout_secs)]
    if pooled_socket is not None:
        cmd += ["--pool-socket", pooled_socket]
    cmd.append(PROMPT)

    t0 = time.monotonic()
    proc = subprocess.Popen(
        cmd,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        cwd=tempfile.gettempdir(),
    )
    err_tee = LineTee(proc.stderr).start()
    out_tee = LineTee(proc.stdout).start()
    try:
        t_inject = err_tee.wait_for("prompt injected", SAMPLE_BUDGET_SECS)
    except TimeoutError:
        proc.kill()
        proc.wait()
        raise
    try:
        code = proc.wait(timeout=SAMPLE_BUDGET_SECS)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
        raise
    t_done = time.monotonic()

    overhead_ms = (t_inject - t0) * 1000.0
    traces = [
        {"internal_ms": ms, "message": msg}
        for _, line in err_tee.lines
        if (parsed := parse_trace_line(line)) is not None
        for ms, msg in [parsed]
    ]
    stdout_text = "".join(text + "\n" for _, text in out_tee.lines)

    def internal_ms_of(fragment):
        for t in traces:
            if fragment in t["message"]:
                return t["internal_ms"]
        return None

    # Validity gates — a failed run aborts the benchmark rather than quietly
    # skewing the statistics (see "variance / outlier handling" in the doc).
    if code != 0:
        raise RuntimeError(
            f"{label}: exit {code}\nstderr:\n" + "\n".join(t for _, t in err_tee.lines)
        )
    driving_prewarmed = any(
        "driving prewarmed worker" in t["message"] for t in traces
    )
    if pooled_socket is not None and not driving_prewarmed:
        raise RuntimeError(
            f"{label}: pooled sample did not drive a prewarmed worker "
            "(quiet stateless fallback would pollute the warm numbers)\n"
            + "\n".join(t for _, t in err_tee.lines)
        )
    if pooled_socket is None and driving_prewarmed:
        raise RuntimeError(f"{label}: stateless sample unexpectedly drove a worker")
    if '"type":"result"' not in stdout_text.replace(" ", ""):
        raise RuntimeError(f"{label}: stdout is not a result object:\n{stdout_text}")

    return {
        "overhead_ms": round(overhead_ms, 3),
        "wall_ms": round((t_done - t0) * 1000.0, 3),
        "exit_code": code,
        "internal_trace_ms": {
            "child_forked": internal_ms_of("child forked"),
            "fifo_opened": internal_ms_of("fifo opened"),
            "prompt_injected": internal_ms_of("prompt injected"),
            "drove_prewarmed_worker": driving_prewarmed,
        },
        "traces": traces,
    }


class Daemon:
    """A `claude-print serve` daemon over a temp socket, with stderr teed."""

    def __init__(self, claude_print, mock, socket_path, pool_size, env):
        self.socket_path = str(socket_path)
        self.proc = subprocess.Popen(
            [claude_print, "--claude-binary", mock, "serve",
             "--pool-size", str(pool_size), "--socket", self.socket_path,
             "--verbose"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            env=env,
            cwd=tempfile.gettempdir(),
        )
        self.tee = LineTee(self.proc.stderr).start()
        self.ready_seen = 0
        self.started_at = time.monotonic()
        self.first_ready_ms = None

    def wait_ready(self, total_ready, budget_secs=WARMUP_BUDGET_SECS):
        """Block until `settled and ready` has been logged `total_ready` times
        in total (counts are cumulative across warmup and replacement)."""
        deadline = time.monotonic() + budget_secs
        while time.monotonic() < deadline:
            count = sum(1 for _, text in self.tee.lines if "settled and ready" in text)
            if count >= total_ready:
                if self.first_ready_ms is None:
                    self.first_ready_ms = (time.monotonic() - self.started_at) * 1000.0
                self.ready_seen = count
                return
            time.sleep(0.005)
        raise TimeoutError(
            f"daemon reached only {self.ready_seen} settled workers, "
            f"wanted {total_ready}, within {budget_secs}s"
        )

    def shutdown(self):
        if self.proc.poll() is not None:
            return self.proc.returncode
        self.proc.send_signal(signal.SIGTERM)
        try:
            self.proc.wait(timeout=SHUTDOWN_BUDGET_SECS)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
            return None
        return self.proc.returncode

    def daemon_log(self):
        return [text for _, text in self.tee.lines]


def summarize(samples):
    values = sorted(s["overhead_ms"] for s in samples)
    mean = statistics.fmean(values)
    stdev = statistics.stdev(values) if len(values) > 1 else 0.0
    return {
        "n": len(values),
        "mean_ms": round(mean, 3),
        "stdev_ms": round(stdev, 3),
        "min_ms": round(values[0], 3),
        "p50_ms": round(percentile(values, 50), 3),
        "p90_ms": round(percentile(values, 90), 3),
        "p95_ms": round(percentile(values, 95), 3),
        "max_ms": round(values[-1], 3),
        "cv_pct": round(100.0 * stdev / mean, 2) if mean else None,
    }


def cpu_model():
    try:
        with open("/proc/cpuinfo", encoding="utf-8") as f:
            for line in f:
                if line.startswith("model name"):
                    return line.split(":", 1)[1].strip()
    except OSError:
        pass
    return platform.processor() or "unknown"


def git_commit():
    try:
        return subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            capture_output=True, text=True, timeout=10,
        ).stdout.strip() or None
    except (OSError, subprocess.SubprocessError):
        return None


def self_check():
    """Deterministic sanity pins for the helpers (no subprocesses)."""
    assert percentile([1.0], 95) == 1.0
    assert percentile([1.0, 2.0, 3.0, 4.0], 50) == 2.5
    assert percentile(sorted(range(101)), 0) == 0.0
    assert percentile(sorted(range(101)), 100) == 100.0
    assert abs(percentile(sorted(range(101)), 95) - 95.0) < 1e-9
    assert parse_trace_line("[claude-print 1234ms] prompt injected") == (
        1234, "prompt injected")
    assert parse_trace_line("[claude-print 0ms] fifo opened") == (0, "fifo opened")
    assert parse_trace_line("claude-print: something") is None
    assert parse_trace_line("[claude-print abc] x") is None
    print("self-check ok")
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--bin-dir", default="target/debug",
                    help="directory holding claude-print and mock-claude")
    ap.add_argument("--samples", type=int, default=10,
                    help="recorded samples per mode (default 10)")
    ap.add_argument("--warmup", type=int, default=3,
                    help="discarded warmup samples per mode (default 3)")
    ap.add_argument("--pool-size", type=int, default=1)
    ap.add_argument("--timeout", type=int, default=60,
                    help="per-invocation --timeout seconds passed to clients")
    ap.add_argument("--mode", choices=["cold", "warm", "both"], default="both")
    ap.add_argument("--output", default="-",
                    help="JSON output path ('-' = stdout)")
    ap.add_argument("--self-check", action="store_true")
    args = ap.parse_args()

    if args.self_check:
        return self_check()

    claude_print = os.path.join(args.bin_dir, "claude-print")
    mock = os.path.join(args.bin_dir, "mock-claude")
    for path in (claude_print, mock):
        if not os.path.exists(path):
            sys.exit(f"missing {path}; run `cargo build` first (see module docstring)")

    version = subprocess.run(
        [claude_print, "--version"], capture_output=True, text=True, timeout=30
    )
    mock_version = subprocess.run(
        [mock, "--version"], capture_output=True, text=True, timeout=30
    )

    tmp_home = tempfile.mkdtemp(prefix="claude-print-bench-home.")
    tmp_config = tempfile.mkdtemp(prefix="claude-print-bench-cfg.")
    os.makedirs(os.path.join(tmp_config, "claude-print"), exist_ok=True)
    # An empty config: no real host config (model defaults, timeouts, hook
    # inheritance) may steer a run.
    with open(os.path.join(tmp_config, "claude-print", "config.toml"), "w") as f:
        f.write("")
    env = build_env(tmp_home, tmp_config)

    result = {
        "schema": "claude-print/startup-overhead-benchmark/1",
        "measured_at_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "scope": (
            "startup/prompt-injection overhead only (process start -> "
            "'prompt injected' verbose trace); does NOT establish "
            "model-latency savings"
        ),
        "environment": {
            "host": os.uname().nodename,
            "kernel": platform.release(),
            "cpu": cpu_model(),
            "python": platform.python_version(),
            "git_commit_at_measurement": git_commit(),
        },
        "versions": {
            "claude_print": version.stdout.strip() or version.stderr.strip(),
            "claude_backend": mock_version.stdout.strip(),
            "claude_backend_note": "mock-claude test fixture, not real Claude Code",
        },
        "harness": {
            "argv": sys.argv,
            "bin_dir": args.bin_dir,
            "claude_print_path": claude_print,
            "mock_claude_path": mock,
        },
        "config": {
            "prompt": PROMPT,
            "output_format": "json",
            "invocation_timeout_secs": args.timeout,
            "backend": "mock-claude (hermetic: no credentials, no network)",
            "home": "throwaway temp dir (same for daemon and clients)",
            "xdg_config_home": "throwaway temp dir with an empty config.toml",
            "concurrency": "sequential — one client at a time, idle box required",
        },
        "method": {
            "overhead_definition": (
                "wall-clock from just before client spawn to arrival of the "
                "'prompt injected' line on the client's stderr (external "
                "clock; the pooled session's internal tracer re-anchors at "
                "run_pooled entry, after acquisition)"
            ),
            "warmup_policy": (
                f"{args.warmup} discarded samples per mode before recording; "
                "warm mode additionally waits for the daemon to log "
                "'settled and ready' for every worker up front AND again "
                "after every sample (each driven worker is destroyed and "
                "replaced) before the next sample starts"
            ),
            "outlier_policy": (
                "no sample is discarded or trimmed; any invalid run (nonzero "
                "exit, missing 'prompt injected' trace, pooled sample without "
                "a 'driving prewarmed worker' trace, or stdout that is not a "
                "result object) aborts the benchmark with the full log; p95 "
                "and cv_pct are reported so dispersion stays visible"
            ),
        },
        "pool": None,
        "results": {},
    }

    modes = [m for m in ("cold", "warm") if args.mode in (m, "both")]

    daemon = None
    try:
        if "warm" in modes:
            socket_path = os.path.join(tmp_config, "bench-pool.sock")
            daemon = Daemon(claude_print, mock, socket_path, args.pool_size, env)
            daemon.wait_ready(args.pool_size)
            result["pool"] = {
                "socket": "temp dir/bench-pool.sock",
                "pool_size": args.pool_size,
                "daemon_initial_warmup_ms": round(daemon.first_ready_ms, 3),
                "daemon_warmup_note": (
                    "daemon start -> first 'settled and ready' log; this is "
                    "the per-daemon cost ADR-005 amortizes across invocations"
                ),
            }

        for mode in modes:
            pooled_socket = daemon.socket_path if mode == "warm" else None
            label = f"[{mode}]"
            # Each drive destroys its worker and the daemon replaces it; the
            # threshold must strictly increase with every drive or the next
            # sample can start while the replacement is still settling — with
            # pool_size=1 the pool is then empty and the client either gets
            # PoolFull (stateless fallback, caught by the sample gate) or a
            # queued acquire that would pollute the warm numbers.
            drives = 0
            for j in range(args.warmup):
                run_sample(claude_print, mock, pooled_socket, args.timeout,
                           env, f"{label} warmup {j + 1}/{args.warmup}")
                drives += 1
                if mode == "warm":
                    daemon.wait_ready(args.pool_size + drives)
            samples = []
            for i in range(args.samples):
                samples.append(
                    run_sample(claude_print, mock, pooled_socket, args.timeout,
                               env, f"{label} sample {i + 1}/{args.samples}")
                )
                drives += 1
                if mode == "warm":
                    # The driven worker is released and replaced; wait until
                    # the replacement is ready so every recorded warm sample
                    # acquires a settled worker (steady-state warm path).
                    daemon.wait_ready(args.pool_size + drives)
            result["results"][mode] = {
                "path": ("warm pool (--pool-socket; prewarmed worker acquired "
                         "over the unix socket)"
                         if mode == "warm" else
                         "stateless cold (no pool; full fork/trust/startup)"),
                "samples": samples,
                "summary": summarize(samples),
            }

        if daemon is not None:
            code = daemon.shutdown()
            result["pool"]["daemon_exit_code"] = code
            result["pool"]["socket_removed_after_shutdown"] = not os.path.exists(
                daemon.socket_path)
            if code != 0:
                raise RuntimeError(
                    f"daemon exited {code} on SIGTERM:\n"
                    + "\n".join(daemon.daemon_log()))
    finally:
        if daemon is not None and daemon.proc.poll() is None:
            daemon.shutdown()
        shutil.rmtree(tmp_home, ignore_errors=True)
        shutil.rmtree(tmp_config, ignore_errors=True)

    payload = json.dumps(result, indent=2)
    if args.output == "-":
        print(payload)
    else:
        with open(args.output, "w") as f:
            f.write(payload + "\n")
        print(f"wrote {args.output}")
        for mode, res in result["results"].items():
            s = res["summary"]
            print(f"{mode}: n={s['n']} mean={s['mean_ms']}ms p50={s['p50_ms']}ms "
                  f"p95={s['p95_ms']}ms max={s['max_ms']}ms cv={s['cv_pct']}%")
    return 0


if __name__ == "__main__":
    sys.exit(main())
