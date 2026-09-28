# Native checks use only processes/windows created by the tests, on disposable GitHub runners.
[CmdletBinding()]
param(
    [string]$EvidenceDir = "target/windows-native-verification",
    [ValidateSet("full", "terminal-only")]
    [string]$VerificationMode = "full"
)
$ErrorActionPreference = "Stop"
if ($env:GITHUB_ACTIONS -ne "true" -or -not $IsWindows) {
    throw "This lane is restricted to a disposable Windows GitHub runner."
}
if ($VerificationMode -eq "terminal-only" -and (
    $env:GITHUB_EVENT_NAME -ne "workflow_dispatch" -or $env:RUNNER_ARCH -ne "ARM64")) {
    throw "Terminal-only verification is restricted to an explicitly selected manual ARM64 runner."
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
    $GuiRequired = $VerificationMode -eq "full"
    $GuiSkipReason = if ($GuiRequired) { $null } else {
        "GUI verification was explicitly omitted: this hosted ARM64 image has a Windows privacy initial-setup overlay that blocks the owned fixture. No privacy choices or desktop ownership guards were changed."
    }
    @{
        commit = $env:GITHUB_SHA; runner_os = $env:RUNNER_OS; runner_arch = $env:RUNNER_ARCH
        image = $env:ImageOS; image_version = $env:ImageVersion
        interactive = $Interactive
        verification_mode = $VerificationMode
        gui_required = $GuiRequired
        gui_skip_reason = $GuiSkipReason
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
    $NativeArguments = @("test", "--locked", "--release", "-p", "extend-agent", "--test", "windows_native")
    if ($GuiRequired) {
        $NativeArguments += @("owned_", "--")
    } else {
        $NativeArguments += @("owned_terminal_children_end_with_their_session_timeout_and_cancel", "--", "--exact")
        Write-Warning $GuiSkipReason
    }
    $NativeArguments += @("--ignored", "--test-threads=1", "--nocapture")
    cargo @NativeArguments 2>&1 |
        Tee-Object -FilePath (Join-Path $Out "native-tests.log")
    $NativeExit = $LASTEXITCODE
    $ExpectedNativeTests = if ($GuiRequired) { 2 } else { 1 }
    $NativeCountMatched = Select-String -LiteralPath (Join-Path $Out "native-tests.log") `
        -Pattern "^test result: ok\. $ExpectedNativeTests passed; 0 failed;" -Quiet
    @{
        agent_exit = $AgentExit; native_exit = $NativeExit; interactive = $Interactive
        verification_mode = $VerificationMode
        gui_required = $GuiRequired
        gui_verified = ($GuiRequired -and $NativeExit -eq 0 -and $NativeCountMatched -and $Interactive)
        gui_skip_reason = $GuiSkipReason
        expected_native_tests = $ExpectedNativeTests
        native_count_confirmed = $NativeCountMatched
        passed = ($AgentExit -eq 0 -and $NativeExit -eq 0 -and $NativeCountMatched -and (-not $GuiRequired -or $Interactive))
    } | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $Out "result.json")
    if ($AgentExit -ne 0 -or $NativeExit -ne 0 -or -not $NativeCountMatched -or ($GuiRequired -and -not $Interactive)) {
        throw "Windows verification failed (agent tests exit $AgentExit, native fixtures exit $NativeExit, expected fixture count confirmed $NativeCountMatched, interactive desktop $Interactive)."
    }
} finally {
    $env:EXTEND_WINDOWS_NATIVE = $PreviousOptIn
    $env:EXTEND_WINDOWS_EVIDENCE = $PreviousEvidence
    Pop-Location
}
