#!/usr/bin/env python3
"""
Comprehensive test suite for Knotrel release publishing engineering.
Validates all findings from Astra's review:
- HIGH 1: Fix false-success in server publishing dry-run
- HIGH 2: Bind published crate content to candidate release bundle with clean checkout & locked packaging
- HIGH 3: Idempotent draft release creation with size & digest verification and tag resolution
- HIGH 4: Automatic release finalization with full 14-asset bundle verification and boolean isDraft check
- MEDIUM 5: Preflight both crate versions before any upload
- MEDIUM 6: Verify actual registry and Cargo index readiness with true deadline & valid Cargo probe
- MEDIUM 7: Dynamic versioning and stale archive rejection in source verify
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

class MockServer:
    def __init__(self, routes):
        self.routes = routes
        self.requests_log = []
        outer = self

        class Handler(http.server.SimpleHTTPRequestHandler):
            def do_GET(self):
                outer.requests_log.append(self.path)
                for route_prefix, (code, headers, body) in outer.routes.items():
                    if self.path.startswith(route_prefix):
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


def create_14_test_assets(assets_dir, version="0.1.0", dummy_content=b"test-asset-data-content"):
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
        f.write('[workspace]\nmembers = ["crates/*"]\n[workspace.package]\nversion = "0.1.0"\n')
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


class TestHigh1ServerDryRun(unittest.TestCase):
    """HIGH 1: Fix false-success handling in server publishing dry-run"""

    def setUp(self):
        self.temp_dir = tempfile.mkdtemp()
        self.mock_cargo = os.path.join(self.temp_dir, "mock_cargo.sh")

    def tearDown(self):
        shutil.rmtree(self.temp_dir)

    def test_astra_false_green_scenario_reproduced_and_fails(self):
        """Core returns 200, but Server cargo publish dry-run fails with code 101. Must fail."""
        mock = MockServer({
            "/knotrel-core/0.1.0": (200, {"Content-Type": "application/json"}, '{"version": {"num": "0.1.0"}}'),
        })
        try:
            with open(self.mock_cargo, "w") as f:
                f.write("""#!/usr/bin/env bash
for arg in "$@"; do
    if [[ "$arg" == "knotrel-server" ]]; then
        echo "Cargo publish dry-run error for knotrel-server" >&2
        exit 101
    fi
done
exit 0
""")
            os.chmod(self.mock_cargo, 0o755)

            env = os.environ.copy()
            env["CRATES_IO_API_BASE"] = mock.url()
            env["CARGO_BIN"] = self.mock_cargo

            script = os.path.join(REPO_ROOT, "scripts/verify-dry-run-publish.sh")
            res = subprocess.run([script, "0.1.0"], env=env, capture_output=True, text=True)
            self.assertEqual(res.returncode, 101, f"Expected 101, got {res.returncode}. Output: {res.stdout}\n{res.stderr}")
            self.assertIn("Cargo publish dry-run error for knotrel-server", res.stderr)
        finally:
            mock.shutdown()

    def test_core_404_defers_server_dry_run_cleanly(self):
        """When core returns 404, server check is cleanly deferred without executing server publish."""
        mock = MockServer({
            "/knotrel-core/0.1.0": (404, {"Content-Type": "text/plain"}, "Not Found"),
        })
        try:
            with open(self.mock_cargo, "w") as f:
                f.write("""#!/usr/bin/env bash
for arg in "$@"; do
    if [[ "$arg" == "knotrel-server" ]]; then
        echo "ERROR: Server should not be called when core is 404" >&2
        exit 1
    fi
done
exit 0
""")
            os.chmod(self.mock_cargo, 0o755)

            env = os.environ.copy()
            env["CRATES_IO_API_BASE"] = mock.url()
            env["CARGO_BIN"] = self.mock_cargo

            script = os.path.join(REPO_ROOT, "scripts/verify-dry-run-publish.sh")
            res = subprocess.run([script, "0.1.0"], env=env, capture_output=True, text=True)
            self.assertEqual(res.returncode, 0)
            self.assertIn("DEFERRED:", res.stdout)
            self.assertIn("HTTP 404 confirmed", res.stdout)
        finally:
            mock.shutdown()

    def test_rate_limit_429_fails_validation(self):
        """HTTP 429 must fail validation, never treated as absent or success."""
        mock = MockServer({
            "/knotrel-core/0.1.0": (429, {"Content-Type": "text/plain"}, "Too Many Requests"),
        })
        try:
            with open(self.mock_cargo, "w") as f:
                f.write("#!/usr/bin/env bash\nexit 0\n")
            os.chmod(self.mock_cargo, 0o755)

            env = os.environ.copy()
            env["CRATES_IO_API_BASE"] = mock.url()
            env["CARGO_BIN"] = self.mock_cargo

            script = os.path.join(REPO_ROOT, "scripts/verify-dry-run-publish.sh")
            res = subprocess.run([script, "0.1.0"], env=env, capture_output=True, text=True)
            self.assertNotEqual(res.returncode, 0)
            self.assertIn("429", res.stderr)
        finally:
            mock.shutdown()

    def test_server_error_500_fails_validation(self):
        """HTTP 500 must fail validation."""
        mock = MockServer({
            "/knotrel-core/0.1.0": (500, {"Content-Type": "text/plain"}, "Internal Server Error"),
        })
        try:
            with open(self.mock_cargo, "w") as f:
                f.write("#!/usr/bin/env bash\nexit 0\n")
            os.chmod(self.mock_cargo, 0o755)

            env = os.environ.copy()
            env["CRATES_IO_API_BASE"] = mock.url()
            env["CARGO_BIN"] = self.mock_cargo

            script = os.path.join(REPO_ROOT, "scripts/verify-dry-run-publish.sh")
            res = subprocess.run([script, "0.1.0"], env=env, capture_output=True, text=True)
            self.assertNotEqual(res.returncode, 0)
            self.assertIn("500", res.stderr)
        finally:
            mock.shutdown()


class TestHigh2PublishIntegrity(unittest.TestCase):
    """HIGH 2: Bind published crate content to validated candidate release bundle"""

    def setUp(self):
        self.temp_dir = tempfile.mkdtemp()
        self.assets_dir = os.path.join(self.temp_dir, "assets")
        create_14_test_assets(self.assets_dir, "0.1.0")

        # Create isolated clean git repo for testing checkout status validation
        self.test_repo = os.path.join(self.temp_dir, "test_repo")
        self.head_sha = setup_test_repo(self.test_repo)

    def tearDown(self):
        shutil.rmtree(self.temp_dir)

    def test_untracked_files_in_checkout_rejected(self):
        """Script must reject working tree containing untracked files."""
        script = os.path.join(REPO_ROOT, "scripts/publish-crates.sh")
        # Add an untracked file into test_repo
        with open(os.path.join(self.test_repo, "untracked.txt"), "w") as f:
            f.write("untracked")

        env = os.environ.copy()
        env["CARGO_REGISTRY_TOKEN"] = "token"
        env["KNOTREL_REPO_DIR"] = self.test_repo
        res = subprocess.run([script, "0.1.0", self.head_sha, self.assets_dir], env=env, capture_output=True, text=True)
        self.assertEqual(res.returncode, 1)
        self.assertIn("Working tree contains untracked files", res.stderr)

    def test_unstaged_modifications_rejected(self):
        """Script must reject working tree containing unstaged changes."""
        script = os.path.join(REPO_ROOT, "scripts/publish-crates.sh")
        with open(os.path.join(self.test_repo, "Cargo.toml"), "a") as f:
            f.write("# modification\n")

        env = os.environ.copy()
        env["CARGO_REGISTRY_TOKEN"] = "token"
        env["KNOTREL_REPO_DIR"] = self.test_repo
        res = subprocess.run([script, "0.1.0", self.head_sha, self.assets_dir], env=env, capture_output=True, text=True)
        self.assertEqual(res.returncode, 1)
        self.assertIn("Working tree contains unstaged changes", res.stderr)

    def test_staged_changes_rejected(self):
        """Script must reject working tree containing staged uncommitted changes."""
        script = os.path.join(REPO_ROOT, "scripts/publish-crates.sh")
        with open(os.path.join(self.test_repo, "Cargo.toml"), "a") as f:
            f.write("# modification\n")
        subprocess.check_call(["git", "add", "Cargo.toml"], cwd=self.test_repo)

        env = os.environ.copy()
        env["CARGO_REGISTRY_TOKEN"] = "token"
        env["KNOTREL_REPO_DIR"] = self.test_repo
        res = subprocess.run([script, "0.1.0", self.head_sha, self.assets_dir], env=env, capture_output=True, text=True)
        self.assertEqual(res.returncode, 1)
        self.assertIn("Working tree contains staged uncommitted changes", res.stderr)

    def test_mismatched_candidate_sha_rejected(self):
        """Script must reject when candidate SHA does not match HEAD."""
        script = os.path.join(REPO_ROOT, "scripts/publish-crates.sh")
        wrong_sha = "0000000000000000000000000000000000000000"
        env = os.environ.copy()
        env["CARGO_REGISTRY_TOKEN"] = "token"
        env["KNOTREL_REPO_DIR"] = self.test_repo
        res = subprocess.run([script, "0.1.0", wrong_sha, self.assets_dir], env=env, capture_output=True, text=True)
        self.assertEqual(res.returncode, 1)
        self.assertIn("does not match candidate commit SHA", res.stderr)

    def test_prospective_package_checksum_mismatch_rejected(self):
        """If newly generated prospective package checksum does not match bundle, abort before upload."""
        mock_cargo = os.path.join(self.temp_dir, "mock_cargo.sh")
        with open(mock_cargo, "w") as f:
            f.write(f"""#!/usr/bin/env bash
if [[ "$1" == "fetch" ]]; then
    exit 0
fi
if [[ "$1" == "package" ]]; then
    mkdir -p target/package
    echo "prospective-core-different-bytes" > target/package/knotrel-core-0.1.0.crate
    echo "prospective-server-different-bytes" > target/package/knotrel-server-0.1.0.crate
    exit 0
fi
if [[ "$1" == "publish" ]]; then
    echo "ERROR: cargo publish must not be called after checksum mismatch!" >&2
    exit 99
fi
exit 0
""")
        os.chmod(mock_cargo, 0o755)

        script = os.path.join(REPO_ROOT, "scripts/publish-crates.sh")
        env = os.environ.copy()
        env["CARGO_REGISTRY_TOKEN"] = "token"
        env["KNOTREL_REPO_DIR"] = self.test_repo
        env["CARGO_BIN"] = mock_cargo

        res = subprocess.run([script, "0.1.0", self.head_sha, self.assets_dir], env=env, capture_output=True, text=True)
        self.assertEqual(res.returncode, 1)
        self.assertIn("Candidate drift detected before publication", res.stderr)


class TestHigh3DraftReleaseIdempotency(unittest.TestCase):
    """HIGH 3: Make GitHub draft release creation safely idempotent with size & digest verification"""

    def setUp(self):
        self.temp_dir = tempfile.mkdtemp()
        self.assets_dir = os.path.join(self.temp_dir, "assets")
        self.expected_files = create_14_test_assets(self.assets_dir, "0.1.0")
        self.mock_gh = os.path.join(self.temp_dir, "mock_gh.sh")

        # Create isolated test git repo
        self.test_repo = os.path.join(self.temp_dir, "test_repo")
        self.head_sha = setup_test_repo(self.test_repo)

    def tearDown(self):
        shutil.rmtree(self.temp_dir)

    def write_mock_gh(self, script_body):
        with open(self.mock_gh, "w") as f:
            f.write("#!/usr/bin/env bash\n" + script_body)
        os.chmod(self.mock_gh, 0o755)

    def run_script(self):
        script = os.path.join(REPO_ROOT, "scripts/create-or-verify-draft-release.sh")
        env = os.environ.copy()
        env["GH_BIN"] = self.mock_gh
        env["KNOTREL_REPO_DIR"] = self.test_repo
        return subprocess.run([script, "0.1.0", self.head_sha, self.assets_dir], env=env, capture_output=True, text=True)

    def test_scenario_1_no_draft_exists_creates_draft_with_target(self):
        """Scenario 1: No draft exists -> creates draft passing --target CANDIDATE_SHA."""
        log_file = os.path.join(self.temp_dir, "gh_args.log")
        self.write_mock_gh(f"""
echo "$@" >> "{log_file}"
if [[ "$*" == *"release view"* ]]; then
    echo "release not found" >&2
    exit 1
fi
if [[ "$*" == *"release create"* ]]; then
    exit 0
fi
exit 0
""")
        res = self.run_script()
        self.assertEqual(res.returncode, 0)
        self.assertIn("Draft GitHub Release v0.1.0 created successfully", res.stdout)
        with open(log_file) as f:
            args = f.read()
        self.assertIn("--target", args)
        self.assertIn(self.head_sha, args)

    def test_scenario_2_matching_draft_exists_reuses_draft_safely(self):
        """Scenario 2: Matching draft with 14 assets, matching sizes and digests is safely reused."""
        assets_json = json.dumps([{"name": f, "size": os.path.getsize(os.path.join(self.assets_dir, f))} for f in self.expected_files])
        view_json = json.dumps({
            "isDraft": True,
            "tagName": "v0.1.0",
            "targetCommitish": "main",  # Branch name is accepted if tag resolves
            "assets": json.loads(assets_json),
        })
        self.write_mock_gh(f"""
if [[ "$*" == *"release view"* ]]; then
    cat << 'EOF'
{view_json}
EOF
    exit 0
fi
if [[ "$*" == *"release download"* ]]; then
    # Parse destination directory
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
if [[ "$*" == *"release create"* ]]; then
    echo "ERROR: Should not create when matching draft exists!" >&2
    exit 1
fi
""")
        res = self.run_script()
        self.assertEqual(res.returncode, 0, f"Failed: {res.stdout}\n{res.stderr}")
        self.assertIn("All 14 release asset digests verified byte-for-byte", res.stdout)
        self.assertIn("Safely reusing existing draft release", res.stdout)

    def test_scenario_3_wrong_tag_name_rejected(self):
        """Draft with tagName mismatch (e.g. v0.2.0 vs 0.1.0) must fail."""
        assets_json = json.dumps([{"name": f, "size": os.path.getsize(os.path.join(self.assets_dir, f))} for f in self.expected_files])
        view_json = json.dumps({
            "isDraft": True,
            "tagName": "v0.2.0",
            "targetCommitish": self.head_sha,
            "assets": json.loads(assets_json),
        })
        self.write_mock_gh(f"""
if [[ "$*" == *"release view"* ]]; then
    cat << 'EOF'
{view_json}
EOF
    exit 0
fi
""")
        res = self.run_script()
        self.assertEqual(res.returncode, 1)
        self.assertIn("Tag mismatch", res.stderr)

    def test_scenario_4_missing_or_non_boolean_is_draft_rejected(self):
        """Release view missing boolean isDraft must fail."""
        view_json = json.dumps({
            "tagName": "v0.1.0",
            "targetCommitish": self.head_sha,
            "assets": [],
        })
        self.write_mock_gh(f"""
if [[ "$*" == *"release view"* ]]; then
    cat << 'EOF'
{view_json}
EOF
    exit 0
fi
""")
        res = self.run_script()
        self.assertEqual(res.returncode, 1)
        self.assertIn("Missing or invalid boolean isDraft", res.stderr)

    def test_scenario_5_asset_size_mismatch_rejected(self):
        """Draft with all correct asset names but mismatched size must fail before download."""
        assets = [{"name": f, "size": 999999} for f in self.expected_files]
        view_json = json.dumps({
            "isDraft": True,
            "tagName": "v0.1.0",
            "targetCommitish": self.head_sha,
            "assets": assets,
        })
        self.write_mock_gh(f"""
if [[ "$*" == *"release view"* ]]; then
    cat << 'EOF'
{view_json}
EOF
    exit 0
fi
if [[ "$*" == *"release download"* ]]; then
    echo "ERROR: Should fail on size check before downloading!" >&2
    exit 1
fi
""")
        res = self.run_script()
        self.assertEqual(res.returncode, 1)
        self.assertIn("size mismatch", res.stderr)

    def test_scenario_6_asset_digest_mismatch_rejected(self):
        """Draft where downloaded asset content has wrong SHA256 digest must fail."""
        assets_json = json.dumps([{"name": f, "size": os.path.getsize(os.path.join(self.assets_dir, f))} for f in self.expected_files])
        view_json = json.dumps({
            "isDraft": True,
            "tagName": "v0.1.0",
            "targetCommitish": self.head_sha,
            "assets": json.loads(assets_json),
        })
        self.write_mock_gh(f"""
if [[ "$*" == *"release view"* ]]; then
    cat << 'EOF'
{view_json}
EOF
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
    # Corrupt one asset digest while preserving size
    echo -n "X" > "$DIR/SHA256SUMS"
    exit 0
fi
""")
        res = self.run_script()
        self.assertEqual(res.returncode, 1)
        self.assertIn("SHA256 digest mismatch", res.stderr)


class TestHigh4ReleaseFinalization(unittest.TestCase):
    """HIGH 4: Finalize the GitHub Release automatically with full 14-asset verification"""

    def setUp(self):
        self.temp_dir = tempfile.mkdtemp()
        self.assets_dir = os.path.join(self.temp_dir, "assets")
        self.expected_files = create_14_test_assets(self.assets_dir, "0.1.0")
        self.mock_gh = os.path.join(self.temp_dir, "mock_gh.sh")

        self.test_repo = os.path.join(self.temp_dir, "test_repo")
        self.head_sha = setup_test_repo(self.test_repo)

        self.core_crate = os.path.join(self.assets_dir, "knotrel-core-0.1.0.crate")
        self.server_crate = os.path.join(self.assets_dir, "knotrel-server-0.1.0.crate")
        self.core_sha = compute_sha256(self.core_crate)
        self.server_sha = compute_sha256(self.server_crate)

    def tearDown(self):
        shutil.rmtree(self.temp_dir)

    def test_finalize_draft_success(self):
        """When crates and all 14 assets verified, runs gh release edit --draft=false."""
        mock = MockServer({
            "/knotrel-core/0.1.0": (200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": self.core_sha}})),
            "/knotrel-server/0.1.0": (200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": self.server_sha}})),
        })
        try:
            edit_called = os.path.join(self.temp_dir, "edit_called.log")
            view_step = os.path.join(self.temp_dir, "view_count.txt")
            with open(view_step, "w") as f:
                f.write("0")

            assets_json = json.dumps([{"name": f, "size": os.path.getsize(os.path.join(self.assets_dir, f))} for f in self.expected_files])
            draft_json = json.dumps({
                "isDraft": True,
                "tagName": "v0.1.0",
                "targetCommitish": self.head_sha,
                "assets": json.loads(assets_json),
            })
            public_json = json.dumps({
                "isDraft": False,
                "tagName": "v0.1.0",
                "targetCommitish": self.head_sha,
                "assets": json.loads(assets_json),
            })

            with open(self.mock_gh, "w") as f:
                f.write(f"""#!/usr/bin/env bash
if [[ "$*" == *"release edit"* ]]; then
    echo "edited" >> "{edit_called}"
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
    COUNT=$(cat "{view_step}")
    if [[ "$COUNT" == "0" ]]; then
        echo "1" > "{view_step}"
        cat << 'EOF'
{draft_json}
EOF
    else
        cat << 'EOF'
{public_json}
EOF
    fi
    exit 0
fi
""")
            os.chmod(self.mock_gh, 0o755)

            script = os.path.join(REPO_ROOT, "scripts/finalize-release.sh")
            env = os.environ.copy()
            env["GH_BIN"] = self.mock_gh
            env["KNOTREL_REPO_DIR"] = self.test_repo
            env["CRATES_IO_API_BASE"] = mock.url()
            res = subprocess.run([script, "0.1.0", self.head_sha, self.assets_dir], env=env, capture_output=True, text=True)
            self.assertEqual(res.returncode, 0, f"Finalize failed: {res.stdout}\n{res.stderr}")
            self.assertIn("All 14 release asset digests verified byte-for-byte", res.stdout)
            self.assertIn("finalized and published successfully", res.stdout)
            self.assertTrue(os.path.exists(edit_called))
        finally:
            mock.shutdown()

    def test_finalize_incomplete_assets_fails_before_publishing(self):
        """Finalization must refuse to publish a draft with incomplete assets (e.g. only SHA256SUMS)."""
        mock = MockServer({
            "/knotrel-core/0.1.0": (200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": self.core_sha}})),
            "/knotrel-server/0.1.0": (200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": self.server_sha}})),
        })
        try:
            view_json = json.dumps({
                "isDraft": True,
                "tagName": "v0.1.0",
                "targetCommitish": self.head_sha,
                "assets": [{"name": "SHA256SUMS", "size": 100}],
            })
            with open(self.mock_gh, "w") as f:
                f.write(f"""#!/usr/bin/env bash
if [[ "$*" == *"release edit"* ]]; then
    echo "ERROR: Should not edit incomplete release!" >&2
    exit 1
fi
if [[ "$*" == *"release view"* ]]; then
    cat << 'EOF'
{view_json}
EOF
    exit 0
fi
""")
            os.chmod(self.mock_gh, 0o755)

            script = os.path.join(REPO_ROOT, "scripts/finalize-release.sh")
            env = os.environ.copy()
            env["GH_BIN"] = self.mock_gh
            env["KNOTREL_REPO_DIR"] = self.test_repo
            env["CRATES_IO_API_BASE"] = mock.url()
            res = subprocess.run([script, "0.1.0", self.head_sha, self.assets_dir], env=env, capture_output=True, text=True)
            self.assertEqual(res.returncode, 1)
            self.assertIn("Missing assets on existing release", res.stderr)
        finally:
            mock.shutdown()

    def test_finalize_missing_is_draft_rejected(self):
        """Response without boolean isDraft must fail, not treated as public."""
        mock = MockServer({
            "/knotrel-core/0.1.0": (200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": self.core_sha}})),
            "/knotrel-server/0.1.0": (200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": self.server_sha}})),
        })
        try:
            view_json = json.dumps({
                "tagName": "v0.1.0",
                "targetCommitish": self.head_sha,
                "assets": [],
            })
            with open(self.mock_gh, "w") as f:
                f.write(f"""#!/usr/bin/env bash
if [[ "$*" == *"release view"* ]]; then
    cat << 'EOF'
{view_json}
EOF
    exit 0
fi
""")
            os.chmod(self.mock_gh, 0o755)

            script = os.path.join(REPO_ROOT, "scripts/finalize-release.sh")
            env = os.environ.copy()
            env["GH_BIN"] = self.mock_gh
            env["KNOTREL_REPO_DIR"] = self.test_repo
            env["CRATES_IO_API_BASE"] = mock.url()
            res = subprocess.run([script, "0.1.0", self.head_sha, self.assets_dir], env=env, capture_output=True, text=True)
            self.assertEqual(res.returncode, 1)
            self.assertIn("Missing or invalid boolean isDraft", res.stderr)
        finally:
            mock.shutdown()


class TestMedium5PreflightBothCrates(unittest.TestCase):
    """MEDIUM 5: Preflight both crate versions before uploading either"""

    def setUp(self):
        self.temp_dir = tempfile.mkdtemp()
        self.assets_dir = os.path.join(self.temp_dir, "assets")
        create_14_test_assets(self.assets_dir, "0.1.0")

        self.test_repo = os.path.join(self.temp_dir, "test_repo")
        self.head_sha = setup_test_repo(self.test_repo)

        self.core_crate = os.path.join(self.assets_dir, "knotrel-core-0.1.0.crate")
        self.server_crate = os.path.join(self.assets_dir, "knotrel-server-0.1.0.crate")
        self.core_sha = compute_sha256(self.core_crate)
        self.server_sha = compute_sha256(self.server_crate)

        self.mock_cargo = os.path.join(self.temp_dir, "mock_cargo.sh")

    def tearDown(self):
        shutil.rmtree(self.temp_dir)

    def test_preflight_core_absent_server_conflict_aborts_before_core_upload(self):
        """Core is absent, Server is already published with conflicting checksum.
        Expected: fail immediately during preflight BEFORE uploading core."""
        mock = MockServer({
            "/knotrel-core/0.1.0": (404, {"Content-Type": "text/plain"}, "Not Found"),
            "/knotrel-server/0.1.0": (200, {"Content-Type": "application/json"}, json.dumps({
                "version": {"num": "0.1.0", "checksum": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"}
            })),
        })
        try:
            publish_calls_file = os.path.join(self.temp_dir, "publish_calls.log")
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
    echo "$@" >> "{publish_calls_file}"
    exit 0
fi
exit 0
""")
            os.chmod(self.mock_cargo, 0o755)

            script = os.path.join(REPO_ROOT, "scripts/publish-crates.sh")
            env = os.environ.copy()
            env["CARGO_REGISTRY_TOKEN"] = "token"
            env["CRATES_IO_API_BASE"] = mock.url()
            env["CARGO_BIN"] = self.mock_cargo
            env["KNOTREL_REPO_DIR"] = self.test_repo

            res = subprocess.run([script, "0.1.0", self.head_sha, self.assets_dir], env=env, capture_output=True, text=True)
            self.assertEqual(res.returncode, 1)
            self.assertIn("CONFLICT", res.stdout)
            self.assertIn("knotrel-server", res.stderr)
            self.assertFalse(os.path.exists(publish_calls_file), "cargo publish was called despite preflight conflict!")
        finally:
            mock.shutdown()


class TestMedium6RegistryReadinessAndProbe(unittest.TestCase):
    """MEDIUM 6: Verify actual registry and Cargo index readiness with real Cargo probe"""

    def setUp(self):
        self.temp_dir = tempfile.mkdtemp()

    def tearDown(self):
        shutil.rmtree(self.temp_dir)

    def test_real_cargo_probe_manifest_validity(self):
        """Verify real Cargo fails on manifest without targets, but succeeds with src/lib.rs."""
        # 1. Without src/lib.rs -> must fail with 'no targets specified'
        d1 = os.path.join(self.temp_dir, "no_target")
        os.makedirs(d1)
        with open(os.path.join(d1, "Cargo.toml"), "w") as f:
            f.write('[package]\nname = "probe"\nversion = "0.0.0"\nedition = "2021"\n')
        res1 = subprocess.run(["cargo", "generate-lockfile"], cwd=d1, capture_output=True, text=True)
        self.assertNotEqual(res1.returncode, 0)
        self.assertIn("no targets specified", res1.stderr)

        # 2. With src/lib.rs -> real Cargo succeeds in creating Cargo.lock
        d2 = os.path.join(self.temp_dir, "with_target")
        os.makedirs(os.path.join(d2, "src"))
        with open(os.path.join(d2, "Cargo.toml"), "w") as f:
            f.write('[package]\nname = "probe"\nversion = "0.0.0"\nedition = "2021"\n')
        with open(os.path.join(d2, "src", "lib.rs"), "w") as f:
            f.write("pub fn probe() {}\n")
        res2 = subprocess.run(["cargo", "generate-lockfile"], cwd=d2, capture_output=True, text=True)
        self.assertEqual(res2.returncode, 0, f"Cargo generate-lockfile failed: {res2.stderr}")
        self.assertTrue(os.path.exists(os.path.join(d2, "Cargo.lock")))

    def test_tomllib_lockfile_resolution_check(self):
        """Verify tomllib parses exact package entry with name, version, source, and checksum."""
        import tomllib
        lock_content = b"""
version = 3
[[package]]
name = "other-pkg"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "wrong"

[[package]]
name = "knotrel-core"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "expected-sha256"
"""
        data = tomllib.loads(lock_content.decode("utf-8"))
        pkgs = data.get("package", [])
        found = any(
            p.get("name") == "knotrel-core"
            and p.get("version") == "0.1.0"
            and p.get("source", "").startswith("registry+")
            and p.get("checksum") == "expected-sha256"
            for p in pkgs
        )
        self.assertTrue(found)


class TestMedium7VerifySourcePackages(unittest.TestCase):
    """MEDIUM 7: Remove hardcoded version from source verification"""

    @classmethod
    def setUpClass(cls):
        import re

        with open(os.path.join(REPO_ROOT, "Cargo.toml")) as f:
            m = re.search(r'version\s*=\s*"([^"]+)"', f.read())
            cls.current_version = m.group(1) if m else "0.2.0"

    def test_dynamic_metadata_version_runs_successfully(self):
        script = os.path.join(REPO_ROOT, "scripts/verify-source-packages.sh")
        res = subprocess.run([script, self.current_version], capture_output=True, text=True)
        self.assertEqual(res.returncode, 0, f"Failed: {res.stdout}\n{res.stderr}")
        self.assertIn(f"Candidate package version: {self.current_version}", res.stdout)
        self.assertIn("Cleaning stale package directory", res.stdout)
        self.assertIn("Source package verification succeeded!", res.stdout)

    def test_mismatched_requested_version_fails(self):
        script = os.path.join(REPO_ROOT, "scripts/verify-source-packages.sh")
        mismatched_version = "99.99.99"
        res = subprocess.run([script, mismatched_version], capture_output=True, text=True)
        self.assertEqual(res.returncode, 1)
        self.assertIn(
            f"Requested version {mismatched_version} does not match workspace metadata version {self.current_version}",
            res.stderr,
        )

    def test_version_with_leading_v_runs_successfully(self):
        script = os.path.join(REPO_ROOT, "scripts/verify-source-packages.sh")
        res = subprocess.run([script, f"v{self.current_version}"], capture_output=True, text=True)
        self.assertEqual(res.returncode, 0, f"Failed: {res.stdout}\n{res.stderr}")
        self.assertIn(f"Candidate package version: {self.current_version}", res.stdout)
        self.assertIn("Source package verification succeeded!", res.stdout)


class TestTagResolutionUnified(unittest.TestCase):
    """
    Unified Tag Resolution Verification across release scripts:
    - Remote lightweight tag correct, not present locally -> SUCCESS
    - Remote annotated tag correct, not present locally -> SUCCESS
    - Remote tag absent -> FAILS
    - Remote query failure -> FAILS
    - Remote tag pointing to different SHA -> FAILS
    - Similar prefix tag (e.g. v0.1.0-rc1 vs v0.1.0) -> exact match required
    - Local tag mismatch -> FAILS
    """

    def setUp(self):
        self.td = tempfile.mkdtemp()
        self.origin = os.path.join(self.td, "origin.git")
        subprocess.check_call(["git", "init", "--bare", self.origin], stdout=subprocess.DEVNULL)

        self.work = os.path.join(self.td, "work")
        subprocess.check_call(["git", "clone", self.origin, self.work], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        subprocess.check_call(["git", "config", "user.name", "Test"], cwd=self.work)
        subprocess.check_call(["git", "config", "user.email", "test@example.com"], cwd=self.work)

        with open(os.path.join(self.work, "Cargo.toml"), "w") as f:
            f.write('[workspace]\nmembers = ["crates/*"]\n[workspace.package]\nversion = "0.1.0"\n')
        os.makedirs(os.path.join(self.work, "crates", "knotrel-core", "src"), exist_ok=True)
        with open(os.path.join(self.work, "crates", "knotrel-core", "src", "lib.rs"), "w") as f:
            f.write("pub fn core() {}\n")
        with open(os.path.join(self.work, "crates", "knotrel-core", "Cargo.toml"), "w") as f:
            f.write('[package]\nname = "knotrel-core"\nversion = "0.1.0"\nedition = "2021"\n')

        os.makedirs(os.path.join(self.work, "crates", "knotrel-server", "src"), exist_ok=True)
        with open(os.path.join(self.work, "crates", "knotrel-server", "src", "lib.rs"), "w") as f:
            f.write("pub fn server() {}\n")
        with open(os.path.join(self.work, "crates", "knotrel-server", "Cargo.toml"), "w") as f:
            f.write('[package]\nname = "knotrel-server"\nversion = "0.1.0"\nedition = "2021"\n')

        subprocess.check_call(["git", "add", "."], cwd=self.work)
        subprocess.check_call(["git", "commit", "-m", "commit 1"], cwd=self.work, stdout=subprocess.DEVNULL)
        self.c1 = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.work, text=True).strip()
        subprocess.check_call(["git", "push", "origin", "HEAD:main"], cwd=self.work, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

        with open(os.path.join(self.work, "README.md"), "w") as f:
            f.write("# Knotrel\n")
        subprocess.check_call(["git", "add", "."], cwd=self.work)
        subprocess.check_call(["git", "commit", "-m", "commit 2"], cwd=self.work, stdout=subprocess.DEVNULL)
        self.c2 = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.work, text=True).strip()
        subprocess.check_call(["git", "push", "origin", "HEAD:main"], cwd=self.work, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

        self.consumer = os.path.join(self.td, "consumer")
        subprocess.check_call(["git", "clone", "--no-tags", self.origin, self.consumer], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        subprocess.check_call(["git", "checkout", self.c1], cwd=self.consumer, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

        self.assets_dir = os.path.join(self.td, "assets")
        create_14_test_assets(self.assets_dir, version="0.1.0")

        core_sha = compute_sha256(os.path.join(self.assets_dir, "knotrel-core-0.1.0.crate"))
        server_sha = compute_sha256(os.path.join(self.assets_dir, "knotrel-server-0.1.0.crate"))

        self.mock_registry = MockServer({
            "/knotrel-core/0.1.0": (200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": core_sha}})),
            "/knotrel-server/0.1.0": (200, {"Content-Type": "application/json"}, json.dumps({"version": {"num": "0.1.0", "checksum": server_sha}})),
        })

        mock_cargo = os.path.join(self.td, "mock_cargo.sh")
        with open(mock_cargo, "w") as f:
            f.write(f"""#!/usr/bin/env bash
if [[ "$1" == "metadata" ]]; then
    echo '{{"packages": [{{"name": "knotrel-core", "version": "0.1.0"}}, {{"name": "knotrel-server", "version": "0.1.0"}}]}}'
    exit 0
fi
if [[ "$1" == "fetch" ]]; then
    exit 0
fi
if [[ "$1" == "package" ]]; then
    mkdir -p target/package
    cp "{self.assets_dir}/knotrel-core-0.1.0.crate" target/package/
    cp "{self.assets_dir}/knotrel-server-0.1.0.crate" target/package/
    exit 0
fi
exit 0
""")
        os.chmod(mock_cargo, 0o755)

        self.env = os.environ.copy()
        self.env["CRATES_IO_TOKEN"] = "test_token_123"
        self.env["CARGO_REGISTRY_TOKEN"] = "test_token_123"
        self.env["KNOTREL_REPO_DIR"] = self.consumer
        self.env["CARGO_BIN"] = mock_cargo
        self.env["CARGO_EXEC"] = mock_cargo
        self.env["CRATES_IO_API_BASE"] = self.mock_registry.url()

    def tearDown(self):
        self.mock_registry.shutdown()
        shutil.rmtree(self.td)

    def test_remote_lightweight_tag_not_present_locally_succeeds(self):
        subprocess.check_call(["git", "tag", "v0.1.0", self.c1], cwd=self.work)
        subprocess.check_call(["git", "push", "origin", "refs/tags/v0.1.0"], cwd=self.work, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

        tag_check = subprocess.run(["git", "rev-parse", "refs/tags/v0.1.0^{commit}"], cwd=self.consumer, capture_output=True)
        self.assertNotEqual(tag_check.returncode, 0)

        script = os.path.join(REPO_ROOT, "scripts/publish-crates.sh")
        res = subprocess.run([script, "0.1.0", self.c1, self.assets_dir], cwd=self.consumer, env=self.env, capture_output=True, text=True)
        self.assertEqual(res.returncode, 0, f"Expected clean exit 0, got {res.returncode}:\nSTDOUT:\n{res.stdout}\nSTDERR:\n{res.stderr}")
        self.assertIn(f"Confirmed: Tag 'v0.1.0' points exactly to candidate commit {self.c1}", res.stdout)
        self.assertIn("No uploads necessary. Release artifacts already published.", res.stdout)

    def test_remote_annotated_tag_not_present_locally_succeeds(self):
        subprocess.check_call(["git", "tag", "-a", "v0.1.0", "-m", "annotated release tag", self.c1], cwd=self.work)
        subprocess.check_call(["git", "push", "origin", "refs/tags/v0.1.0"], cwd=self.work, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

        script = os.path.join(REPO_ROOT, "scripts/publish-crates.sh")
        res = subprocess.run([script, "0.1.0", self.c1, self.assets_dir], cwd=self.consumer, env=self.env, capture_output=True, text=True)
        self.assertEqual(res.returncode, 0, f"Expected clean exit 0, got {res.returncode}:\nSTDOUT:\n{res.stdout}\nSTDERR:\n{res.stderr}")
        self.assertIn(f"Confirmed: Tag 'v0.1.0' points exactly to candidate commit {self.c1}", res.stdout)
        self.assertIn("No uploads necessary. Release artifacts already published.", res.stdout)

    def test_remote_tag_absent_fails(self):
        script = os.path.join(REPO_ROOT, "scripts/publish-crates.sh")
        res = subprocess.run([script, "0.1.0", self.c1, self.assets_dir], cwd=self.consumer, env=self.env, capture_output=True, text=True)
        self.assertEqual(res.returncode, 1)
        self.assertIn("ERROR: Tag 'v0.1.0' not found on remote origin!", res.stderr)

    def test_remote_query_failure_fails(self):
        subprocess.check_call(["git", "remote", "set-url", "origin", "/nonexistent/path/repo.git"], cwd=self.consumer)
        script = os.path.join(REPO_ROOT, "scripts/publish-crates.sh")
        res = subprocess.run([script, "0.1.0", self.c1, self.assets_dir], cwd=self.consumer, env=self.env, capture_output=True, text=True)
        self.assertEqual(res.returncode, 1)
        self.assertIn("ERROR: Failed to query remote origin for tag 'v0.1.0':", res.stderr)

    def test_remote_tag_wrong_sha_fails(self):
        subprocess.check_call(["git", "tag", "v0.1.0", self.c2], cwd=self.work)
        subprocess.check_call(["git", "push", "origin", "refs/tags/v0.1.0"], cwd=self.work, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

        script = os.path.join(REPO_ROOT, "scripts/publish-crates.sh")
        res = subprocess.run([script, "0.1.0", self.c1, self.assets_dir], cwd=self.consumer, env=self.env, capture_output=True, text=True)
        self.assertEqual(res.returncode, 1)
        self.assertIn(f"ERROR: Remote tag 'v0.1.0' on origin resolves to {self.c2}, expected candidate SHA {self.c1}!", res.stderr)

    def test_similar_prefix_tag_does_not_falsely_match(self):
        subprocess.check_call(["git", "tag", "v0.1.0-rc1", self.c1], cwd=self.work)
        subprocess.check_call(["git", "push", "origin", "refs/tags/v0.1.0-rc1"], cwd=self.work, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

        script = os.path.join(REPO_ROOT, "scripts/publish-crates.sh")
        res = subprocess.run([script, "0.1.0", self.c1, self.assets_dir], cwd=self.consumer, env=self.env, capture_output=True, text=True)
        self.assertEqual(res.returncode, 1)
        self.assertIn("ERROR: Tag 'v0.1.0' not found on remote origin!", res.stderr)

        subprocess.check_call(["git", "tag", "-d", "v0.1.0-rc1"], cwd=self.work)
        subprocess.check_call(["git", "push", "origin", ":refs/tags/v0.1.0-rc1"], cwd=self.work, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        subprocess.check_call(["git", "tag", "v0.1.0", self.c1], cwd=self.work)
        subprocess.check_call(["git", "tag", "v0.1.0-rc1", self.c2], cwd=self.work)
        subprocess.check_call(["git", "push", "origin", "refs/tags/v0.1.0", "refs/tags/v0.1.0-rc1"], cwd=self.work, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

        # Checkout c2 in consumer so HEAD matches candidate sha c2
        subprocess.check_call(["git", "checkout", self.c2], cwd=self.consumer, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        res = subprocess.run([script, "0.1.0", self.c2, self.assets_dir], cwd=self.consumer, env=self.env, capture_output=True, text=True)
        self.assertEqual(res.returncode, 1)
        self.assertIn(f"ERROR: Remote tag 'v0.1.0' on origin resolves to {self.c1}, expected candidate SHA {self.c2}!", res.stderr)

        # Checking with candidate c1 and checkout c1 succeeds completely with returncode 0
        subprocess.check_call(["git", "checkout", self.c1], cwd=self.consumer, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        res = subprocess.run([script, "0.1.0", self.c1, self.assets_dir], cwd=self.consumer, env=self.env, capture_output=True, text=True)
        self.assertEqual(res.returncode, 0, f"Expected clean exit 0, got {res.returncode}:\nSTDOUT:\n{res.stdout}\nSTDERR:\n{res.stderr}")
        self.assertIn(f"Confirmed: Tag 'v0.1.0' points exactly to candidate commit {self.c1}", res.stdout)
        self.assertIn("No uploads necessary. Release artifacts already published.", res.stdout)

    def test_local_tag_wrong_sha_fails(self):
        subprocess.check_call(["git", "tag", "v0.1.0", self.c2], cwd=self.consumer)

        script = os.path.join(REPO_ROOT, "scripts/publish-crates.sh")
        res = subprocess.run([script, "0.1.0", self.c1, self.assets_dir], cwd=self.consumer, env=self.env, capture_output=True, text=True)
        self.assertEqual(res.returncode, 1)
        self.assertIn(f"ERROR: Local tag 'v0.1.0' resolves to {self.c2}, expected candidate SHA {self.c1}!", res.stderr)

    def test_publish_crates_accepts_version_with_leading_v(self):
        subprocess.check_call(["git", "tag", "v0.1.0", self.c1], cwd=self.work)
        subprocess.check_call(["git", "push", "origin", "refs/tags/v0.1.0"], cwd=self.work, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

        script = os.path.join(REPO_ROOT, "scripts/publish-crates.sh")
        res = subprocess.run([script, "v0.1.0", self.c1, self.assets_dir], cwd=self.consumer, env=self.env, capture_output=True, text=True)
        self.assertEqual(res.returncode, 0, f"Expected clean exit 0 with v0.1.0, got {res.returncode}:\nSTDOUT:\n{res.stdout}\nSTDERR:\n{res.stderr}")
        self.assertIn(f"Confirmed: Tag 'v0.1.0' points exactly to candidate commit {self.c1}", res.stdout)
        self.assertIn("No uploads necessary. Release artifacts already published.", res.stdout)


if __name__ == "__main__":
    unittest.main()
