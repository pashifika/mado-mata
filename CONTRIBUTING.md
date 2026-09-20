# Contributing

## Public checkout and checks

Public contributions need Git, the Python/tool setup in [docs/ci.md](docs/ci.md),
and a GitHub account for PRs. They do not need Rasen, private design material,
a sibling engine checkout, or an agent's personal configuration.

The checkout must preserve `CLAUDE.md -> AGENTS.md` as a relative symlink.
See [Claude symlink setup](docs/development-guidance.md#claude-symlink) for Git
and Windows link-creation prerequisites; a flattened text file is not equivalent.

Run `python3 tools/ci/check.py` from the product root after the documented setup.
It checks governance, workflows, and documentation; no application build or
runtime command exists yet. Include actual commands, results, and unexecuted
scope in the PR. Keep private paths, credentials, screenshots, and raw native
execution evidence out of public descriptions.

## Select the route before implementation

Record the Change or issue identifier, scope, integration topic, and intended
PR base. Use a single lowercase kebab-case suffix after each branch prefix;
`dev/runtime-comparison` is valid, but nested names and empty suffixes are not.

Only these routes are supported:

| Head | Base | Head repository | Merge method |
| --- | --- | --- | --- |
| `change/<change-name>` or `fix/<change-name>` | `dev/<topic>` | Product or fork | Squash |
| `dev/<topic>` | `main` | Product only | Merge commit |
| `fix/<change-name>` for an emergency | `main` | Product or fork | Merge commit |
| `sync/<topic>` | The matching `dev/<topic>` | Product only | Merge commit |

An ordinary `change/` PR cannot target `main`. A fork cannot impersonate a
product integration or sync branch. The branch check validates metadata, not
whether an emergency is justified or the implementation is correct.

A maintainer creates each new topic from a current, successfully checked `main`
commit. Topic creation has a required-check exemption; subsequent updates need
checked PRs. This is not server-side enforcement of the branch's starting
ancestry. The intended next M0 route is `change/m0-runtime-comparison` into
`dev/runtime-comparison`; naming it here does not authorize starting M0 before
its governance prerequisite is verified.

## Ordinary Change and promotion

Start from the current integration tip, not `main`. For example, once the
maintainer has made `dev/runtime-comparison` available:

```sh
git fetch origin
git switch --create change/m0-runtime-comparison origin/dev/runtime-comparison
```

The example assumes `origin` is the product repository; fork contributors use
their configured product remote for the base and push to their own fork. Stop
if the intended branch already exists or local work prevents switching; do not
reset it. Push only the implementation branch and open a PR with an explicit
base. GitHub CLI users can select the base with:

```sh
gh pr create --base dev/runtime-comparison --head change/m0-runtime-comparison
```

The maintainer resolves conversations, confirms the current `CI Gate` succeeds,
and squash-merges the leaf PR. When the topic's intended scope is complete,
open `dev/<topic> -> main` and use a merge commit. Do not squash or rebase a
promotion: the topic and `main` must retain shared ancestry. If strict checks
require an update, reconcile through the sync procedure before promotion.

## Checked emergency fix

Use `fix/<change-name>` based on current `main` only for an urgent, narrow fix.
Its PR targets `main` and states the emergency reason, scope, actual verification,
and which active topics need reconciliation. Use a merge commit after the same
`CI Gate` and conversation requirements as a promotion. Urgency does not permit
a direct push, administrative merge bypass, or disabled check.

## Reconcile main into an active topic

Create a same-repository `sync/<topic>` from the current topic tip, then merge
`main` into it. For the runtime topic, the sequence is:

```sh
git fetch origin
git switch --create sync/runtime-comparison origin/dev/runtime-comparison
git merge origin/main
```

Resolve conflicts without discarding either branch's intended work, run the
checks, push the sync branch, and open its PR against `dev/runtime-comparison`.
Select a merge commit, not squash. If the topic moves, merge its latest tip
into the sync branch and rerun checks before merging the PR.

Do not use a direct `main -> dev/<topic>` PR. Strict up-to-date checks can require
its head to contain the topic's unpublished work. Starting the sync branch at
the topic keeps that work off `main` while preserving ancestry.

## Merge and retirement responsibilities

Zero required approvals permits solo maintenance; it is not independent review.
There is no standing bypass. Merge only after the required current checks pass
and conversations are resolved. Repository settings allow merge and squash,
not rebase; topic rules permit both and cannot select by head prefix, so the
maintainer must choose the method in the route table.

Automatic branch deletion is disabled. Retire a topic deliberately only after
promotion, inspection of open child PRs and dependent work, and confirmation
that its required history is reachable from `main`. Retire leaf and sync
branches only when their PRs and dependent work are finished. Main deletion and
protected-branch force pushes are prohibited; do not probe these destructively.

## Maintainer planning and administration

Maintainers keep Rasen context and Change artifacts in an independent private
nested repository, as described in
[development guidance](docs/development-guidance.md#maintainer-only-planning).
Public issues and PRs must contain enough scope and acceptance information to
contribute without that access. Do not publish private planning links.

Versioned rulesets do not install themselves. Administrative setup, migration,
live readback, and recovery belong to
[repository governance](docs/repository-governance.md), not contributor CI.

[Temporary required-gate failure probe](missing-governance-verification-target.md)
