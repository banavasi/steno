# voice-mentor installer (Windows) — installs the latest prebuilt mentor.exe
# from GitHub Releases into $env:LOCALAPPDATA\voice-mentor\bin.
#   irm https://raw.githubusercontent.com/banavasi/voice-mentor/main/install.ps1 | iex
$ErrorActionPreference = "Stop"

$repo = "banavasi/voice-mentor"
$binDir = if ($env:MENTOR_INSTALL_DIR) { $env:MENTOR_INSTALL_DIR } else { "$env:LOCALAPPDATA\voice-mentor\bin" }
New-Item -ItemType Directory -Force -Path $binDir | Out-Null

Write-Host "→ downloading mentor-x86_64-windows.exe (latest release)"
Invoke-WebRequest -Uri "https://github.com/$repo/releases/latest/download/mentor-x86_64-windows.exe" `
    -OutFile "$binDir\mentor.exe"

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -notlike "*$binDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$binDir", "User")
    Write-Host "✓ added $binDir to your user PATH (restart the terminal)"
}
Write-Host "✓ installed. Run 'mentor' — the first start offers the STT model download,"
Write-Host "and 'mentor doctor' walks the remaining setup."
