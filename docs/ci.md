# Repository and controlled runtime CI

## Install the pinned tools

Use Git, Python 3.11 or newer, Rustup, Node.js **24.18.0** with its bundled npm,
and a native C/C++ build toolchain. Install Node from its
[official release](https://nodejs.org/dist/v24.18.0/) and verify the release's
signed checksums. Rustup installs Rust **1.97.1** with the command below.
Linux needs a C compiler and linker; macOS needs Xcode Command Line Tools;
Windows needs the Visual Studio C++ build tools and Windows SDK for Rust's
MSVC target. The controlled build vendors Lua and does not require OCR models,
capture permissions, or a sibling engine checkout.

The actionlint/lychee installer supports Linux x86_64/aarch64 and macOS
arm64/x86_64. It does not provide Windows assets; use a supported Linux
environment for the full check, or run the documented controlled-runtime mode
on Windows. This tooling limitation is not an application support decision.

The pinned binary-only Python requirements cover CPython 3.11 through 3.14.
Other interpreter versions need reviewed wheel hashes; do not fall back to an
unpinned source build when installation refuses them.

Use a Python virtual environment rather than modifying an externally managed
system installation. In a POSIX shell:

```sh
python3 -m venv .cache/repository-ci/venv
. .cache/repository-ci/venv/bin/activate
python3 -m pip install --require-hashes -r tools/ci/requirements.txt
python3 tools/ci/install_tools.py
rustup toolchain install 1.97.1 --profile minimal
python3 tools/ci/check.py
```

The explicit installer downloads the host's pinned native tools, verifies their
SHA-256 values, and installs them under ignored `.cache/repository-ci`.
Installation needs network access; the local-link check does not contact remote
sites. Do not replace the installer with an unpinned package-manager download or
skip integrity verification when a download fails.

The full check runs `npm ci --ignore-scripts --no-audit --no-fund` only in the
application-owned compiler directory inside a temporary tracked-file snapshot.
It verifies Node's exact version, installs the lockfile's integrity-pinned
TypeScript package, and runs compiler self-checks before the Rust checks. It
never runs a workload package's installer or lifecycle scripts. Cargo and npm
need access to public dependency sources on a fresh checkout; only the
Markdown link check is offline.

The version and integrity sources are:

| Dependency | Pin | Authoritative file |
| --- | --- | --- |
| actionlint | 1.7.12 | [toolchain.json](../tools/ci/toolchain.json), including host assets and SHA-256 values |
| lychee | 0.24.2 | [toolchain.json](../tools/ci/toolchain.json), including host assets and SHA-256 values |
| PyYAML | 6.0.3 | [requirements.txt](../tools/ci/requirements.txt), hash-pinned Python distributions |
| Rust | 1.97.1 | [check.py](../tools/ci/check.py), explicit `cargo +1.97.1` |
| Node.js | 24.18.0 | [check.py](../tools/ci/check.py) and [workflow](../.github/workflows/ci.yml) |
| TypeScript | 5.9.3 | [Compiler manifest](../tools/runtime-comparison/compiler/package.json) and [lockfile](../tools/runtime-comparison/compiler/package-lock.json) |
| GitHub Actions | Full commit SHAs | [Workflow](../.github/workflows/ci.yml) and [toolchain.json](../tools/ci/toolchain.json) |

CI uses Python 3.13, `ubuntu-24.04`, `macos-15`, and `windows-2025`. The macOS job
prints and requires `arm64`; runtime checks also print their actual host identity.
Update versions, integrity values, workflow references, and this guide together
through a checked Change. A new installer host needs a verified release asset.

## Local check scope

The complete local entrypoint is:

```sh
python3 tools/ci/check.py
```

It covers repository policy, workflow/local-link checks, governance regression
tests, and real controlled runtime build, tests, compiler checks, and CLI
execution. Repository inputs come from tracked product files, not recursive
discovery of ignored planning, sibling checkouts, installed authoring packages,
or extracted reference applications. New files must enter the intended tracked
change before that inventory covers them; never force-add private directories.

The full check has these responsibilities:

- Validate ruleset JSON and intended semantics, exact required-context agreement,
  action pins, read-only workflow permissions, and checkout credential removal.
- Check the canonical guide/symlink and reject tracked private planning or
  unintended machine-local files.
- Run actionlint against tracked workflows. `-shellcheck=` and `-pyflakes=`
  deliberately disable its optional external integrations rather than depending
  on unpinned executables; Actions syntax and expression checking remain in scope.
- Run lychee in offline mode for local Markdown targets and fragments. External
  website availability is not a merge prerequisite.
- Exercise accepted/refused branch routes, malformed metadata, repository-policy
  failures, and gate outcomes through behavioral tests.
- Install and check the trusted TypeScript compiler with package scripts disabled.
- Build and test the locked Rust comparison executable, then execute its
  controlled `check` suite for direct Rust, JavaScript, TypeScript, and Lua.
  These commands do not enable the optional `engine` feature.

For governance policy and its behavioral tests only, after Python dependency
setup:

```sh
python3 tools/ci/check.py --policy-only
```

This does not run actionlint, lychee, Rust, Node, the TypeScript compiler, or the
controlled executable. It is not full CI evidence.

For policy, governance tests, and the controlled runtime without actionlint or
lychee:

```sh
python3 tools/ci/check.py --runtime-only
```

On Windows, use `python` in place of `python3` and preserve the tracked symlink
as described in [development guidance](development-guidance.md#claude-symlink).
Hosted Windows enables Git symlink checkout before fetching the repository.
The two narrower modes are mutually exclusive. Missing tools, invalid input,
or failing commands remain failures; neither mode substitutes for the full gate.
These checks do not install remote rulesets or verify live GitHub enforcement.

## Direct controlled commands

For iteration in the public working tree, install the trusted compiler and run
the same commands the local check uses:

```sh
npm ci --ignore-scripts --no-audit --no-fund --prefix tools/runtime-comparison/compiler
node tools/runtime-comparison/compiler/compile.mjs --self-check
cargo +1.97.1 build --locked --manifest-path tools/runtime-comparison/Cargo.toml
cargo +1.97.1 test --locked --manifest-path tools/runtime-comparison/Cargo.toml
cargo +1.97.1 run --locked --manifest-path tools/runtime-comparison/Cargo.toml -- check
```

`check` emits JSON evidence for the exercised controlled cases. The explicit
toolchain selector avoids dependence on the user's Rust default. To execute a
specific plan or summarize saved results, the CLI also accepts:

```sh
cargo +1.97.1 run --locked --manifest-path tools/runtime-comparison/Cargo.toml -- run <plan.json> <package-root>
cargo +1.97.1 run --locked --manifest-path tools/runtime-comparison/Cargo.toml -- report <results.json>
```

Replace the angle-bracket arguments with actual paths; they are not shell
redirections. Keep plans containing native paths and raw results private.
See the [runtime guide](runtime-comparison.md) for host and measurement contracts
and the [CI-first comparison report](runtime-comparison-results.md) for verified
scope and remaining qualification blockers.

## Hosted workflow and required gate

The [workflow](../.github/workflows/ci.yml) runs on PRs targeting `main` and
`dev/**`, protected-branch pushes, and manual dispatch. PR events include
`opened`, `synchronize`, `reopened`, `ready_for_review`, and `edited` so a base
change is rechecked. There are no workflow path filters. Manual dispatch becomes
available when the workflow is on the default branch.

The stable result is intentionally event-specific:

| Event | Gate check name | Required by branch rulesets |
| --- | --- | --- |
| Pull request | `CI Gate` | Yes |
| Push | `CI Gate (push)` | No |
| Manual dispatch | `CI Gate (manual)` | No |

The `gate` job runs with `always()` and requires all four explicit jobs:

| Job | Coverage |
| --- | --- |
| `branch-flow` | Event and branch-route validation |
| `repository` | Full local check, including the controlled executable on Linux |
| `runtime-macos` | Policy, governance tests, and controlled runtime on Apple Silicon macOS |
| `runtime-windows` | Policy, governance tests, and controlled runtime on Windows |

Only success from every expected job passes. Failure, cancellation, missing
results, unexpected dependencies, and skipped work cannot produce a successful
gate. There are no optional lanes. A successful push/manual result on the same
SHA cannot replace the PR-required result. The aggregate reads `NEEDS_JSON` as
data; keep its expected job set synchronized with the workflow when adding a lane.

Branch flow reads `GITHUB_EVENT_NAME`, `GITHUB_EVENT_PATH`, and
`GITHUB_REPOSITORY`; non-PR contexts also use `GITHUB_REF`. PR metadata is JSON
data, never shell source. Non-PR runs explicitly validate their event/ref context
instead of silently skipping the mandatory job. The accepted route policy is
owned by [CONTRIBUTING.md](../CONTRIBUTING.md#select-the-route-before-implementation).

The workflow uses read-only repository authority, credential-free checkout,
pinned actions/tools, bounded jobs, and event-scoped concurrency cancellation.
It does not use secrets, administration tokens, `pull_request_target`, private
Rasen access, or self-hosted interactive desktops. Superseding one PR run must
not cancel another PR's run or turn a cancellation into success.

Administrative activation requires an observed successful PR check and its
GitHub Actions app identity; use the
[governance runbook](repository-governance.md#observe-the-pr-check-before-activation).
A source-level policy check is not proof that GitHub is enforcing the payload.

## M0 and native qualification

Hosted jobs exercise the real comparison executable with deterministic controlled
observations and a non-native input sink. Cross-platform success is controlled
evidence, not replay, native qualification, desktop packaging, or runtime
adoption. The optional `engine` integration and its external prerequisites are
separate from these commands; no CI lane discovers windows, requests permission,
captures the desktop, changes focus, sends OS input, or operates a game.

M0 acceptance still requires explicitly authorized native Windows and Apple
Silicon macOS evidence for capture, template recognition, OCR, input, lifecycle,
and runtime adoption. Controlled success proves none of those native scenarios.
Missing authority, permissions, models, dependencies, or an OS lane leaves native
acceptance `BLOCKED` or `UNEXECUTED`, not passed. The qualification report remains
`Blocked(reason)` when neither candidate has the required native and budget
evidence. The [macOS-first development selection](adr/0001-runtime-comparison-boundaries.md#macos-first-development-selection)
allows manual development to proceed separately; it does not waive native
acceptance requirements or authorize hosted game operations.
