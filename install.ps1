# voice-mentor installer (Windows) — fetches the latest prebuilt mentor.exe
# from GitHub Releases into $env:LOCALAPPDATA\voice-mentor\bin via the gh CLI.
#   gh api repos/banavasi/voice-mentor/contents/install.ps1 -H "Accept: application/vnd.github.raw" | iex
$ErrorActionPreference = "Stop"

$repo = "banavasi/voice-mentor"
$binDir = if ($env:MENTOR_INSTALL_DIR) { $env:MENTOR_INSTALL_DIR } else { "$env:LOCALAPPDATA\voice-mentor\bin" }
New-Item -ItemType Directory -Force -Path $binDir | Out-Null

if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
    Write-Error "the repo is private: install + auth the gh CLI (https://cli.github.com), or: cargo install --git https://github.com/$repo"
}

Write-Host "→ installing mentor-x86_64-windows.exe (latest release) to $binDir\mentor.exe"
gh release download --repo $repo --pattern "mentor-x86_64-windows.exe" --output "$binDir\mentor.exe" --clobber

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -notlike "*$binDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$binDir", "User")
    Write-Host "✓ added $binDir to your user PATH (restart the terminal)"
}
Write-Host "✓ installed. Run 'mentor' — the first start offers the STT model download,"
Write-Host "and 'mentor doctor' walks the remaining setup."
