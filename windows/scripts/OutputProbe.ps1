# SPDX-License-Identifier: GPL-3.0-or-later
# Output only, inside a test-owned PowerShell/ConPTY session.
param([string]$Tag, [int]$Lines, [string]$Gate)
$ErrorActionPreference = 'Stop'
$probeProcess = [Diagnostics.Process]::GetCurrentProcess()
$previousPriority = $probeProcess.PriorityClass
$previousColor = [Console]::ForegroundColor
$previousEncoding = [Console]::OutputEncoding
try {
    $probeProcess.PriorityClass = 'BelowNormal'
    [Console]::OutputEncoding = New-Object Text.UTF8Encoding($false)
    [Console]::WriteLine('READY_' + $Tag)
    $deadline = (Get-Date).AddSeconds(60)
    while (-not [IO.File]::Exists($Gate)) {
        if ((Get-Date) -gt $deadline) { throw 'Output start gate timed out' }
        Start-Sleep -Milliseconds 20
    }
    $korean = [string][char]0xD55C + [char]0xAE00
    $fill = 'x' * 120
    [Console]::ForegroundColor = 'Green'
    for ($i=0; $i -lt $Lines; $i++) { [Console]::WriteLine($Tag + '_' + $i + '_' + $korean + '_' + $fill) }
    [Console]::ForegroundColor = $previousColor
    [Console]::WriteLine('DONE_' + $Tag + '_' + $korean)
} finally {
    [Console]::ForegroundColor = $previousColor
    [Console]::OutputEncoding = $previousEncoding
    $probeProcess.PriorityClass = $previousPriority
    $probeProcess.Dispose()
}
