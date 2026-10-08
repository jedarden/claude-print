# Supplied-Capture Record Handoff Contract

This note defines the bounded handoff of one caller-supplied, validated,
ordered record sequence. It is self-contained: the contract is about the
supplied capture and the destination writer only. It does not infer a sequence
from a file, transcript, repository, stream, or any other source.

## Freeze-stage input

The caller supplies the complete capture directly as an in-memory sequence;
the freeze stage has no other data input. In `src/emitter.rs`,
`capture_records(records)` wraps that supplied sequence as a
`CapturedRecordSequence`, and `forward_records(capture)` consumes that
capture. `handoff_records(writer, records)` is the convenience composition:
`writer` is only the output destination, and `records` is the one supplied
capture. No pathname, file handle, repository root, source selector, stream,
or fallback source is accepted; no filesystem lookup or scan is performed.

Validation is a caller-side precondition. The freeze stage does not revalidate
or reinterpret the capture, and it does not discover replacement content. In
this contract, each sequence element is one supplied block (called a record
in the Rust API); its position and boundary are part of the capture.

## Input boundary

The input is exactly one finite sequence

```text
R = [r₀, r₁, …, rₙ₋₁]
```

supplied at the handoff boundary. Each `rᵢ` is one finite byte string: an
ordered vector of bytes, not text that must be decoded. The outer sequence is
the boundary of the handoff. No record before `r₀` or after `rₙ₋₁` belongs to
it, and the handoff does not extend the sequence by discovering more records.

“Bounded” means that the handoff consumes that finite sequence and then stops;
it does not read until an end marker, EOF, or an externally determined limit.
The caller owns the choice of sequence and its order. The handoff does not
parse, validate, normalize, deduplicate, reorder, or otherwise inspect record
contents.

## Boundary versus bytes

Record boundaries are part of the handoff input even though they are not
encoded in the writer's byte stream. Before emission, the sequence remains a
sequence of separate records, so these inputs are different:

```text
[b"ab", b"cd"]
[b"abcd"]
```

They happen to produce the same output bytes, but the first has two records
and the second has one. Likewise, duplicate records are not deduplicated, and
empty records are not discarded from the sequence merely because they carry
no bytes. The handoff preserves one supplied item, in its supplied position,
for every record in `R` until the byte-oriented emission step.

## Output

On a successful handoff, the writer receives exactly the ordered byte
concatenation

```text
r₀ || r₁ || … || rₙ₋₁
```

where `||` is concatenation. The handoff contributes no separator, newline,
length prefix, wrapper, envelope, or other byte. It introduces no inferred or
synthetic record. The record boundaries therefore do not survive in the final
byte stream; consumers that need boundaries must retain the supplied
sequence, rather than recover them from output bytes.

The edge cases are normative:

| Supplied sequence | Record count | Output bytes |
|---|---:|---|
| `[]` | 0 | empty |
| `[b""]` | 1 | empty |
| `[b"", b""]` | 2 | empty |
| `[b"left", b"right"]` | 2 | `b"leftright"` |
| `[b"same", b"same"]` | 2 | `b"samesame"` |
| `[b"a\x00\xff\n\r\t"]` | 1 | the same bytes, unchanged |

An empty sequence emits zero bytes. An empty record also emits zero bytes,
but remains a record in the supplied sequence. Non-UTF-8 bytes, NULs,
control bytes, CR/LF bytes, unusual whitespace, Unicode encoded as bytes, and
any other byte values are opaque and are forwarded unchanged. No byte value
has special meaning to this contract.

## Scope exclusion

The handoff reads only the supplied record sequence and writes to the caller's
writer. Filesystem reads, repository reads (including repository metadata or
history), transcript discovery, stdin reads, network reads, environment-based
record discovery, and reads from any unrelated source are out of scope. Such
sources cannot add records, alter record bytes, or alter the sequence boundary
under this contract.

The implementation is the capture/forward/emission path in `src/emitter.rs`;
the output behavior is exercised by the emitter tests in `tests/emitter.rs`.
