#!/usr/bin/env python3
"""Run the README network scenario against a fresh, temporary local server.

Requires Python 3.9+ and a Knotrel server binary with v0.5.0 lifecycle support.
Only the child server is modified; no existing server or graph is contacted.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import urllib.request


def run(binary):
    """Start an isolated server, verify each operation, and always stop it."""
    env = {k: v for k, v in os.environ.items() if not k.startswith("KNOTREL_")}
    env.update(KNOTREL_ADDR="127.0.0.1:0", KNOTREL_MODE="standalone",
               KNOTREL_ENGINE="compact-bfs", KNOTREL_TOPOLOGY_LIFECYCLE="true",
               LOG_FORMAT="json", RUST_LOG="info")
    with tempfile.TemporaryDirectory(prefix="knotrel-demo-") as directory, \
            open(Path(directory) / "server.log", "wb") as log:
        process = subprocess.Popen([str(Path(binary).resolve())], env=env,
                                   stdout=log, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + 15
            address = None
            while time.monotonic() < deadline:
                for line in (Path(directory) / "server.log").read_text(
                        encoding="utf-8", errors="replace").splitlines():
                    try:
                        fields = json.loads(line).get("fields", {})
                    except json.JSONDecodeError:
                        continue
                    if fields.get("message") == "Knotrel listening; graph state is in memory":
                        address = fields["address"]
                        break
                if address:
                    break
                if process.poll() is not None:
                    raise RuntimeError("Server exited during startup")
                time.sleep(0.05)
            if not address:
                raise RuntimeError("Server did not announce its address within 15 seconds")
            client = urllib.request.build_opener(urllib.request.ProxyHandler({}))

            def operation(payload, expected):
                request = urllib.request.Request(
                    f"http://{address}/v1/operations",
                    data=json.dumps(payload).encode(),
                    headers={"Content-Type": "application/json"})
                with client.open(request, timeout=5) as response:
                    actual = json.load(response)
                    if response.status != 200 or actual != expected:
                        raise RuntimeError(f"Unexpected response for {payload}: {actual}")

            changed = {"result": "changed", "changed": True}
            for source, target in [(1, 2), (2, 4), (1, 3), (3, 4)]:
                operation({"op": "link", "source": str(source), "target": str(target)}, changed)

            def check(label, expected):
                operation({"op": "connected", "source": "1", "target": "4"},
                          {"result": "connected", "connected": expected})
                print(f"{label}: connected={str(expected).lower()}")

            check("Two available paths", True)
            for node, active, label, connected in [
                ("2", False, "A offline; B still available", True),
                ("3", False, "A and B offline", False),
                ("2", True, "A restored", True),
            ]:
                operation({"op": "set_node_active", "node": node, "active": active}, changed)
                check(label, connected)
            print("PASS: all expected responses verified; temporary server will stop.")
        except BaseException:
            print((Path(directory) / "server.log").read_text(
                encoding="utf-8", errors="replace"))
            raise
        finally:
            if process.poll() is None:
                process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", help="Path to the knotrel-server executable")
    run(parser.parse_args().binary)
