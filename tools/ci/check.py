"""Run tracked repository governance and the controlled runtime comparison."""

import argparse
import os
import platform
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from tooling import ROOT, host_platform, installed_tool, load_manifest

RUST_VERSION = "1.97.1"
NODE_VERSION = "24.18.0"
RUNTIME_ROOT = Path("tools/runtime-comparison")


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


def check_runtime(root):
    """Build and exercise the public, non-native executable and trusted compiler."""
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
    run([*cargo, "run", *manifest, "--", "check"], root)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--policy-only", action="store_true", help="governance policy and its tests only; no runtime or external tools")
    modes.add_argument("--runtime-only", action="store_true", help="policy, its tests, and controlled runtime; omit actionlint and lychee")
    args = parser.parse_args()
    if sys.version_info < (3, 11):
        print("Repository checks require Python 3.11 or newer.", file=sys.stderr)
        return 1
    try:
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
                check_runtime(snapshot)
        if args.policy_only:
            print("Repository checks passed (policy and governance tests only; runtime and external tools unexecuted).")
        elif args.runtime_only:
            print("Repository checks passed (policy, governance tests, controlled runtime; actionlint and lychee unexecuted).")
        else:
            print("Repository checks passed (policy, governance tests, actionlint, offline local links, controlled runtime).")
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
