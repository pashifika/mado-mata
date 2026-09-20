# Repository CI

## Install the pinned tools

Use Git and Python 3.11 or newer from the product repository root. The full
native-tool installation supports Linux x86_64/aarch64 and macOS arm64/x86_64.
Native Windows tool installation is not provided by this baseline; use a
supported Linux environment for the full check. This tooling limitation is not
an application platform-support decision.

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
python3 tools/ci/check.py
```

The explicit installer downloads the host's pinned native tools, verifies their
SHA-256 values, and installs them under ignored `.cache/repository-ci`.
Installation needs network access; the local-link check does not contact remote
sites. Do not replace the installer with an unpinned package-manager download or
skip integrity verification when a download fails.

The version and integrity sources are:

| Dependency | Pin | Authoritative file |
| --- | --- | --- |
| actionlint | 1.7.12 | [toolchain.json](../tools/ci/toolchain.json), including host assets and SHA-256 values |
| lychee | 0.24.2 | [toolchain.json](../tools/ci/toolchain.json), including host assets and SHA-256 values |
| PyYAML | 6.0.3 | [requirements.txt](../tools/ci/requirements.txt), hash-pinned Python distributions |
| GitHub Actions | Full commit SHAs | [Workflow](../.github/workflows/ci.yml) and [toolchain.json](../tools/ci/toolchain.json) |

CI uses `ubuntu-24.04` and Python 3.13. Update versions, integrity values,
workflow references, and this guide together through a checked Change. A new
host needs a verified release asset before claiming installer support.

## Local check scope

The complete local entrypoint is:

```sh
python3 tools/ci/check.py
```

It covers repository policy, maintained workflow/local-link checks, and the
behavioral regression suite. Repository inputs come from tracked product files,
not recursive discovery of ignored planning or extracted reference applications.
New files must enter the intended tracked change before that inventory can
cover them; never force-add private directories to make a check see them.

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

For policy and behavioral tests without the external native tools, after the
Python dependency setup:

```sh
python3 tools/ci/check.py --policy-only
```

This does not run actionlint or lychee and is not full CI evidence. Missing tools,
invalid input, or a failing check must remain failures; do not hide them with
skip flags or claim a narrower command covers the complete gate. These commands
do not install remote rulesets or verify live GitHub enforcement.

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

The `gate` job runs with `always()` and requires the explicit `branch-flow` and
`repository` jobs. Only success from every expected job passes. Failure,
cancellation, missing results, unexpected dependencies, and skipped work cannot
produce a successful gate. A successful push/manual result on the same SHA
cannot replace the PR-required result. The aggregate reads `NEEDS_JSON` as data;
keep its expected job set synchronized with the workflow when adding a lane.

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

This is a governance baseline, not application build or runtime qualification.
There are no existing MadoMata application build, launch, or runtime comparison
commands to run. The first delivery introducing the M0 executable harness must
add its real reproducible build and controlled checks to this stable gate and
update the command guidance in the same delivery. Record native/linker
prerequisites and unavailable lanes instead of adding dummy success jobs.

M0's initial library/runtime acceptance still needs explicitly authorized native
Windows and macOS evidence for capture, OCR, input, lifecycle, and runtime
adoption. Hosted success proves none of those scenarios. Missing authority,
permissions, dependencies, or an OS lane leaves that native acceptance unproven
or blocked. Later primary-OS application iteration does not inherit a mandatory
dual-OS execution gate from this governance baseline.
