# Builds the Windows zip for Silicon Bridge. Run on Windows with the Rust toolchain (MSVC) installed:
#
#   powershell -ExecutionPolicy Bypass -File apps\desktop\windows\build-zip.ps1
#
# Layout:
#   Silicon Bridge\bridge-agent.exe     the app (tray icon; `bridge-agent.exe run --headless` for servers)
#   Silicon Bridge\README.txt
#
# Windows uses Bridge's own driver (UI Automation, SendInput, GDI capture), so no Node or
# agent-device ships with it. The window uses WebView2, which Windows 10 and 11 already have.
# Not signed, not published.
$ErrorActionPreference = "Stop"
$Root = Resolve-Path (Join-Path $PSScriptRoot "..\..\..")
$Version = (Select-String -Path (Join-Path $Root "Cargo.toml") -Pattern '^version = "(.*)"').Matches[0].Groups[1].Value
$Arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "x64" }
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Root "target" }
$Out = Join-Path $TargetDir "desktop\windows"
$Stage = Join-Path $Out "Silicon Bridge"

cargo build --release -p bridge-agent --manifest-path (Join-Path $Root "Cargo.toml")
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

Remove-Item -Recurse -Force $Stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $Stage | Out-Null
Copy-Item (Join-Path $TargetDir "release\bridge-agent.exe") $Stage
@"
Silicon Bridge $Version for Windows

  bridge-agent.exe run                 start it (tray icon; shows the pairing code)
  bridge-agent.exe run --headless      no tray icon; status in the console
  bridge-agent.exe install-autostart   start at login (a Run key under HKCU)
  bridge-agent.exe status              what it is doing
  bridge-agent.exe probe               what this computer can do right now

A Silicon can't use this computer while it is locked, and admin prompts always need you.
"@ | Set-Content -Encoding UTF8 (Join-Path $Stage "README.txt")

$Zip = Join-Path $Out "Silicon-Bridge-$Version-windows-$Arch.zip"
Remove-Item -Force $Zip -ErrorAction SilentlyContinue
Compress-Archive -Path $Stage -DestinationPath $Zip
Write-Host "Built $Zip"
& (Join-Path $Stage "bridge-agent.exe") probe
