# SPDX-License-Identifier: MIT
# Builds victory_glyphs.dll (no SDK, no downloads).
#   plugins\glyphs\build.ps1 [-Out <folder>]
param([string]$Out = (Join-Path $PSScriptRoot '..\..\target\glyphs-plugin'))
$ErrorActionPreference = 'Stop'
$Out = [IO.Path]::GetFullPath($Out)
Push-Location $PSScriptRoot
try {
    cargo +stable build --release --locked --target-dir $Out
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
} finally { Pop-Location }
$dll = Join-Path $Out 'release\victory_glyphs.dll'
Write-Host "Built $dll"
Write-Host "Before a run: `$env:VICTORY_GLYPHS_PLUGIN = '$dll'   (see docs\GLYPHS.md)"
