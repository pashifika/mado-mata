"""Deterministic route, gate, policy, tracked-input, and runtime-output regressions."""

from copy import deepcopy
from contextlib import redirect_stderr, redirect_stdout
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from branch_flow import validate_event
import check
from check import copy_tracked
from gate import evaluate
from install_tools import extract_binary
from policy import (
    REQUIRED_FILES, check_ci_workflow, check_claude, check_paths,
    check_ruleset, read_json, read_workflow,
)
from tooling import load_manifest

CI_DIR = Path(__file__).resolve().parent
ROOT = CI_DIR.parents[1]
REPO = "owner/project"
GATE_JOBS = ("branch-flow", "repository", "runtime-macos", "runtime-windows")


def pull_request(base="dev/runtime", head="change/capture", fork=False):
    return {
        "action": "opened", "repository": {"full_name": REPO},
        "pull_request": {
            "base": {"ref": base, "repo": {"full_name": REPO}},
            "head": {"ref": head, "repo": {"full_name": "contributor/project" if fork else REPO}},
        },
    }


def cli(script, environment, directory):
    return subprocess.run(
        [sys.executable, "-B", str(CI_DIR / script)], cwd=directory,
        env={"PATH": os.environ.get("PATH", ""), "PYTHONDONTWRITEBYTECODE": "1", **environment},
        text=True, capture_output=True, check=False,
    )


class BranchFlowTests(unittest.TestCase):
    def test_routes(self):
        cases = [
            ("topic promotion", "main", "dev/runtime", False, True),
            ("fork impersonating topic", "main", "dev/runtime", True, False),
            ("emergency", "main", "fix/urgent-input", False, True),
            ("fork emergency", "main", "fix/urgent-input", True, True),
            ("ordinary main change", "main", "change/capture", False, False),
            ("topic change", "dev/runtime", "change/capture", False, True),
            ("fork topic change", "dev/runtime", "change/capture", True, True),
            ("fork topic fix", "dev/runtime", "fix/capture", True, True),
            ("matching synchronization", "dev/runtime", "sync/runtime", False, True),
            ("wrong sync topic", "dev/runtime", "sync/other", False, False),
            ("fork impersonating sync", "dev/runtime", "sync/runtime", True, False),
            ("main directly into topic", "dev/runtime", "main", False, False),
            ("topic into another topic", "dev/runtime", "dev/other", False, False),
            ("unprotected base", "change/runtime", "fix/capture", False, False),
            ("nested target", "dev/runtime/nested", "change/capture", False, False),
            ("nested head", "main", "dev/runtime/nested", False, False),
            ("uppercase slug", "dev/runtime", "change/Capture", False, False),
            ("empty slug", "main", "fix/", False, False),
            ("repeated separator", "dev/runtime", "change/two--words", False, False),
        ]
        for scenario, base, head, fork, accepted in cases:
            with self.subTest(scenario=scenario):
                event = pull_request(base, head, fork)
                if accepted:
                    message = validate_event("pull_request", event, REPO)
                    self.assertIn(f"{head} -> {base}", message)
                else:
                    with self.assertRaises(ValueError):
                        validate_event("pull_request", event, REPO)

    def test_malformed_or_inconsistent_event_metadata(self):
        cases = []
        for field in ("repository", "pull_request"):
            event = pull_request()
            event[field] = None
            cases.append(event)
        for field in ("base", "head"):
            for bad in (None, [], {"ref": 42, "repo": {"full_name": REPO}}, {"ref": "main", "repo": None}):
                event = pull_request()
                event["pull_request"][field] = bad
                cases.append(event)
        for bad_action in (None, [], {}, 1, "closed"):
            event = pull_request()
            event["action"] = bad_action
            cases.append(event)
        for location in ("repository", "base"):
            event = pull_request()
            if location == "repository":
                event["repository"]["full_name"] = "wrong/project"
            else:
                event["pull_request"]["base"]["repo"]["full_name"] = "wrong/project"
            cases.append(event)
        for event in [None, [], *cases]:
            with self.subTest(event=event), self.assertRaises(ValueError):
                validate_event("pull_request", event, REPO)

    def test_non_pr_contexts_are_explicit_and_checked(self):
        with tempfile.TemporaryDirectory() as directory:
            event_path = Path(directory) / "event.json"
            for name, ref, payload_ref, accepted in [
                ("push", "refs/heads/main", "refs/heads/main", True),
                ("push", "refs/heads/dev/runtime", "refs/heads/dev/runtime", True),
                ("workflow_dispatch", "refs/heads/fix/ci-manual-dispatch", "refs/heads/fix/ci-manual-dispatch", True),
                ("push", "refs/heads/main", "refs/heads/dev/runtime", False),
                ("push", "refs/heads/change/checks", "refs/heads/change/checks", False),
                ("workflow_dispatch", "refs/tags/v1", "refs/tags/v1", False),
                ("workflow_dispatch", "refs/heads/main", "main", False),
                ("workflow_dispatch", "refs/heads/main", "refs/heads/other", False),
                ("schedule", "refs/heads/main", "refs/heads/main", False),
                ("push", None, "refs/heads/main", False),
            ]:
                with self.subTest(name=name, ref=ref, payload_ref=payload_ref):
                    event_path.write_text(json.dumps({
                        "repository": {"full_name": REPO}, "ref": payload_ref,
                    }), encoding="utf-8")
                    environment = {
                        "GITHUB_EVENT_NAME": name, "GITHUB_EVENT_PATH": str(event_path),
                        "GITHUB_REPOSITORY": REPO,
                    }
                    if ref is not None:
                        environment["GITHUB_REF"] = ref
                    result = cli("branch_flow.py", environment, directory)
                    self.assertEqual(result.returncode, 0 if accepted else 1, result.stderr)

    def test_cli_refuses_bad_json_and_treats_refs_as_data(self):
        with tempfile.TemporaryDirectory() as directory:
            event_path = Path(directory) / "event.json"
            environment = {"GITHUB_EVENT_NAME": "pull_request", "GITHUB_EVENT_PATH": str(event_path), "GITHUB_REPOSITORY": REPO}
            for payload in ("{broken", json.dumps(pull_request(head="change/$(touch injected)")), json.dumps({"repository": {"full_name": REPO}, "action": []})):
                event_path.write_text(payload, encoding="utf-8")
                result = cli("branch_flow.py", environment, directory)
                self.assertEqual(result.returncode, 1)
                self.assertIn("Branch flow failed:", result.stderr)
                self.assertNotIn("Traceback", result.stderr)
                self.assertFalse((Path(directory) / "injected").exists())
            event_path.write_text(json.dumps(pull_request()), encoding="utf-8")
            self.assertEqual(cli("branch_flow.py", environment, directory).returncode, 0)


class GateTests(unittest.TestCase):
    def test_only_all_success_passes(self):
        for job in GATE_JOBS:
            for status in ("success", "failure", "cancelled", "skipped", "", None, "neutral"):
                with self.subTest(job=job, status=status):
                    needs = {name: {"result": "success"} for name in GATE_JOBS}
                    needs[job]["result"] = status
                    if status == "success":
                        evaluate(needs)
                    else:
                        with self.assertRaisesRegex(ValueError, job):
                            evaluate(needs)

    def test_missing_extra_and_malformed_dependencies_fail(self):
        success = {name: {"result": "success"} for name in GATE_JOBS}
        cases = [None, [], {}, {**success, "unexpected": {"result": "success"}}]
        for name in GATE_JOBS:
            cases.extend([
                {job: value for job, value in success.items() if job != name},
                {**success, name: {}},
                {**success, name: "success"},
            ])
        for needs in cases:
            with self.subTest(needs=needs), self.assertRaises(ValueError):
                evaluate(needs)

    def test_gate_cli_reads_json_and_fails_cleanly(self):
        with tempfile.TemporaryDirectory() as directory:
            for env in ({}, {"NEEDS_JSON": "{bad"}, {"NEEDS_JSON": "[]"}, {"NEEDS_JSON": '{"branch-flow": {"result": "success"}}'}):
                result = cli("gate.py", env, directory)
                self.assertEqual(result.returncode, 1)
                self.assertIn("CI gate failed:", result.stderr)
                self.assertNotIn("Traceback", result.stderr)
            result = cli("gate.py", {"NEEDS_JSON": json.dumps({name: {"result": "success"} for name in GATE_JOBS})}, directory)
            self.assertEqual(result.returncode, 0)


class RuntimeOutputTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name) / "checkout"
        self.snapshot = Path(directory.name) / "snapshot"
        self.root.mkdir()
        self.snapshot.mkdir()
        self.results = self.root / ".cache/repository-ci/runtime-results"
        self.stdout_path = self.results / "runtime-results.json"
        self.stderr_path = self.results / "runtime-stderr.log"

    def invoke(self, payload, diagnostics=b"", returncode=0):
        (self.snapshot / "stdout.bin").write_bytes(payload)
        (self.snapshot / "stderr.bin").write_bytes(diagnostics)
        command = [
            sys.executable, "-B", "-c",
            "from pathlib import Path; import sys; "
            "sys.stdout.buffer.write(Path('stdout.bin').read_bytes()); "
            "sys.stderr.buffer.write(Path('stderr.bin').read_bytes()); "
            "sys.exit(int(sys.argv[1]))",
            str(returncode),
        ]
        output, errors = io.StringIO(), io.StringIO()
        try:
            with redirect_stdout(output), redirect_stderr(errors):
                check.run_runtime_check(command, self.snapshot, self.results)
        finally:
            self.console = output.getvalue() + errors.getvalue()

    def test_large_passing_report_is_concise_and_byte_exact(self):
        rows = [
            {"id": f"expected-refusal-{index}", "status": "PASS", "execution_status": "FAIL",
             "oracle": "expected refusal", "observations": {"detail": ["raw-only-\u2603" * 100] * 20}}
            for index in range(256)
        ]
        payload = json.dumps({"version": 1, "check_passed": True, "rows": rows}, ensure_ascii=False, indent=2).encode("utf-8")
        diagnostics = b"cargo diagnostics\r\n" + b"verbose output\n" * 10_000 + b"\xff"
        self.invoke(payload, diagnostics)
        self.assertEqual(self.stdout_path.read_bytes(), payload)
        self.assertEqual(self.stderr_path.read_bytes(), diagnostics)
        self.assertLess(len(self.console.encode("utf-8")), 2_000)
        self.assertIn(str(len(rows)), self.console)
        self.assertIn(str(self.stdout_path), self.console)
        self.assertIn(str(self.stderr_path), self.console)
        self.assertNotIn("raw-only", self.console)
        self.assertNotIn("verbose output", self.console)

    def test_failed_rows_and_diagnostics_are_useful_but_bounded(self):
        rows = [
            {"id": f"failed-case-{index}-" + "i" * 10_000, "status": "FAIL",
             "reason": "cleanup-incomplete " + "\n\r\x1b" * 10_000,
             "oracle": "must-release-owner " + "o" * 10_000,
             "primary": {"category": "CleanupFault", "message": "owner-still-held " + "m" * 10_000,
                         "context": {"stage": "loader-preflight"}}}
            for index in range(100)
        ]
        payload = json.dumps({"version": 1, "check_passed": False, "rows": rows}).encode("utf-8")
        diagnostics = b"critical-start\n" + b"x" * 100_000 + b"\ncritical-end"
        with self.assertRaises(subprocess.CalledProcessError) as caught:
            self.invoke(payload, diagnostics, 7)
        self.assertEqual(caught.exception.returncode, 7)
        self.assertEqual(self.stdout_path.read_bytes(), payload)
        self.assertEqual(self.stderr_path.read_bytes(), diagnostics)
        self.assertLess(len(self.console.encode("utf-8")), 24_000)
        for useful in ("failed-case-0-", "cleanup-incomplete", "must-release-owner",
                       "CleanupFault", "owner-still-held", "loader-preflight", "critical-start", "critical-end"):
            self.assertIn(useful, self.console)
        self.assertNotIn("failed-case-99-", self.console)
        self.assertNotIn("\x1b", self.console)

    def test_no_failure_signal_can_be_overridden_by_a_success_signal(self):
        for scenario, passed, status, exit_code in [
            ("process exit", True, "PASS", 9),
            ("reported failure", False, "PASS", 0),
            ("failed row", True, "FAIL", 0),
            ("blocked row", True, "BLOCKED", 0),
            ("unexecuted row", True, "UNEXECUTED", 0),
            ("inapplicable row", True, "NOT_APPLICABLE", 0),
        ]:
            with self.subTest(scenario=scenario):
                payload = json.dumps({"version": 1, "check_passed": passed,
                                      "rows": [{"id": "controlled-outcome", "status": status}]}).encode("utf-8")
                error_type = subprocess.CalledProcessError if exit_code else ValueError
                with self.assertRaises(error_type) as caught:
                    self.invoke(payload, b"critical diagnostic", exit_code)
                if exit_code:
                    self.assertEqual(caught.exception.returncode, exit_code)
                self.assertEqual(self.stdout_path.read_bytes(), payload)
                self.assertEqual(self.stderr_path.read_bytes(), b"critical diagnostic")
                self.assertIn("critical diagnostic", self.console)

    def test_invalid_reports_cannot_pass_and_keep_raw_evidence(self):
        valid = {"version": 1, "check_passed": True, "rows": [{"id": "case", "status": "PASS"}]}
        malformed = [
            ("empty", b""),
            ("truncated", b'{"unfinished": "' + b"x" * 100_000),
            ("invalid encoding", b"\xff"),
            ("duplicate keys", b'{"version":1,"check_passed":false,"check_passed":true,"rows":[{"id":"case","status":"PASS"}]}'),
            ("nonfinite number", json.dumps({**valid, "extra": float("nan")}).encode("utf-8")),
            ("huge duplicate key", ('{"' + "k" * 100_000 + '":0,"' + "k" * 100_000 + '":1}').encode("utf-8")),
            ("deep nesting", b"[" * 2_000 + b"]" * 2_000),
        ]
        shapes = [
            ("array", []),
            ("missing version", {key: value for key, value in valid.items() if key != "version"}),
            ("wrong version", {**valid, "version": 2}),
            ("boolean version", {**valid, "version": True}),
            ("missing verdict", {key: value for key, value in valid.items() if key != "check_passed"}),
            ("truthy verdict", {**valid, "check_passed": "true"}),
            ("missing rows", {key: value for key, value in valid.items() if key != "rows"}),
            ("non-array rows", {**valid, "rows": {}}),
            ("empty rows", {**valid, "rows": []}),
            ("non-object row", {**valid, "rows": [None]}),
            ("missing id", {**valid, "rows": [{"status": "PASS"}]}),
            ("empty id", {**valid, "rows": [{"id": "", "status": "PASS"}]}),
            ("missing status", {**valid, "rows": [{"id": "case"}]}),
            ("unknown status", {**valid, "rows": [{"id": "case", "status": "success"}]}),
            ("non-string status", {**valid, "rows": [{"id": "case", "status": []}]}),
        ]
        malformed.extend((scenario, json.dumps(value).encode("utf-8")) for scenario, value in shapes)
        for scenario, payload in malformed:
            with self.subTest(scenario=scenario):
                with self.assertRaises(ValueError):
                    self.invoke(payload, b"parser diagnostic\r\n\xff")
                self.assertEqual(self.stdout_path.read_bytes(), payload)
                self.assertEqual(self.stderr_path.read_bytes(), b"parser diagnostic\r\n\xff")
                self.assertIn("parser diagnostic", self.console)
                self.assertLess(len(self.console.encode("utf-8")), 12_000)
        with self.assertRaises(subprocess.CalledProcessError) as caught:
            self.invoke(b"truncated-before-exit", b"fatal worker error", 23)
        self.assertEqual(caught.exception.returncode, 23)
        self.assertEqual(self.stdout_path.read_bytes(), b"truncated-before-exit")
        self.assertIn("truncated-before-exit", self.console)
        self.assertIn("fatal worker error", self.console)

    def test_empty_output_and_failed_launch_do_not_reuse_stale_evidence(self):
        payload = b'{"version":1,"check_passed":true,"rows":[{"id":"old","status":"PASS"}]}'
        self.invoke(payload, b"old diagnostics")
        with self.assertRaises(ValueError):
            self.invoke(b"")
        self.assertEqual(self.stdout_path.read_bytes(), b"")
        self.assertEqual(self.stderr_path.read_bytes(), b"")
        self.stdout_path.write_bytes(payload)
        self.stderr_path.write_bytes(b"old diagnostics")
        with redirect_stdout(io.StringIO()), self.assertRaises(OSError):
            check.run_runtime_check([str(self.root / "missing-command")], self.snapshot, self.results)
        self.assertEqual(self.stdout_path.read_bytes(), b"")
        self.assertEqual(self.stderr_path.read_bytes(), b"")

    def invoke_public_mode(self, arguments, payload):
        # Replace only unrelated tools; the runtime check still launches a real
        # child inside main's disposable tracked-only snapshot.
        files = {
            "emitter.py": (
                "from pathlib import Path\n"
                "import sys\n"
                "assert not Path('private.txt').exists()\n"
                "sys.stdout.buffer.write(Path('payload.json').read_bytes())\n"
                "sys.stderr.buffer.write(b'controlled stderr\\r\\n')\n"
            ).encode("utf-8"),
            "payload.json": payload,
        }
        for name, data in files.items():
            (self.root / name).write_bytes(data)
        (self.root / "private.txt").write_text("untracked private input", encoding="utf-8")
        real_run = subprocess.run
        snapshots = []

        def controlled_process(command, **kwargs):
            if command[-1] == "--version":
                return subprocess.CompletedProcess(command, 0, stdout=f"v{check.NODE_VERSION}\n")
            if command[-2:] != ["--", "check"]:
                raise AssertionError(f"unexpected external command: {command}")
            snapshots.append(kwargs["cwd"])
            return real_run([sys.executable, "-B", "emitter.py"], **kwargs)

        output, errors = io.StringIO(), io.StringIO()
        with (
            patch.object(check, "ROOT", self.root),
            patch.object(sys, "argv", ["check.py", *arguments]),
            patch.object(check, "load_manifest", return_value={"tools": {"actionlint": {}, "lychee": {}}}),
            patch.object(check, "host_platform", return_value="darwin-arm64"),
            patch.object(check, "installed_tool", return_value=sys.executable),
            patch("check.platform.platform", return_value="controlled-test-host"),
            patch("policy.tracked_files", return_value=list(files)),
            patch("policy.check_paths"),
            patch("policy.check_repository"),
            patch.object(check, "run"),
            patch("check.shutil.which", return_value=sys.executable),
            patch("check.subprocess.run", side_effect=controlled_process),
            redirect_stdout(output), redirect_stderr(errors),
        ):
            code = check.main()
        return code, output.getvalue() + errors.getvalue(), snapshots

    def test_public_runtime_modes_retain_evidence_after_snapshot_removal(self):
        for arguments in ([], ["--runtime-only"]):
            for status in ("PASS", "FAIL"):
                with self.subTest(arguments=arguments, status=status):
                    payload = json.dumps({"version": 1, "check_passed": True,
                                          "rows": [{"id": "controlled-case", "status": status}]}).encode("utf-8")
                    code, console, snapshots = self.invoke_public_mode(arguments, payload)
                    self.assertEqual(code, 0 if status == "PASS" else 1, console)
                    self.assertEqual(self.stdout_path.read_bytes(), payload)
                    self.assertEqual(self.stderr_path.read_bytes(), b"controlled stderr\r\n")
                    self.assertIn(str(self.stdout_path), console)
                    self.assertEqual(len(snapshots), 1)
                    self.assertFalse(snapshots[0].exists())

    def test_policy_only_does_not_create_runtime_evidence(self):
        code, console, snapshots = self.invoke_public_mode(["--policy-only"], b"must not execute")
        self.assertEqual(code, 0, console)
        self.assertEqual(snapshots, [])
        self.assertFalse(self.results.exists())

    def test_early_public_check_failure_discards_previous_runtime_evidence(self):
        self.results.mkdir(parents=True)
        for arguments in ([], ["--runtime-only"]):
            with self.subTest(arguments=arguments):
                self.stdout_path.write_bytes(b"old results")
                self.stderr_path.write_bytes(b"old diagnostics")
                output, errors = io.StringIO(), io.StringIO()
                with (
                    patch.object(check, "ROOT", self.root),
                    patch.object(sys, "argv", ["check.py", *arguments]),
                    patch.object(check, "load_manifest", side_effect=ValueError("missing prerequisite")),
                    redirect_stdout(output), redirect_stderr(errors),
                ):
                    self.assertEqual(check.main(), 1)
                self.assertFalse(self.stdout_path.exists())
                self.assertFalse(self.stderr_path.exists())


class RepositoryPolicyTests(unittest.TestCase):
    def setUp(self):
        self.manifest = load_manifest()
        self.workflow = read_workflow(ROOT / ".github/workflows/ci.yml")

    def test_ruleset_rejects_weakened_or_inoperable_protection(self):
        for topic in (False, True):
            name = "topic-development" if topic else "main"
            source = read_json(ROOT / f".github/rulesets/{name}.json")
            self.assertGreater(check_ruleset(source, topic), 0)
            cases = []
            for key, value in (("enforcement", "evaluate"), ("bypass_actors", [{"actor_id": 1, "actor_type": "Integration", "bypass_mode": "always"}])):
                data = deepcopy(source)
                data[key] = value
                cases.append(data)
            data = deepcopy(source)
            data["rules"] = [rule for rule in data["rules"] if rule["type"] != "non_fast_forward"]
            cases.append(data)
            data = deepcopy(source)
            data["conditions"]["ref_name"]["exclude"] = ["refs/heads/dev/unsafe"]
            cases.append(data)
            for key, value in (("strict_required_status_checks_policy", False), ("do_not_enforce_on_create", not topic),
                               ("required_status_checks", [{"context": "CI Gate (push)", "integration_id": 15368}]),
                               ("required_status_checks", [{"context": "CI Gate", "integration_id": True}]),
                               ("required_status_checks", [{"context": "CI Gate", "integration_id": 0}])):
                data = deepcopy(source)
                next(rule for rule in data["rules"] if rule["type"] == "required_status_checks")["parameters"][key] = value
                cases.append(data)
            data = deepcopy(source)
            next(rule for rule in data["rules"] if rule["type"] == "pull_request")["parameters"]["required_review_thread_resolution"] = False
            cases.append(data)
            data = deepcopy(source)
            if topic:
                data["rules"].append({"type": "deletion"})
            else:
                data["rules"] = [rule for rule in data["rules"] if rule["type"] != "deletion"]
            cases.append(data)
            for data in cases:
                with self.subTest(topic=topic, data=data), self.assertRaises(ValueError):
                    check_ruleset(data, topic)

    def test_workflow_refuses_gate_bypass_and_privileged_execution(self):
        check_ci_workflow(self.workflow, self.manifest["actions"])
        cases = []
        for field, value in (("name", "CI Gate"), ("if", "${{ success() }}"), ("needs", ["repository"])):
            workflow = deepcopy(self.workflow)
            workflow["jobs"]["gate"][field] = value
            cases.append(workflow)
        workflow = deepcopy(self.workflow)
        workflow["jobs"]["gate"]["steps"][-1]["env"]["NEEDS_JSON"] = '{"branch-flow":{"result":"success"},"repository":{"result":"success"}}'
        cases.append(workflow)
        for field, value in (("permissions", {"contents": "write"}), ("concurrency", {"group": "all", "cancel-in-progress": True})):
            workflow = deepcopy(self.workflow)
            workflow[field] = value
            cases.append(workflow)
        workflow = deepcopy(self.workflow)
        workflow["on"]["pull_request"]["paths"] = ["src/**"]
        cases.append(workflow)
        workflow = deepcopy(self.workflow)
        workflow["on"]["pull_request"]["types"].remove("edited")
        cases.append(workflow)
        workflow = deepcopy(self.workflow)
        workflow["on"]["pull_request_target"] = workflow["on"].pop("pull_request")
        cases.append(workflow)
        for key, value in (("continue-on-error", True), ("if", "false"), ("runs-on", "self-hosted"), ("timeout-minutes", 0)):
            workflow = deepcopy(self.workflow)
            workflow["jobs"]["repository"][key] = value
            cases.append(workflow)
        for mutate in ("unpinned", "credentials", "masked failure", "shell interpolation"):
            workflow = deepcopy(self.workflow)
            steps = workflow["jobs"]["repository"]["steps"]
            check_step = next(step for step in steps if "tools/ci/check.py" in step.get("run", ""))
            if mutate == "unpinned":
                steps[0]["uses"] = "actions/checkout@v7"
            elif mutate == "credentials":
                steps[0]["with"]["persist-credentials"] = True
            elif mutate == "masked failure":
                check_step["run"] += " || true"
            else:
                check_step["run"] = "echo '${{ github.event.pull_request.title }}'"
            cases.append(workflow)
        workflow = deepcopy(self.workflow)
        workflow["jobs"]["repository"]["steps"][0]["run"] = 7
        cases.append(workflow)
        for workflow in cases:
            with self.subTest(workflow=workflow), self.assertRaises(ValueError):
                check_ci_workflow(workflow, self.manifest["actions"])

    def test_controlled_lanes_cannot_skip_execution_or_change_toolchains(self):
        for name in ("repository", "runtime-macos", "runtime-windows"):
            for mutation in ("skip job", "skip step", "mask failure", "policy only", "wrong runner",
                             "floating rust", "floating node", "missing node", "implicit cache", "redirect command"):
                with self.subTest(job=name, mutation=mutation):
                    workflow = deepcopy(self.workflow)
                    job = workflow["jobs"][name]
                    steps = job["steps"]
                    check = next(step for step in steps if "tools/ci/check.py" in step.get("run", ""))
                    node = next(step for step in steps if step.get("uses", "").startswith("actions/setup-node@"))
                    if mutation == "skip job":
                        job["if"] = "false"
                    elif mutation == "skip step":
                        check["if"] = "false"
                    elif mutation == "mask failure":
                        check["continue-on-error"] = True
                    elif mutation == "policy only":
                        check["run"] = "python tools/ci/check.py --policy-only"
                    elif mutation == "wrong runner":
                        job["runs-on"] = "macos-15-intel"
                    elif mutation == "floating rust":
                        rust = next(step for step in steps if "rustup toolchain install" in step.get("run", ""))
                        rust["run"] = "rustup toolchain install stable --profile minimal"
                    elif mutation == "floating node":
                        node["with"]["node-version"] = "24"
                    elif mutation == "missing node":
                        steps.remove(node)
                    elif mutation == "implicit cache":
                        node["with"]["package-manager-cache"] = True
                    else:
                        check["working-directory"] = "other-checkout"
                    with self.assertRaises(ValueError):
                        check_ci_workflow(workflow, self.manifest["actions"])

    def test_runtime_artifacts_cannot_widen_or_weaken_checks(self):
        mutations = (
            "wrong pin", "broad path", "extra private path", "missing stderr", "hidden excluded",
            "retention changed", "shared artifact name", "conditional upload", "upload ignores failure",
            "conditional check", "check ignores failure", "missing upload", "duplicate upload",
            "upload before check", "step after upload",
        )
        for name in ("repository", "runtime-macos", "runtime-windows"):
            for mutation in mutations:
                with self.subTest(job=name, mutation=mutation):
                    workflow = deepcopy(self.workflow)
                    steps = workflow["jobs"][name]["steps"]
                    check_step = next(step for step in steps if "tools/ci/check.py" in step.get("run", ""))
                    upload = next(step for step in steps if step.get("uses", "").startswith("actions/upload-artifact@"))
                    if mutation == "wrong pin":
                        upload["uses"] = "actions/upload-artifact@" + "0" * 40
                    elif mutation == "broad path":
                        upload["with"]["path"] = ".cache/**"
                    elif mutation == "extra private path":
                        upload["with"]["path"] += "rasen/**\n"
                    elif mutation == "missing stderr":
                        upload["with"]["path"] = ".cache/repository-ci/runtime-results/runtime-results.json"
                    elif mutation == "hidden excluded":
                        upload["with"]["include-hidden-files"] = False
                    elif mutation == "retention changed":
                        upload["with"]["retention-days"] = 90
                    elif mutation == "shared artifact name":
                        upload["with"]["name"] = "controlled-runtime"
                    elif mutation == "conditional upload":
                        upload["if"] = "${{ success() }}"
                    elif mutation == "upload ignores failure":
                        upload["continue-on-error"] = True
                    elif mutation == "conditional check":
                        check_step["if"] = "${{ always() }}"
                    elif mutation == "check ignores failure":
                        check_step["continue-on-error"] = True
                    elif mutation == "missing upload":
                        steps.remove(upload)
                    elif mutation == "duplicate upload":
                        steps.append(deepcopy(upload))
                    elif mutation == "upload before check":
                        steps.remove(upload)
                        steps.insert(steps.index(check_step), upload)
                    else:
                        steps.append({"name": "Extra step", "run": "echo unexpected"})
                    with self.assertRaises(ValueError):
                        check_ci_workflow(workflow, self.manifest["actions"])
        for name in ("branch-flow", "gate"):
            with self.subTest(non_runtime_job=name):
                workflow = deepcopy(self.workflow)
                upload = next(step for step in workflow["jobs"]["repository"]["steps"]
                              if step.get("uses", "").startswith("actions/upload-artifact@"))
                workflow["jobs"][name]["steps"].append(deepcopy(upload))
                with self.assertRaises(ValueError):
                    check_ci_workflow(workflow, self.manifest["actions"])

    def test_windows_symlink_setup_must_precede_checkout(self):
        workflow = deepcopy(self.workflow)
        steps = workflow["jobs"]["runtime-windows"]["steps"]
        steps[0], steps[1] = steps[1], steps[0]
        with self.assertRaisesRegex(ValueError, "before checkout"):
            check_ci_workflow(workflow, self.manifest["actions"])

    def test_tracked_private_paths_and_missing_public_guides_are_refused(self):
        check_paths(sorted(REQUIRED_FILES))
        for forbidden in ("rasen/config.yaml", ".rasen/evidence/run.json", "examples/app.py", "local_docs/design.md", ".omp/config.yml", ".omp/sessions/log", ".omp/rules/unapproved.md", ".cache/tool", "tools/ci/__pycache__/policy.pyc"):
            with self.subTest(path=forbidden), self.assertRaisesRegex(ValueError, "path|state"):
                check_paths([*REQUIRED_FILES, forbidden])
        with self.assertRaisesRegex(ValueError, "CLAUDE.md"):
            check_paths(sorted(REQUIRED_FILES - {"CLAUDE.md"}))

    def test_canonical_symlink_refuses_copies_wrong_targets_and_broken_links(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            canonical = root / "AGENTS.md"
            canonical.write_text("# Agent guide\n", encoding="utf-8")
            entry = root / "CLAUDE.md"
            entry.write_text("AGENTS.md", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "relative symlink"):
                check_claude(root)
            entry.unlink()
            for target in ("missing.md", str(canonical)):
                with self.subTest(target=target):
                    entry.symlink_to(target)
                    with self.assertRaisesRegex(ValueError, "relative symlink"):
                        check_claude(root)
                    entry.unlink()
            entry.symlink_to("AGENTS.md")
            check_claude(root)
            snapshot = root / "snapshot"
            snapshot.mkdir()
            copy_tracked(root, snapshot, ["AGENTS.md", "CLAUDE.md"])
            check_claude(snapshot)
            canonical.unlink()
            with self.assertRaisesRegex(ValueError, "regular canonical guide"):
                check_claude(root)

    def test_ambiguous_and_unsafe_metadata_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "input"
            for text in ('{"name":"main","name":"other"}', '{broken'):
                path.write_text(text, encoding="utf-8")
                with self.assertRaises(ValueError):
                    read_json(path)
            for text in ("jobs: {}\njobs: {}\n", "!!python/object/apply:builtins.len ['unsafe']", "jobs: [unterminated"):
                path.write_text(text, encoding="utf-8")
                with self.assertRaises(ValueError):
                    read_workflow(path)

    def test_untracked_targets_cannot_satisfy_public_links(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "working"
            snapshot = Path(directory) / "snapshot"
            root.mkdir()
            snapshot.mkdir()
            (root / "guide.md").write_text("[private](private.md)\n", encoding="utf-8")
            (root / "private.md").write_text("Private file\n", encoding="utf-8")
            copy_tracked(root, snapshot, ["guide.md"])
            self.assertEqual((snapshot / "guide.md").read_text(encoding="utf-8"), "[private](private.md)\n")
            self.assertFalse((snapshot / "private.md").exists())
            (root / "shortcut.md").symlink_to("private.md")
            with self.assertRaisesRegex(ValueError, "tracked repository file"):
                copy_tracked(root, snapshot, ["shortcut.md"])

    def test_archive_links_cannot_replace_installed_executable(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive_path = root / "tool.tar.gz"
            with tarfile.open(archive_path, "w:gz") as archive:
                member = tarfile.TarInfo("actionlint")
                member.type = tarfile.SYMTYPE
                member.linkname = "../../outside"
                archive.addfile(member)
            with self.assertRaisesRegex(ValueError, "regular"):
                extract_binary(archive_path, root / "installed", "actionlint")
            self.assertFalse((root / "installed").exists())
            with tarfile.open(archive_path, "w:gz") as archive:
                member = tarfile.TarInfo("release/actionlint")
                member.size = 6
                archive.addfile(member, io.BytesIO(b"binary"))
            extract_binary(archive_path, root / "installed", "actionlint")
            self.assertEqual((root / "installed").read_bytes(), b"binary")


if __name__ == "__main__":
    unittest.main()
