"""Smoke-test the exact PrintAgent candidate image without printer access."""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import urllib.request

ADMIN_TOKEN = "container-smoke-admin-token"


def run(*args: str) -> str:
    return subprocess.check_output(args, text=True).strip()


def main() -> None:
    image = sys.argv[1]
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        config = root / "config.toml"
        data = root / "data"
        data.mkdir()
        data.chmod(0o777)
        config.write_text(
            'listen = "0.0.0.0:8080"\n'
            'data_dir = "/var/lib/zpl-agent"\n'
            'storage_mode = "metadata_only"\n'
            'mdns_enabled = false\n'
            f'admin_token = "{ADMIN_TOKEN}"\n',
            encoding="utf-8",
        )
        container = run(
            "docker",
            "run",
            "-d",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges:true",
            "--read-only",
            "--tmpfs",
            "/tmp:rw,noexec,nosuid,size=16m",
            "-v",
            f"{config.resolve()}:/etc/zpl-agent/config.toml:ro",
            "-v",
            f"{data.resolve()}:/var/lib/zpl-agent",
            "-p",
            "127.0.0.1::8080",
            image,
        )
        try:
            binding = run("docker", "port", container, "8080/tcp").splitlines()[0]
            host, port = binding.rsplit(":", 1)
            deadline = time.monotonic() + 60
            while True:
                try:
                    request = urllib.request.Request(
                        f"http://{host}:{port}/v1/agent",
                        headers={"Authorization": f"Bearer {ADMIN_TOKEN}"},
                    )
                    with urllib.request.urlopen(request, timeout=2) as response:
                        document = json.load(response)
                        if response.status == 200 and document.get("data"):
                            break
                except OSError:
                    if time.monotonic() >= deadline:
                        raise
                    time.sleep(1)
            if run("docker", "exec", container, "id", "-u") == "0":
                raise RuntimeError("PrintAgent candidate runs as root")
        finally:
            subprocess.run(["docker", "rm", "-f", container], check=False)


if __name__ == "__main__":
    main()
