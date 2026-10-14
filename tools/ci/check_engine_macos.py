"""Run model-free engine consumer tests on native arm64 macOS; never capture a game."""

import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tomllib

from check import ROOT, RUNTIME_ROOT, RUST_VERSION, run


MACOS_FLOOR = (26, 5, 2)


def main():
    try:
        if sys.platform != "darwin" or platform.machine() != "arm64":
            raise ValueError("engine CI requires native arm64 macOS")
        version = tuple(int(part) for part in platform.mac_ver()[0].split("."))
        if version < MACOS_FLOOR:
            raise ValueError("engine CI requires macOS >=26.5.2; do not lower the deployment floor")
        print(f"Engine consumer host: {platform.platform()} {platform.machine()}", flush=True)
        scratch = ROOT / ".cache/engine-consumer"
        # Unix socket fixtures append names within macOS's short pathname limit.
        temporary = ROOT / ".cache/et"
        temporary.mkdir(parents=True, exist_ok=True)
        os.environ.update({
            "TEMP": str(temporary), "TMP": str(temporary), "TMPDIR": str(temporary),
            "MACOSX_DEPLOYMENT_TARGET": ".".join(map(str, MACOS_FLOOR)),
            "HOMEBREW_CACHE": str(scratch / "homebrew"),
            "npm_config_cache": str(scratch / "npm"),
        })
        manifest = RUNTIME_ROOT / "Cargo.toml"
        dependency = tomllib.loads((ROOT / manifest).read_text(encoding="utf-8"))["dependencies"]["mado-pilot"]
        cargo = ["cargo", f"+{RUST_VERSION}"]
        metadata = json.loads(subprocess.check_output(
            [*cargo, "metadata", "--locked", "--format-version", "1", "--manifest-path", str(manifest),
             "--features", "engine", "--filter-platform", "aarch64-apple-darwin"],
            cwd=ROOT, text=True,
        ))
        source = f"git+{dependency['git']}?rev={dependency['rev']}#{dependency['rev']}"
        packages = [item for item in metadata["packages"]
                    if item["name"] == "mado-pilot" and item["source"] == source]
        if len(packages) != 1:
            raise ValueError("Cargo must resolve exactly the manifest-pinned mado-pilot source")
        checkout = Path(subprocess.check_output(
            ["git", "-C", str(Path(packages[0]["manifest_path"]).parent), "rev-parse", "--show-toplevel"],
            text=True,
        ).strip())
        revision = subprocess.check_output(["git", "-C", str(checkout), "rev-parse", "HEAD"], text=True).strip()
        if revision != dependency["rev"]:
            raise ValueError("native setup checkout does not match the consumer dependency revision")
        # Follow this exact upstream revision's hosted setup: OpenCV major 4,
        # not the incompatible unversioned formula. Print the actual minor version.
        run(["brew", "install", "opencv@4"], ROOT)
        run(["brew", "list", "--versions", "opencv@4"], ROOT)
        run(["npm", "ci", "--ignore-scripts", "--no-audit", "--no-fund",
             "--prefix", str(RUNTIME_ROOT / "compiler")], ROOT)
        run(["node", str(RUNTIME_ROOT / "compiler/compile.mjs"), "--self-check"], ROOT)
        run([sys.executable, str(checkout / "tools/setup-native.py"), "--",
             *cargo, "test", "--locked", "--manifest-path", str(manifest), "--features", "engine",
             "--target-dir", str(RUNTIME_ROOT / "target/engine-consumer-ci"), "--lib"], ROOT)
        return 0
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"macOS engine check failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
