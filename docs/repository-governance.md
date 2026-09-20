# Repository governance

## Intended protection and its limits

The product default branch is `main`. Contributor routes and merge selection
belong to [CONTRIBUTING.md](../CONTRIBUTING.md). The versioned administration
payloads are [main.json](../.github/rulesets/main.json) and
[topic-development.json](../.github/rulesets/topic-development.json).
Committing these files does not install or update GitHub settings. Administrators
must apply them explicitly and retain live readback evidence; a local check
cannot certify remote enforcement.

Both rulesets require PRs, resolved review conversations, strict `CI Gate` from
the observed GitHub Actions app, and no force pushes. They have no bypass actors,
zero required approvals, stale-approval dismissal, and no code-owner or last-push
approval requirement. Zero approvals supports solo maintenance, not independent
review; add independent approval when a reliable second reviewer is available.

Main permits only merge commits and blocks deletion. Topics permit squash and
merge, allow deliberate retirement, and match both `refs/heads/dev/*` and the
recursive catch-all `refs/heads/dev/**/*`. CI rejects nested topic names even
though the catch-all protects them. No linear-history rule is used because
promotion and reconciliation must preserve merge ancestry.

Repository settings enable merge and squash, disable rebase merge, and disable
automatic branch deletion. Topic rules cannot distinguish sync PRs from ordinary
leaf PRs; maintainers must select the correct method. The metadata check cannot
prove urgency, content correctness, or branch ancestry and is not an immutable
security boundary against an authorized maintainer changing the workflow.

Topic required checks use `do_not_enforce_on_create: true`. A maintainer may
create a topic at a verified, successfully checked `main` tip, whose push result
is not the PR-only required context. Later updates remain protected. This
exemption does not enforce the chosen starting commit server-side.

## Administrative preparation

Use an authenticated GitHub CLI account with repository administration access.
Run the following from the product checkout, not `rasen/`. These procedures
change public repository settings; do not run them as ordinary contributor setup
or in Actions. The examples use a POSIX shell and the public repository identity:

```sh
export REPO=pashifika/mado-mata
gh auth status
gh api "repos/$REPO" \
  --jq '{full_name,default_branch,permissions,allow_merge_commit,allow_squash_merge,allow_rebase_merge,delete_branch_on_merge}'
gh api --paginate "repos/$REPO/rulesets?includes_parents=true&per_page=100"
gh api --paginate "repos/$REPO/pulls?state=open&per_page=100" \
  --jq '.[] | {number,head:.head.ref,base:.base.ref}'
```

Record current refs, repository settings, every applicable ruleset's ID and full
payload, legacy branch protection, and the intended repair/rollback boundary in
private administrative evidence. Inspect inherited rules as well as repository
rules; an inaccessible setting or unexpected overlap is a blocker, not permission
to overwrite it. Pause unrelated writes during migration/bootstrap.

## Observe the PR check before activation

Do not activate an unobserved required context. Identify a successful PR run of
the tracked [workflow](../.github/workflows/ci.yml), confirm its PR and revision,
and inspect the exact gate and app. Read the actual run ID interactively:

```sh
gh run list --repo "$REPO" --workflow ci.yml --event pull_request
printf 'Successful PR run ID: '
read -r RUN_ID
gh run view "$RUN_ID" --repo "$REPO" \
  --json event,conclusion,headSha,url
CHECK_RUN_URL=$(gh api --paginate "repos/$REPO/actions/runs/$RUN_ID/jobs?per_page=100" \
  --jq '.jobs[] | select(.name == "CI Gate") | .check_run_url')
gh api "$CHECK_RUN_URL" \
  --jq '{name,conclusion,head_sha,app:{id:.app.id,slug:.app.slug},html_url}'
```

Require one successful `CI Gate` from the expected workflow's PR run and the
GitHub Actions app, not a push/manual result or an unrelated check with the same
name. Compare the returned positive integer app ID with `integration_id` in both
payloads. Resolve discrepancies through a reviewed payload change before
installation; do not guess an ID or remove source binding to make a gate pass.
See [CI gate semantics](ci.md#hosted-workflow-and-required-gate).

## Install or update rulesets

Inspect the checked-out payload and the live list together. Match a payload's
`name` to its repository-owned ruleset; record its ID. If names collide or an
inherited rule owns the intended policy, stop and reconcile ownership. Never
create another copy merely because an update is inconvenient.

For the main payload, select the real file:

```sh
RULESET_FILE=.github/rulesets/main.json
python3 -m json.tool "$RULESET_FILE"
gh api --paginate "repos/$REPO/rulesets?includes_parents=true&per_page=100" \
  --jq '.[] | {id,name,source_type,source,enforcement}'
```

Only when no matching repository ruleset exists, create it and retain the
returned ID:

```sh
gh api --method POST "repos/$REPO/rulesets" --input "$RULESET_FILE"
```

For an existing ruleset, enter its inspected ID and replace its configuration
with the reviewed payload instead:

```sh
printf 'Existing repository ruleset ID: '
read -r RULESET_ID
gh api "repos/$REPO/rulesets/$RULESET_ID"
gh api --method PUT "repos/$REPO/rulesets/$RULESET_ID" --input "$RULESET_FILE"
gh api "repos/$REPO/rulesets/$RULESET_ID"
```

Repeat the appropriate create or update operation with
`RULESET_FILE=.github/rulesets/topic-development.json`. Do not execute both
create and update as an unconditional recipe. Apply repository merge settings
separately:

```sh
gh api --method PATCH "repos/$REPO" \
  -F allow_merge_commit=true -F allow_squash_merge=true \
  -F allow_rebase_merge=false -F delete_branch_on_merge=false
```

## Read back and prove effective enforcement

Read active rules for both intended and accidentally nested names. The rules
endpoint supports names of branches that do not yet exist, so these queries do
not create test branches:

```sh
gh api --paginate "repos/$REPO/rules/branches/main?per_page=100"
gh api --paginate "repos/$REPO/rules/branches/dev%2Frepository-governance?per_page=100"
gh api --paginate "repos/$REPO/rules/branches/dev%2Fruntime-comparison?per_page=100"
gh api --paginate "repos/$REPO/rules/branches/dev%2Fnested%2Fprobe?per_page=100"
gh api "repos/$REPO" \
  --jq '{default_branch,allow_merge_commit,allow_squash_merge,allow_rebase_merge,delete_branch_on_merge}'
```

Compare the effective PR, conversation, force-push, deletion, merge-method,
strict-check, and app-source requirements with the payloads. Inspect the full
ruleset readback for empty bypass actors, creation exemption, and active
enforcement. Do not infer GitHub ruleset pattern behavior from Actions globs.

Retain a real passing PR run and a controlled failing PR that GitHub reports as
blocked, then correct the failure and rerun checks. Do not merge intentional
failure, attempt a force push, or delete a protected branch to test protection.
Verify the implementation enters its topic by squash and reaches `main` through
a checked merge-commit promotion. Confirm the final main push run and new-topic
creation at that checked tip, with effective protection on subsequent updates.
Until these exercises pass, live governance setup and the M0 prerequisite remain
incomplete even if all source checks pass.

## Default-branch migration and bootstrap

The product migration renames `master` to `main`; it must preserve the old tip
and reachable history and leave the nested planning repository unchanged. Before
performing or recovering a migration, record the old SHA, local/remote refs,
open PR bases, rules, and administrative permission. Stop on unrelated local
work, a conflicting `main`, or remote movement since inspection.

For a repository still on the recorded original state, the administrator uses
GitHub's rename operation rather than publishing an independently created main:

```sh
gh api --method POST "repos/$REPO/branches/master/rename" -f new_name=main
```

Do not repeat the rename on an already migrated repository. Existing clones
with an unambiguous local `master` and no local `main` follow GitHub's local
reference update procedure:

```sh
git branch -m master main
git fetch origin
git branch --set-upstream-to=origin/main main
git remote set-head origin --auto
```

Read back GitHub's default branch, verify the recorded old tip is an ancestor of
`origin/main`, and inspect local upstream and remote HEAD. Review active PR
bases, workflow triggers, raw URLs, and other branch-dependent configuration;
redirects do not update every consumer. Do not force-reset a conflicting branch
or create an obsolete compatibility branch.

When no workflow exists yet, first install the non-check PR/force-push/deletion
protections and merge settings, with no bypass actors. Record the temporary
absence of the required-check rule explicitly; it is not the final payload.
Create `dev/repository-governance` from the verified main tip and the governance
Change branch from that topic. After the real PR gate and its app are observed,
update the same ruleset IDs with the final checked-in payloads. Complete live
pass/fail evidence before releasing the M0 prerequisite.

## Recovery without rewriting shared history

Preserve the last known valid refs/settings and the failed operation's evidence.
If product work has advanced, fix forward through checked PRs; never roll back
by force-resetting a protected branch. An administrator may make a narrowly
scoped, explicitly recorded settings repair for a wrong context, app binding,
or pattern. Restore enforcement and repeat readback and PR checks before
claiming recovery. Do not leave a standing bypass or silently lower protection.

Only before development has started, and after repeating conflict/history checks,
may an administrator reverse the branch rename and restore recorded references
and settings. A partially restored configuration is not successful completion.
Topic/leaf retirement follows the reachability and dependent-work conditions in
[CONTRIBUTING.md](../CONTRIBUTING.md#merge-and-retirement-responsibilities);
there is no automated deletion/reset recovery command.

The authoritative interfaces are GitHub's
[rules API](https://docs.github.com/en/rest/repos/rules),
[branch rename procedure](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-branches-in-your-repository/renaming-a-branch),
and [GitHub CLI API command](https://cli.github.com/manual/gh_api).
