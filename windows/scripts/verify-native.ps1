# SPDX-License-Identifier: GPL-3.0-or-later
# Exercises only the newly launched native window. Never addresses the WSL instance.
param([string]$BuildDirectory = "$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug", [int]$Cycles = 0, [switch]$Interactive)
$ErrorActionPreference = 'Stop'
$OutputEncoding = [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$BuildDirectory = (Resolve-Path $BuildDirectory).Path
$cli = Join-Path $BuildDirectory 'flowmuxctl.exe'
$gui = Join-Path $BuildDirectory 'flowmux.exe'
$doctor = (& $cli doctor | ConvertFrom-Json)
if ($LASTEXITCODE -ne 0 -or (-not $Interactive -and -not $doctor.background_testing)) {
    throw 'Background verification requires a working debug build; no window was launched.'
}
if ($Cycles -gt 0) { Add-Type -Path (Join-Path $PSScriptRoot 'HandleProbe.cs') }
Add-Type -Path (Join-Path $PSScriptRoot 'NativeInput.cs')
$evidenceDirectory = $(if ($env:FLOWMUX_TEST_ARTIFACT_ROOT) { $env:FLOWMUX_TEST_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\dist\evidence' })
New-Item -ItemType Directory -Path $evidenceDirectory -Force | Out-Null
$previousBackground = $env:FLOWMUX_TEST_BACKGROUND
try {
    $env:FLOWMUX_TEST_BACKGROUND = $(if ($Interactive) { $null } else { '1' })
    $process = Start-Process -FilePath $gui -ArgumentList '--temporary' -PassThru
} finally { $env:FLOWMUX_TEST_BACKGROUND = $previousBackground }
$discovery = Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($process.Id).json"
$pipeName = $null
$evidence = [ordered]@{ pid = $process.Id; started = (Get-Date).ToString('o'); checks = @(); ime = 'not tested'; mode = $(if ($Interactive) { 'interactive' } else { 'background' }) }
function Invoke-Flowmux([string[]]$Arguments) {
    $output = & $cli --pipe $script:pipeName --json @Arguments
    if ($LASTEXITCODE -ne 0) { throw "flowmuxctl failed: $Arguments" }
    return (($output -join "`n") | ConvertFrom-Json)
}
function Wait-Ready {
    $deadline = (Get-Date).AddSeconds(30)
    do {
        $tree = Invoke-Flowmux @('tree')
        if (@($tree.surfaces | Where-Object { -not $_.ready }).Count -eq 0) { return $tree }
        Start-Sleep -Milliseconds 200
    } while ((Get-Date) -lt $deadline)
    throw 'Terminal did not become ready'
}
try {
    $deadline = (Get-Date).AddSeconds(20)
    while (-not (Test-Path $discovery)) {
        if ($process.HasExited) { throw "Windows host exited: $($process.ExitCode)" }
        if ((Get-Date) -gt $deadline) { throw 'Window discovery timed out' }
        Start-Sleep -Milliseconds 100
    }
    $pipeName = (Get-Content -Raw $discovery | ConvertFrom-Json).pipe
    $evidence.pipe = $pipeName
    $tree = Wait-Ready
    $window = [IntPtr]::new([long]$tree.window_handle)
    if (-not $Interactive -and (-not $tree.background_testing -or [NativeInput]::IsWindowVisible($window) -or [NativeInput]::GetForegroundWindow() -eq $window)) { throw 'Background host unexpectedly exposed a window' }
    $identity = Invoke-Flowmux @('identify')
    $pane = $identity.pane
    $originalSurface = $identity.surface
    $originalPid = $tree.surfaces[0].pid
    $evidence.checks += @{ name = 'native_conpty_start'; passed = ($originalPid -gt 0); pid = $originalPid }
    # Build Korean text from code points to keep this script compatible with PowerShell 5.1 source encoding.
    $korean = [string][char]0xd55c + [char]0xae00 + [char]0xc785 + [char]0xb825
    Invoke-Flowmux @('send-keys', $pane, "Write-Output ('FLOWMUX_NATIVE_' + '$korean')") | Out-Null
    Invoke-Flowmux @('send-key', 'Enter', '--pane', $pane) | Out-Null
    $deadline = (Get-Date).AddSeconds(15)
    do {
        $screen = Invoke-Flowmux @('read-screen', $pane)
        if ($screen.text.Contains("FLOWMUX_NATIVE_$korean")) { break }
        Start-Sleep -Milliseconds 200
    } while ((Get-Date) -lt $deadline)
    if (-not $screen.text.Contains("FLOWMUX_NATIVE_$korean")) { throw 'Korean UTF-8 round trip failed' }
    $evidence.checks += @{ name = 'korean_utf8_roundtrip_via_cli_not_ime'; passed = $true; sequence = $screen.sequence }
    Invoke-Flowmux @('split', 'vertical') | Out-Null
    Invoke-Flowmux @('split', 'horizontal') | Out-Null
    Invoke-Flowmux @('split', 'vertical') | Out-Null
    $tree = Wait-Ready
    if ($tree.surfaces.Count -ne 4) { throw 'Expected four terminal surfaces' }
    if (($tree.surfaces | Where-Object { $_.id -eq $originalSurface }).pid -ne $originalPid) { throw 'Original process was restarted during splits' }
    $evidence.checks += @{ name = 'four_splits_keep_original_process'; passed = $true }
    Invoke-Flowmux @('new-tab') | Out-Null
    $tree = Wait-Ready
    $newSurface = (Invoke-Flowmux @('identify')).surface
    $newPid = ($tree.surfaces | Where-Object { $_.id -eq $newSurface }).pid
    Invoke-Flowmux @('close-tab', $newSurface) | Out-Null
    $deadline = (Get-Date).AddSeconds(5)
    while (Get-Process -Id $newPid -ErrorAction SilentlyContinue) {
        if ((Get-Date) -gt $deadline) { throw 'Closed tab process is still alive' }
        Start-Sleep -Milliseconds 100
    }
    $evidence.checks += @{ name = 'tab_close_terminates_shell'; passed = $true }
    $evidence.cycleSamples = @()
    for ($cycle = 0; $cycle -lt $Cycles; $cycle++) {
        Invoke-Flowmux @('new-tab') | Out-Null
        $tree = Wait-Ready
        $surface = (Invoke-Flowmux @('identify')).surface
        $shellPid = ($tree.surfaces | Where-Object { $_.id -eq $surface }).pid
        if (-not $shellPid) { throw "Cycle $cycle has no shell PID" }
        Invoke-Flowmux @('close-tab', $surface) | Out-Null
        $deadline = (Get-Date).AddSeconds(5)
        while (Get-Process -Id $shellPid -ErrorAction SilentlyContinue) {
            if ((Get-Date) -gt $deadline) { throw "Cycle $cycle left its shell alive" }
            Start-Sleep -Milliseconds 25
        }
        if (($cycle + 1) % 25 -eq 0) {
            $process.Refresh()
            $evidence.cycleSamples += @{ cycle = ($cycle + 1); hostHandles = $process.HandleCount; hostPrivateBytes = $process.PrivateMemorySize64; handleTypes = [HandleProbe]::Types($process.Id) }
            Write-Host "Completed $($cycle + 1) native tab lifecycle cycles"
        }
    }
    if ($Cycles -gt 0) { $evidence.checks += @{ name = 'repeated_tab_create_close'; cycles = $Cycles; passed = $true } }
    if ($evidence.cycleSamples.Count -ge 2) {
        $first = $evidence.cycleSamples[0]; $last = $evidence.cycleSamples[-1]
        $processDelta = $last.handleTypes['Process'] - $first.handleTypes['Process']
        $totalDelta = $last.hostHandles - $first.hostHandles
        $bounded = ($processDelta -le 4 -and $totalDelta -le 24)
        $evidence.checks += @{ name = 'handles_stable_after_warmup'; passed = $bounded; processHandleDelta = $processDelta; totalHandleDelta = $totalDelta }
        if (-not $bounded) { throw 'Native host handle count grew during repeated tab close' }
    }
    $evidence.tree = Invoke-Flowmux @('tree')
    if (-not $Interactive -and ([NativeInput]::IsWindowVisible($window) -or [NativeInput]::GetForegroundWindow() -eq $window)) { throw 'Background host took desktop focus' }
    $evidence.status = 'passed_smoke_only'
} catch {
    $evidence.status = 'failed'
    $evidence.error = $_.Exception.Message
    throw
} finally {
    $evidence.finished = (Get-Date).ToString('o')
    if ($pipeName) {
        try { Invoke-Flowmux @('quit') | Out-Null }
        catch { $evidence.status = 'failed'; $evidence.cleanupError = $_.Exception.Message }
    }
    if (-not $process.HasExited) {
        if (-not $process.WaitForExit(10000)) {
            $evidence.status = 'failed'; $evidence.cleanupError = 'Native host did not exit within 10 seconds'
            Stop-Process -Id $process.Id -Force
        }
    }
    if ($evidence.status -eq 'failed') { if ($screen) { $screen.text | Set-Content -Encoding UTF8 (Join-Path $evidenceDirectory 'screen.txt') }; $evidence | ConvertTo-Json -Depth 30 | Set-Content -Encoding UTF8 (Join-Path $evidenceDirectory 'native-smoke.json') }
}
[ordered]@{status=$evidence.status;checks=$evidence.checks.Count}|ConvertTo-Json -Compress
if ($evidence.status -ne 'passed_smoke_only') { throw 'Native smoke checks failed; see native-smoke.json.' }
