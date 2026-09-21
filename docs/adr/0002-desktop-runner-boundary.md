# ADR 0002: Desktop runner boundary and structured logs

Status: Accepted for the controlled macOS development application.

## Decision

Use Tauri **2.11.6**, tauri-build **2.6.3**, React **19.3.0**, TypeScript
**5.9.3**, and Vite **8.3.0**. The npm Tauri API is **2.11.1** and CLI
**2.11.5**. Exact manifests and lockfiles own the dependency versions.

The trusted WebView renders forms and status. It never evaluates package code.
The shared Rust `DesktopController` captures package/profile inputs and invokes
the existing supervised runtime executable. CLI `current_exe` behavior remains
unchanged. The application supplies a fixed development-build runner path;
package data and IPC cannot choose the executable or acquire native authority.
The existing application-owned compiler installation remains required. This is
not a relocatable release bundle or an additional-OS support commitment.

The owned child publishes its complete build identity on the correlated startup
channel. The supervisor validates its owned PID and retains that identity; a
missing identity remains unknown, never replaced by the desktop binary's metadata.

JavaScript inspection combines QuickJS declaration-only syntax validation with
`vm.SourceTextModule.link()` in the existing bounded trusted compiler worker.
The latter is enabled with Node's `--experimental-vm-modules` worker flag.
Neither pass evaluates module bodies. The link pass receives only captured
sources and Inventory-authorized edges, including requested executable `.d.ts`
modules; unrequested type declarations are not runtime linking obligations.
This uses the engines' parsers/linker rather than a second handwritten export
resolver, and supplies no ambient Node loader or package authority.

Source numbers are checked before WebView serialization; unsafe exact integers
and signed zero are refused without rewriting package or stored data. Only
WebView inputs under a `number` schema are normalized to floating-point values,
so JSON integer spellings do not change a saved float's meaning. Both application
and runner enable serde_json's `float_roundtrip` parser: the default parser
changed a valid large floating value in the save/reopen regression. This keeps
the same numeric meaning through storage, IPC, and the separately built child.

Keep one preparing/running/stopping reservation until the owned worker finishes,
the supervisor reaps its child, and the independent outcome is recorded. Stop
uses an atomic control signal, not the log channel. Ordinary window closure
requests shutdown off the UI thread. The pinned macOS default Quit path reaches
Tauri `RunEvent::Exit` without `ExitRequested`; that final callback waits for the
same cached, bounded shutdown even when ordinary close is already in progress.
The close worker posts its final exit on the event thread only if termination
has not begun. Forced/incomplete outcomes are not rewritten as success.
Unexpected application loss retains the runner's parent-loss contract.

## Shell evidence

Before extending the shell, the actual macOS WKWebView selected and statically
inspected the TypeScript fixture, launched the real owned child, requested Stop,
and displayed a cancelled terminal outcome with clean cleanup. A subsequent
owned run was closed from the WebView; the application exited normally and no
runtime child remained. This proves the shell boundary, not completed workflow
acceptance, native qualification, or packaged distribution.

Review verification separately exercised the ordinary window-close path. The
native Quit route is traced through the pinned muda/tao/Tauri source, not tested
by sending a system shortcut or changing focus. It is not native-input evidence.

A development-only `webdriver` Cargo feature pins
`tauri-plugin-wdio-webdriver` **1.4.0** for actual WebView interaction and snapshots.
It is registered only in debug builds with that explicit feature. Normal builds
have no automation listener. No command mocking, screen capture permission, or
OS input is needed for the controlled acceptance surface.

## Logging

Use `tracing` **0.1.41** and `tracing-subscriber` **0.3.20** with an
application-internal layer. Rust events and explicitly imported structured
Script records converge at the same sanitizer and fan-out. A process-local
subscriber does not collect child logs implicitly.

Do not use `tracing-appender` here: its queue-drop counter does not establish
whether a worker's disk write succeeded. The required size rotation, independent
I/O-failure status, and bounded shutdown would still need explicit ownership.
A small owned writer provides these contracts without a general logging library.

The GUI delivery queue and file queue each hold at most 256 records. The GUI
context owns displayed items; its separately persisted retention limit is
1–10,000, initially 1,000. File output is JSONL, at most three 1 MiB files.
Correlation fields are shared before fan-out; delivery to both sinks is not an
atomic transaction. Sensitive fields are redacted before either output.

Queue loss, display eviction, and writer failures are separate counters. A failed
file sink stops without recursive logging or retry and leaves GUI/control paths
available. Custom recovery diagnostics pass through the same bounded sanitizer;
ordinary script labels such as `ocr` are not themselves sensitive content.
The retained writer guard waits at most 500 ms on shutdown. Its incomplete or
failed status is observed through an independent sanitized stderr diagnostic,
bounded to a further 50 ms wait, not through the failed file sink. An expired
flush or abrupt process exit never promises persistence. Run results remain
independent of all logging sinks.
