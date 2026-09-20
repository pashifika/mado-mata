# Runtime comparison: replay and native prerequisites

The optional `engine` feature consumes the public `mado-pilot` facade at
`2c9d57a53e44ffc97315975c3ca46a766d6c8539`. It does not use a sibling path dependency,
private platform APIs, a fake OCR backend, or a substitute input route.

**Native execution is blocked at this pin.** The facade cannot bind its opaque
`TargetId` to an executable/application-bundle path and exact process lifetime.
The harness refuses native plans before engine construction, discovery,
permission reads, capture, or input. Providing additional permission or a window
title does not remove this blocker. Tasks requiring native attachment, receipts,
permission/focus refusal, target loss, and both-OS qualification remain non-passing.
Replay is a real recognition path, not native qualification.

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

Use the pinned upstream [native build procedure](https://github.com/pashifika/mado-pilot/blob/2c9d57a53e44ffc97315975c3ca46a766d6c8539/CONTRIBUTING.md#native-development-prerequisites)
and [OCR dependency procedure](https://github.com/pashifika/mado-pilot/blob/2c9d57a53e44ffc97315975c3ca46a766d6c8539/docs/third-party-dependencies.md#implemented-onnx-runtime-prerequisite).
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
  [manifest example](https://github.com/pashifika/mado-pilot/blob/2c9d57a53e44ffc97315975c3ca46a766d6c8539/fixtures/assets/phase1-slice/madopilot-package.json).
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
in-flight work, close error, and confirmed replay cleanup. Compatible model/engine
owners remain a separately counted runner baseline. At runner shutdown, after
all hosts are dropped, `release_runner_resources` drops that baseline. The
upstream ONNX Runtime API library itself remains process-global until process
exit; dropping an engine is not a claim that the native library unloaded.
Unreturned work requires the runner's containment result, not a clean outcome.

## Native authority record and upstream prerequisite

A native plan must still carry the required prospective record. The declarative
validator requires `native_config.native` with:

- `executable_or_bundle` and `permission_executable`: absolute paths; no game hash.
- `process_id`, `process_lifetime`, `window_rule`, `operating_system` (`windows` or
  `macos`, matching the host), and `hardware`.
- `capture`: `approved`, `duration_ms`, `max_frames`, `wait_ms`, `interval_ms`.
- `input`: independent `approved`, `duration_ms`, `max_actions`, `route`, `focus`,
  and nonempty `representative_actions`.
- `geometry`: the placement object above; `recognition_language`,
  `visible_postcondition`, `cleanup_ms`, and `containment_ms`.

All authorities must be positive, finite, and within the plan's enclosing bounds.
Routes are explicitly `system`, Windows `window_message`, or macOS
`process_directed`; focus is `preserve` or `require_focused`. These declarations
are not grants from the OS, nor evidence that a route can act on the target.
No launcher, elevation, focus change, process termination, restart allowance,
route substitution, or automatic retry is supplied.

The unresolved source-level prerequisite is precise:

- [`TargetDescription`](https://github.com/pashifika/mado-pilot/blob/2c9d57a53e44ffc97315975c3ca46a766d6c8539/crates/automation/capture/src/descriptor.rs#L252-L329)
  exposes only ID, descriptive name, extent, format, coordinates, and capability.
- [`TargetCapability`](https://github.com/pashifika/mado-pilot/blob/2c9d57a53e44ffc97315975c3ca46a766d6c8539/crates/automation/core/src/capability.rs#L472-L549)
  exposes kind/capture/input capability, not process/path/lifetime provenance.
- The public facade's engine discovery/open operations provide no path/lifetime
  correlation operation. The upstream standalone
  [example](https://github.com/pashifika/mado-pilot/blob/2c9d57a53e44ffc97315975c3ca46a766d6c8539/examples/rust-input-workflow/src/flow.rs#L75-L82)
  selects an exact window title, which is insufficient for this application's
  authorization contract.

These findings were checked against direct source because graph coverage for the
pin was stale and examples were excluded. They establish an integration
prerequisite, not an observed native runtime defect. An upstream public API must
bind the selected path and process/window lifetime to the returned target and
preserve that binding through open and dispatch. Only a separately reviewed pin
update plus actual Windows/macOS evidence may remove the blocker. Title-only
matching, PID-only matching, native-handle guessing, and fixture/private APIs
are not repairs.

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
