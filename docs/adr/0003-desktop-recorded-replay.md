# ADR 0003: Desktop recorded replay and OCR environment

Status: Accepted for the macOS development application; not live native qualification.

## Decision

Keep the GUI and controlled runner non-native. Build the optional engine runner
in a separate fixed checkout target directory. Neither packages, profiles, nor
IPC can select an executable, supply a complete Plan, or acquire native authority.
A missing engine artifact or pre-Rust loader failure must not prevent controlled
operation or become a successful initialization result.

Store one optional OCR environment in the existing atomic App settings, separate
from portable profiles. Saving validates structure only. Rust canonicalizes the
selected locations and derives model/runtime/library identities. Shared resource
validation is available without the engine feature and is reused by the real
engine path. Parent capture and child validation both remain required; a prior
Check is not a cache or admission token. Selected library identities are not an
exhaustive loaded-image audit.

Use the existing replay object as a bounded descriptor. Project it from captured
package assets; do not introduce another corpus format, directory scanner, Plan
editor, or registry. Replay has no live native configuration and retains the
controlled-non-native input sink. Only the package workflow is admitted, not
controlled fault-injection scenarios.

Check and Start reserve one owned controller slot before settings/resource I/O.
The worker acquires the existing settings/profile store lock and acknowledges
ownership before the command returns its operation ID. It holds that lock through
input capture, then releases it before resource hashing and execution. Later
saves cannot overtake the snapshot. Stop, progress, terminal delivery, and close
containment remain shared; logs cannot determine lifecycle truth.

An explicit internal Check operation initializes the actual SDK backend without
resolving a workload profile, compiling/evaluating package code, or executing
readiness/workflow. Missing corpus permits file validation only. Initialization
milestones come from the child; Check records `NotExecuted` and absent VM/workflow
metrics. No synthetic VM success substitutes for backend initialization.

Configure loader search directories only on the engine-child command. On
Windows, capture the trusted Node executable's absolute location before narrowing
DLL search paths; do not restore ambient DLL directories merely to find the
compiler. Keep `ORT_DISABLE_TELEMETRY=1` at child startup. A pre-spawn prerequisite
refusal retains structured `BLOCKED` evidence with known no-child cleanup and no
invented build/startup identity.

## Bounds corrected by consuming evidence

The package and decoded-frame bounds below record the earlier consuming evidence.
[ADR 0006](0006-saved-image-recognition-observations.md#image-policy) supersedes
them for saved-image authoring and shared package/replay capture. This historical
evidence does not establish acceptance at the new ceilings.

Package capture stays at the existing **1 MiB** for inspection, Check, and both
Start lanes. Its bound participates in inventory identity: using 2 MiB only for
inspection caused an unchanged controlled package to fail `StaleIdentity`.
A real-controller regression reproduced that failure and passed after all capture
paths were aligned. Only expanded replay frames use the reviewed **2 MiB**
envelope; four 640x110 RGBA observations require 1,126,400 bytes even when repeated
frames share one smaller package asset.

Controlled execution retains **10 s**. Replay and Check use **30 s**, including
reserved preparation. The first real Check exhausted 10 s before SDK initialization
while parent and child validated a selected 102,121,436-byte resource set. That
cancelled attempt remains a failure, not a successful initialization. The next
bound was declared before remeasurement; validation was neither removed nor
cached. Cleanup **1 s**, containment **2 s**, and controller shutdown **14 s**
remain unchanged. Stop does not wait for the normal operation deadline.

## Evidence and limits

The actual macOS WKWebView saved/reopened the environment, reported missing-corpus
and missing-engine prerequisites, and ran controlled work while the engine
artifact was absent. With the real engine child, Check initialized successfully
without evaluating a package whose module body throws. Saved template-first and
OCR-first profiles produced their independently declared distinct decisions and
newer-frame postconditions over authorized recorded frames, followed by clean
cleanup. Stop during an observed SDK initialization returned cancellation and
independent cleanup, not an initialization pass.

Replay fault messages can contain recognized text. The ordinary surface exposes
only safe classification; full bounded diagnostics require explicit disclosure.
The retained Check card preserves its actionable failure summary and resets
private disclosure for each operation. Scripts may still explicitly log messages;
this is not permission to publish private content.

The [desktop guide](../desktop.md#recorded-replay-acceptance) owns the complete
local acceptance procedure. Hosted checks exercise structural/behavioral contracts,
not real OCR or WebView acceptance. Windows engine execution, live capture/input,
R6, runtime adoption, and release packaging remain separate qualifications.
