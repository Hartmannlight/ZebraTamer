#!/usr/bin/env python3
"""Render the dated "ente" label and submit it through the local zpl-agent."""

from __future__ import annotations

import argparse
from datetime import date
import json
from pathlib import Path
import re
import sys
import time
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import Request, urlopen


ASSET_DIRECTORY = Path("/usr/local/share/kuche-pi-audio/labels")
BACKGROUND = ASSET_DIRECTORY / "opened_am_bg_203.zpl"
TEMPLATE = ASSET_DIRECTORY / "opened_am_print_template_203.zpl"
DATE_TOKEN = b"{{DATUM}}"
DATE_FORMAT = re.compile(r"\d{2}\.\d{2}\Z")
PRINTER_NAME = re.compile(r"[A-Za-z0-9._-]+\Z")
DEFAULT_AGENT_URL = "http://127.0.0.1:8080"
GRAPHIC_ACCEPT_TIMEOUT_SECONDS = 35


def render_parts(background: bytes, template: bytes, label_date: str) -> tuple[bytes, bytes]:
    if not DATE_FORMAT.fullmatch(label_date):
        raise ValueError("Datum muss das Format TT.MM haben")
    if template.count(DATE_TOKEN) != 1:
        raise ValueError("Die ZPL-Vorlage muss {{DATUM}} genau einmal enthalten")
    return background.rstrip(b"\r\n") + b"\r\n", template.replace(DATE_TOKEN, label_date.encode("ascii"))


def render(background: bytes, template: bytes, label_date: str) -> bytes:
    return b"".join(render_parts(background, template, label_date))


def submit_job(agent_url: str, printer: str, payload: bytes, description: str) -> str:
    if not PRINTER_NAME.fullmatch(printer):
        raise ValueError("Ungültiger Druckername")
    endpoint = f"{agent_url.rstrip('/')}/v1/printers/{quote(printer, safe='._-')}/jobs"
    request = Request(
        endpoint,
        data=payload,
        method="POST",
        headers={
            "Content-Type": "application/zpl",
            "X-ZPL-Origin": "kuche-pi-f16",
            "X-ZPL-Description": description,
        },
    )
    try:
        with urlopen(request, timeout=10) as response:
            if response.status != 202:
                raise OSError(f"zpl-agent antwortete mit HTTP {response.status}")
            body = json.load(response)
    except HTTPError as error:
        raise OSError(f"zpl-agent antwortete mit HTTP {error.code}") from error
    except URLError as error:
        raise OSError(f"zpl-agent ist nicht erreichbar: {error.reason}") from error
    job_id = body.get("data", {}).get("id")
    if not isinstance(job_id, str):
        raise OSError("zpl-agent hat keine Job-ID zurückgegeben")
    return job_id


def wait_for_transport(agent_url: str, job_id: str, timeout_seconds: float) -> None:
    endpoint = f"{agent_url.rstrip('/')}/v1/jobs/{quote(job_id, safe='-')}"
    deadline = time.monotonic() + timeout_seconds
    while True:
        try:
            with urlopen(endpoint, timeout=5) as response:
                body = json.load(response)
        except (HTTPError, URLError) as error:
            raise OSError(f"zpl-agent Jobstatus nicht erreichbar: {error}") from error
        job = body.get("data", {})
        state = job.get("state")
        if state == "transport_accepted":
            return
        if state in {"failed", "outcome_unknown"}:
            raise OSError(f"Grafik-Upload fehlgeschlagen: {job.get('error') or state}")
        if time.monotonic() >= deadline:
            raise OSError("Grafik-Upload wurde nicht innerhalb von 35 Sekunden übernommen")
        time.sleep(0.25)


def send_to_printer(agent_url: str, printer: str, graphic_job: bytes, label_job: bytes) -> None:
    graphic_id = submit_job(agent_url, printer, graphic_job, "F16 Grafik-Upload")
    wait_for_transport(agent_url, graphic_id, GRAPHIC_ACCEPT_TIMEOUT_SECONDS)
    submit_job(agent_url, printer, label_job, "F16 Tagesetikett")


def main() -> None:
    parser = argparse.ArgumentParser(description="Druckt das Tagesetikett über zpl-agent")
    parser.add_argument("--printer", default="ente", help="zpl-agent-Drucker-ID (Standard: ente)")
    parser.add_argument("--agent-url", default=DEFAULT_AGENT_URL, help="zpl-agent Basis-URL")
    parser.add_argument("--date", default=date.today().strftime("%d.%m"), help="TT.MM; nur für einen Nachdruck")
    parser.add_argument("--dry-run", action="store_true", help="ZPL nur auf stdout ausgeben")
    # Accepted as a no-op for compatibility with the installed audio-buttons
    # unit from older kuche-pi releases.
    parser.add_argument("--cache-only", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--refresh-cache", action="store_true", help=argparse.SUPPRESS)
    arguments = parser.parse_args()

    try:
        if arguments.cache_only:
            return
        background = BACKGROUND.read_bytes()
        template = TEMPLATE.read_bytes()
        graphic_job, label_job = render_parts(background, template, arguments.date)
        if arguments.dry_run:
            sys.stdout.buffer.write(graphic_job + label_job)
            return
        send_to_printer(arguments.agent_url, arguments.printer, graphic_job, label_job)
    except (OSError, ValueError) as error:
        print(f"Etikett wurde nicht gedruckt: {error}", file=sys.stderr)
        raise SystemExit(1)


if __name__ == "__main__":
    main()
