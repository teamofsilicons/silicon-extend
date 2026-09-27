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
    @{
        commit = $env:GITHUB_SHA; runner_os = $env:RUNNER_OS; runner_arch = $env:RUNNER_ARCH
        image = $env:ImageOS; image_version = $env:ImageVersion
        interactive = [Environment]::UserInteractive
        session_id = [Diagnostics.Process]::GetCurrentProcess().SessionId
        limits = "Disposable runner desktop; not physical Windows, sleep/lock/UAC, or multi-monitor verification."
    } | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $Out "runner.json")
    # Reuse the release build's dependencies; tests use private directories and loopback fakes.
    cargo test --locked --release -p extend-agent --lib --test fake_service 2>&1 |
        Tee-Object -FilePath (Join-Path $Out "agent-tests.log")
    if ($LASTEXITCODE -ne 0) { throw "Native extend-agent unit/integration tests failed." }
    if (-not [Environment]::UserInteractive) { throw "No interactive desktop; native GUI proof is unavailable." }
    $env:EXTEND_WINDOWS_NATIVE = "disposable-runner"
    $env:EXTEND_WINDOWS_EVIDENCE = $Out
    cargo test --locked --release -p extend-agent --test windows_native owned_ -- --ignored --test-threads=1 --nocapture 2>&1 |
        Tee-Object -FilePath (Join-Path $Out "native-tests.log")
    if ($LASTEXITCODE -ne 0) { throw "Owned Windows window/process verification failed." }
} finally {
    $env:EXTEND_WINDOWS_NATIVE = $PreviousOptIn
    $env:EXTEND_WINDOWS_EVIDENCE = $PreviousEvidence
    Pop-Location
}
