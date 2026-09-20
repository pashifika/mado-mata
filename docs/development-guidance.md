# Development guidance ownership

## Canonical sources

Edit the owner of a rule rather than copying it into another handbook:

| File | Responsibility |
| --- | --- |
| [AGENTS.md](../AGENTS.md) | Cross-agent setup entry, execution constraints, verification and safety boundaries |
| [CLAUDE.md](../CLAUDE.md) | Relative symlink to `AGENTS.md`, without a duplicated policy body |
| [CONTRIBUTING.md](../CONTRIBUTING.md) | Human branch, PR, emergency, sync, and retirement workflow |
| [repository-governance.md](repository-governance.md) | Administrative settings, installation, readback, migration, and recovery |
| [ci.md](ci.md) | Reproducible check installation, commands, coverage, and limitations |
| [.omp/rules/mado-mata-execution.md](../.omp/rules/mado-mata-execution.md) | OMP-specific execution/discovery pointers only |
| Private `rasen/config.yaml` | Planning context and proposal/design/specs/tasks constraints |

Use ordinary Markdown for the canonical guide, following
[agents.md](https://agents.md/). Keep the root guide short; introduce a nested
guide only when a real component has different requirements. Do not introduce
`.omp/AGENTS.md` or `.claude/CLAUDE.md` as competing root policy sources.

## OMP discovery and fresh-session verification

Launch `omp` from the product root. Native project rules are discovered from
`<cwd>/.omp/rules/*.{md,mdc}`; launching from a nested directory does not inherit
that root rules directory. Standalone ancestor `AGENTS.md` discovery is a
different mechanism and is not proof that the OMP rule loaded.

The shared rule uses a unique filename, a `description`, and
`alwaysApply: true`. Without a trigger condition, `alwaysApply` injects the body
as an always-apply rule. A description alone only advertises on-demand content;
`globs` alone does not guarantee automatic application. Do not name this rule
`RULES`, add TTSR fields, or duplicate it in `.omp/RULES.md`.

After editing instructions or upgrading OMP, verify a fresh root-launched
session instead of assuming hot reload:

1. Record the installed `omp --version` and launch directory.
2. In the interactive TUI, inspect `/extensions` for the project context and
   rule, including their source and any disabled or shadowed state.
3. Confirm the selected root context resolves to `AGENTS.md` content. OMP may
   select either same-depth standalone entry; both must resolve to the same
   canonical content through the relative symlink.
4. Read `rule://mado-mata-execution` in that session and inspect its always-apply
   state. Merely finding the file on disk is insufficient.
5. Inspect a separate nested-cwd session when verifying discovery behavior;
   absence of the root project rule there is the documented limitation. Return
   to a root-launched session for repository work.

The 2026-09-20 root-startup check with OMP 18.2.6 inspected a fresh RPC
`get_state` response: its system prompt contained the full root `AGENTS.md` and
the `MadoMata OMP execution` rule body. A fresh session from `docs/` inherited
root `AGENTS.md` but omitted that rule body. Preserve this effective-context
check when upgrading; a rule-list command that omits always-apply rules is not
an equivalent oracle. In RPC mode, use `get_state` directly: sending
`/extensions` as a prompt can start a model turn rather than open the TUI
inventory.

Use the installed `omp://context-files.md` and
`omp://rulebook-matching-pipeline.md` documentation for version-specific
troubleshooting. These are OMP internal resources, not public-check dependencies.
Record missing client/diagnostic access as unverified instead of claiming a
successful load. Do not repair discovery by copying private skill directories
or inventing `.omp/config.yml` keys.

## Claude symlink

`CLAUDE.md` is a relative symbolic link whose target is exactly `AGENTS.md`.
Edit the canonical file only. There is no separate Claude policy or Markdown
import to maintain. Launch Claude from the product root, as with OMP.

Git must materialize the link, not a regular file containing its target name.
On Windows, enable symbolic-link creation (for example, Developer Mode with
a supported Git installation) and `core.symlinks` before checkout. A fresh
symlink-enabled clone can be requested with:

```sh
git -c core.symlinks=true clone https://github.com/pashifika/mado-mata.git
```

If the environment cannot create symlinks, use a supported Linux checkout;
do not replace the link with copied instructions or a regular import file.
The repository check rejects flattened, absolute, wrong-target, and broken
canonical links. Symlink support is a contributor checkout prerequisite,
not a change to the application's intended Windows/macOS support.

Use this non-interactive client diagnostic from the product root:

```sh
claude -p /context --tools '' --strict-mcp-config \
  --mcp-config '{"mcpServers":{}}' --no-session-persistence \
  --setting-sources project --output-format json
```

Inspect the root `CLAUDE.md` memory entry in a fresh `/context` result after
upgrades, and verify the filesystem link resolves to the canonical guide.
Unlike a Markdown import, a symlink does not require a separate `AGENTS.md`
entry in that client inventory. Record unavailable client diagnostics as
unverified rather than claiming a successful load.

On 2026-09-20, Claude 2.1.231 listed the root `CLAUDE.md` from both root and
`docs/` startup. Filesystem verification confirmed the relative target and
that `CLAUDE.md` and `AGENTS.md` resolve to the same file. No model call was
needed for this client inventory check.

## Maintainer-only planning

Public source work and CI require no Rasen installation or private access.
Maintainers who perform planning must provision their authorized planning
checkout at `rasen/` as its own Git repository, not a submodule or product-tree
copy. Obtain its access and remote through private maintainer channels; do not
put a private remote URL in public setup instructions.

Before planning mutations, require `rasen/.git` and inspect
`git -C rasen rev-parse --show-toplevel`; its result must be exactly the nested
`rasen` directory. Stop if it resolves to the product root. Keep the planning
branch and upstream independent of product branches. Run planning Git operations
with `git -C rasen` or an equivalent nested working directory; never stage those
paths through the product repository or override their ignore rules.

Run Rasen from the product root with `RASEN_LANG=en` on every invocation and
preserve the CLI-resolved project/store/target-line selectors. Planning context
belongs in `rasen/config.yaml`; artifact-specific rules belong under its
`proposal`, `design`, `specs`, and `tasks` rules. They constrain approved scope,
English artifacts, branch/base selection, native evidence, upstream defect
ownership, and verification. They must reference the public workflow instead
of duplicating always-on agent instructions. Preserve the planning repository's
schema, profile, tools, and project identity when changing those rules.

Verify generated artifact instructions actually receive the context and rules.
Commit and push proposal/archive boundaries only in the planning repository to
its own configured upstream; deliver product changes through product PRs.

The product ignores `rasen/`, `.rasen/`, extracted `examples/`, `local_docs/`, and
machine-specific OMP state. Only intended shared `.omp/rules/` Markdown belongs
in product tracking. Leave existing `.omp/config.yml`, credentials, sessions,
personal skills, and evidence private and unchanged when sharing a rule.
