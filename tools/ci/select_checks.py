"""Skip duplicate dev push checks only for an exact-head promotion PR."""

import json
import os
from pathlib import Path
import re
import subprocess
import sys

from branch_flow import REPOSITORY, SLUG, object_at, repository_name

SHA = re.compile(r"[0-9a-fA-F]{40}")


def valid_sha(value):
    return isinstance(value, str) and SHA.fullmatch(value) is not None and value != "0" * 40


def covering_pr(pages, repository, branch, sha) -> bool:
    """Require valid slurped pages and a same-repository, exact-head open PR."""
    if (
        not isinstance(repository, str) or not REPOSITORY.fullmatch(repository)
        or not isinstance(branch, str) or not re.fullmatch(rf"dev/{SLUG}", branch)
        or not valid_sha(sha) or not isinstance(pages, list)
    ):
        return False
    expected = repository.casefold()
    covered = False
    try:
        # Inspect every page before trusting a match: a partially malformed
        # response is not confirmation that push checks can be suppressed.
        for page in pages:
            if not isinstance(page, list):
                return False
            for pr in page:
                base = object_at(pr, "base")
                head = object_at(pr, "head")
                base_repository = repository_name(object_at(base, "repo"))
                head_repository = repository_name(object_at(head, "repo"))
                if (
                    pr.get("state") not in ("open", "closed")
                    or not isinstance(base.get("ref"), str) or not base["ref"]
                    or not isinstance(head.get("ref"), str) or not head["ref"]
                    or not valid_sha(head.get("sha"))
                ):
                    return False
                if (
                    pr["state"] == "open" and base["ref"] == "main"
                    and base_repository == expected and head_repository == expected
                    and head["ref"] == branch and head["sha"].lower() == sha.lower()
                ):
                    covered = True
    except ValueError:
        return False
    return covered


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


def reject_constant(value):
    raise ValueError("non-finite JSON number")


def main() -> int:
    output = os.environ.get("GITHUB_OUTPUT")
    if not output:
        print("Check selection failed: GITHUB_OUTPUT is required.", file=sys.stderr)
        return 1

    skip_checks = False
    ref = os.environ.get("GITHUB_REF", "")
    repository = os.environ.get("GITHUB_REPOSITORY", "")
    sha = os.environ.get("GITHUB_SHA", "")
    if (
        os.environ.get("GITHUB_EVENT_NAME") == "push"
        and re.fullmatch(rf"refs/heads/dev/{SLUG}", ref)
        and REPOSITORY.fullmatch(repository) and valid_sha(sha)
    ):
        branch = ref.removeprefix("refs/heads/")
        owner = repository.split("/", 1)[0]
        try:
            response = subprocess.run(
                [
                    "gh", "api", "--method", "GET", f"repos/{repository}/pulls",
                    "--paginate", "--slurp", "-f", "state=open", "-f", "base=main",
                    "-f", f"head={owner}:{branch}", "-f", "per_page=100",
                ],
                check=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                text=True, encoding="utf-8", timeout=30,
            )
            pages = json.loads(
                response.stdout, object_pairs_hook=unique_object,
                parse_constant=reject_constant,
            )
            skip_checks = covering_pr(pages, repository, branch, sha)
        except (OSError, subprocess.SubprocessError, ValueError, RecursionError):
            # Error details can contain API responses or credentials. Neither
            # lookup nor parse failure is permission to omit required work.
            print("::warning::Promotion PR lookup could not be confirmed; running push checks.")

    try:
        with Path(output).open("a", encoding="utf-8") as destination:
            destination.write(f"skip-checks={str(skip_checks).lower()}\n")
    except (OSError, ValueError):
        print("Check selection failed: cannot write GITHUB_OUTPUT.", file=sys.stderr)
        return 1
    if skip_checks:
        print("An open promotion PR covers this exact dev head; skipping duplicate push checks.")
    else:
        print("No covering promotion PR confirmed; running checks.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
