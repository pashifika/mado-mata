# ADR 0001: bounded runtime comparison without native authority substitution

- Status: accepted for the comparison; production runtime adoption blocked
- Date: 2026-09-20
- Change: `m0-runtime-comparison`
- Integration: `change/m0-runtime-comparison` → `dev/runtime-comparison`

## Context

M0 must compare actual JavaScript/Lua interpreters over a common host, with an
independent Rust workload and TypeScript authoring. It must not confuse a compiled
adapter, a controlled receipt, or logical cancellation with native qualification,
application effect, or completed physical cleanup. A CI-first delivery is useful
without inventing a desktop application or an authorized native target.

Two original assumptions did not survive dependency inspection:

1. `rquickjs`'s `rust-alloc` feature cannot be combined with its normal hard memory
   limit. The [0.14.0 API documentation](https://docs.rs/rquickjs/0.14.0/rquickjs/struct.Runtime.html#method.set_memory_limit)
   explicitly states that `set_memory_limit` has no effect with that allocator.
2. Public MadoPilot revision `2c9d57a53e44ffc97315975c3ca46a766d6c8539` does not
   correlate its opaque `TargetId` with a configured executable/application-bundle
   path and exact process/window lifetime. Its
   [target description](https://github.com/pashifika/mado-pilot/blob/2c9d57a53e44ffc97315975c3ca46a766d6c8539/crates/automation/capture/src/descriptor.rs#L252-L329)
   and [capability description](https://github.com/pashifika/mado-pilot/blob/2c9d57a53e44ffc97315975c3ca46a766d6c8539/crates/automation/core/src/capability.rs#L472-L549)
   lack that provenance. The public consumer example selects by window title,
   which does not meet this application's target contract.

## Decision

Use the default QuickJS allocator with explicit finite memory limits, not
`rust-alloc`. Pin `rquickjs` 0.14.0 (`std`, `loader`), `mlua` 0.12.1
(`lua54`, `vendored`, `serde`), Lua 5.4.9, QuickJS-ng 0.16.2, TypeScript 5.9.3,
Node.js 24.18.0, Rust 1.97.1, and the full public MadoPilot Git revision.

Build actual standalone supervisor/child execution, immutable inventories,
interpreters, strict authoring compilation, and controlled behavioral checks.
Keep ordinary hosted CI free of capture, input, permission prompts, native model
paths, and private checkouts. Engine/OpenCV integration is an optional build;
real replay and native evidence remain separate from controlled outcomes.

Refuse native plans before engine construction, discovery, permission reads,
capture, or input until the public facade can preserve the required target
binding. Title-only/PID-only matching, private APIs, guessed handles, focus
changes, fallback providers, or another input route are not acceptable shortcuts.
This is an upstream integration prerequisite, not an observed engine runtime bug.
No engine repair or native execution success is claimed.

Use independent cancellation/watchdog progress and owned-child termination for
non-returning work. Keep logical completion, retained owners, cleanup records,
and external exit observations separate. Never kill a Rust thread or an unrelated
target to obtain a passing result. A compiler subprocess must validate the owner
PID supplied before its launch; adopting an already-reparented PID is not proof
of ownership.

Latch host failures before constructing script-visible error objects. Package
code can mutate error prototypes, and error allocation itself can fail. Diagnostic
work cannot decide whether an already-issued host refusal remains authoritative.
Replay managed IDs include a fresh attempt identity, not just process-local
engine/stream counters. Cached terminal queries require no new owner capacity.

## Consequences and verification

The controlled corpus can establish interpreter, loader, ordering, readonly,
refusal, and process-containment behavior on hosted Windows/macOS/Linux builds.
It cannot establish capture/OCR/input permission, real application effect, native
cleanup, native warm-reuse performance, or the initial both-OS adoption gate.

Actual VM memory exhaustion, prototype-error refusal, module identity/cycles,
Stop/control loss, held-owner containment, and compiler strict-policy cases are
exercised by the executable checks. The optional public-facade integration is
compiled separately; model/corpus execution needs explicit private prerequisites.

A future upstream pin must provide path/lifetime correlation through discovery,
open, and dispatch, then pass consuming Windows and Apple Silicon macOS scenarios.
Only that evidence and prospective numerical budgets can support an
`Adopt(candidate)` decision. Until then the result is `Blocked(reason)` even when
all available CI checks pass. See [usage and evidence boundaries](../runtime-comparison.md)
and [native setup](../runtime-native.md).
