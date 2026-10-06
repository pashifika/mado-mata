"""Validate GitHub event metadata without executing PR-controlled strings."""

import json
import os
from pathlib import Path
import re
import sys

SLUG = r"[a-z0-9]+(?:-[a-z0-9]+)*"
REPOSITORY = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+")
PR_ACTIONS = {"opened", "synchronize", "reopened", "ready_for_review"}


def object_at(value, key):
    child = value.get(key) if isinstance(value, dict) else None
    if not isinstance(child, dict):
        raise ValueError(f"missing or invalid object: {key}")
    return child


def repository_name(value):
    name = value.get("full_name") if isinstance(value, dict) else None
    if not isinstance(name, str) or not REPOSITORY.fullmatch(name):
        raise ValueError("missing or invalid repository.full_name")
    return name.casefold()


def branch(value):
    if not isinstance(value, str):
        raise ValueError("missing or invalid branch ref")
    if value == "main":
        return "main", ""
    match = re.fullmatch(rf"(dev|change|fix|sync)/({SLUG})", value)
    if not match:
        raise ValueError(f"unsupported branch ref: {value!r}")
    return match.groups()


def validate_event(event_name, event, repository, ref=None):
    if not isinstance(repository, str) or not REPOSITORY.fullmatch(repository):
        raise ValueError("GITHUB_REPOSITORY must be owner/repository")
    if not isinstance(event, dict):
        raise ValueError("event must be a JSON object")
    expected = repository.casefold()
    if repository_name(object_at(event, "repository")) != expected:
        raise ValueError("event repository does not match GITHUB_REPOSITORY")

    if event_name == "pull_request":
        if not isinstance(event.get("action"), str) or event["action"] not in PR_ACTIONS:
            raise ValueError("missing or unsupported pull_request action")
        pr = object_at(event, "pull_request")
        base = object_at(pr, "base")
        head = object_at(pr, "head")
        if repository_name(object_at(base, "repo")) != expected:
            raise ValueError("PR base repository does not match GITHUB_REPOSITORY")
        same_repository = repository_name(object_at(head, "repo")) == expected
        base_kind, base_topic = branch(base.get("ref"))
        head_kind, head_topic = branch(head.get("ref"))
        allowed = (
            base_kind == "main"
            and (head_kind == "fix" or (head_kind == "dev" and same_repository))
        ) or (
            base_kind == "dev"
            and (
                head_kind in {"change", "fix"}
                or (head_kind == "sync" and head_topic == base_topic and same_repository)
            )
        )
        if not allowed:
            raise ValueError(
                f"unsupported PR route: {head['ref']!r} -> {base['ref']!r} "
                f"(same repository: {same_repository})"
            )
        return f"Allowed PR route: {head['ref']} -> {base['ref']}"

    if not isinstance(event_name, str) or event_name not in {"push", "workflow_dispatch"}:
        raise ValueError(f"unsupported event: {event_name!r}")
    if not isinstance(ref, str) or not ref.startswith("refs/heads/"):
        raise ValueError("GITHUB_REF must identify a branch under refs/heads/")
    short_ref = ref.removeprefix("refs/heads/")
    kind, _ = branch(short_ref)
    if event_name == "push":
        if kind not in {"main", "dev"}:
            raise ValueError("push checks require main or a dev/<topic> branch")
        if event.get("ref") != ref:
            raise ValueError("push event ref does not match GITHUB_REF")
    elif event.get("ref") != ref:
        raise ValueError(
            f"workflow_dispatch event ref {event.get('ref')!r} does not match GITHUB_REF {ref!r}"
        )
    return f"Valid {event_name} context: {ref}; PR routing is not applicable."


def main():
    try:
        event_path = os.environ.get("GITHUB_EVENT_PATH")
        if not event_path:
            raise ValueError("GITHUB_EVENT_PATH is required")
        event = json.loads(Path(event_path).read_text(encoding="utf-8"))
        print(validate_event(
            os.environ.get("GITHUB_EVENT_NAME"), event,
            os.environ.get("GITHUB_REPOSITORY"), os.environ.get("GITHUB_REF"),
        ))
    except (OSError, ValueError, RecursionError) as error:
        print(f"Branch flow failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
