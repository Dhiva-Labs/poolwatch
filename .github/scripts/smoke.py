#!/usr/bin/env python3
"""End-to-end smoke test for a built poolwatch binary.

Starts a throwaway HTTP server that answers the three monerod RPC endpoints
poolwatch uses with the captured fixtures from tests/fixtures/, then runs the
binary in `--once` mode against it and checks the printed report and exit
codes. A second run against a closed port checks the failure path. This
exercises the real executable (CLI, config parsing, HTTP client, JSON
parsing, report formatting) on the platform it was built for.

usage: smoke.py <path-to-poolwatch-binary>
"""

import http.server
import json
import pathlib
import subprocess
import sys
import tempfile
import threading

ROOT = pathlib.Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "tests" / "fixtures"
HEIGHT = 3733712

GET_INFO = json.dumps(
    {
        "id": "0",
        "jsonrpc": "2.0",
        "result": {
            "height": HEIGHT,
            "status": "OK",
            "version": "0.18.4.0-release",
            "incoming_connections_count": 3,
            "outgoing_connections_count": 12,
        },
    }
).encode()


class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length") or 0))
        routes = {
            "/get_transaction_pool": (FIXTURES / "monerod_pool.json").read_bytes(),
            "/get_transaction_pool_stats": (
                FIXTURES / "monerod_pool_stats.json"
            ).read_bytes(),
            "/json_rpc": GET_INFO,
        }
        body = routes.get(self.path)
        self.send_response(200 if body is not None else 404)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body or b"")))
        self.end_headers()
        self.wfile.write(body or b"")

    def log_message(self, *args):
        pass


def write_config(directory, port):
    log = (directory / "poolwatch.log").as_posix()
    config = directory / "poolwatch.toml"
    config.write_text(
        f"""[general]
poll_interval_secs = 5
stats_interval_secs = 2
log_file = "{log}"

[[chain]]
id = "monero-main"
display_name = "Monero"
ticker = "XMR"
family = "cryptonote"
rpc_url = "http://127.0.0.1:{port}"
decimals = 12
""",
        encoding="utf-8",
    )
    return config


def run(binary, *args):
    proc = subprocess.run(
        [str(binary), *args], capture_output=True, text=True, timeout=60
    )
    print(f"$ poolwatch {' '.join(args)}  ->  exit {proc.returncode}")
    print(proc.stdout, end="")
    print(proc.stderr, end="", file=sys.stderr)
    return proc


def check(condition, message):
    if not condition:
        print(f"SMOKE FAILED: {message}", file=sys.stderr)
        sys.exit(1)


def main():
    binary = pathlib.Path(sys.argv[1]).resolve()

    version = run(binary, "--version")
    check(version.returncode == 0, "--version must exit 0")
    check(version.stdout.startswith("poolwatch "), "--version output unexpected")

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    live_port = server.server_address[1]

    # A free port with nothing listening on it, for the failure path.
    probe = http.server.HTTPServer(("127.0.0.1", 0), Handler)
    dead_port = probe.server_address[1]
    probe.server_close()

    with tempfile.TemporaryDirectory() as tmp:
        tmp = pathlib.Path(tmp)

        ok = run(binary, "--config", str(write_config(tmp, live_port)), "--once")
        check(ok.returncode == 0, "--once against the mock node must exit 0")
        check("Monero (monero-main): OK" in ok.stdout, "chain not reported OK")
        check("txs: 3" in ok.stdout, "expected the 3 fixture transactions")
        check(f"height: {HEIGHT}" in ok.stdout, "expected the mock node height")

        bad = run(binary, "--config", str(write_config(tmp, dead_port)), "--once")
        check(bad.returncode == 1, "--once with no node must exit 1")
        check("ERROR" in bad.stdout, "unreachable node must be reported as ERROR")

    server.shutdown()
    print("SMOKE OK")


if __name__ == "__main__":
    main()
