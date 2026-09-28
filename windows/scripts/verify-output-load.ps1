# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden debug host only. No desktop keyboard/mouse input or activation.
param([string]$BuildDirectory = "$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug", [int]$Lines = 10000)
if (-not $env:FLOWMUX_TEST_ARTIFACT_ROOT) { throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.' }
$ErrorActionPreference = 'Stop'
$OutputEncoding = [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$BuildDirectory = (Resolve-Path $BuildDirectory).Path
$cli = Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor = (& $cli doctor | ConvertFrom-Json)
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) { throw 'A working debug build is required; no window was launched.' }
$directory = Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT 'output-load'
New-Item -ItemType Directory -Path $directory -Force | Out-Null
$directory = (Resolve-Path $directory).Path
$probe = Join-Path $PSScriptRoot 'OutputProbe.ps1'
Add-Type -Path (Join-Path $PSScriptRoot 'NativeInput.cs')
$previous = $env:FLOWMUX_TEST_BACKGROUND
try {
    $env:FLOWMUX_TEST_BACKGROUND = '1'
    $process = Start-Process -FilePath (Join-Path $BuildDirectory 'flowmux.exe') -ArgumentList '--temporary' -PassThru
} finally { $env:FLOWMUX_TEST_BACKGROUND = $previous }
$process.PriorityClass = 'BelowNormal'
$script:pipeName = $null
$evidence = [ordered]@{ pid=$process.Id; started=(Get-Date).ToString('o'); mode='background'; stages=@() }
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
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    throw 'Terminal readiness timed out'
}
function Leaves($Node) {
    if ($Node.kind -eq 'leaf') { return $Node }
    Leaves $Node.first
    Leaves $Node.second
}
function Wait-Marker([string]$Surface, [string]$Marker) {
    $deadline = (Get-Date).AddSeconds(90)
    do {
        $screen = Invoke-Flowmux @('read-screen', '--surface', $Surface)
        if ($screen.text.Contains('NativeCommandFailed') -or $screen.text.Contains('PSSecurityException')) {
            throw "Output writer could not run in surface $Surface"
        }
        # read-screen preserves physical rows; a narrow pane can wrap this marker.
        if ($screen.text.Replace("`n", '').Contains($Marker)) { return $screen }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    throw "Output marker missing from surface $Surface : $Marker"
}
function Process-Sample {
    $rows = @(Get-CimInstance Win32_Process -Property ProcessId,ParentProcessId)
    $ids = New-Object 'System.Collections.Generic.HashSet[int]'
    $ids.Add($process.Id) | Out-Null
    do {
        $before = $ids.Count
        foreach ($row in $rows) { if ($ids.Contains([int]$row.ParentProcessId)) { $ids.Add([int]$row.ProcessId) | Out-Null } }
    } while ($ids.Count -gt $before)
    $private = [long]0; $working = [long]0; $names = @{}
    foreach ($id in $ids) {
        $p = Get-Process -Id $id -ErrorAction SilentlyContinue
        if ($p) { $private += $p.PrivateMemorySize64; $working += $p.WorkingSet64; $names[$p.ProcessName]++ }
    }
    return @{ processes=$names; privateBytes=$private; workingSetBytes=$working; note='Point-in-time owned process tree; not a peak-memory measurement.' }
}
try {
    $discovery = Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($process.Id).json"
    $deadline = (Get-Date).AddSeconds(20)
    while (-not (Test-Path $discovery)) {
        if ($process.HasExited -or (Get-Date) -gt $deadline) { throw 'Host did not start' }
        Start-Sleep -Milliseconds 100
    }
    $script:pipeName = (Get-Content -Raw $discovery | ConvertFrom-Json).pipe
    $tree = Wait-Ready
    $window = [IntPtr]::new([long]$tree.window_handle)
    foreach ($count in @(1,4,16)) {
        while ($tree.surfaces.Count -lt $count) {
            $largest = $tree.surfaces | Sort-Object -Property @{Expression={$_.cols * $_.rows};Descending=$true} | Select-Object -First 1
            Invoke-Flowmux @('focus-tab', $largest.id) | Out-Null
            $direction = $(if ($largest.cols -gt $largest.rows * 2) { 'vertical' } else { 'horizontal' })
            Invoke-Flowmux @('split', $direction) | Out-Null
            $tree = Wait-Ready
        }
        $leaves = @(Leaves $tree.workspaces[0].root)
        $gate = Join-Path $directory ([Guid]::NewGuid().ToString() + '.gate')
        $jobs = @()
        for ($i=0; $i -lt $leaves.Count; $i++) {
            $tag = 's' + $count + 'p' + $i
            $pane = $leaves[$i].id; $surface = $leaves[$i].content.active
            Invoke-Flowmux @('send-keys', $pane, "& '$probe' '$tag' $Lines '$gate'") | Out-Null
            Invoke-Flowmux @('send-key', 'Enter', '--pane', $pane) | Out-Null
            $jobs += @{ surface=$surface; marker=('DONE_' + $tag + '_' + [char]0xD55C + [char]0xAE00) }
            Wait-Marker $surface ('READY_' + $tag) | Out-Null
        }
        $before = Process-Sample
        $watch = [Diagnostics.Stopwatch]::StartNew()
        [IO.File]::WriteAllText($gate, 'start')
        foreach ($job in $jobs) { Wait-Marker $job.surface $job.marker | Out-Null }
        $watch.Stop()
        $evidence.stages += @{ panes=$count; linesPerPane=$Lines; milliseconds=$watch.ElapsedMilliseconds; passed=$true; before=$before; after=(Process-Sample) }
        if ([NativeInput]::IsWindowVisible($window) -or [NativeInput]::GetForegroundWindow() -eq $window) { throw 'Background host exposed a window' }
        Write-Host "Background output completed for $count panes"
        $tree = Invoke-Flowmux @('tree')
    }
    # Keep a separate tab inactive throughout its output, then read it directly.
    $identity = Invoke-Flowmux @('identify')
    $hidden = $identity.surface; $gate = Join-Path $directory ([Guid]::NewGuid().ToString() + '.gate')
    Invoke-Flowmux @('send-keys', $identity.pane, "& '$probe' 'hidden' $Lines '$gate'") | Out-Null
    Invoke-Flowmux @('send-key', 'Enter', '--pane', $identity.pane) | Out-Null
    Wait-Marker $hidden 'READY_hidden' | Out-Null
    Invoke-Flowmux @('new-tab') | Out-Null
    Wait-Ready | Out-Null
    $active = Invoke-Flowmux @('identify')
    [IO.File]::WriteAllText($gate, 'start')
    $screen = Wait-Marker $hidden ('DONE_hidden_' + [char]0xD55C + [char]0xAE00)
    if ((Invoke-Flowmux @('identify')).surface -ne $active.surface) { throw 'Reading hidden output changed focus' }
    $evidence.hiddenTab = @{ passed=$true; surface=$hidden; sequence=$screen.sequence; focusUnchanged=$true }
    $evidence.status = 'passed_background_output_subset'
} catch {
    $evidence.status='failed'; $evidence.error=$_.Exception.Message
    throw
} finally {
    if ($script:pipeName) { try { Invoke-Flowmux @('quit') | Out-Null } catch {} }
    if (-not $process.HasExited -and -not $process.WaitForExit(10000)) { Stop-Process -Id $process.Id -Force }
    $evidence.finished=(Get-Date).ToString('o')
    if($evidence.status -eq 'failed'){$evidence | ConvertTo-Json -Depth 15 | Set-Content -Encoding UTF8 (Join-Path $directory 'output-load.json')}
}
[ordered]@{status=$evidence.status;checks=$evidence.stages.Count}|ConvertTo-Json -Compress
