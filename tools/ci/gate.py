"""The required gate accepts only the complete, successful mandatory job set."""

import json
import os
import sys

EXPECTED_JOBS = frozenset({"branch-flow", "repository", "runtime-macos", "runtime-windows"})
SELECTOR_JOB = "dev-push-policy"
CHECKS_IF = (
    "${{ !cancelled() && (github.event_name != 'push' || "
    "needs.dev-push-policy.outputs.skip-checks != 'true') }}"
)
GATE_IF = (
    "${{ always() && (github.event_name != 'push' || "
    "needs.dev-push-policy.outputs.skip-checks != 'true') }}"
)
GATE_NAMES = {
    "pull_request": "CI Gate",
    "push": "CI Gate (push)",
    "workflow_dispatch": "CI Gate (manual)",
}
GATE_NAME_EXPRESSION = (
    "${{ github.event_name == 'pull_request' && 'CI Gate' || "
    "github.event_name == 'push' && 'CI Gate (push)' || 'CI Gate (manual)' }}"
)


def evaluate(needs):
    expected = EXPECTED_JOBS | {SELECTOR_JOB}
    if not isinstance(needs, dict) or set(needs) != expected:
        raise ValueError("needs must contain exactly: " + ", ".join(sorted(expected)))
    selector = needs[SELECTOR_JOB]
    result = selector.get("result") if isinstance(selector, dict) else None
    failures = [] if result in ("success", "skipped") else [f"{SELECTOR_JOB}={result!r}"]
    for name in sorted(EXPECTED_JOBS):
        job = needs[name]
        result = job.get("result") if isinstance(job, dict) else None
        if result != "success":
            failures.append(f"{name}={result!r}")
    if failures:
        raise ValueError("CI dependencies did not succeed: " + ", ".join(failures))


def main():
    try:
        raw = os.environ.get("NEEDS_JSON")
        if raw is None:
            raise ValueError("NEEDS_JSON is required")
        evaluate(json.loads(raw))
    except (ValueError, RecursionError) as error:
        print(f"CI gate failed: {error}", file=sys.stderr)
        return 1
    print("CI gate passed: all mandatory jobs succeeded.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
