"""Explicitly download and install the host's checksum-pinned CI tools."""

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import shutil
import sys
import tarfile
import tempfile
import urllib.error
import urllib.request

from tooling import ROOT, host_platform, load_manifest, sha256_file, tool_location

MAX_ARCHIVE_BYTES = 128 * 1024 * 1024


def extract_binary(archive_path, destination, name):
    # Never extract archive paths, links, ownership, or permissions into the tree.
    with tarfile.open(archive_path, "r:gz") as archive:
        matches = [member for member in archive.getmembers() if PurePosixPath(member.name).name == name]
        if len(matches) != 1 or not matches[0].isfile() or matches[0].size > MAX_ARCHIVE_BYTES:
            raise ValueError(f"release archive must contain one regular {name} binary")
        source = archive.extractfile(matches[0])
        if source is None:
            raise ValueError(f"cannot read {name} from release archive")
        with source, destination.open("wb") as target:
            shutil.copyfileobj(source, target)
    destination.chmod(0o755)


def install(name, pin, host):
    destination = tool_location(name, pin, host)
    destination.parent.mkdir(parents=True, exist_ok=True)
    asset = pin["assets"][host]
    # Temporary files share the destination filesystem so replacement is atomic.
    with tempfile.TemporaryDirectory(prefix="install-", dir=destination.parent) as directory:
        temporary = Path(directory)
        archive_path = temporary / "release.tar.gz"
        digest = hashlib.sha256()
        request = urllib.request.Request(asset["url"], headers={"User-Agent": "mado-mata-repository-ci"})
        with urllib.request.urlopen(request, timeout=60) as response, archive_path.open("wb") as target:
            if not response.url.startswith("https://"):
                raise ValueError("refusing a non-HTTPS download redirect")
            size = 0
            while chunk := response.read(1024 * 1024):
                size += len(chunk)
                if size > MAX_ARCHIVE_BYTES:
                    raise ValueError(f"release archive exceeds size limit for {name}")
                target.write(chunk)
                digest.update(chunk)
        if digest.hexdigest() != asset["sha256"]:
            raise ValueError(f"SHA256 mismatch for {name}; refusing to install")
        binary = temporary / name
        extract_binary(archive_path, binary, name)
        receipt = temporary / "receipt.json"
        receipt.write_text(json.dumps({
            "archive_sha256": asset["sha256"],
            "binary_sha256": sha256_file(binary),
        }) + "\n", encoding="utf-8")
        binary.replace(destination)
        receipt.replace(destination.with_name("receipt.json"))
    print(f"Installed {name} {pin['version']} ({host}) in {destination.relative_to(ROOT)}")


def main():
    argparse.ArgumentParser(description=__doc__).parse_args()
    if sys.version_info < (3, 11):
        print("Installation requires Python 3.11 or newer.", file=sys.stderr)
        return 1
    try:
        tools = load_manifest()["tools"]
        host = host_platform()
        # Refuse an unsupported host before downloading either tool.
        for name, pin in tools.items():
            tool_location(name, pin, host)
        for name, pin in tools.items():
            install(name, pin, host)
    except (OSError, ValueError, tarfile.TarError, urllib.error.URLError) as error:
        print(f"Tool installation failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
