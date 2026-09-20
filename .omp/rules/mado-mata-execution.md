---
description: OMP execution and discovery guidance for MadoMata
alwaysApply: true
---

# MadoMata OMP execution

Use the root `AGENTS.md` as the canonical project guide and follow its links for
contributor workflow. Do not maintain a second policy body in OMP configuration.
Use the installed relevant skills and OMP's specialized tools; personal skill
search paths are local setup, not prerequisites for the public repository.

Start OMP from the product repository root. This rule is discovered from the
startup cwd's `.omp/rules/`, not by walking ancestor rule directories. After
changing rules or context files, verify their effective discovery in a fresh
session using the TUI's `/extensions` or RPC `get_state` context inspection;
file presence alone is not proof of activation.
See `docs/development-guidance.md` for ownership and discovery limitations.
