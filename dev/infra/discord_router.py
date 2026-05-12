#!/usr/bin/env python3
"""Small Discord interaction router for multiple Thala services.

Discord only allows one interaction endpoint per application. This process
keeps that single public endpoint and forwards the signed request body to the
right local Thala service without modifying the payload.
"""

from __future__ import annotations

import json
import logging
import os
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any


DEFAULT_BIND = "127.0.0.1:8792"
DEFAULT_MAIN_URL = "http://127.0.0.1:8789/api/discord/interaction"
DEFAULT_CHIROPRO_URL = "http://127.0.0.1:8791/api/discord/interaction"


def env(name: str, default: str) -> str:
    value = os.environ.get(name, "").strip()
    return value or default


MAIN_URL = env("THALA_ROUTER_MAIN_URL", DEFAULT_MAIN_URL)
CHIROPRO_URL = env("THALA_ROUTER_CHIROPRO_URL", DEFAULT_CHIROPRO_URL)
DEFAULT_TARGET = env("THALA_ROUTER_DEFAULT_TARGET", "main").lower()

ROUTE_HINTS = {
    hint.strip().lower()
    for hint in env(
        "THALA_ROUTER_CHIROPRO_HINTS",
        "chiropro,chiro pro,makotec-xyz/chiropro,github.com/makotec-xyz/chiropro",
    ).split(",")
    if hint.strip()
}


def extract_text(value: Any) -> list[str]:
    """Collect command option strings from a Discord interaction payload."""
    texts: list[str] = []
    if isinstance(value, dict):
        for key, nested in value.items():
            if key == "value" and isinstance(nested, str):
                texts.append(nested)
            else:
                texts.extend(extract_text(nested))
    elif isinstance(value, list):
        for item in value:
            texts.extend(extract_text(item))
    return texts


def route_for_payload(payload: dict[str, Any]) -> str:
    interaction_type = payload.get("type")

    if interaction_type == 3:
        custom_id = str(payload.get("data", {}).get("custom_id", ""))
        parts = custom_id.split(":")
        task_id = parts[4] if len(parts) >= 5 else ""
        if task_id.startswith("chiropro-"):
            return "chiropro"
        if task_id.startswith("thala-"):
            return "main"
        return DEFAULT_TARGET

    if interaction_type == 2:
        data = payload.get("data", {})
        command_name = str(data.get("name", "")).lower()
        text = " ".join(extract_text(data)).lower()
        route_text = f"{command_name} {text}"

        if any(hint in route_text for hint in ROUTE_HINTS):
            return "chiropro"
        if route_text.strip().startswith(("chiropro:", "chiropro ", "/chiropro")):
            return "chiropro"
        if route_text.strip().startswith(("thala:", "thala ", "/thala")):
            return "main"
        return DEFAULT_TARGET

    return DEFAULT_TARGET


def target_url(route: str) -> str:
    return CHIROPRO_URL if route == "chiropro" else MAIN_URL


class RouterHandler(BaseHTTPRequestHandler):
    server_version = "ThalaDiscordRouter/0.1"

    def do_POST(self) -> None:
        if self.path not in {"/api/discord/interaction", "/chiropro/api/discord/interaction"}:
            self.send_error(404)
            return

        try:
            content_length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            self.send_error(400, "Invalid Content-Length")
            return

        body = self.rfile.read(content_length)
        route = DEFAULT_TARGET
        try:
            payload = json.loads(body)
            route = route_for_payload(payload)
        except json.JSONDecodeError:
            logging.warning("Forwarding invalid JSON to default target")

        url = target_url(route)
        logging.info("Routing Discord interaction path=%s route=%s url=%s", self.path, route, url)

        headers = {
            "Content-Type": self.headers.get("Content-Type", "application/json"),
            "X-Signature-Ed25519": self.headers.get("X-Signature-Ed25519", ""),
            "X-Signature-Timestamp": self.headers.get("X-Signature-Timestamp", ""),
            "User-Agent": self.headers.get("User-Agent", "ThalaDiscordRouter/0.1"),
        }
        req = urllib.request.Request(url, data=body, headers=headers, method="POST")

        try:
            with urllib.request.urlopen(req, timeout=20) as resp:
                response_body = resp.read()
                self.send_response(resp.status)
                self.send_header("Content-Type", resp.headers.get("Content-Type", "application/json"))
                self.send_header("Content-Length", str(len(response_body)))
                self.end_headers()
                self.wfile.write(response_body)
        except urllib.error.HTTPError as exc:
            response_body = exc.read()
            self.send_response(exc.code)
            self.send_header("Content-Type", exc.headers.get("Content-Type", "application/json"))
            self.send_header("Content-Length", str(len(response_body)))
            self.end_headers()
            self.wfile.write(response_body)
        except Exception as exc:  # noqa: BLE001 - last-resort HTTP boundary
            logging.exception("Discord route failed: %s", exc)
            response_body = b'{"error":"Discord router upstream failed"}'
            self.send_response(502)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(response_body)))
            self.end_headers()
            self.wfile.write(response_body)

    def log_message(self, fmt: str, *args: Any) -> None:
        logging.info("%s - %s", self.address_string(), fmt % args)


def main() -> None:
    logging.basicConfig(level=os.environ.get("LOG_LEVEL", "INFO"), format="%(asctime)s %(levelname)s %(message)s")
    bind = env("THALA_DISCORD_ROUTER_BIND", DEFAULT_BIND)
    host, port_text = bind.rsplit(":", 1)
    server = ThreadingHTTPServer((host, int(port_text)), RouterHandler)
    logging.info(
        "Discord router listening bind=%s default=%s main=%s chiropro=%s",
        bind,
        DEFAULT_TARGET,
        MAIN_URL,
        CHIROPRO_URL,
    )
    server.serve_forever()


if __name__ == "__main__":
    main()
