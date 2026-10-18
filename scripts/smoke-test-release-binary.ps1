# smoke-test-release-binary.ps1
# Native Windows PowerShell black-box smoke tests for knotrel-server.exe.
# Tests loopback binding, health/readiness, graph mutations, exact queries,
# error handling, Windows platform process termination, and fresh restart.

param(
    [string]$BinaryPath = ""
)

$ErrorActionPreference = "Stop"

if (-not $BinaryPath) {
    if (Test-Path "target\release\knotrel-server.exe") {
        $BinaryPath = "target\release\knotrel-server.exe"
    } elseif (Test-Path "target\x86_64-pc-windows-msvc\release\knotrel-server.exe") {
        $BinaryPath = "target\x86_64-pc-windows-msvc\release\knotrel-server.exe"
    } elseif (Test-Path "target/release/knotrel-server") {
        $BinaryPath = "target/release/knotrel-server"
    } else {
        Write-Error "Release executable knotrel-server.exe not found. Specify -BinaryPath."
        exit 1
    }
}

$BinaryPath = (Resolve-Path $BinaryPath).Path
Write-Host "=== Windows Release Binary Smoke Test ==="
Write-Host "Target executable: $BinaryPath"

$TempDir = [System.IO.Path]::Combine([System.IO.Path]::GetTempPath(), [System.IO.Path]::GetRandomFileName())
[System.IO.Directory]::CreateDirectory($TempDir) | Out-Null
Write-Host "Temporary directory: $TempDir"

$ActiveProcesses = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()

function Cleanup-SmokeTest {
    Write-Host "Cleaning up test processes and temporary files..."
    foreach ($proc in $ActiveProcesses) {
        if ($proc -and -not $proc.HasExited) {
            try {
                $proc.Kill()
                $null = $proc.WaitForExit(3000)
            } catch {
                # Ignore errors during cleanup termination
            }
        }
        if ($proc) {
            try { $proc.Dispose() } catch {}
        }
    }
    if (Test-Path $TempDir) {
        try {
            Remove-Item -Path $TempDir -Recurse -Force -ErrorAction SilentlyContinue
        } catch {
            # Ignore directory cleanup lock errors
        }
    }
}

try {
    # 1. Non-loopback address rejection test
    Write-Host "--- 1. Testing non-loopback protection boundary ---"
    $rejectOut = Join-Path $TempDir "reject.stdout"
    $rejectErr = Join-Path $TempDir "reject.stderr"

    $env:KNOTREL_ADDR = "0.0.0.0:8080"
    $env:KNOTREL_ALLOW_NON_LOOPBACK = "false"

    $pReject = Start-Process -FilePath $BinaryPath -RedirectStandardOutput $rejectOut -RedirectStandardError $rejectErr -PassThru
    $ActiveProcesses.Add($pReject)

    $exited = $pReject.WaitForExit(5000)
    if (-not $exited) {
        $pReject.Kill()
        $null = $pReject.WaitForExit(2000)
        Write-Error "Server failed to exit when bound to non-loopback address within 5s."
        exit 1
    }
    $rejectOutput = ""
    if (Test-Path $rejectOut) { $rejectOutput += (Get-Content $rejectOut -Raw) }
    if (Test-Path $rejectErr) { $rejectOutput += "`n" + (Get-Content $rejectErr -Raw) }

    if ($pReject.ExitCode -eq 0) {
        Write-Error "Server unexpectedly exited 0 with non-loopback address."
        exit 1
    }
    if ($rejectOutput -notmatch "KNOTREL_ADDR must be a loopback address") {
        Write-Error "Server did not output expected non-loopback error: $rejectOutput"
        exit 1
    }
    Write-Host "Confirmed: Non-loopback address rejected at startup."

    function Spawn-Server([string]$LogName) {
        $stdoutPath = Join-Path $TempDir "$LogName.stdout"
        $stderrPath = Join-Path $TempDir "$LogName.stderr"

        $env:RUST_LOG = "info"
        $env:LOG_FORMAT = "text"
        $env:NO_COLOR = "1"
        $env:KNOTREL_ENGINE = "compact-bfs"
        $env:KNOTREL_ADDR = "127.0.0.1:0"
        $env:KNOTREL_ALLOW_NON_LOOPBACK = "false"
        $env:KNOTREL_MAX_PENDING_JOBS = "32"
        $env:KNOTREL_MAX_NODES = "100000"
        $env:KNOTREL_MAX_EDGES = "500000"
        $env:KNOTREL_SHUTDOWN_TIMEOUT_MS = "10000"

        $proc = Start-Process -FilePath $BinaryPath -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath -PassThru
        $ActiveProcesses.Add($proc)

        # Real deadline stopwatch for dynamic port discovery
        $port = ""
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        $timeoutMs = 5000

        while ($sw.ElapsedMilliseconds -lt $timeoutMs) {
            Start-Sleep -Milliseconds 50
            if ($proc.HasExited) {
                $out = if (Test-Path $stdoutPath) { Get-Content $stdoutPath -Raw } else { "" }
                $err = if (Test-Path $stderrPath) { Get-Content $stderrPath -Raw } else { "" }
                Write-Error "Server process exited prematurely during startup (code: $($proc.ExitCode)). Stdout: $out Stderr: $err"
                exit 1
            }
            $content = ""
            if (Test-Path $stdoutPath) {
                $content += (Get-Content $stdoutPath -Raw -ErrorAction SilentlyContinue)
            }
            if (Test-Path $stderrPath) {
                $content += "`n" + (Get-Content $stderrPath -Raw -ErrorAction SilentlyContinue)
            }
            if ($content -match "address=127\.0\.0\.1:(\d+)") {
                $port = $Matches[1]
                break
            }
        }

        if (-not $port) {
            Write-Error "Server failed to output bound port within 5s."
            exit 1
        }

        # Real deadline stopwatch for /ready endpoint
        $ready = $false
        $swReady = [System.Diagnostics.Stopwatch]::StartNew()
        while ($swReady.ElapsedMilliseconds -lt 5000) {
            if ($proc.HasExited) {
                Write-Error "Server process exited while waiting for /ready (code: $($proc.ExitCode))"
                exit 1
            }
            try {
                $r = Invoke-WebRequest -Uri "http://127.0.0.1:$port/ready" -UseBasicParsing -TimeoutSec 2
                if ($r.StatusCode -eq 200 -and $r.Content -match '"status":"ready"') {
                    $ready = $true
                    break
                }
            } catch {
                # Connection refused until listener is ready
            }
            Start-Sleep -Milliseconds 50
        }

        if (-not $ready) {
            Write-Error "Server at port $port failed to become ready within 5s."
            exit 1
        }

        return @($proc, $port)
    }

    # 2. Spawning instance 1 on dynamic port
    Write-Host "--- 2. Spawning release server instance on loopback ---"
    $server1, $port1 = Spawn-Server "server1.log"
    Write-Host "Instance 1 running on 127.0.0.1:$port1 (PID $($server1.Id))"

    # 3. Health, readiness, and info endpoints
    Write-Host "--- 3. Verifying health, readiness, and info endpoints ---"
    $h = Invoke-WebRequest -Uri "http://127.0.0.1:$port1/health" -UseBasicParsing -TimeoutSec 3
    if ($h.StatusCode -ne 200 -or $h.Content -ne '{"status":"ok"}') {
        Write-Error "Invalid health response: $($h.Content)"
        exit 1
    }
    Write-Host "Health: $($h.Content)"

    $r = Invoke-WebRequest -Uri "http://127.0.0.1:$port1/ready" -UseBasicParsing -TimeoutSec 3
    if ($r.StatusCode -ne 200 -or $r.Content -ne '{"status":"ready"}') {
        Write-Error "Invalid ready response: $($r.Content)"
        exit 1
    }
    Write-Host "Ready: $($r.Content)"

    $info = Invoke-WebRequest -Uri "http://127.0.0.1:$port1/v1/info" -UseBasicParsing -TimeoutSec 3
    if ($info.StatusCode -ne 200 -or $info.Content -notmatch '"engine":"compact-bfs"') {
        Write-Error "Invalid info response: $($info.Content)"
        exit 1
    }
    Write-Host "Info: $($info.Content)"

    # 4. Mutations and queries
    Write-Host "--- 4. Verifying graph mutations and connectivity queries ---"
    function Send-Op([string]$body) {
        return Invoke-WebRequest -Uri "http://127.0.0.1:$port1/v1/operations" -Method Post -Body $body -ContentType "application/json" -UseBasicParsing -TimeoutSec 3
    }

    $l1 = Send-Op '{"op":"link","source":"100","target":"200"}'
    if ($l1.StatusCode -ne 200 -or $l1.Content -ne '{"result":"changed","changed":true}') {
        Write-Error "Failed link 100->200: $($l1.Content)"
        exit 1
    }

    $l2 = Send-Op '{"op":"link","source":"200","target":"300"}'
    if ($l2.StatusCode -ne 200 -or $l2.Content -ne '{"result":"changed","changed":true}') {
        Write-Error "Failed link 200->300: $($l2.Content)"
        exit 1
    }

    $c1 = Send-Op '{"op":"connected","source":"100","target":"300"}'
    if ($c1.StatusCode -ne 200 -or $c1.Content -ne '{"result":"connected","connected":true}') {
        Write-Error "Expected connected 100->300: $($c1.Content)"
        exit 1
    }

    $cut = Send-Op '{"op":"cut","source":"200","target":"300"}'
    if ($cut.StatusCode -ne 200 -or $cut.Content -ne '{"result":"changed","changed":true}') {
        Write-Error "Failed cut 200->300: $($cut.Content)"
        exit 1
    }

    $c2 = Send-Op '{"op":"connected","source":"100","target":"300"}'
    if ($c2.StatusCode -ne 200 -or $c2.Content -ne '{"result":"connected","connected":false}') {
        Write-Error "Expected disconnected 100->300: $($c2.Content)"
        exit 1
    }
    Write-Host "Dynamic connectivity operations verified successfully."

    # 5. Error handling
    Write-Host "--- 5. Verifying documented error handling ---"
    try {
        Send-Op '{"op":"link","source":"100","target":"100"}'
        Write-Error "Self loop unexpectedly succeeded."
        exit 1
    } catch {
        $res = $_.Exception.Response
        if ($res.StatusCode.value__ -ne 422) {
            Write-Error "Expected 422 for self-loop, got: $($res.StatusCode.value__)"
            exit 1
        }
    }

    try {
        Send-Op '{"op":"connected","source":"100","target":"999999"}'
        Write-Error "Unknown node unexpectedly succeeded."
        exit 1
    } catch {
        $res = $_.Exception.Response
        if ($res.StatusCode.value__ -ne 404) {
            Write-Error "Expected 404 for unknown node, got: $($res.StatusCode.value__)"
            exit 1
        }
    }

    try {
        Send-Op '{'
        Write-Error "Malformed JSON unexpectedly succeeded."
        exit 1
    } catch {
        $res = $_.Exception.Response
        if ($res.StatusCode.value__ -ne 400) {
            Write-Error "Expected 400 for malformed JSON, got: $($res.StatusCode.value__)"
            exit 1
        }
    }
    Write-Host "Documented error responses verified."

    # 6. Windows platform process termination
    # Note: On Windows, POSIX signals (SIGTERM/SIGINT) do not exist.
    # TerminateProcess ($proc.Kill()) performs an immediate, platform-appropriate forced termination.
    # Graceful signal-based shutdown is verified on POSIX platforms.
    Write-Host "--- 6. Stopping instance 1 (Windows forced process termination) ---"
    $server1.Kill()
    $exited1 = $server1.WaitForExit(3000)
    if (-not $exited1) {
        Write-Error "Instance 1 failed to terminate within 3000ms after Kill()."
        exit 1
    }
    Write-Host "Instance 1 stopped."

    # 7. Restart with fresh process: verify volatile state is empty
    Write-Host "--- 7. Verifying fresh restart starts with empty graph ---"
    $server2, $port2 = Spawn-Server "server2.log"
    Write-Host "Instance 2 running on 127.0.0.1:$port2 (PID $($server2.Id))"

    $stats = Invoke-WebRequest -Uri "http://127.0.0.1:$port2/v1/stats" -UseBasicParsing -TimeoutSec 3
    if ($stats.StatusCode -ne 200 -or $stats.Content -notmatch '"node_count":0' -or $stats.Content -notmatch '"edge_count":0') {
        Write-Error "Fresh restart does not reflect empty graph: $($stats.Content)"
        exit 1
    }
    Write-Host "Stats on fresh instance: $($stats.Content)"

    try {
        Invoke-WebRequest -Uri "http://127.0.0.1:$port2/v1/operations" -Method Post -Body '{"op":"connected","source":"100","target":"200"}' -ContentType "application/json" -UseBasicParsing -TimeoutSec 3
        Write-Error "Query on prior nodes unexpectedly succeeded on fresh instance."
        exit 1
    } catch {
        $res = $_.Exception.Response
        if ($res.StatusCode.value__ -ne 404) {
            Write-Error "Expected 404 unknown_node on fresh instance, got: $($res.StatusCode.value__)"
            exit 1
        }
    }

    $server2.Kill()
    $exited2 = $server2.WaitForExit(3000)
    if (-not $exited2) {
        Write-Error "Instance 2 failed to terminate within 3000ms after Kill()."
        exit 1
    }
    Write-Host "Instance 2 stopped."

    Write-Host "=== Windows Release Binary Smoke Test Passed Successfully! ==="
} finally {
    Cleanup-SmokeTest
}
