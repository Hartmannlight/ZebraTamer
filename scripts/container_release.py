"""Publish tested PrintAgent archives and immutable multiarch indexes."""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import subprocess
import sys


IMAGE = "ghcr.io/hartmannlight/print-agent"


def run(*args: str) -> str:
    return subprocess.check_output(args, text=True).strip()


def output(key: str, value: str) -> None:
    with Path(os.environ["GITHUB_OUTPUT"]).open("a", encoding="utf-8") as stream:
        stream.write(f"{key}={value}\n")


def assert_absent(reference: str) -> None:
    result = subprocess.run(
        ["docker", "buildx", "imagetools", "inspect", reference],
        capture_output=True,
        text=True,
    )
    if result.returncode == 0:
        raise RuntimeError(f"Refusing to overwrite immutable reference {reference}")
    message = (result.stderr + result.stdout).lower()
    if not any(term in message for term in ("not found", "manifest unknown", "no such manifest")):
        raise RuntimeError(f"Cannot establish whether {reference} exists: {result.stderr}")


def context() -> tuple[str, str | None, str]:
    sha = os.environ["GITHUB_SHA"]
    ref = os.environ["GITHUB_REF"]
    version = ref.removeprefix("refs/tags/") if ref.startswith("refs/tags/") else None
    if not re.fullmatch(r"[a-f0-9]{40}", sha):
        raise RuntimeError("Unexpected source revision")
    if ref != "refs/heads/main" and not (version and re.fullmatch(r"v\d+\.\d+\.\d+", version)):
        raise RuntimeError("Publication is restricted to main and exact stable tags")
    build = f"sha-{sha}-r{os.environ['GITHUB_RUN_ID']}-{os.environ['GITHUB_RUN_ATTEMPT']}"
    return sha, version, build


def main() -> None:
    sha, version, build = context()
    mode = sys.argv[1]
    if mode == "platform":
        arch = os.environ["ARCH"]
        if arch not in {"amd64", "arm64"}:
            raise RuntimeError("Unexpected platform")
        tag = f"{IMAGE}:{build}-{arch}"
        assert_absent(tag)
        run("docker", "load", "-i", "candidate/image.tar")
        run("docker", "tag", "candidate:print-agent", tag)
        run("docker", "push", tag)
        manifest = json.loads(
            run("docker", "buildx", "imagetools", "inspect", tag, "--format", "{{json .Manifest}}")
        )
        Path("metadata").mkdir(exist_ok=True)
        Path(f"metadata/{arch}.json").write_text(
            json.dumps({"arch": arch, "reference": f"{IMAGE}@{manifest['digest']}"}),
            encoding="utf-8",
        )
        output("digest", manifest["digest"])
        return
    if mode == "merge":
        entries = [json.loads(path.read_text(encoding="utf-8")) for path in Path("metadata").glob("*.json")]
        if len(entries) != 2 or {entry["arch"] for entry in entries} != {"amd64", "arm64"}:
            raise RuntimeError("Both platform images are required")
        immutable = f"{IMAGE}:{build}"
        assert_absent(immutable)
        references = [entry["reference"] for entry in entries]
        run("docker", "buildx", "imagetools", "create", "-t", immutable, *references)
        manifest = json.loads(
            run("docker", "buildx", "imagetools", "inspect", immutable, "--format", "{{json .Manifest}}")
        )
        platforms = {
            (item.get("platform", {}).get("os"), item.get("platform", {}).get("architecture"))
            for item in manifest["manifests"]
        }
        if not {("linux", "amd64"), ("linux", "arm64")} <= platforms:
            raise RuntimeError("Published index is missing a supported platform")
        digest = manifest["digest"]
        if version:
            run("git", "fetch", "origin", "main")
            subprocess.run(["git", "merge-base", "--is-ancestor", sha, "FETCH_HEAD"], check=True)
            assert_absent(f"{IMAGE}:{version}")
            run("docker", "buildx", "imagetools", "create", "-t", f"{IMAGE}:{version}", f"{IMAGE}@{digest}")
        elif run("git", "ls-remote", "origin", "refs/heads/main").split()[0] == sha:
            run("docker", "buildx", "imagetools", "create", "-t", f"{IMAGE}:latest", f"{IMAGE}@{digest}")
        Path("release-digest.txt").write_text(
            f"print-agent={IMAGE}@{digest}\n", encoding="utf-8"
        )
        output("digest", digest)
        return
    raise RuntimeError("Unknown release operation")


if __name__ == "__main__":
    main()
