"""Repository-specific governance invariants, not a general rules engine."""

import json
from pathlib import Path
import re
import subprocess

import yaml

from check import NODE_VERSION, RUST_VERSION
from gate import CHECKS_IF, EXPECTED_JOBS, GATE_IF, GATE_NAME_EXPRESSION, GATE_NAMES, SELECTOR_JOB

PRIVATE_ROOTS = {"rasen", ".rasen", "examples", "local_docs", ".cache", ".venv"}
SHARED_RULE = ".omp/rules/mado-mata-execution.md"
REQUIRED_FILES = {
    "AGENTS.md", "CLAUDE.md", "CONTRIBUTING.md", SHARED_RULE,
    "docs/ci.md", "docs/repository-governance.md", "docs/development-guidance.md",
    ".github/workflows/ci.yml", ".github/rulesets/main.json",
    ".github/rulesets/topic-development.json", "tools/ci/toolchain.json",
    "tools/ci/check.py", "tools/ci/branch_flow.py", "tools/ci/gate.py", "tools/ci/select_checks.py",
    "tools/ci/policy.py", "tools/ci/tooling.py", "tools/ci/install_tools.py",
    "tools/ci/test_ci.py", "tools/ci/requirements.txt",
    "tools/runtime-comparison/Cargo.toml", "tools/runtime-comparison/Cargo.lock",
    "tools/runtime-comparison/compiler/package.json", "tools/runtime-comparison/compiler/package-lock.json",
    "tools/runtime-comparison/compiler/compile.mjs",
}
CONCURRENCY_GROUP = "${{ github.workflow }}-${{ github.event_name }}-${{ github.event.pull_request.number || github.ref }}"
PR_TYPES = {"opened", "synchronize", "reopened", "ready_for_review", "edited"}
PUSH_NAME_SUFFIX = "${{ github.event_name == 'push' && ' (push)' || '' }}"
HOSTED_RUNNERS = {
    SELECTOR_JOB: "ubuntu-24.04",
    "branch-flow": "ubuntu-24.04",
    "repository": "ubuntu-24.04",
    "runtime-macos": "macos-15",
    "runtime-windows": "windows-2025",
    "gate": "ubuntu-24.04",
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def unique_mapping(pairs):
    result = {}
    for key, value in pairs:
        require(isinstance(key, str), "mapping keys must be strings")
        require(key not in result, f"duplicate mapping key: {key!r}")
        result[key] = value
    return result


class WorkflowLoader(yaml.SafeLoader):
    """Safe YAML with Actions-compatible booleans and duplicate keys refused."""


def yaml_mapping(loader, node):
    return unique_mapping(loader.construct_pairs(node, deep=True))


WorkflowLoader.add_constructor(yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, yaml_mapping)
WorkflowLoader.yaml_implicit_resolvers = {
    key: [(tag, pattern) for tag, pattern in entries if tag != "tag:yaml.org,2002:bool"]
    for key, entries in WorkflowLoader.yaml_implicit_resolvers.items()
}
WorkflowLoader.add_implicit_resolver(
    "tag:yaml.org,2002:bool", re.compile(r"^(?:true|false|True|False|TRUE|FALSE)$"), list("tTfF")
)


def read_json(path):
    try:
        return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=unique_mapping)
    except (OSError, ValueError, RecursionError) as error:
        raise ValueError(f"{path.name}: {error}") from error


def read_workflow(path):
    try:
        return yaml.load(path.read_text(encoding="utf-8"), Loader=WorkflowLoader)
    except (OSError, ValueError, yaml.YAMLError, RecursionError) as error:
        raise ValueError(f"{path.name}: invalid workflow YAML: {error}") from error


def tracked_files(root):
    result = subprocess.run(
        ["git", "ls-files", "-z", "--cached"], cwd=root,
        check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    return sorted(set(result.stdout.decode("utf-8").rstrip("\0").split("\0")))


def check_paths(paths):
    for path in paths:
        parts = Path(path).parts
        require(bool(parts) and not Path(path).is_absolute() and ".." not in parts,
                f"invalid tracked path: {path!r}")
        require(parts[0] not in PRIVATE_ROOTS, f"private path must not be tracked: {path!r}")
        require(parts[0] != ".omp" or path == SHARED_RULE,
                f"unapproved OMP machine/state path: {path!r}")
        require("__pycache__" not in parts and not path.endswith(".pyc"),
                f"generated Python state must not be tracked: {path!r}")
        require(Path(path).name != ".lycheeignore",
                f"link-check exclusions must not silently suppress tracked links: {path!r}")
    missing = REQUIRED_FILES - set(paths)
    require(not missing, "required tracked files missing: " + ", ".join(sorted(missing)))


def check_claude(root):
    path = root / "CLAUDE.md"
    require(path.is_symlink() and path.readlink() == Path("AGENTS.md"),
            "CLAUDE.md must be a relative symlink to AGENTS.md; enable Git symlink support")
    require((root / "AGENTS.md").is_file() and not (root / "AGENTS.md").is_symlink(),
            "AGENTS.md must be a regular canonical guide")


def check_ruleset(data, topic):
    label = "topic-development" if topic else "main"
    require(isinstance(data, dict), f"{label}: ruleset must be an object")
    require(data.get("name") == label and data.get("target") == "branch"
            and data.get("enforcement") == "active", f"{label}: incorrect identity, target, or enforcement")
    require(data.get("bypass_actors") == [], f"{label}: standing bypass actors are forbidden")
    include = ["refs/heads/dev/*", "refs/heads/dev/**/*"] if topic else ["refs/heads/main"]
    require(data.get("conditions") == {"ref_name": {"include": include, "exclude": []}},
            f"{label}: incorrect branch targets or exclusions")
    rules = data.get("rules")
    require(isinstance(rules, list), f"{label}: rules must be an array")
    indexed = {}
    for rule in rules:
        require(isinstance(rule, dict) and isinstance(rule.get("type"), str), f"{label}: invalid rule")
        kind = rule["type"]
        require(kind not in indexed, f"{label}: duplicate rule {kind}")
        indexed[kind] = rule
    expected = {"non_fast_forward", "pull_request", "required_status_checks"}
    if not topic:
        expected.add("deletion")
    require(set(indexed) == expected, f"{label}: rules must be exactly {sorted(expected)}")
    for kind in expected - {"pull_request", "required_status_checks"}:
        require(indexed[kind] == {"type": kind}, f"{label}: invalid {kind} rule")
    pr = indexed["pull_request"].get("parameters")
    require(isinstance(pr, dict), f"{label}: missing pull_request parameters")
    for key, value in {
        "dismiss_stale_reviews_on_push": True,
        "require_code_owner_review": False,
        "require_last_push_approval": False,
        "required_review_thread_resolution": True,
    }.items():
        require(pr.get(key) is value, f"{label}: incorrect {key}")
    require(type(pr.get("required_approving_review_count")) is int
            and pr["required_approving_review_count"] == 0, f"{label}: required approvals must be zero")
    methods = ["squash", "merge"] if topic else ["merge"]
    require(pr.get("allowed_merge_methods") == methods, f"{label}: incorrect allowed_merge_methods")
    status = indexed["required_status_checks"].get("parameters")
    require(isinstance(status, dict), f"{label}: missing required_status_checks parameters")
    require(status.get("strict_required_status_checks_policy") is True,
            f"{label}: required checks must be strict")
    require(status.get("do_not_enforce_on_create") is topic, f"{label}: incorrect creation exemption")
    checks = status.get("required_status_checks")
    require(isinstance(checks, list) and len(checks) == 1 and isinstance(checks[0], dict),
            f"{label}: exactly one required check is expected")
    check = checks[0]
    require(check.get("context") == GATE_NAMES["pull_request"], f"{label}: required context must be CI Gate")
    app = check.get("integration_id")
    require(type(app) is int and app > 0, f"{label}: integration_id must be a positive integer")
    return app


def check_workflow_hygiene(workflow, pins, label):
    require(isinstance(workflow, dict), f"{label}: workflow must be an object")
    require(workflow.get("permissions") == {"contents": "read"}, f"{label}: permissions must be contents: read")
    triggers = workflow.get("on")
    require(isinstance(triggers, dict) and "pull_request_target" not in triggers,
            f"{label}: triggers must be a mapping without pull_request_target")
    require("secrets." not in json.dumps(workflow).lower(), f"{label}: CI must not consume secrets")
    jobs = workflow.get("jobs")
    require(isinstance(jobs, dict) and jobs, f"{label}: jobs must be a nonempty object")
    require("defaults" not in workflow, f"{label}: workflow defaults must not redirect public commands")
    for name, job in jobs.items():
        where = f"{label}/{name}"
        require(isinstance(job, dict), f"{where}: job must be an object")
        expected_runner = HOSTED_RUNNERS.get(name, "ubuntu-24.04") if label == "ci.yml" else "ubuntu-24.04"
        require(job.get("runs-on") == expected_runner, f"{where}: use the declared hosted runner {expected_runner}")
        timeout = job.get("timeout-minutes")
        require(type(timeout) is int and 1 <= timeout <= 30, f"{where}: timeout must be 1-30 minutes")
        if label == "ci.yml" and name == SELECTOR_JOB:
            require(job.get("permissions") == {"contents": "read", "pull-requests": "read"},
                    f"{where}: only the selector may read promotion PR metadata")
        else:
            require(job.get("permissions", {"contents": "read"}) in ({}, {"contents": "read"}),
                    f"{where}: job permissions must not escalate authority")
        require(not {"continue-on-error", "strategy", "container", "services", "defaults"} & job.keys(),
                f"{where}: no optional, matrix, container jobs, or command overrides")
        steps = job.get("steps")
        require(isinstance(steps, list) and steps, f"{where}: steps must be a nonempty array")
        checkouts = 0
        for step in steps:
            require(isinstance(step, dict), f"{where}: each step must be an object")
            require(("uses" in step) != ("run" in step), f"{where}: step must have exactly one of uses or run")
            runtime_upload = (
                label == "ci.yml" and name in {"repository", "runtime-macos", "runtime-windows"}
                and isinstance(step.get("uses"), str)
                and step["uses"].startswith("actions/upload-artifact@")
            )
            require("continue-on-error" not in step,
                    f"{where}: mandatory steps cannot ignore failures")
            if runtime_upload:
                require(step.get("if") in ("${{ always() }}", "always()"),
                        f"{where}: controlled evidence upload must run with always()")
                require("actions/upload-artifact" in pins,
                        f"{where}: controlled evidence upload must have a manifest pin")
            else:
                require("if" not in step, f"{where}: mandatory steps cannot be skipped")
            require("working-directory" not in step, f"{where}: public commands must run from the checkout root")
            if "uses" in step:
                uses = step["uses"]
                require(isinstance(uses, str) and re.fullmatch(r"[\w.-]+/[\w./-]+@[0-9a-f]{40}", uses),
                        f"{where}: every action must use a full commit SHA")
                action, sha = uses.split("@")
                if action in pins:
                    require(sha == pins[action]["sha"], f"{where}: {action} disagrees with toolchain.json")
                if action == "actions/checkout":
                    checkouts += 1
                    settings = step.get("with")
                    require(isinstance(settings, dict) and settings.get("persist-credentials") is False,
                            f"{where}: checkout must disable persist-credentials")
                    require("ref" not in settings, f"{where}: checkout must use the event's default revision")
            elif "run" in step:
                require(isinstance(step["run"], str) and "${{" not in step["run"],
                        f"{where}: pass event metadata as data, never interpolate shell source")
            else:
                raise ValueError(f"{where}: step needs uses or run")
        require(checkouts == 1, f"{where}: exactly one credential-free checkout is required")


def check_ci_workflow(workflow, pins):
    check_workflow_hygiene(workflow, pins, "ci.yml")
    triggers = workflow["on"]
    require(set(triggers) == {"pull_request", "push", "workflow_dispatch"}, "ci.yml: unexpected triggers")
    pr = triggers["pull_request"]
    require(isinstance(pr, dict) and set(pr) == {"branches", "types"},
            "ci.yml: PR trigger requires branches and types, with no path filters")
    require(pr["branches"] == ["main", "dev/**"], "ci.yml: incorrect PR target branches")
    require(isinstance(pr["types"], list) and all(isinstance(item, str) for item in pr["types"])
            and set(pr["types"]) == PR_TYPES, "ci.yml: PR triggers must include base edits")
    require(triggers["push"] == {"branches": ["main", "dev/**"]}, "ci.yml: incorrect push trigger or path filter")
    require(triggers["workflow_dispatch"] in (None, {}), "ci.yml: manual dispatch must not require inputs")
    require(workflow.get("concurrency") == {"group": CONCURRENCY_GROUP, "cancel-in-progress": True},
            "ci.yml: concurrency must isolate events/PRs/refs and cancel superseded runs")
    jobs = workflow["jobs"]
    require(set(jobs) == EXPECTED_JOBS | {SELECTOR_JOB, "gate"},
            "ci.yml: job set disagrees with selector and mandatory gate dependencies")
    selector = jobs[SELECTOR_JOB]
    require(selector.get("if") == "${{ github.event_name == 'push' }}" and "needs" not in selector,
            "ci.yml: the promotion PR selector must run independently on pushes only")
    require(selector.get("timeout-minutes") == 2, "ci.yml: the selector must finish within two minutes")
    require(set(selector) <= {"name", "if", "runs-on", "timeout-minutes", "permissions", "outputs", "steps"},
            "ci.yml: the selector must not override execution context")
    require(selector.get("name", SELECTOR_JOB) not in GATE_NAMES.values(),
            "ci.yml: gate contexts are reserved for the aggregate job")
    require(selector.get("outputs") == {"skip-checks": "${{ steps.promotion-pr.outputs.skip-checks }}"},
            "ci.yml: selector output must come from the promotion PR lookup")
    selector_steps = selector["steps"]
    require(len(selector_steps) == 2
            and selector_steps[0].get("uses", "").startswith("actions/checkout@")
            and selector_steps[0].get("with") == {"persist-credentials": False}
            and set(selector_steps[0]) <= {"name", "uses", "with"},
            "ci.yml: selector must use only the pinned event checkout followed by the lookup")
    lookup = selector_steps[1]
    require(lookup.get("id") == "promotion-pr"
            and lookup.get("env") == {"GH_TOKEN": "${{ github.token }}"}
            and set(lookup) <= {"name", "id", "run", "env"},
            "ci.yml: selector must expose the lookup output and use only the read-only workflow token")
    for name in EXPECTED_JOBS:
        require(jobs[name].get("if") == CHECKS_IF
                and jobs[name].get("needs") in (SELECTOR_JOB, [SELECTOR_JOB]),
                f"{name}: mandatory checks may skip only a push covered by its promotion PR")
        display_name = jobs[name].get("name")
        require(isinstance(display_name, str) and display_name.endswith(PUSH_NAME_SUFFIX)
                and bool(display_name.removesuffix(PUSH_NAME_SUFFIX).strip())
                and "${{" not in display_name.removesuffix(PUSH_NAME_SUFFIX)
                and display_name.removesuffix(PUSH_NAME_SUFFIX).strip() not in GATE_NAMES.values(),
                f"{name}: mandatory job names must distinguish push checks without claiming gate contexts")
    gate = jobs["gate"]
    require(gate.get("name") == GATE_NAME_EXPRESSION, "ci.yml: gate names must distinguish PR, push, and manual events")
    require(gate.get("if") == GATE_IF, "ci.yml: gate must always evaluate unless a push is covered by its promotion PR")
    needs = gate.get("needs")
    dependencies = EXPECTED_JOBS | {SELECTOR_JOB}
    require(isinstance(needs, list) and all(isinstance(name, str) for name in needs)
            and set(needs) == dependencies and len(needs) == len(dependencies),
            "ci.yml: gate needs must match the selector and every mandatory job")
    commands = {
        SELECTOR_JOB: ["python3 tools/ci/select_checks.py"],
        "branch-flow": ["python3 tools/ci/branch_flow.py"],
        "repository": [
            f"rustup toolchain install {RUST_VERSION} --profile minimal",
            "python3 -m pip install --require-hashes -r tools/ci/requirements.txt",
            "python3 tools/ci/install_tools.py", "python3 tools/ci/check.py",
        ],
        "gate": ["python3 tools/ci/gate.py"],
        "runtime-macos": [
            "python3 -c \"import platform; print(platform.platform(), platform.machine()); assert platform.machine() == 'arm64'\"",
            f"rustup toolchain install {RUST_VERSION} --profile minimal",
            "python3 -m pip install --require-hashes -r tools/ci/requirements.txt",
            "python3 tools/ci/check.py --runtime-only",
        ],
        "runtime-windows": [
            "git config --global core.symlinks true",
            f"rustup toolchain install {RUST_VERSION} --profile minimal",
            "python -m pip install --require-hashes -r tools/ci/requirements.txt",
            "python tools/ci/check.py --runtime-only",
        ],
    }
    for name, expected in commands.items():
        actual = [step["run"].strip() for step in jobs[name]["steps"] if "run" in step]
        require(actual == expected, f"ci.yml/{name}: required public commands must run without masking failures")
    for name in ("repository", "runtime-macos", "runtime-windows"):
        steps = jobs[name]["steps"]
        for action, settings in (
            ("actions/setup-python", {"python-version": "3.13"}),
            ("actions/setup-node", {"node-version": NODE_VERSION, "package-manager-cache": False}),
        ):
            setup = [step for step in steps if step.get("uses", "").startswith(action + "@")]
            require(len(setup) == 1 and setup[0].get("with") == settings,
                    f"ci.yml/{name}: {action} must install the pinned tool without implicit caching")
        check_index = next(index for index, step in enumerate(steps) if "tools/ci/check.py" in step.get("run", ""))
        uploads = [step for step in steps if step.get("uses", "").startswith("actions/upload-artifact@")]
        require(len(uploads) == 1 and steps[check_index + 1:] == uploads,
                f"ci.yml/{name}: exactly one controlled evidence upload must immediately follow checks")
        require(uploads[0].get("with") == {
            "name": "controlled-runtime-${{ runner.os }}-${{ runner.arch }}",
            "path": (
                ".cache/repository-ci/runtime-results/runtime-results.json\n"
                ".cache/repository-ci/runtime-results/runtime-stderr.log\n"
            ),
            "retention-days": 7,
            "include-hidden-files": True,
            "if-no-files-found": "warn",
        }, f"ci.yml/{name}: upload only the two controlled evidence files with bounded retention")
        require(all(index < check_index for index, step in enumerate(steps)
                    if "uses" in step and step not in uploads),
                f"ci.yml/{name}: checkout and tool setup must precede checks")
    windows = jobs["runtime-windows"]["steps"]
    require(windows[0].get("run") == "git config --global core.symlinks true",
            "ci.yml/runtime-windows: preserve symlinks before checkout")
    gate_step = next(step for step in gate["steps"] if "run" in step)
    require(gate_step.get("env") == {"NEEDS_JSON": "${{ toJSON(needs) }}"},
            "ci.yml: gate must read the actual needs results through NEEDS_JSON")


def check_repository(root, paths, manifest):
    check_paths(paths)
    check_claude(root)
    apps = []
    for name, topic in (("main", False), ("topic-development", True)):
        apps.append(check_ruleset(read_json(root / ".github" / "rulesets" / f"{name}.json"), topic))
    require(apps[0] == apps[1], "rulesets disagree on the required GitHub Actions integration_id")
    for path in paths:
        if path.startswith(".github/workflows/") and Path(path).suffix in {".yml", ".yaml"}:
            workflow = read_workflow(root / path)
            if path == ".github/workflows/ci.yml":
                check_ci_workflow(workflow, manifest["actions"])
            else:
                check_workflow_hygiene(workflow, manifest["actions"], path)
                require(all(job.get("name", name) not in GATE_NAMES.values()
                            for name, job in workflow["jobs"].items()),
                        f"{path}: gate contexts are reserved for ci.yml")
