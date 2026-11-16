# Install the latest `poker` release on Windows (PowerShell):
#   irm https://raw.githubusercontent.com/ICYBAWSS/pokertui/main/install.ps1 | iex
# Play once without installing (everything is deleted when you quit):
#   & ([scriptblock]::Create((irm https://raw.githubusercontent.com/ICYBAWSS/pokertui/main/install.ps1))) -Temp
param([switch]$Temp)
$ErrorActionPreference = 'Stop'

$repo = 'ICYBAWSS/pokertui'
$dir  = Join-Path $env:LOCALAPPDATA 'Programs\poker'
$url  = "https://github.com/$repo/releases/latest/download/poker-x86_64-pc-windows-msvc.zip"
$zip  = Join-Path $env:TEMP 'poker.zip'

if ($Temp) {
    $tmp = Join-Path $env:TEMP ("poker-" + [guid]::NewGuid())
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null
    try {
        Write-Host 'Downloading poker for Windows...'
        Invoke-WebRequest -Uri $url -OutFile (Join-Path $tmp 'poker.zip') -UseBasicParsing
        Expand-Archive -Path (Join-Path $tmp 'poker.zip') -DestinationPath $tmp -Force
        $env:POKER_HOME = Join-Path $tmp 'home'
        & (Join-Path $tmp 'poker.exe')
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
    return
}

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
