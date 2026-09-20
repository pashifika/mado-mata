"""The required gate accepts only the complete, successful mandatory job set."""

import json
import os
import sys

EXPECTED_JOBS = frozenset({"branch-flow", "repository"})
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
    if not isinstance(needs, dict) or set(needs) != EXPECTED_JOBS:
        raise ValueError("needs must contain exactly branch-flow and repository")
    failures = []
    for name in sorted(EXPECTED_JOBS):
        job = needs[name]
        result = job.get("result") if isinstance(job, dict) else None
        if result != "success":
            failures.append(f"{name}={result!r}")
    if failures:
        raise ValueError("mandatory jobs did not succeed: " + ", ".join(failures))


def main():
    try:
        raw = os.environ.get("NEEDS_JSON")
        if raw is None:
            raise ValueError("NEEDS_JSON is required")
        evaluate(json.loads(raw))
    except (ValueError, RecursionError) as error:
        print(f"CI gate failed: {error}", file=sys.stderr)
        return 1
    print("CI gate passed: branch-flow and repository both succeeded.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
