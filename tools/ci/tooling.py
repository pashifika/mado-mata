"""Pinned native tools shared by the explicit installer and offline checker."""

import hashlib
import json
import os
from pathlib import Path
import platform
import re

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = Path(__file__).with_name("toolchain.json")


def load_manifest():
    data = json.loads(MANIFEST.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or set(data) != {"actions", "tools"}:
        raise ValueError("toolchain.json must contain actions and tools")
    tools = data["tools"]
    if not isinstance(tools, dict) or set(tools) != {"actionlint", "lychee"}:
        raise ValueError("toolchain.json must pin actionlint and lychee")
    for name, pin in tools.items():
        if not isinstance(pin, dict) or not re.fullmatch(r"\d+\.\d+\.\d+", str(pin.get("version", ""))):
            raise ValueError(f"invalid version pin for {name}")
        assets = pin.get("assets")
        if not isinstance(assets, dict) or not assets:
            raise ValueError(f"missing platform assets for {name}")
        for host, asset in assets.items():
            if host not in {"linux-x86_64", "linux-arm64", "darwin-x86_64", "darwin-arm64"}:
                raise ValueError(f"unsupported tool platform: {host}")
            if not isinstance(asset, dict):
                raise ValueError(f"invalid asset for {name}/{host}")
            url = asset.get("url")
            if not isinstance(url, str) or not re.fullmatch(
                r"https://github\.com/[\w-]+/[\w-]+/releases/download/[\w.-]+/[\w.-]+\.tar\.gz", url
            ):
                raise ValueError(f"invalid release URL for {name}/{host}")
            digest = asset.get("sha256")
            if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
                raise ValueError(f"invalid SHA256 for {name}/{host}")
    actions = data["actions"]
    if not isinstance(actions, dict) or set(actions) != {"actions/checkout", "actions/setup-python"}:
        raise ValueError("toolchain.json must pin checkout and setup-python")
    for name, pin in actions.items():
        if not isinstance(pin, dict) or not re.fullmatch(r"[0-9a-f]{40}", str(pin.get("sha", ""))):
            raise ValueError(f"invalid action commit pin for {name}")
    return data


def host_platform():
    machine = platform.machine().lower()
    machine = {"aarch64": "arm64", "amd64": "x86_64"}.get(machine, machine)
    return f"{platform.system().lower()}-{machine}"


def tool_location(name, pin, host, root=ROOT):
    if host not in pin["assets"]:
        raise ValueError(f"{name} has no pinned asset for {host}; full checks require Linux or macOS on x64/arm64")
    return root / ".cache" / "repository-ci" / f"{name}-{pin['version']}-{host}" / name


def sha256_file(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def installed_tool(name, pin, host, root=ROOT):
    binary = tool_location(name, pin, host, root)
    try:
        receipt = json.loads(binary.with_name("receipt.json").read_text(encoding="utf-8"))
        valid = (
            isinstance(receipt, dict)
            and receipt.get("archive_sha256") == pin["assets"][host]["sha256"]
            and binary.is_file()
            and not binary.is_symlink()
            and receipt.get("binary_sha256") == sha256_file(binary)
            and os.access(binary, os.X_OK)
        )
    except (OSError, ValueError):
        valid = False
    if not valid:
        raise ValueError(f"{name} is missing, stale, or modified; run python3 tools/ci/install_tools.py")
    return binary
