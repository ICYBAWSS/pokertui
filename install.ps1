# Install the latest `poker` release on Windows (PowerShell):
#   irm https://raw.githubusercontent.com/ICYBAWSS/pokertui/main/install.ps1 | iex
$ErrorActionPreference = 'Stop'

$repo = 'ICYBAWSS/pokertui'
$dir  = Join-Path $env:LOCALAPPDATA 'Programs\poker'
$url  = "https://github.com/$repo/releases/latest/download/poker-x86_64-pc-windows-msvc.zip"
$zip  = Join-Path $env:TEMP 'poker.zip'

Write-Host 'Downloading poker for Windows...'
New-Item -ItemType Directory -Force -Path $dir | Out-Null
Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
Expand-Archive -Path $zip -DestinationPath $dir -Force
Remove-Item $zip

# Add to the user's PATH once.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not (($userPath -split ';') -contains $dir)) {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$dir", 'User')
    Write-Host "Added $dir to your PATH."
}

Write-Host "Installed: $dir\poker.exe"
Write-Host 'Open a new terminal (Windows Terminal recommended) and run: poker'
