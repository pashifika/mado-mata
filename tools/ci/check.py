"""Run tracked governance, controlled runtime, and desktop application checks."""

import argparse
import json
import os
import platform
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from tooling import ROOT, host_platform, installed_tool, load_manifest

RUST_VERSION = "1.98.1"
NODE_VERSION = "24.18.0"
RUNTIME_ROOT = Path("tools/runtime-comparison")
DESKTOP_ROOT = Path("apps/desktop")
RUNTIME_RESULTS = Path(".cache/repository-ci/runtime-results")
FAILURE_ROW_LIMIT = 10
FIELD_TEXT_LIMIT = 240
DIAGNOSTIC_BYTES = 4096


def copy_tracked(root, destination, paths):
    """Use working-tree contents, but never let untracked files satisfy a link."""
    root = root.resolve()
    tracked = set(paths)
    for name in paths:
        source = root / name
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True)
        if source.is_symlink():
            link = os.readlink(source)
            try:
                resolved = source.resolve(strict=True).relative_to(root).as_posix()
            except (OSError, ValueError, RuntimeError) as error:
                raise ValueError(f"tracked symlink escapes the public checkout: {name!r}") from error
            if Path(link).is_absolute() or resolved not in tracked:
                raise ValueError(f"tracked symlink must target a tracked repository file: {name!r}")
            target.symlink_to(link)
        elif source.is_file():
            shutil.copyfile(source, target)
        else:
            raise ValueError(f"tracked file is missing or is an unsupported gitlink: {name!r}")


def run(command, root):
    print("Running: " + " ".join(str(part) for part in command), flush=True)
    subprocess.run(command, cwd=root, check=True, env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"})


def bounded_text(value, limit=FIELD_TEXT_LIMIT, *, tail=False):
    """Keep report text on one bounded console line, including control characters."""
    if not isinstance(value, str):
        return f"<{type(value).__name__}>"
    fragment = value[-limit:] if tail else value[:limit]
    escaped = json.dumps(fragment, ensure_ascii=True)[1:-1]
    omitted = "..." if len(value) > limit or len(escaped) > limit else ""
    return omitted + escaped[-limit:] if tail else escaped[:limit] + omitted


def print_diagnostic_excerpt(path, label):
    """Show both ends of a diagnostic stream without reading it all into memory."""
    size = path.stat().st_size
    if not size:
        return
    half = DIAGNOSTIC_BYTES // 2
    with path.open("rb") as source:
        print(f"{label}: {bounded_text(source.read(half).decode('utf-8', errors='replace'), half)}", file=sys.stderr)
        if size > half:
            if size > DIAGNOSTIC_BYTES:
                print(f"{label}: ... {size - DIAGNOSTIC_BYTES} bytes omitted ...", file=sys.stderr)
                source.seek(-half, os.SEEK_END)
            print(f"{label}: {bounded_text(source.read(half).decode('utf-8', errors='replace'), half, tail=True)}", file=sys.stderr)


def read_runtime_report(path):
    from policy import unique_mapping

    def reject_constant(value):
        raise ValueError(f"invalid JSON number: {value}")

    report = json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=unique_mapping, parse_constant=reject_constant)
    if not isinstance(report, dict) or type(report.get("version")) is not int or report["version"] != 1:
        raise ValueError("runtime report must be a version-1 object")
    if type(report.get("check_passed")) is not bool:
        raise ValueError("runtime report must contain boolean check_passed")
    rows = report.get("rows")
    if not isinstance(rows, list) or not rows:
        raise ValueError("runtime report must contain a nonempty rows array")
    for index, row in enumerate(rows):
        if not isinstance(row, dict) or not isinstance(row.get("id"), str) or not row["id"]:
            raise ValueError(f"runtime report row {index} must contain a nonempty string id")
        if row.get("status") not in ("PASS", "FAIL", "BLOCKED", "UNEXECUTED", "NOT_APPLICABLE"):
            raise ValueError(f"runtime report row {index} has an invalid status")
    return report


def run_runtime_check(command, root, results_directory):
    """Retain byte-exact controlled evidence outside the disposable tracked tree."""
    results_directory.mkdir(parents=True, exist_ok=True)
    results_path = results_directory / "runtime-results.json"
    stderr_path = results_directory / "runtime-stderr.log"
    print("Running: " + " ".join(str(part) for part in command), flush=True)
    print(f"Controlled runtime evidence: {results_path} ; {stderr_path}", flush=True)
    # Open both streams before starting the command so a failed launch cannot
    # reuse a previous invocation's report or diagnostics.
    with results_path.open("wb") as stdout, stderr_path.open("wb") as stderr:
        result = subprocess.run(command, cwd=root, stdout=stdout, stderr=stderr, check=False,
                                env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"})
    report = None
    try:
        report = read_runtime_report(results_path)
    except (OSError, ValueError, RecursionError) as error:
        print(f"Invalid controlled runtime report: {bounded_text(str(error))}", file=sys.stderr)
        print_diagnostic_excerpt(results_path, "Runtime stdout")
    failed = 0
    if report is not None:
        rows = report["rows"]
        failed = sum(row["status"] != "PASS" for row in rows)
        print(f"Controlled runtime: total={len(rows)} pass={len(rows) - failed} fail={failed} "
              f"check_passed={str(report['check_passed']).lower()}", flush=True)
        shown = 0
        for row in rows:
            if row["status"] == "PASS":
                continue
            fields = [f"id={bounded_text(row['id'])}", f"status={row['status']}"]
            for name in ("reason", "oracle", "entry_outcome"):
                if row.get(name) is not None:
                    fields.append(f"{name}={bounded_text(row[name])}")
            primary = row.get("primary")
            if isinstance(primary, dict):
                for name in ("category", "message"):
                    if primary.get(name) is not None:
                        fields.append(f"primary.{name}={bounded_text(primary[name])}")
                context = primary.get("context")
                if isinstance(context, dict) and context.get("stage") is not None:
                    fields.append(f"stage={bounded_text(context['stage'])}")
            print("Runtime failure: " + " ; ".join(fields), file=sys.stderr)
            shown += 1
            if shown == FAILURE_ROW_LIMIT:
                break
        if failed > shown:
            print(f"Runtime failures: {failed - shown} additional rows retained in {results_path}", file=sys.stderr)
    if result.returncode or report is None or not report["check_passed"] or failed:
        print_diagnostic_excerpt(stderr_path, "Runtime stderr")
        if result.returncode:
            # Do not attach raw stderr: main's generic command handler prints it.
            raise subprocess.CalledProcessError(result.returncode, command)
        raise ValueError("controlled runtime check failed; see retained evidence")


def check_runtime(root, results_directory):
    """Exercise the controlled runtime and desktop without native authority."""
    print(f"Controlled runtime host: {platform.platform()} ({platform.machine()})", flush=True)
    node = shutil.which("node")
    npm = shutil.which("npm")
    if node is None or npm is None:
        raise ValueError(f"install Node.js {NODE_VERSION} with its bundled npm")
    version = subprocess.run([node, "--version"], cwd=root, check=True, capture_output=True, text=True).stdout.strip()
    if version != f"v{NODE_VERSION}":
        raise ValueError(f"Node.js {NODE_VERSION} is required; found {version!r}")
    compiler = root / RUNTIME_ROOT / "compiler"
    run([npm, "ci", "--ignore-scripts", "--no-audit", "--no-fund"], compiler)
    run([node, "compile.mjs", "--self-check"], compiler)
    cargo = ["cargo", f"+{RUST_VERSION}"]
    manifest = ["--locked", "--manifest-path", RUNTIME_ROOT / "Cargo.toml"]
    run([*cargo, "build", *manifest], root)
    run([*cargo, "test", *manifest], root)
    run_runtime_check([*cargo, "run", *manifest, "--", "check"], root, results_directory)
    run([npm, "ci", "--ignore-scripts", "--no-audit", "--no-fund", "--prefix", DESKTOP_ROOT], root)
    run([npm, "test", "--prefix", DESKTOP_ROOT], root)
    run([npm, "run", "build", "--prefix", DESKTOP_ROOT], root)
    desktop_manifest = ["--locked", "--manifest-path", DESKTOP_ROOT / "src-tauri/Cargo.toml"]
    run([*cargo, "test", *desktop_manifest, "--no-default-features", "--lib"], root)
    if platform.system() == "Darwin":
        run([*cargo, "build", *desktop_manifest, "--features", "custom-protocol"], root)
    else:
        print("Desktop shell build unexecuted: supported only on macOS; frontend and core checked.", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--policy-only", action="store_true", help="governance policy and its tests only; no runtime, desktop, or external tools")
    modes.add_argument("--runtime-only", action="store_true", help="policy, its tests, controlled runtime, and desktop; omit actionlint and lychee")
    args = parser.parse_args()
    if sys.version_info < (3, 11):
        print("Repository checks require Python 3.11 or newer.", file=sys.stderr)
        return 1
    try:
        results_directory = ROOT / RUNTIME_RESULTS
        if not args.policy_only:
            # Clear old evidence even if policy, dependencies, or the build fail
            # before the runtime command starts.
            for name in ("runtime-results.json", "runtime-stderr.log"):
                (results_directory / name).unlink(missing_ok=True)
        try:
            from policy import check_paths, check_repository, tracked_files
        except ModuleNotFoundError as error:
            if error.name != "yaml":
                raise
            raise ValueError("install Python dependencies: python3 -m pip install --require-hashes -r tools/ci/requirements.txt") from error
        manifest = load_manifest()
        paths = tracked_files(ROOT)
        check_paths(paths)
        with tempfile.TemporaryDirectory(prefix="mado-mata-ci-") as directory:
            temporary = Path(directory)
            snapshot = temporary / "repository"
            snapshot.mkdir()
            copy_tracked(ROOT, snapshot, paths)
            check_repository(snapshot, paths, manifest)
            print("Tracked repository policy passed.", flush=True)
            run([sys.executable, "-B", "-m", "unittest", "discover", "-s", "tools/ci", "-p", "test_*.py"], snapshot)
            if not args.policy_only and not args.runtime_only:
                host = host_platform()
                tools = {name: installed_tool(name, pin, host) for name, pin in manifest["tools"].items()}
                # Explicit empty configs and an isolated tracked-only tree prevent
                # machine-local config/ignore files from changing the check scope.
                actionlint_config = temporary / "actionlint.yaml"
                actionlint_config.write_text("{}\n", encoding="utf-8")
                lychee_config = temporary / "lychee.toml"
                lychee_config.write_text("", encoding="utf-8")
                workflows = ["./" + path for path in paths if path.startswith(".github/workflows/") and Path(path).suffix in {".yaml", ".yml"}]
                markdown = ["./" + path for path in paths if Path(path).suffix.lower() == ".md"]
                run([tools["actionlint"], "-shellcheck=", "-pyflakes=", "-config-file", actionlint_config, *workflows], snapshot)
                run([tools["lychee"], "--offline", "--include-fragments", "--no-progress", "--no-ignore", "--hidden",
                     "--config", lychee_config, "--root-dir", snapshot, "--", *markdown], snapshot)
            if not args.policy_only:
                check_runtime(snapshot, results_directory)
        if args.policy_only:
            print("Repository checks passed (policy and governance tests only; runtime, desktop, and external tools unexecuted).")
        elif args.runtime_only:
            print("Repository checks passed (policy, governance tests, controlled runtime, desktop checks; actionlint and lychee unexecuted).")
        else:
            print("Repository checks passed (policy, governance tests, actionlint, offline local links, controlled runtime, desktop checks).")
    except subprocess.CalledProcessError as error:
        print(f"Repository checks failed: command exited {error.returncode}", file=sys.stderr)
        if error.stderr:
            print(error.stderr if isinstance(error.stderr, str) else error.stderr.decode("utf-8", errors="replace"), file=sys.stderr)
        return 1
    except (OSError, ValueError, RecursionError) as error:
        print(f"Repository checks failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
