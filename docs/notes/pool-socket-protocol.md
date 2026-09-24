# Pool Socket Protocol — Wire Specification (v1)

Normative specification of the ADR-005 warm-PTY-pool wire protocol as
implemented by `src/pool.rs`. This is the contract both ends of the socket
honor today; every behavioral claim here is pinned by a test named in the
[Test map](#test-map). The prose contract (fallback classification, budget
rules) lives in the `src/pool.rs` module header and ADR-005
(`docs/plan/plan.md`); this note is the byte-level complement — frames,
ordering, limits, and what each side owes the other when something is wrong.

Protocol version: **v1** — the version described here is the only version
that has ever shipped. See [Versioning and compatibility](#versioning-and-compatibility)
for what that means when the two ends of the socket are not the same build.

This note is the internals document. The operator-facing workflow — starting
`serve`, choosing the socket and pool size, invoking `--pool-socket`,
shutdown, permissions, and the fallback-vs-hard-failure policy in operator
terms — lives in the README's
[Warm PTY pool (ADR-005)](../../README.md#warm-pty-pool-adr-005) section;
`AGENTS.md` §"Pool operations" carries the same quick reference for repo
agents.

## Transport

* **Socket:** `AF_UNIX` / `SOCK_STREAM`. Path is chosen by whoever starts the
  daemon (`serve --socket <path>`, default `/tmp/claude-print-pool.sock`,
  `DEFAULT_SOCKET_PATH`); clients name the same path with
  `--pool-socket <path>`.
* **Socket node permissions:** `0600`, owner-only. The daemon narrows the
  process umask to `0077` across `bind(2)` and then sets the mode explicitly,
  so the node is never world-connectable even for one instant
  (`PoolServer::bind_socket`). Anyone who can connect to the socket can
  acquire a warmed `claude` worker — the permission is the access control.
* **Stale node handling:** a daemon that binds replaces any existing node at
  the path. At shutdown it unlinks the node **only** if it still resolves to
  the `(dev, ino)` it created and is still a socket — a path that was taken
  over by another daemon (or replaced by any other file) is left exactly as
  found (`PoolServer::cleanup`).
* **Concurrency:** the daemon accepts concurrently (one thread per
  connection); the pool itself is serialized behind the manager mutex.
  Workers are handed out first-come-first-served; there is no queue.

## Framing

Every JSON frame on the wire — request **and** response — is:

```
+---------------------+--------------------------------+
| length: u32, big-   | JSON payload, exactly `length` |
| endian, 4 bytes     | bytes long, UTF-8              |
+---------------------+--------------------------------+
```

* **Length cap:** `length` must be ≤ `MAX_REQUEST_BYTES` = 64 KiB. The cap is
  enforced by **both** sides on frames they receive (the daemon on requests,
  the client on responses). The wire length is an unbounded `u32`; without
  the cap a hostile 4-byte prefix could pin up to 4 GiB of peer memory.
  A frame whose prefix exceeds the cap is a protocol violation — the
  receiving side closes the connection and sends nothing further.
* **One request per connection.** The daemon reads exactly one request frame,
  answers, and closes. A client that wants a second exchange (e.g. release
  after acquire) opens a new connection. A client may also close without
  sending anything; that is a clean no-op for the daemon, not an error.
* Frames are plain length-prefixing, not self-delimiting: a peer that sends a
  body shorter than its prefix claims stalls the receiver until the
  connection closes or the receiver's read timeout fires (below).

## Request frames (client → daemon)

Requests are internally tagged JSON objects: the `type` field selects the
variant, spelled `snake_case`.

### `acquire`

```json
{"type": "acquire", "timeout_secs": 60}
```

| Field | Type | Required | Meaning |
|---|---|---|---|
| `type` | string | yes | exactly `"acquire"` |
| `timeout_secs` | u64 | no (default `60`) | how long the caller is willing to wait for a worker |

`timeout_secs` is **advisory** in v1: the current daemon never queues — it
answers immediately, either with an assignment or with a `pool_full` refusal
— so it reads the field and ignores it. Clients still send it because it is
part of the frame and a future queuing daemon is within its rights to honor
it. A hand-rolled client MAY omit the field; the daemon defaults it to 60.

### `release`

```json
{"type": "release", "worker_id": "0e0c3f0e-..."}
```

| Field | Type | Required | Meaning |
|---|---|---|---|
| `type` | string | yes | exactly `"release"` |
| `worker_id` | string | yes | the id the client received in `worker_assigned` |

Releasing destroys the worker; it is never handed to a second client
(INV-9). The daemon spawns a replacement at its next maintain tick.

### Anything else

An unparseable body, a body that is not an object, an unknown `type` tag, or
a missing required field is a **malformed request**: the daemon closes the
connection **without sending any reply frame** (it logs one
`Connection error: ...` line on its stderr). There is deliberately no
`error` frame for requests the daemon could not parse — a peer that cannot
speak v1 gets a closed socket, not a negotiation.

## Response frames (daemon → client)

### `worker_assigned`

```json
{
  "type": "worker_assigned",
  "worker_id": "0e0c3f0e-...",
  "message": "Worker ready",
  "stop_fifo": "/tmp/claude-print-<daemon-pid>-xxxx/stop.fifo",
  "pid": 12345,
  "cwd": "/working/dir"
}
```

| Field | Type | Present | Meaning |
|---|---|---|---|
| `type` | string | always | exactly `"worker_assigned"` |
| `worker_id` | string | always, non-empty | daemon's id for the worker; echo it back to `release` it |
| `message` | string | always | human-readable, informational (`"Worker ready"`); clients must not branch on it |
| `stop_fifo` | string | v1 assignment: non-empty | daemon-side Stop FIFO the worker's relay hook writes its payload into; the client reads the Stop payload from here |
| `pid` | u32 | v1 assignment: non-zero | worker process id; feeds the client watchdog (which still never signals it — the worker is daemon-owned) |
| `cwd` | string | v1 assignment: non-empty | directory the worker was spawned in (the daemon's); the stream-json reader resolves this invocation's transcript under this cwd's `~/.claude/projects/` slug — bound via the per-worker identity file, with directory discovery only as the identity-less fallback (see [Per-worker hook artifacts and session binding](#per-worker-hook-artifacts-and-session-binding)) |

`stop_fifo`, `pid`, and `cwd` are declared with `serde(default)` so the frame
**parses** without them. That tolerance exists for parsing only — it is what
lets a current client produce a precise diagnostic about a legacy daemon. A
client that receives an assignment missing any of the four required values
(`worker_id` non-empty, `stop_fifo` non-empty, `pid` non-zero, `cwd`
non-empty) treats the acquire as a **protocol failure**, with an error
message naming the missing field (`AcquiredWorker::validate_parts`):

* empty `worker_id` → `worker_assigned carried an empty worker_id`
* empty `stop_fifo` → `worker_assigned carried no stop_fifo — the daemon predates the Stop-FIFO handoff this client requires`
* `pid == 0` → `worker_assigned carried no worker pid`
* empty `cwd` → `worker_assigned carried no worker cwd`

The PTY master fd accompanies this frame as ancillary data — see
[SCM\_RIGHTS fd transfer](#scm_rights-fd-transfer). An assignment is only
usable with its fd; the client receives the fd **before** validating the
frame fields, so a broken transfer is reported as exactly that.

**Release replies reuse this frame type.** A successful release is answered
`worker_assigned` with `"message": "Worker released"` and all three payload
fields empty/zero. The frame type reuse is a v1 quirk; the client treats any
`worker_assigned` as success on the release exchange and ignores the payload
(in fact the client's release path does not parse the reply at all — the
teardown is already triggered, the reply is drained only so the daemon's
connection thread completes cleanly).

### `error`

```json
{"type": "error", "error": "Pool full - no ready workers", "code": "pool_full"}
```

| Field | Type | Meaning |
|---|---|---|
| `type` | string | exactly `"error"` |
| `error` | string | human-readable detail; may change between releases, never parse it |
| `code` | string | machine-readable refusal class, see table |

The v1 code registry is closed — exactly these five, spelled `snake_case`:

| Code | Emitted by the v1 daemon when | Client classification |
|---|---|---|
| `pool_full` | acquire with no Ready worker | `PoolUnavailable` → stateless fallback |
| `shutting_down` | acquire or release while shutdown is requested | `PoolUnavailable` → stateless fallback |
| `invalid_worker_id` | release naming an id the daemon does not hold | `PoolUnavailable` → stateless fallback |
| `acquire_timeout` | **reserved** — defined, never emitted by v1 (the daemon does not queue) | `PoolUnavailable` → stateless fallback |
| `internal_error` | **reserved** — defined, never emitted by v1 (unparseable input closes the connection instead) | `PoolUnavailable` → stateless fallback |

An unknown `code` value is a **parse failure** on the client (the registry is
a closed serde enum), which classifies as a hard protocol failure, not a
fallback — see [Versioning](#versioning-and-compatibility), rule R4.

There is no reply frame of any kind for a malformed request (see above); an
`error` frame is only ever sent in answer to a well-formed request the daemon
declines.

## SCM\_RIGHTS fd transfer

The `worker_assigned` JSON frame is immediately followed by one `sendmsg(2)`
carrying the worker's PTY master fd as ancillary data:

* **Ordering:** always after the complete JSON frame, never before or
  interleaved. A client reads the length-prefixed JSON first, then performs
  one `recvmsg(2)` for the fd.
* **In-band payload:** the `sendmsg` carries exactly one byte of regular data
  (a `NUL`). It exists because some platforms dislike empty-data control
  messages; clients must drain it as part of the `recvmsg` and MUST NOT
  interpret it as protocol data. It can never be mistaken for a frame: it
  sits outside the JSON frame's length prefix.
* **Control message:** one `SCM_RIGHTS` cmsg (`SOL_SOCKET`, `SCM_RIGHTS`)
  carrying exactly one fd. A client walks the kernel-formatted cmsg list and
  takes the first usable `SCM_RIGHTS` fd (payload ≥ `sizeof(int)`, fd ≥ 0).
* **Close-on-exec:** the client receives with `MSG_CMSG_CLOEXEC`; the
  transferred fd can never leak into an `exec`ed child. (The daemon's own
  fds are CLOEXEC from creation.)
* **Failure semantics:** data arriving with no usable fd in the control
  buffer is an immediate protocol failure — the client does not wait for a
  second message. A daemon that sends the assignment frame and then dies (or
  closes) before the `sendmsg` produces the same failure via EOF/timeout.
  The kernel discards an in-flight, never-received fd when the receiving
  socket closes, so a client that fails here leaks nothing.
* **What the fd is:** the master end of the worker's PTY. It is a capability
  to the worker — poll it for output, write the prompt to it, close it when
  done (the `AcquiredWorker` owner closes it after the release exchange).

## Per-worker hook artifacts and session binding

The `worker_assigned` frame names one artifact of a per-worker set. Each worker
owns a daemon-side hook temp dir (one `HookInstaller` per worker at
`create_worker`, named with the daemon's pid):

```
<TMPDIR>/claude-print-<daemon-pid>-<rand>/
├── settings.json         # the per-worker --settings file (relay hooks only)
├── hook.sh               # Stop relay → stop.fifo
├── identity.sh           # UserPromptSubmit relay → session-identity.json
├── stop.fifo             # what the frame's stop_fifo field names
└── session-identity.json # per-worker session identity, written at prompt submission
```

The worker's claude runs with `--settings <that>/settings.json` and
`--setting-sources=` (empty), so pool workers never fire user hooks — the
isolation behavior of the stateless `--no-inherit-hooks` mode, applied
unconditionally because per-invocation flags cannot reach an already-running
worker. Full hook mechanics, payload shapes, and the binding ladder:
`docs/notes/hook-design.md`.

* **Two relays per worker.** The daemon installs the same two relay hooks the
  stateless client uses: Stop → `stop.fifo` (the payload the client reads from
  the frame's `stop_fifo` to learn the turn finished) and UserPromptSubmit →
  `session-identity.json` (the identity the client's stream-json reader binds
  to, written the instant the prompt is submitted — before any assistant
  transcript event exists).
* **The identity path is derived, not delivered.** The frame carries only
  `stop_fifo`; the client reconstructs the identity file path as its sibling —
  `stop_fifo().with_file_name("session-identity.json")` (`SESSION_IDENTITY_FILE`).
  Both artifacts share a directory because both relays are generated by the
  same installer; the sibling layout is therefore part of this contract, not
  an implementation detail, and is pinned on the daemon side
  (`src/hook.rs::identity_path_is_stop_fifo_sibling`) and exercised through
  the client derivation in every pooled stream-json e2e below.
* **Why binding, not discovery.** All workers share the daemon's `cwd`, so
  every concurrent client's worker writes its transcript into the **same**
  projects dir — same-cwd concurrency is the warm pool's steady state, the
  exact shape in which picking a transcript by newest mtime forwarded a
  sibling's session wholesale (claudepr-a927ec0c). The client's reader binds
  to the transcript named by *its* worker's identity file and forwards
  nothing until positively bound; the Stop payload read from `stop_fifo`
  retargets the reader to the resolved transcript path before the final
  drain as the authoritative backstop (same path → no-op, no duplicate tail;
  different or unbound → rebind at the injection-snapshot offset).
* **Same-cwd guarantee.** Every forwarded stream-json byte belongs to the
  client that drove the worker: the result event names the driving worker's
  session, and no sibling session id occurs anywhere in the forwarded stream.
  Pinned end-to-end at full scale — three concurrent stream-json clients on
  one `--pool-size 3` daemon, one shared cwd/HOME, all three transcripts
  materializing in one tight window while every other reader is mid-bind — by
  `tests/pool_adversarial_e2e.rs::concurrent_stream_json_clients_share_a_cwd_without_forwarding_siblings`.
  Residual limit, identity-less sessions only (a claude without
  UserPromptSubmit support): such a run binds only a single unambiguous new
  transcript and otherwise forwards nothing live — output arrives whole and
  uncontaminated at the drain, but not streamed.
* **Cleanup lifecycle.** These artifacts live and die with the worker,
  daemon-side. The client creates none of them and removes none of them (the
  pooled session runs with no `HookInstaller` at all); its watchdog never
  signals the worker either — teardown belongs to the daemon. `release` →
  `destroy_worker` closes the PTY master, SIGTERMs the worker's process
  group, reaps it, and drops the installer, removing that worker's
  `stop.fifo` and `session-identity.json`. A client that dies mid-session
  abandons its artifacts to the daemon's destroy path (the daemon owns the
  worker regardless of the client's fate); a crashed daemon's artifacts fall
  to claude-print's startup orphan sweep (60 s age + dead-owner-PID proof —
  see hook-design.md). A released worker is never reused (INV-9), and
  sequential callers observe fresh, distinct artifacts per invocation —
  distinct Stop FIFO, pid, and PTY per caller, no crossing
  prompt/env/session — pinned by
  `src/pool.rs::two_sequential_pooled_invocations_observe_zero_cross_caller_leakage`
  and
  `tests/pool_socket_e2e.rs::sequential_clients_get_fresh_replaced_workers_with_no_cross_caller_leakage`.

## Timeouts and deadlines

| Timer | Value | Where | Effect when it fires |
|---|---|---|---|
| Client acquire budget | `min(60 s, max(1, --timeout))` | client, whole exchange | the exchange fails; if the failure is "silence past the deadline" it classifies as a hard protocol failure (the daemon accepted and then hung — that is broken, not busy) |
| Client release budget | 10 s (`RELEASE_TIMEOUT_SECS`) | client, whole release exchange | release gives up; best-effort by contract, the daemon reclaims the worker at its own shutdown either way |
| Daemon per-connection read timeout | 5 s | daemon, `read(2)` on the connection | connection closed, no reply |
| Daemon accept/maintain tick | 250 ms | daemon, `poll(2)` on the listener | bounds shutdown-response latency and replacement-spawn cadence |
| Worker warmup cap | 120 s | daemon, internal | not wire-visible; a worker that fails warmup is destroyed and replaced, it never becomes an assignment |

The client's budget is enforced across the **whole** exchange, not per
`read(2)`: every stage (connect, request write, response read, fd receive) is
`poll(2)`-bounded against the same deadline, so a daemon dribbling one byte
per interval cannot stretch the exchange past it (INV-12).

## Versioning and compatibility

There is **no version field and no handshake** in v1. Versioning is a
compatibility *model*, not a number, and it rests on five rules:

* **R1 — unknown fields are ignored, both directions.** serde is used without
  `deny_unknown_fields`. Adding a field to any frame is compatible in every
  direction: an old daemon ignores a new client's extra request fields; an
  old client ignores a new daemon's extra response fields (and fills missing
  `serde(default)` fields with empty values — which rule R3 then judges).
* **R2 — unknown `type` tags are hard parse failures, both directions.** The
  request and response enums are closed. A new frame type added server-side
  is invisible to old clients **only until it is sent to one** — then the old
  client hard-fails. A new request type sent by a new client to an old daemon
  is answered with a silent close, not an error frame. Adding a frame type is
  a **breaking change** for deployed peers; do it only with a client release
  that understands it first.
* **R3 — the required-payload floor is the old-daemon detector.** The
  `serde(default)` extras on `worker_assigned` are the one *designed*
  version-mismatch path: a daemon older than the Stop-FIFO handoff parses
  fine, transfers a fine fd, and is then rejected as a hard protocol failure
  naming the missing field. Falling back would silently mask a daemon that is
  running pre-handoff code, so it is a hard error (exit 2), by design.
* **R4 — unknown error codes are hard parse failures.** The code registry is
  closed; a newer daemon answering with a code an older client does not know
  turns that client's invocation into a hard protocol failure instead of a
  stateless fallback. Like R2 this makes new codes a **breaking change**:
  ship the client that knows the code before the daemon that emits it.
* **R5 — no negotiation by design.** Daemon and client ship in the same
  binary and are deployed together; when the fleet runs them apart, R1–R4 are
  the contract. If a version field is ever added it must arrive as a new
  frame type (R2 breaking) or an ignored additive field (R1, informational
  only) — there is no third door.

### Version-mismatch matrix

| Daemon \ Client | current client | legacy client (pre-fifo) |
|---|---|---|
| **current daemon** | works | extra assignment fields ignored; works |
| **legacy daemon** (no `stop_fifo`/`pid`/`cwd`) | hard protocol failure naming the missing field (R3) | works |
| **future daemon** (unknown response `type` or error `code`) | hard protocol failure (R2/R4) — same as for a current client | hard protocol failure |
| **future client** (unknown request `type`) | silent close from the v1 daemon (malformed request, no reply) | silent close |

## Malformed-input catalog

What each side does with each malformation. "Hard failure" = client exits 2
with `error: pool protocol failure: ...`; it never falls back (INV-10).

| Malformation | Daemon behavior | Client behavior |
|---|---|---|
| length prefix > 64 KiB | close, no reply | hard failure before allocating the body |
| body is not JSON / not an object | close, no reply | hard failure (`malformed response`) |
| unknown `type` tag | close, no reply | hard failure (`malformed response`) |
| `worker_assigned` missing a required field | n/a (daemon is the sender) | hard failure naming the field (R3) |
| `worker_assigned` followed by no fd / unusable fd | n/a | hard failure (`fd transfer failed`) |
| EOF before a complete request frame (incl. mid-prefix, mid-body) | clean close of an empty exchange, connection thread exits — no spin, no log | hard failure (`pool closed the connection mid-response`) |
| EOF mid-response / after draining the request | n/a | hard failure |
| silence past the client's deadline | n/a | hard failure at the deadline (`timed out waiting for the pool`) |
| stalled partial frame (no EOF) | connection dropped by the 5 s read timeout | hard failure at the client deadline first (1 s–60 s < 5 s never holds: the client deadline is always the binding one) |
| well-formed request the daemon declines | `error` frame, then close | `PoolUnavailable` → stateless fallback behind exactly one verbose diagnostic |

## Worked example

One full acquire, driven by hand (length prefixes shown as hex):

```
C→D  00 00 00 1b  {"type":"acquire","timeout_secs":60}        (27 bytes)
D→C  00 00 00 xx  {"type":"worker_assigned","worker_id":"...",
                   "message":"Worker ready","stop_fifo":"/tmp/.../stop.fifo",
                   "pid":12345,"cwd":"/srv/daemon"}            (xx bytes)
D→C  (sendmsg: 1 NUL byte in-band + SCM_RIGHTS cmsg carrying the PTY master fd)
     ...client drives the session over the transferred fd...
C→D  (new connection)
     00 00 00 2a  {"type":"release","worker_id":"..."}         (42 bytes)
D→C  00 00 00 xx  {"type":"worker_assigned","worker_id":"...",
                   "message":"Worker released","stop_fifo":"","pid":0,"cwd":""}
```

Refusal example (`pool_full`):

```
C→D  00 00 00 1b  {"type":"acquire","timeout_secs":60}
D→C  00 00 00 45  {"type":"error","error":"Pool full - no ready workers",
                   "code":"pool_full"}
```

## Security notes

* The socket node is `0600` from the first instant (umask-narrowed bind plus
  explicit `chmod`); connect permission is the only authentication.
* Every fd involved is `CLOEXEC` — the client's connected socket
  (`SOCK_CLOEXEC` at `socket(2)`), the daemon's accepted connections (std
  sets CLOEXEC on `accept`), and the transferred PTY master
  (`MSG_CMSG_CLOEXEC` on `recvmsg`).
* The transferred fd is a raw capability to a warmed `claude` worker; it
  grants exactly one prompt's worth of that worker. Release destroys it — a
  released worker is never reused, so no state can leak across callers
  (INV-9).

## Test map

Every behavior above is pinned. Focused, in-process compatibility pins live
in `tests/pool_protocol_compat.rs` (no daemons, no workers — pure wire);
end-to-end shapes (real daemon + mock-claude subprocesses) live in the
existing suites.

| Spec claim | Pinned by |
|---|---|
| request frames parse; `timeout_secs` defaults to 60; unknown fields ignored | `pool_protocol_compat::wire_frames_*` |
| closed tag/code registries: unknown `type`/`code` are parse failures (R2/R4) | `pool_protocol_compat::unknown_*` |
| legacy assignment parses with empty extras but fails validation naming the field (R3) | `pool_protocol_compat::assignment_missing_required_*` |
| error-code → classification mapping, all five codes | `pool_protocol_compat::every_documented_error_code_*` |
| client sends exactly the documented acquire/release frames | `pool_protocol_compat::client_sends_the_documented_acquire_frame`, `::dropping_the_worker_*` |
| fd transfer: usable fd, `FD_CLOEXEC`, full payload extraction | `pool_protocol_compat::happy_path_*` |
| oversized prefix, garbage, wrong shape, mid-response close, silence → hard failure, never fallback | `pool_protocol_compat::oversized_*`, `::garbage_*`, `::unknown_response_shape_*`, `::daemon_close_*`, `::silent_daemon_*` |
| absent socket → `Unreachable`, fallback; invocation classification seams | `pool_protocol_compat::missing_socket_*`, `::invocation_classification_*` |
| daemon refusals: `pool_full`, `invalid_worker_id`, `shutting_down`; malformed/oversized/unknown requests closed with no reply; daemon stays healthy | `pool_protocol_compat::daemon_side_*` (raw-wire against a real `PoolServer`) |
| e2e: malformed-daemon arms (close/ wrong shape/ assignment-without-fd), exhaustion fallback, stale socket, release isolation | `tests/pool_socket_e2e.rs` |
| e2e: crash/silence/restart budgets, orphan containment | `tests/pool_failure_e2e.rs` |
| e2e: concurrent acquisition, zero sibling contamination | `tests/pool_adversarial_e2e.rs` |
| per-worker relay hooks installed (Stop + UserPromptSubmit); identity file is the Stop FIFO's sibling | `src/hook.rs::settings_json_has_user_prompt_submit_identity_hook`, `::identity_path_is_stop_fifo_sibling`, `::identity_sh_is_executable_and_targets_identity_file` |
| reader binds via identity (exact path, not newest mtime), forwards nothing while unbound; late identity outranks a matured fallback bind; retarget is a no-op on the same path and rebinds from the snapshot offset on ambiguity | `src/emitter.rs::bound_reader_forwards_nothing_while_identity_unresolved`, `::bound_reader_binds_identity_exact_path_not_newest_mtime`, `tests/integration/scenarios.rs::stream_json_reader_identity_binding_wins_over_newest_sibling`, `::stream_json_reader_binds_when_identity_arrives_late`, `::stream_json_reader_late_identity_rebinds_after_fallback_bind_matured`, `::stream_json_reader_refuses_ambiguous_candidates_until_retarget`, `::stream_json_reader_retarget_same_path_does_not_duplicate`, `::stream_json_reader_retarget_binds_from_snapshot_offset` |
| e2e: same-cwd concurrency — three concurrent stream-json clients, one daemon, zero sibling contamination | `tests/pool_adversarial_e2e.rs::concurrent_stream_json_clients_share_a_cwd_without_forwarding_siblings` |
| e2e: pooled stream-json forwards the driven worker's transcript | `tests/pool_socket_e2e.rs::pooled_stream_json_invocation_forwards_the_worker_transcript` |
| e2e: sequential clients get fresh hook artifacts per invocation (Stop FIFO, pid, PTY, session — no cross-caller leakage) | `src/pool.rs::two_sequential_pooled_invocations_observe_zero_cross_caller_leakage`, `tests/pool_socket_e2e.rs::sequential_clients_get_fresh_replaced_workers_with_no_cross_caller_leakage` |
| serve contract: bind/teardown/0600/reaping | `tests/serve.rs` |
