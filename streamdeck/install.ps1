# Builds the plugin and installs it into Stream Deck (restarts Stream Deck).
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
$plugin = Join-Path $PSScriptRoot 'com.xenside.audioduck.sdPlugin'
cargo build --release --manifest-path (Join-Path $root 'Cargo.toml')
New-Item -ItemType Directory -Force (Join-Path $plugin 'bin') | Out-Null
$sd = Get-Process StreamDeck -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty Path
Get-Process StreamDeck, audioduck-streamdeck -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 1
Copy-Item (Join-Path $root 'target\release\audioduck-streamdeck.exe') (Join-Path $plugin 'bin') -Force
$dest = Join-Path $env:APPDATA 'Elgato\StreamDeck\Plugins\com.xenside.audioduck.sdPlugin'
if (Test-Path $dest) { Remove-Item $dest -Recurse -Force }
Copy-Item $plugin $dest -Recurse
if (-not $sd) { $sd = Join-Path $env:ProgramFiles 'Elgato\StreamDeck\StreamDeck.exe' }
Start-Process $sd
