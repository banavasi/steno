# steno installer (Windows) — installs the latest prebuilt steno.exe
# from GitHub Releases into $env:LOCALAPPDATA\steno\bin.
#   irm https://raw.githubusercontent.com/banavasi/steno/main/install.ps1 | iex
$ErrorActionPreference = "Stop"

$repo = "banavasi/steno"
$binDir = if ($env:STENO_INSTALL_DIR) { $env:STENO_INSTALL_DIR } else { "$env:LOCALAPPDATA\steno\bin" }
New-Item -ItemType Directory -Force -Path $binDir | Out-Null

Write-Host "→ downloading steno-x86_64-windows.exe (latest release)"
Invoke-WebRequest -Uri "https://github.com/$repo/releases/latest/download/steno-x86_64-windows.exe" `
    -OutFile "$binDir\steno.exe"

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -notlike "*$binDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$binDir", "User")
    Write-Host "✓ added $binDir to your user PATH (restart the terminal)"
}
Write-Host "✓ installed. Run 'steno' — the first start offers the STT model download,"
Write-Host "and 'steno doctor' walks the remaining setup."
