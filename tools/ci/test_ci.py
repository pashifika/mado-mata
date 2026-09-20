"""Deterministic route, gate, policy, and tracked-input regression cases."""

from copy import deepcopy
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest

from branch_flow import validate_event
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
        for job in ("branch-flow", "repository"):
            for status in ("success", "failure", "cancelled", "skipped", "", None, "neutral"):
                with self.subTest(job=job, status=status):
                    needs = {"branch-flow": {"result": "success"}, "repository": {"result": "success"}}
                    needs[job]["result"] = status
                    if status == "success":
                        evaluate(needs)
                    else:
                        with self.assertRaisesRegex(ValueError, job):
                            evaluate(needs)

    def test_missing_extra_and_malformed_dependencies_fail(self):
        success = {"branch-flow": {"result": "success"}, "repository": {"result": "success"}}
        for needs in (None, [], {}, {"branch-flow": {"result": "success"}},
                      {**success, "unexpected": {"result": "success"}},
                      {**success, "repository": {}}, {**success, "repository": "success"}):
            with self.subTest(needs=needs), self.assertRaises(ValueError):
                evaluate(needs)

    def test_gate_cli_reads_json_and_fails_cleanly(self):
        with tempfile.TemporaryDirectory() as directory:
            for env in ({}, {"NEEDS_JSON": "{bad"}, {"NEEDS_JSON": "[]"}, {"NEEDS_JSON": '{"branch-flow": {"result": "success"}}'}):
                result = cli("gate.py", env, directory)
                self.assertEqual(result.returncode, 1)
                self.assertIn("CI gate failed:", result.stderr)
                self.assertNotIn("Traceback", result.stderr)
            result = cli("gate.py", {"NEEDS_JSON": json.dumps({name: {"result": "success"} for name in ("branch-flow", "repository")})}, directory)
            self.assertEqual(result.returncode, 0)


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
            if mutate == "unpinned":
                steps[0]["uses"] = "actions/checkout@v7"
            elif mutate == "credentials":
                steps[0]["with"]["persist-credentials"] = True
            elif mutate == "masked failure":
                steps[-1]["run"] += " || true"
            else:
                steps[-1]["run"] = "echo '${{ github.event.pull_request.title }}'"
            cases.append(workflow)
        workflow = deepcopy(self.workflow)
        workflow["jobs"]["repository"]["steps"][0]["run"] = 7
        cases.append(workflow)
        for workflow in cases:
            with self.subTest(workflow=workflow), self.assertRaises(ValueError):
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
