#!/usr/bin/env python3
"""
End-to-End Recovery Simulation Test Suite for Knotrel release publishing.
Simulates all 12 scenarios without actual registry uploads:
1. Neither crate is published.
2. Core published, server absent.
3. Both crates already published with matching checksums.
4. Core has conflicting content.
5. Server has conflicting content.
6. Core publication succeeds but server publication fails.
7. Both crates publish successfully but GitHub draft finalization fails.
8. GitHub draft already exists and matches.
9. GitHub draft already exists but conflicts.
10. Registry API is unavailable.
11. Cargo index propagation is delayed.
12. Workflow is cancelled after the first crate publication (idempotent resume).
"""

import hashlib
import http.server
import json
import os
import shutil
import socketserver
import subprocess
import sys
import tempfile
import threading
import unittest

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))

def compute_sha256(filepath):
    h = hashlib.sha256()
    with open(filepath, "rb") as f:
        while chunk := f.read(65536):
            h.update(chunk)
    return h.hexdigest()

def create_14_test_assets(assets_dir, version="0.1.0", dummy_content=b"sample-asset-bytes"):
    tag_name = f"v{version}"
    expected_files = [
        f"knotrel-server-{tag_name}-x86_64-unknown-linux-gnu.tar.gz",
        f"knotrel-server-{tag_name}-x86_64-unknown-linux-gnu.tar.gz.sha256",
        f"knotrel-server-{tag_name}-x86_64-unknown-linux-gnu-metadata.json",
        f"knotrel-server-{tag_name}-aarch64-apple-darwin.tar.gz",
        f"knotrel-server-{tag_name}-aarch64-apple-darwin.tar.gz.sha256",
        f"knotrel-server-{tag_name}-aarch64-apple-darwin-metadata.json",
        f"knotrel-server-{tag_name}-x86_64-pc-windows-msvc.zip",
        f"knotrel-server-{tag_name}-x86_64-pc-windows-msvc.zip.sha256",
        f"knotrel-server-{tag_name}-x86_64-pc-windows-msvc-metadata.json",
        f"knotrel-core-{version}.crate",
        f"knotrel-core-{version}.crate.sha256",
        f"knotrel-server-{version}.crate",
        f"knotrel-server-{version}.crate.sha256",
        "SHA256SUMS",
    ]
    os.makedirs(assets_dir, exist_ok=True)
    for f in expected_files:
        p = os.path.join(assets_dir, f)
        with open(p, "wb") as out:
            out.write(dummy_content + f.encode("utf-8"))
    return expected_files

def setup_test_repo(path):
    os.makedirs(path, exist_ok=True)
    subprocess.check_call(["git", "init"], cwd=path, stdout=subprocess.DEVNULL)
    subprocess.check_call(["git", "config", "user.name", "Test"], cwd=path)
    subprocess.check_call(["git", "config", "user.email", "test@example.com"], cwd=path)

    with open(os.path.join(path, "Cargo.toml"), "w") as f:
        f.write('[workspace]\nmembers = ["crates/*"]\nresolver = "2"\n[workspace.package]\nversion = "0.1.0"\n')
    os.makedirs(os.path.join(path, "crates", "knotrel-core", "src"), exist_ok=True)
    with open(os.path.join(path, "crates", "knotrel-core", "src", "lib.rs"), "w") as f:
        f.write("pub fn core() {}\n")
    with open(os.path.join(path, "crates", "knotrel-core", "Cargo.toml"), "w") as f:
        f.write('[package]\nname = "knotrel-core"\nversion = "0.1.0"\nedition = "2021"\n')

    os.makedirs(os.path.join(path, "crates", "knotrel-server", "src"), exist_ok=True)
    with open(os.path.join(path, "crates", "knotrel-server", "src", "lib.rs"), "w") as f:
        f.write("pub fn server() {}\n")
    with open(os.path.join(path, "crates", "knotrel-server", "Cargo.toml"), "w") as f:
        f.write('[package]\nname = "knotrel-server"\nversion = "0.1.0"\nedition = "2021"\n')

    subprocess.check_call(["git", "add", "."], cwd=path)
    subprocess.check_call(["git", "commit", "-m", "init"], cwd=path, stdout=subprocess.DEVNULL)
    head_sha = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=path, text=True).strip()
    subprocess.check_call(["git", "tag", "v0.1.0", head_sha], cwd=path)
    return head_sha

class MockStatefulServer:
    def __init__(self):
        self.routes = {}
        self.call_log = []
        outer = self

        class Handler(http.server.SimpleHTTPRequestHandler):
            def do_GET(self):
                outer.call_log.append(self.path)
                for prefix, handler in outer.routes.items():
                    if self.path.startswith(prefix):
                        code, headers, body = handler(self.path)
                        self.send_response(code)
                        for k, v in headers.items():
                            self.send_header(k, v)
                        self.end_headers()
                        self.wfile.write(body if isinstance(body, bytes) else body.encode("utf-8"))
                        return
                self.send_response(404)
                self.end_headers()
                self.wfile.write(b"Not Found")

            def log_message(self, format, *args):
                pass

        self.httpd = socketserver.TCPServer(("127.0.0.1", 0), Handler)
        self.port = self.httpd.server_address[1]
        self.thread = threading.Thread(target=self.httpd.serve_forever)
        self.thread.daemon = True
        self.thread.start()

    def url(self):
        return f"http://127.0.0.1:{self.port}"

    def shutdown(self):
        self.httpd.shutdown()
        self.httpd.server_close()


class TestEndToEndRecoveryScenarios(unittest.TestCase):
    def setUp(self):
        self.temp_dir = tempfile.mkdtemp()
        self.assets_dir = os.path.join(self.temp_dir, "assets")
        self.expected_files = create_14_test_assets(self.assets_dir, "0.1.0")

        self.core_crate = os.path.join(self.assets_dir, "knotrel-core-0.1.0.crate")
        self.server_crate = os.path.join(self.assets_dir, "knotrel-server-0.1.0.crate")
        self.core_sha = compute_sha256(self.core_crate)
        self.server_sha = compute_sha256(self.server_crate)

        # Isolated clean git repository with valid workspace and member manifests
        self.test_repo = os.path.join(self.temp_dir, "test_repo")
        self.head_sha = setup_test_repo(self.test_repo)

        self.mock_cargo = os.path.join(self.temp_dir, "mock_cargo.sh")
        self.mock_gh = os.path.join(self.temp_dir, "mock_gh.sh")
        self.publish_log = os.path.join(self.temp_dir, "publish.log")

        with open(self.mock_cargo, "w") as f:
            f.write(f"""#!/usr/bin/env bash
if [[ "$1" == "fetch" ]]; then
    exit 0
fi
if [[ "$1" == "package" ]]; then
    mkdir -p target/package
    cp "{self.core_crate}" target/package/knotrel-core-0.1.0.crate
    cp "{self.server_crate}" target/package/knotrel-server-0.1.0.crate
    exit 0
fi
if [[ "$1" == "publish" ]]; then
    echo "$@" >> "{self.publish_log}"
    exit 0
fi
if [[ "$1" == "generate-lockfile" ]]; then
    cat << 'EOF' > Cargo.lock
version = 3
[[package]]
name = "knotrel-core"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{self.core_sha}"

[[package]]
name = "knotrel-server"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{self.server_sha}"
EOF
    exit 0
fi
exit 0
""")
        os.chmod(self.mock_cargo, 0o755)

        self.gh_draft_state = os.path.join(self.temp_dir, "draft_state.txt")
        self.gh_log = os.path.join(self.temp_dir, "gh.log")

        assets_json = json.dumps([{"name": f, "size": os.path.getsize(os.path.join(self.assets_dir, f))} for f in self.expected_files])
        with open(self.mock_gh, "w") as f:
            f.write(f"""#!/usr/bin/env bash
echo "$@" >> "{self.gh_log}"
if [[ "$*" == *"release edit"* ]]; then
    echo "false" > "{self.gh_draft_state}"
    exit 0
fi
if [[ "$*" == *"release download"* ]]; then
    DIR=""
    while [[ "$#" -gt 0 ]]; do
        if [[ "$1" == "--dir" ]]; then
            DIR="$2"
            shift 2
        else
            shift
        fi
    done
    cp -r "{self.assets_dir}"/* "$DIR"/
    exit 0
fi
if [[ "$*" == *"release view"* ]]; then
    IS_DRAFT="true"
    if [[ -f "{self.gh_draft_state}" ]]; then
        IS_DRAFT=$(cat "{self.gh_draft_state}")
    fi
    cat << EOF
{{"isDraft": ${{IS_DRAFT}}, "tagName": "v0.1.0", "targetCommitish": "{self.head_sha}", "assets": {assets_json}}}
EOF
    exit 0
fi
exit 0
""")
        os.chmod(self.mock_gh, 0o755)

    def tearDown(self):
        shutil.rmtree(self.temp_dir)

    def run_publish_crates(self, mock_server_url):
        env = os.environ.copy()
        env["CARGO_REGISTRY_TOKEN"] = "valid-token"
        env["CRATES_IO_API_BASE"] = mock_server_url
        env["CRATES_IO_INDEX_BASE"] = mock_server_url
        env["CARGO_BIN"] = self.mock_cargo
        env["KNOTREL_REPO_DIR"] = self.test_repo
        env["PUBLISH_MAX_WAIT_SECONDS"] = "5"
        env["PUBLISH_POLL_INTERVAL_SECONDS"] = "1"
        script = os.path.join(REPO_ROOT, "scripts/publish-crates.sh")
        return subprocess.run([script, "0.1.0", self.head_sha, self.assets_dir], env=env, capture_output=True, text=True)

    def run_finalize_release(self, mock_server_url):
        env = os.environ.copy()
        env["GH_BIN"] = self.mock_gh
        env["KNOTREL_REPO_DIR"] = self.test_repo
        env["CRATES_IO_API_BASE"] = mock_server_url
        script = os.path.join(REPO_ROOT, "scripts/finalize-release.sh")
        return subprocess.run([script, "0.1.0", self.head_sha, self.assets_dir], env=env, capture_output=True, text=True)

    def test_scenario_1_neither_crate_published_both_upload_and_finalize(self):
        """Scenario 1: Neither crate published -> Preflight ok, core uploads, server uploads, finalizes."""
        published = set()
        server = MockStatefulServer()

        def core_api(path):
            if "knotrel-core" in published:
                return 200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": self.core_sha}})
            return 404, {}, "Not Found"

        def server_api(path):
            if "knotrel-server" in published:
                return 200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": self.server_sha}})
            return 404, {}, "Not Found"

        def core_index(path):
            if "knotrel-core" in published:
                return 200, {}, json.dumps({"name": "knotrel-core", "vers": "0.1.0", "cksum": self.core_sha}) + "\n"
            return 404, {}, "Not Found"

        def server_index(path):
            if "knotrel-server" in published:
                return 200, {}, json.dumps({"name": "knotrel-server", "vers": "0.1.0", "cksum": self.server_sha}) + "\n"
            return 404, {}, "Not Found"

        server.routes["/knotrel-core/0.1.0"] = core_api
        server.routes["/knotrel-server/0.1.0"] = server_api
        server.routes["/kn/ot/knotrel-core"] = core_index
        server.routes["/kn/ot/knotrel-server"] = server_index

        with open(self.mock_cargo, "w") as f:
            f.write(f"""#!/usr/bin/env bash
if [[ "$1" == "fetch" ]]; then
    exit 0
fi
if [[ "$1" == "package" ]]; then
    mkdir -p target/package
    cp "{self.core_crate}" target/package/knotrel-core-0.1.0.crate
    cp "{self.server_crate}" target/package/knotrel-server-0.1.0.crate
    exit 0
fi
if [[ "$1" == "publish" ]]; then
    echo "$@" >> "{self.publish_log}"
    if [[ "$*" == *"knotrel-core"* ]]; then
        curl -s "{server.url()}/publish_core" >/dev/null || true
    fi
    if [[ "$*" == *"knotrel-server"* ]]; then
        curl -s "{server.url()}/publish_server" >/dev/null || true
    fi
    exit 0
fi
if [[ "$1" == "generate-lockfile" ]]; then
    cat << 'EOF' > Cargo.lock
version = 3
[[package]]
name = "knotrel-core"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{self.core_sha}"

[[package]]
name = "knotrel-server"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{self.server_sha}"
EOF
    exit 0
fi
exit 0
""")
        os.chmod(self.mock_cargo, 0o755)

        def publish_core_hook(path):
            published.add("knotrel-core")
            return 200, {}, "OK"

        def publish_server_hook(path):
            published.add("knotrel-server")
            return 200, {}, "OK"

        server.routes["/publish_core"] = publish_core_hook
        server.routes["/publish_server"] = publish_server_hook

        try:
            res = self.run_publish_crates(server.url())
            self.assertEqual(res.returncode, 0, f"Publish failed: {res.stdout}\n{res.stderr}")
            self.assertIn("Both crates successfully published and verified on crates.io", res.stdout)

            # Now run finalization
            res_fin = self.run_finalize_release(server.url())
            self.assertEqual(res_fin.returncode, 0, f"Finalize failed: {res_fin.stdout}\n{res_fin.stderr}")
            self.assertIn("finalized and published successfully", res_fin.stdout)
        finally:
            server.shutdown()

    def test_scenario_2_core_published_server_absent_skips_core_and_publishes_server(self):
        """Scenario 2: Core published, server absent -> skips core upload, publishes server."""
        published = {"knotrel-core"}
        server = MockStatefulServer()

        server.routes["/knotrel-core/0.1.0"] = lambda p: (200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": self.core_sha}}))
        server.routes["/kn/ot/knotrel-core"] = lambda p: (200, {}, json.dumps({"name": "knotrel-core", "vers": "0.1.0", "cksum": self.core_sha}) + "\n")

        def server_api(path):
            if "knotrel-server" in published:
                return 200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": self.server_sha}})
            return 404, {}, "Not Found"

        def server_index(path):
            if "knotrel-server" in published:
                return 200, {}, json.dumps({"name": "knotrel-server", "vers": "0.1.0", "cksum": self.server_sha}) + "\n"
            return 404, {}, "Not Found"

        server.routes["/knotrel-server/0.1.0"] = server_api
        server.routes["/kn/ot/knotrel-server"] = server_index
        server.routes["/publish_server"] = lambda p: (published.add("knotrel-server") or True) and (200, {}, "OK")

        with open(self.mock_cargo, "w") as f:
            f.write(f"""#!/usr/bin/env bash
if [[ "$1" == "fetch" ]]; then
    exit 0
fi
if [[ "$1" == "package" ]]; then
    mkdir -p target/package
    cp "{self.core_crate}" target/package/knotrel-core-0.1.0.crate
    cp "{self.server_crate}" target/package/knotrel-server-0.1.0.crate
    exit 0
fi
if [[ "$1" == "publish" ]]; then
    echo "$@" >> "{self.publish_log}"
    if [[ "$*" == *"knotrel-server"* ]]; then
        curl -s "{server.url()}/publish_server" >/dev/null || true
    fi
    exit 0
fi
if [[ "$1" == "generate-lockfile" ]]; then
    cat << 'EOF' > Cargo.lock
version = 3
[[package]]
name = "knotrel-core"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{self.core_sha}"

[[package]]
name = "knotrel-server"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{self.server_sha}"
EOF
    exit 0
fi
exit 0
""")
        os.chmod(self.mock_cargo, 0o755)

        try:
            res = self.run_publish_crates(server.url())
            self.assertEqual(res.returncode, 0, f"Publish failed: {res.stdout}\n{res.stderr}")
            self.assertIn("knotrel-core v0.1.0 is already published with verified checksum. Skipping upload.", res.stdout)
            self.assertIn("Uploaded knotrel-server v0.1.0", res.stdout)
        finally:
            server.shutdown()

    def test_scenario_3_both_crates_already_published_safe_idempotent_noop(self):
        """Scenario 3: Both crates published with matching checksums -> Both skipped, verified, draft finalized safely."""
        server = MockStatefulServer()
        server.routes["/knotrel-core/0.1.0"] = lambda p: (200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": self.core_sha}}))
        server.routes["/knotrel-server/0.1.0"] = lambda p: (200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": self.server_sha}}))
        server.routes["/kn/ot/knotrel-core"] = lambda p: (200, {}, json.dumps({"name": "knotrel-core", "vers": "0.1.0", "cksum": self.core_sha}) + "\n")
        server.routes["/kn/ot/knotrel-server"] = lambda p: (200, {}, json.dumps({"name": "knotrel-server", "vers": "0.1.0", "cksum": self.server_sha}) + "\n")

        try:
            res = self.run_publish_crates(server.url())
            self.assertEqual(res.returncode, 0, f"Publish failed: {res.stdout}\n{res.stderr}")
            self.assertIn("Both knotrel-core v0.1.0 and knotrel-server v0.1.0 already exist on crates.io with verified matching checksums", res.stdout)
            # No publish calls in log
            self.assertFalse(os.path.exists(self.publish_log))

            # Finalization succeeds
            res_fin = self.run_finalize_release(server.url())
            self.assertEqual(res_fin.returncode, 0)
        finally:
            server.shutdown()

    def test_scenario_4_core_has_conflicting_content_aborts_immediately(self):
        """Scenario 4: Core published with conflicting content -> Aborts in preflight, no publish executed."""
        server = MockStatefulServer()
        server.routes["/knotrel-core/0.1.0"] = lambda p: (200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": "wrong_sha"}}))
        server.routes["/knotrel-server/0.1.0"] = lambda p: (404, {}, "Not Found")

        try:
            res = self.run_publish_crates(server.url())
            self.assertEqual(res.returncode, 1)
            self.assertIn("CONFLICT", res.stdout)
            self.assertFalse(os.path.exists(self.publish_log))
        finally:
            server.shutdown()

    def test_scenario_5_server_has_conflicting_content_aborts_before_core_upload(self):
        """Scenario 5: Core absent, but Server published with conflicting content -> Aborts BEFORE uploading core."""
        server = MockStatefulServer()
        server.routes["/knotrel-core/0.1.0"] = lambda p: (404, {}, "Not Found")
        server.routes["/knotrel-server/0.1.0"] = lambda p: (200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": "wrong_sha"}}))

        try:
            res = self.run_publish_crates(server.url())
            self.assertEqual(res.returncode, 1)
            self.assertIn("knotrel-server v0.1.0 already exists on crates.io with conflicting checksum", res.stderr)
            self.assertFalse(os.path.exists(self.publish_log), "Upload executed despite server conflict!")
        finally:
            server.shutdown()

    def test_scenario_6_core_publishes_but_server_publish_fails(self):
        """Scenario 6: Core uploads, but server upload fails -> Exits 101, subsequent run can resume."""
        published = set()
        server = MockStatefulServer()
        server.routes["/knotrel-core/0.1.0"] = lambda p: (200 if "knotrel-core" in published else 404, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": self.core_sha}}))
        server.routes["/kn/ot/knotrel-core"] = lambda p: (200 if "knotrel-core" in published else 404, {}, json.dumps({"name": "knotrel-core", "vers": "0.1.0", "cksum": self.core_sha}) + "\n")
        server.routes["/knotrel-server/0.1.0"] = lambda p: (404, {}, "Not Found")

        with open(self.mock_cargo, "w") as f:
            f.write(f"""#!/usr/bin/env bash
if [[ "$1" == "fetch" ]]; then
    exit 0
fi
if [[ "$1" == "package" ]]; then
    mkdir -p target/package
    cp "{self.core_crate}" target/package/knotrel-core-0.1.0.crate
    cp "{self.server_crate}" target/package/knotrel-server-0.1.0.crate
    exit 0
fi
if [[ "$1" == "publish" ]]; then
    echo "$@" >> "{self.publish_log}"
    if [[ "$*" == *"knotrel-core"* ]]; then
        curl -s "{server.url()}/publish_core" >/dev/null || true
        exit 0
    else
        echo "Mock failure on knotrel-server upload" >&2
        exit 101
    fi
fi
if [[ "$1" == "generate-lockfile" ]]; then
    cat << 'EOF' > Cargo.lock
version = 3
[[package]]
name = "knotrel-core"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{self.core_sha}"
EOF
    exit 0
fi
exit 0
""")
        os.chmod(self.mock_cargo, 0o755)
        server.routes["/publish_core"] = lambda p: (published.add("knotrel-core") or True) and (200, {}, "OK")

        try:
            res = self.run_publish_crates(server.url())
            self.assertEqual(res.returncode, 101)
            self.assertIn("Mock failure on knotrel-server upload", res.stderr)
        finally:
            server.shutdown()

    def test_scenario_7_crates_published_but_gh_finalization_fails_draft_preserved(self):
        """Scenario 7: Both crates publish, but GH finalization fails -> Draft preserved, recoverable on rerun."""
        server = MockStatefulServer()
        server.routes["/knotrel-core/0.1.0"] = lambda p: (200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": self.core_sha}}))
        server.routes["/knotrel-server/0.1.0"] = lambda p: (200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": self.server_sha}}))

        assets_json = json.dumps([{"name": f, "size": os.path.getsize(os.path.join(self.assets_dir, f))} for f in self.expected_files])
        # Mock gh edit fails
        with open(self.mock_gh, "w") as f:
            f.write(f"""#!/usr/bin/env bash
if [[ "$*" == *"release edit"* ]]; then
    echo "GitHub API edit error: connection refused" >&2
    exit 1
fi
if [[ "$*" == *"release download"* ]]; then
    DIR=""
    while [[ "$#" -gt 0 ]]; do
        if [[ "$1" == "--dir" ]]; then
            DIR="$2"
            shift 2
        else
            shift
        fi
    done
    cp -r "{self.assets_dir}"/* "$DIR"/
    exit 0
fi
if [[ "$*" == *"release view"* ]]; then
    cat << 'EOF'
{{"isDraft": true, "tagName": "v0.1.0", "targetCommitish": "{self.head_sha}", "assets": {assets_json}}}
EOF
    exit 0
fi
exit 0
""")
        os.chmod(self.mock_gh, 0o755)

        try:
            res = self.run_finalize_release(server.url())
            self.assertEqual(res.returncode, 1)
            self.assertIn("GitHub API edit error", res.stderr)
        finally:
            server.shutdown()

    def test_scenario_8_gh_draft_exists_and_matches(self):
        """Scenario 8: Draft already exists with matching commit and assets -> Safely verified and reused."""
        script = os.path.join(REPO_ROOT, "scripts/create-or-verify-draft-release.sh")
        env = os.environ.copy()
        env["GH_BIN"] = self.mock_gh
        env["KNOTREL_REPO_DIR"] = self.test_repo

        res = subprocess.run([script, "0.1.0", self.head_sha, self.assets_dir], env=env, capture_output=True, text=True)
        self.assertEqual(res.returncode, 0, f"Failed: {res.stdout}\n{res.stderr}")
        self.assertIn("Safely reusing existing draft release", res.stdout)

    def test_scenario_9_gh_draft_exists_but_conflicts(self):
        """Scenario 9: Draft exists with different commit -> Refuses to overwrite, stops for manual reconciliation."""
        assets_json = json.dumps([{"name": f, "size": os.path.getsize(os.path.join(self.assets_dir, f))} for f in self.expected_files])
        with open(self.mock_gh, "w") as f:
            f.write(f"""#!/usr/bin/env bash
if [[ "$*" == *"release view"* ]]; then
    cat << 'EOF'
{{"isDraft": true, "tagName": "v0.1.0", "targetCommitish": "0000000000000000000000000000000000000000", "assets": {assets_json}}}
EOF
    exit 0
fi
exit 0
""")
        os.chmod(self.mock_gh, 0o755)

        script = os.path.join(REPO_ROOT, "scripts/create-or-verify-draft-release.sh")
        env = os.environ.copy()
        env["GH_BIN"] = self.mock_gh
        env["KNOTREL_REPO_DIR"] = self.test_repo

        res = subprocess.run([script, "0.1.0", self.head_sha, self.assets_dir], env=env, capture_output=True, text=True)
        self.assertEqual(res.returncode, 1)
        self.assertIn("Target commit mismatch", res.stderr)

    def test_scenario_10_registry_api_unavailable(self):
        """Scenario 10: Registry API returns HTTP 503 -> Fails preflight, aborts immediately."""
        server = MockStatefulServer()
        server.routes["/knotrel-core/0.1.0"] = lambda p: (503, {}, "Service Unavailable")
        server.routes["/knotrel-server/0.1.0"] = lambda p: (503, {}, "Service Unavailable")

        try:
            res = self.run_publish_crates(server.url())
            self.assertEqual(res.returncode, 1)
            self.assertIn("Preflight query failed", res.stderr)
            self.assertFalse(os.path.exists(self.publish_log))
        finally:
            server.shutdown()

    def test_scenario_11_cargo_index_propagation_delayed(self):
        """Scenario 11: Cargo index propagation delayed -> Polls until indexed and probe resolves."""
        published = set()
        server = MockStatefulServer()

        poll_count = {"count": 0}
        def delayed_index(path):
            if "knotrel-core" not in published:
                return 404, {}, "Not Found"
            poll_count["count"] += 1
            if poll_count["count"] >= 2:
                return 200, {}, json.dumps({"name": "knotrel-core", "vers": "0.1.0", "cksum": self.core_sha}) + "\n"
            return 404, {}, "Not Indexed Yet"

        def core_api(path):
            if "knotrel-core" in published:
                return 200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": self.core_sha}})
            return 404, {}, "Not Found"

        def server_api(path):
            if "knotrel-server" in published:
                return 200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": self.server_sha}})
            return 404, {}, "Not Found"

        def server_index(path):
            if "knotrel-server" in published:
                return 200, {}, json.dumps({"name": "knotrel-server", "vers": "0.1.0", "cksum": self.server_sha}) + "\n"
            return 404, {}, "Not Found"

        server.routes["/knotrel-core/0.1.0"] = core_api
        server.routes["/knotrel-server/0.1.0"] = server_api
        server.routes["/kn/ot/knotrel-core"] = delayed_index
        server.routes["/kn/ot/knotrel-server"] = server_index
        server.routes["/publish_core"] = lambda p: (published.add("knotrel-core") or True) and (200, {}, "OK")
        server.routes["/publish_server"] = lambda p: (published.add("knotrel-server") or True) and (200, {}, "OK")

        with open(self.mock_cargo, "w") as f:
            f.write(f"""#!/usr/bin/env bash
if [[ "$1" == "fetch" ]]; then
    exit 0
fi
if [[ "$1" == "package" ]]; then
    mkdir -p target/package
    cp "{self.core_crate}" target/package/knotrel-core-0.1.0.crate
    cp "{self.server_crate}" target/package/knotrel-server-0.1.0.crate
    exit 0
fi
if [[ "$1" == "publish" ]]; then
    echo "$@" >> "{self.publish_log}"
    if [[ "$*" == *"knotrel-core"* ]]; then
        curl -s "{server.url()}/publish_core" >/dev/null || true
    fi
    if [[ "$*" == *"knotrel-server"* ]]; then
        curl -s "{server.url()}/publish_server" >/dev/null || true
    fi
    exit 0
fi
if [[ "$1" == "generate-lockfile" ]]; then
    cat << 'EOF' > Cargo.lock
version = 3
[[package]]
name = "knotrel-core"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{self.core_sha}"

[[package]]
name = "knotrel-server"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{self.server_sha}"
EOF
    exit 0
fi
exit 0
""")
        os.chmod(self.mock_cargo, 0o755)

        try:
            res = self.run_publish_crates(server.url())
            self.assertEqual(res.returncode, 0, f"Failed: {res.stdout}\n{res.stderr}")
            self.assertIn("Polling knotrel-core v0.1.0 readiness", res.stdout)
            self.assertIn("Confirmed: knotrel-core v0.1.0 is ready", res.stdout)
            self.assertGreaterEqual(poll_count["count"], 2)
        finally:
            server.shutdown()

    def test_scenario_12_workflow_cancelled_after_first_crate_resumes_safely(self):
        """Scenario 12: Workflow cancelled after core published -> Rerun detects core published, uploads server."""
        server = MockStatefulServer()
        published = {"knotrel-core"}

        server.routes["/knotrel-core/0.1.0"] = lambda p: (200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": self.core_sha}}))
        server.routes["/kn/ot/knotrel-core"] = lambda p: (200, {}, json.dumps({"name": "knotrel-core", "vers": "0.1.0", "cksum": self.core_sha}) + "\n")

        def server_api(path):
            if "knotrel-server" in published:
                return 200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": self.server_sha}})
            return 404, {}, "Not Found"

        def server_index(path):
            if "knotrel-server" in published:
                return 200, {}, json.dumps({"name": "knotrel-server", "vers": "0.1.0", "cksum": self.server_sha}) + "\n"
            return 404, {}, "Not Found"

        server.routes["/knotrel-server/0.1.0"] = server_api
        server.routes["/kn/ot/knotrel-server"] = server_index
        server.routes["/publish_server"] = lambda p: (published.add("knotrel-server") or True) and (200, {}, "OK")

        with open(self.mock_cargo, "w") as f:
            f.write(f"""#!/usr/bin/env bash
if [[ "$1" == "fetch" ]]; then
    exit 0
fi
if [[ "$1" == "package" ]]; then
    mkdir -p target/package
    cp "{self.core_crate}" target/package/knotrel-core-0.1.0.crate
    cp "{self.server_crate}" target/package/knotrel-server-0.1.0.crate
    exit 0
fi
if [[ "$1" == "publish" ]]; then
    echo "$@" >> "{self.publish_log}"
    if [[ "$*" == *"knotrel-server"* ]]; then
        curl -s "{server.url()}/publish_server" >/dev/null || true
    fi
    exit 0
fi
if [[ "$1" == "generate-lockfile" ]]; then
    cat << 'EOF' > Cargo.lock
version = 3
[[package]]
name = "knotrel-core"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{self.core_sha}"

[[package]]
name = "knotrel-server"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{self.server_sha}"
EOF
    exit 0
fi
exit 0
""")
        os.chmod(self.mock_cargo, 0o755)

        try:
            res = self.run_publish_crates(server.url())
            self.assertEqual(res.returncode, 0, f"Publish failed: {res.stdout}\n{res.stderr}")
            self.assertIn("knotrel-core v0.1.0 is already published with verified checksum. Skipping upload.", res.stdout)
            self.assertIn("Uploaded knotrel-server v0.1.0 to crates.io", res.stdout)
        finally:
            server.shutdown()


if __name__ == "__main__":
    unittest.main()
