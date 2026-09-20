# Runtime comparison: replay and native prerequisites

The optional `engine` feature consumes the public `mado-pilot` facade at
`85ccc580cd28ffb9b0b52271f6c87f1af0109a04`. It does not use a sibling path dependency,
private platform APIs, a fake OCR backend, or a substitute input route.

**Native integration is available; native qualification is not complete.** The
facade now exposes optional retained-process provenance. The harness requires an
exact window name, canonical executable/application-bundle path, process ID, and
opaque process lifetime before opening the native session. Missing, ambiguous,
or mismatched provenance is refused. Native capture, OCR, and input receipts are
real SDK operations, not the controlled sink. A submitted receipt still does not
prove application effect. Both-OS workload and lifecycle evidence remain required.

## Install the engine prerequisites

The default controlled build does not need these dependencies. For the optional
engine build, acquire the following separately; the harness downloads nothing:

- Rust 1.97.1, the application's locked Git dependency, and a native C++ toolchain.
- Apple Silicon macOS: Xcode Command Line Tools, OpenCV 4 development files, and
  libclang. The pinned engine qualifies macOS 26 with deployment minimum 26.5.2;
  it does not qualify macOS 27 automatically.
- Windows x64: an x64 MSVC developer shell, Windows SDK, OpenCV 4.14.0 development
  files/DLLs, and LLVM/libclang 22.1.8.
- ONNX Runtime **1.29.0**, C API **17**, CPU provider. Supply the canonical regular
  file `libonnxruntime.1.29.0.dylib` on Apple Silicon or `onnxruntime.dll` on Windows.
  No `PATH` search, alternate runtime, accelerator preference, or fallback is used.
- The accepted detector and recognizer below, under one canonical model root.

Use the pinned upstream [native build procedure](https://github.com/pashifika/mado-pilot/blob/85ccc580cd28ffb9b0b52271f6c87f1af0109a04/CONTRIBUTING.md#native-development-prerequisites)
and [OCR dependency procedure](https://github.com/pashifika/mado-pilot/blob/85ccc580cd28ffb9b0b52271f6c87f1af0109a04/docs/third-party-dependencies.md#implemented-onnx-runtime-prerequisite).
Its `tools/setup-native.py` configures only the command it launches; it installs
nothing. A separately obtained, revision-pinned public checkout can provide that
setup tool without becoming the application's dependency source. From this
repository root, with that tool's absolute path:

```sh
MACOSX_DEPLOYMENT_TARGET=26.5.2 python3 /absolute/pinned-mado-pilot/tools/setup-native.py -- cargo +1.97.1 build --locked --manifest-path tools/runtime-comparison/Cargo.toml --features engine
```

Windows, from the x64 MSVC developer shell:

```bat
python C:\pinned-mado-pilot\tools\setup-native.py --opencv-root C:\native\opencv --libclang-path "C:\Program Files\LLVM\bin" -- cargo +1.97.1 build --locked --manifest-path tools/runtime-comparison/Cargo.toml --features engine
```

Keep the same selected loader environment for execution. On macOS it must expose
OpenCV `core`, `imgproc`, `imgcodecs`, and their shared dependencies; on Windows
it must expose the selected versioned `opencv_world` DLL and dependencies. A
successful build is not evidence that replay, capture, OCR, or input executed.

The supervisor sets `ORT_DISABLE_TELEMETRY=1` in every runtime-child command
before process startup. In ONNX Runtime 1.29.0, the POSIX provider's API-level
opt-out alone leaves its uploader alive and still emits `ProcessInfo`; the
[process-wide opt-out](https://github.com/microsoft/onnxruntime/blob/v1.29.0/onnxruntime/core/platform/telemetry_environment.h)
prevents uploader and device-ID initialization. The existing API-level opt-out
remains in use, including Windows' separate ETW control model. No ambient process
environment is mutated after threads start, and no exit exception is suppressed.
Internal child invocations outside the supervisor must supply the same startup
environment; they are not a supported shortcut around supervision.

### Accepted OCR content

Both supported profiles use these exact Apache-2.0 model bytes. Obtain them
through the pinned upstream acquisition procedure; do not rename arbitrary
models to these paths.

| Relative path under `model_root` | Bytes | SHA-256 |
|---|---:|---|
| `rapidocr-v3.9.2/ch_PP-OCRv4_det_mobile.onnx` | 4745517 | `d2a7720d45a54257208b1e13e36a8479894cb74155a5efe29462512d42f49da9` |
| `rapidocr-v3.9.2/PP-OCRv6_rec_small.onnx` | 21234383 | `6f327246b50388f3c176ae304bd95767ea6dc0c9ae92153ef8cbe210b3c14884` |

Supported `model` and `profile` values must be equal and one of:

- `g-004-rapidocr-ppocrv4-det-v6-rec-small-v1`
- `phase-3-1-rapidocr-ppocrv4-det-v6-rec-small-bounded-v2`

`language` is `horizontal-ja-basic-latin-ascii-digits-ui-symbols-v1`, `provider`
is `cpu`, and `runtime_profile` is `onnxruntime-1.29.0-api17-cpu`. CoreML is not a
supported alternative here; CUDA requires a different qualified configuration
and is not enabled by this comparison. Unsupported choices fail rather than fall
back. The facade validates the complete accepted model tuple and runtime API
before publishing an engine.

## Prepare a private replay package and plan

Use `lane: "replay"` and the normal candidate, profile, limits, prospective budgets,
and sample policy. No live capture is performed to create the corpus. Supply
previously recorded, authorized images converted offline to tightly packed RGBA8
(or BGRA8 where the inventory format permits it). Do not commit screenshots,
recognized text, model paths, or private plans.

Each recorded frame, PNG template, and MadoPilot package manifest must be a
**declared captured asset** in the workload's `package.json`:

- Frames use `format: "raw-rgba8"` with exact width and height.
- Templates use `format: "png"` with their decoded width and height.
- The engine manifest uses `format: "json"`, `width: 0`, `height: 0`.
- `replay.package_entries` maps engine-relative package paths to those asset IDs.
  Its `madopilot-package.json` entry must be a valid upstream asset manifest,
  including template content SHA-256, dimensions, license, and match defaults.
  The facade supports PNG templates, not raw template pixels. See the pinned
  [manifest example](https://github.com/pashifika/mado-pilot/blob/85ccc580cd28ffb9b0b52271f6c87f1af0109a04/fixtures/assets/phase1-slice/madopilot-package.json).
- `replay.templates` maps host recognition asset aliases to template IDs declared
  by that engine manifest. Neither map grants filesystem access.

The ordinary workflow consumes one readiness frame, one decision frame, and one
newer postcondition frame. Supply at least those three observations; unmatched
query waits can consume additional frames. Set the profile ROI to the recorded dimensions.
Keep the complete package within `limits.snapshot_files` and
`limits.snapshot_bytes`; three 320-by-160 RGBA frames occupy 614400 bytes before
other package files. For qualification, use real observations appropriate to the
declared oracle, not generated success pixels or pre-filled OCR results.

A separately labeled integration smoke may use an identified generated upstream
fixture and its independent oracle. Such a static smoke does not qualify recorded
gameplay, application transitions, postconditions, or performance. The
[executed static-fixture smoke](runtime-comparison-results.md#real-engine-initialization-and-static-fixture-recognition)
uses two observations and deliberately performs no input or postcondition.

The following is the `native_config` shape for replay. Replace the illustrative
paths, sizes, hashes, aliases, and frame declarations with the reviewed local
inputs. SHA-256 strings must contain exactly 64 lowercase hexadecimal digits;
resource lengths must be positive and at most 1 GiB.

```json
{
  "version": 1,
  "ocr": {
    "model": "phase-3-1-rapidocr-ppocrv4-det-v6-rec-small-bounded-v2",
    "profile": "phase-3-1-rapidocr-ppocrv4-det-v6-rec-small-bounded-v2",
    "language": "horizontal-ja-basic-latin-ascii-digits-ui-symbols-v1",
    "provider": "cpu",
    "runtime_profile": "onnxruntime-1.29.0-api17-cpu",
    "model_root": "/canonical/private/model-root",
    "runtime": {
      "path": "/canonical/private/libonnxruntime.1.29.0.dylib",
      "sha256": "REPLACE_WITH_REVIEWED_RUNTIME_SHA256",
      "bytes": 1
    }
  },
  "native_libraries": [
    {
      "path": "/canonical/private/opencv-library",
      "sha256": "REPLACE_WITH_REVIEWED_LIBRARY_SHA256",
      "bytes": 1
    }
  ],
  "native": null,
  "replay": {
    "corpus_id": "reviewed-corpus-v1",
    "frames": [
      {"asset":"ready_frame","width":320,"height":160,"pixel_format":"rgba8","captured_ns":0,"discontinuous":false,"placement":null},
      {"asset":"decision_frame","width":320,"height":160,"pixel_format":"rgba8","captured_ns":100000000,"discontinuous":false,"placement":null},
      {"asset":"after_frame","width":320,"height":160,"pixel_format":"rgba8","captured_ns":200000000,"discontinuous":false,"placement":null}
    ],
    "package_entries": {
      "madopilot-package.json": "engine_manifest",
      "templates/button.png": "button_png"
    },
    "templates": {"button": "panel.button"}
  }
}
```

List every selected non-system native library, including transitive dependencies,
in `native_libraries`. These are reviewed file identities, not an automatic audit
of the process loader's complete loaded-image list. Record the loader environment
and actual selected libraries with the private invocation evidence. Canonical
paths reject symlink aliases; hash the canonical target file.
On Windows, preserve the exact result of Rust's `std::fs::canonicalize`, including
its extended path prefix (`\\?\...`). Ordinary Python `Path.resolve()` output was
refused by this strict equality check; do not strip or replace the Rust canonical
form when projecting private configuration.

Recorded timestamps must increase strictly. `placement: null` deliberately
provides capture-pixel geometry only. If the recording has authoritative placement,
use `{"desktop_origin":[0.0,0.0],"logical_size":[320.0,160.0],"scale":[1.0,1.0]}`
with its real recorded values. Do not invent desktop coordinates. Discontinuities
invalidate prior actionable stream generations.

Run with the same native setup environment:

```sh
cargo +1.97.1 run --locked --manifest-path tools/runtime-comparison/Cargo.toml --features engine -- run /private/replay-plan.json /private/replay-package
```

The input queue remains the host's **controlled-non-native** sink. Recognition uses
`replay_engine_with_ocr_provider`, `load_package`, `prepare_from_package`,
`find_template`, and `recognize`; no fixed model answers are used. A successful
empty result is `null`, while engine statuses and asset fault kind/stage remain
attributable errors. Query waits run real recognition over recorded frames under
a finite deadline; exhausted corpus is the facade's typed capture failure, not
an invented final frame. A successful query returns its own managed source
observation; release that observation and result as well as the query.

Postconditions run OCR on a strictly newer frame with the same stream, epoch, and
geometry. They compare the declared exact text with actual recognized regions.
The input receipt is independent: submitted input, frame freshness, and observed
text do not establish causation or native application effect.

## Ownership, cancellation, and content replacement

One process-scoped compatible engine/model/template set is reused across fresh
attempt sessions. Initialization is not repeated per recognition call. The
configuration identity binds the engine pin, complete configuration, and captured
asset bytes. Model, runtime, and listed native-library bytes are checked before
initialization/reuse and after first initialization. Replacing a stored file with
different bytes fails identity validation; it does not silently become the old
configuration. Changing the configured identities requires a new runner.

Frames and facade results remain in Rust. Managed release invalidates handles;
query creation retains an independent source observation. Stop reaches the engine
cancellation token on a separate thread, independent of the host/native work
mutex. Late values are checked again before publication. Logical cancellation
never claims that a blocked backend has returned.

`finish` cancels, releases retained results/observations, and closes the session
with separate finite cleanup authority. Its facts distinguish remaining owners,
in-flight work, close error, and confirmed session cleanup. An incomplete native
input-release receipt remains incomplete even if session close later succeeds.
Compatible model/engine owners remain a separately counted runner baseline. At
runner shutdown, after
all hosts are dropped, `release_runner_resources` drops that baseline. The
upstream ONNX Runtime API library itself remains process-global until process
exit; dropping an engine is not a claim that the native library unloaded.
Unreturned work requires the runner's containment result, not a clean outcome.

## Native target selection and finite authority

Use `lane: "native"`, `native_config.replay: null`, and the required prospective
`native_config.native` record:

- `executable_or_bundle`: the exact canonical executable or application-bundle
  path. `permission_executable`: the canonical running comparison executable.
  Neither is a game-content hash.
- `process_id`: the nonzero native PID. `process_lifetime`: exactly 16 lowercase
  hexadecimal digits of the provider's opaque `TargetProcessIdentity::lifetime`.
  This is not a portable timestamp or a value to compare across providers.
- `window_rule`: the complete, exact window name, not a pattern.
- `operating_system`: `windows` or `macos`, matching the host; `hardware` records
  the operator's selected machine.
- `capture`: `approved`, `duration_ms`, `max_frames`, `wait_ms`, `interval_ms`.
- `input`: independent `approved`, `duration_ms`, `max_actions`, `route`, `focus`,
  and nonempty `representative_actions`.
- `geometry`: the placement object above; `recognition_language`,
  `visible_postcondition`, `cleanup_ms`, and `containment_ms`.
- Optional `package_entries` and `templates` use the same captured-asset mapping
  as replay. Omit both for OCR-only execution.

All authorities must be positive, finite, and within the plan's enclosing bounds.
The native and enclosing `containment_ms` must be equal; the supervisor cannot
silently apply a longer containment allowance. Native cleanup uses its own
declared bound. Capture/input durations conservatively include attempt setup.
Capture `max_frames` counts successful frame acquisitions, not every frame the
SDK's continuous native producer generates. Required native pacing and bounded
consumer acquisition intervals both apply. Geometry must match the actual frame's
origin, logical extent, and scale; guessed placement is refused.
Routes are explicitly `system`, Windows `window_message`, or macOS
`process_directed`; focus is `preserve` or `require_focused`. These declarations
are not grants from the OS, nor evidence that a route can act on the target.
No launcher, elevation, focus change, process termination, restart allowance,
route substitution, or automatic retry is supplied.

Prepare a complete workflow before its acceptance run: collect authorized
reference frames, establish recognition regions and control locations, then author
the script. Distinguish OCR postconditions from image-template postconditions.
Offline replay supports preparation; it is not native game-effect acceptance.

One native execution retains its capture session across observation, recognition,
wait, and input operations; cleanup closes it on termination. To verify a
Start-to-finish workflow, keep all its steps inside that one execution. Repeated
per-click `run` commands and independent screenshot inspectors open and close
different sessions and must not be reported as a continuous-capture trial.

The Rust facade's
[`TargetProcessIdentity`](https://github.com/pashifika/mado-pilot/blob/85ccc580cd28ffb9b0b52271f6c87f1af0109a04/crates/automation/capture/src/descriptor.rs)
is optional discovery provenance, not continuing liveness or input authority.
macOS publishes paths from its retained application and exact launch-value bits.
Windows publishes the executable path and creation value from the same retained
process handle used by its window authority. Native open and per-event dispatch
retain their existing lifetime guards. No public C ABI change is required.

Prepare the private record from an authorized public-facade discovery and actual
frame geometry. If an OS runs an installed bundle through another mounted path,
establish that correspondence independently and pin the observed runtime path.
A shared bundle identifier, title, or PID alone is not an identity repair.

The action shapes are `{"kind":"key_down","key":"A"}`,
`{"kind":"key_up","key":"A"}`, and
`{"kind":"click","x":100,"y":200,"button":"left"}`. Click coordinates are finite
capture pixels inside the retained observation; buttons are `left`, `right`, or
`middle`. A native click expands into move/press/release, and all three SDK events
consume the native attempt budget. Keys must be balanced inside one sequence;
unowned releases, repeated presses, unsupported names, and held keys are refused.
`representative_actions` declares approved operation kinds, not a title-based
permission or an instruction to execute those example actions.

`limits.max_actions` also bounds JavaScript promise creation and executed jobs.
Keep adequate VM startup allowance there and use `native.input.max_actions` for
a tighter physical-input event bound; a three-event input allowance does not
require a three-promise VM allowance.

`submit` admits bounded work; `settle` invokes the configured route using the
retained frame stamp and unchanged-geometry policy. Native receipts retain SDK
event counts, route/address scope, evidence strength, possible partial effects,
and release obligations. No native receipt is generated from controlled effects.
A refused/partial sequence closes admission and is never replayed automatically.
Verify application effect independently on a strictly newer compatible frame.

## Failure stages and outer invocation evidence

Private results distinguish configuration unset, missing/unreadable files,
unsupported OCR tuples, malformed configuration, model/content mismatch, facade
initialization failure, and operation cancellation/deadline/backend failures.
The facade's public `Error` has status and detail but deliberately no native
`source()` chain; both exposed fields are preserved. No hidden platform cause is
invented. Asset errors additionally preserve kind and load stage.

OpenCV is linked eagerly. A missing library can prevent even the supervisor from
entering Rust, so internal preflight cannot report that failure. Keep an **outer
invocation record** using an already-running shell or Python process:

1. Record UTC time, OS/architecture, binary SHA-256, engine revision, selected
   environment identity, private plan/package identities, and the intended argv.
2. Launch the exact binary in the selected loader environment, redirecting stdout
   and stderr to private bounded evidence storage; retain the shell/process exit
   or signal. On Windows retain the numeric process exit code unchanged.
3. Retain the loader diagnostic separately from application JSON. If no Rust
   startup/result record exists, report `BLOCKED` at `pre_rust_loader` or
   `child_startup` as evidenced, with the observed exit and diagnostic reference.
   If the cause cannot be established, preserve unknown startup failure rather
   than labeling every nonzero exit a loader error.
4. Mark dependent replay/native scenarios unexecuted/blocked. Never synthesize
   readiness, receipts, cleanup completion, or host preflight for an unreachable
   process. Do not rerun against another target or provider to hide the failure.

Apply the plan's finite process/containment deadline and evidence byte ceiling
outside the executable too. The outer recorder must not kill the target process.
Public reports retain sanitized stage/status/identity references, not screenshots,
OCR text, raw command lines, local paths, or unsanitized loader output.
