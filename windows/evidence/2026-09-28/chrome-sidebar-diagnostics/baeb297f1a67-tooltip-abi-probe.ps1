# Diagnostic only; root runs this under an owned 10-second run-check Job.
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
Add-Type -Path (Join-Path $PSScriptRoot 'TooltipAbiProbe.cs')
[TooltipAbiProbe]::Run() | ConvertTo-Json -Depth 5
