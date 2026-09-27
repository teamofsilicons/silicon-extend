# Native checks use only processes/windows created by the tests, on disposable GitHub runners.
[CmdletBinding()]
param([string]$EvidenceDir = "target/windows-native-verification")
$ErrorActionPreference = "Stop"
if ($env:GITHUB_ACTIONS -ne "true" -or -not $IsWindows) {
    throw "This lane is restricted to a disposable Windows GitHub runner."
}
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..\..\..")).Path
$Out = [IO.Path]::GetFullPath((Join-Path $Root $EvidenceDir))
if (-not $Out.StartsWith($Root + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw "EvidenceDir must be inside this checkout."
}
New-Item -ItemType Directory -Force $Out | Out-Null
$PreviousOptIn = $env:EXTEND_WINDOWS_NATIVE
$PreviousEvidence = $env:EXTEND_WINDOWS_EVIDENCE
Push-Location $Root
try {
    $Interactive = [Environment]::UserInteractive
    @{
        commit = $env:GITHUB_SHA; runner_os = $env:RUNNER_OS; runner_arch = $env:RUNNER_ARCH
        image = $env:ImageOS; image_version = $env:ImageVersion
        interactive = $Interactive
        session_id = [Diagnostics.Process]::GetCurrentProcess().SessionId
        limits = "Disposable runner desktop; not physical Windows, sleep/lock/UAC, or multi-monitor verification."
    } | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $Out "runner.json")
    # Reuse the release build's dependencies; tests use private directories and loopback fakes.
    cargo test --locked --release -p extend-agent --lib --test fake_service --no-fail-fast 2>&1 |
        Tee-Object -FilePath (Join-Path $Out "agent-tests.log")
    $AgentExit = $LASTEXITCODE
    # Gather both results even when a portable unit assertion fails. Failure in either lane
    # still fails the job; one failure must not hide independent native evidence.
    $env:EXTEND_WINDOWS_NATIVE = "disposable-runner"
    $env:EXTEND_WINDOWS_EVIDENCE = $Out
    cargo test --locked --release -p extend-agent --test windows_native owned_ -- --ignored --test-threads=1 --nocapture 2>&1 |
        Tee-Object -FilePath (Join-Path $Out "native-tests.log")
    $NativeExit = $LASTEXITCODE
    @{
        agent_exit = $AgentExit; native_exit = $NativeExit; interactive = $Interactive
        passed = ($AgentExit -eq 0 -and $NativeExit -eq 0 -and $Interactive)
    } | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $Out "result.json")
    if ($AgentExit -ne 0 -or $NativeExit -ne 0 -or -not $Interactive) {
        throw "Windows verification failed (agent tests exit $AgentExit, native fixtures exit $NativeExit, interactive desktop $Interactive)."
    }
} finally {
    $env:EXTEND_WINDOWS_NATIVE = $PreviousOptIn
    $env:EXTEND_WINDOWS_EVIDENCE = $PreviousEvidence
    Pop-Location
}
