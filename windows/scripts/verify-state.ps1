# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden debug hosts and a unique state directory only. No desktop input.
param([string]$BuildDirectory = "$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference = 'Stop'
$OutputEncoding = [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$BuildDirectory = (Resolve-Path $BuildDirectory).Path
$cli = Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor = (& $cli doctor | ConvertFrom-Json)
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) { throw 'A working debug build is required; no window was launched.' }
Add-Type -Path (Join-Path $PSScriptRoot 'NativeInput.cs')
$directory = Join-Path $(if ($env:FLOWMUX_TEST_ARTIFACT_ROOT) { $env:FLOWMUX_TEST_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\dist\evidence' }) ('state-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $directory -Force | Out-Null
$directory = (Resolve-Path $directory).Path
$stateDirectory = Join-Path $directory 'state'
$hosts = New-Object 'System.Collections.Generic.List[System.Diagnostics.Process]'
$evidence = [ordered]@{ started=(Get-Date).ToString('o'); mode='background'; checks=@(); hosts=@() }
function Start-Owned([string[]]$LaunchArgs = @()) {
    $priorBackground = $env:FLOWMUX_TEST_BACKGROUND
    $priorState = $env:FLOWMUX_TEST_STATE_DIR
    try {
        $env:FLOWMUX_TEST_BACKGROUND = '1'
        $env:FLOWMUX_TEST_STATE_DIR = $stateDirectory
        $options = @{ FilePath=(Join-Path $BuildDirectory 'flowmux.exe'); PassThru=$true }
        if ($LaunchArgs.Count -gt 0) { $options.ArgumentList = $LaunchArgs }
        $hostProcess = Start-Process @options
        $hosts.Add($hostProcess)
        return $hostProcess
    } finally {
        $env:FLOWMUX_TEST_BACKGROUND = $priorBackground
        $env:FLOWMUX_TEST_STATE_DIR = $priorState
    }
}
function Invoke-Flowmux([string[]]$Arguments) {
    $output = & $cli --pipe $script:pipeName --json @Arguments
    if ($LASTEXITCODE -ne 0) { throw "flowmuxctl failed: $Arguments" }
    return (($output -join "`n") | ConvertFrom-Json)
}
function Connect-Owned($Process) {
    $discovery = Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($Process.Id).json"
    $deadline = (Get-Date).AddSeconds(30)
    while (-not (Test-Path $discovery)) {
        if ($Process.HasExited -or (Get-Date) -gt $deadline) { throw 'Test host did not start' }
        Start-Sleep -Milliseconds 100
    }
    $script:pipeName = (Get-Content -Raw $discovery | ConvertFrom-Json).pipe
    do {
        $tree = Invoke-Flowmux @('tree')
        if (@($tree.surfaces | Where-Object { -not $_.ready }).Count -eq 0) {
            $window = [IntPtr]([long]$tree.window_handle)
            if (-not $tree.background_testing -or [NativeInput]::IsWindowVisible($window) -or [NativeInput]::GetForegroundWindow() -eq $window) {
                throw 'Background host became visible or foreground'
            }
            return $tree
        }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    throw 'Terminal restore/readiness timed out'
}
function Stop-Owned($Process) {
    Invoke-Flowmux @('quit') | Out-Null
    if (-not $Process.WaitForExit(10000) -or $Process.ExitCode -ne 0) { throw 'Host did not close cleanly' }
}
function Write-Marker([string]$Surface, [string]$Marker) {
    Invoke-Flowmux @('focus-tab', $Surface) | Out-Null
    $pane = (Invoke-Flowmux @('identify')).pane
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes("Write-Host '$Marker' -ForegroundColor Green"))
    Invoke-Flowmux @('send-keys', $pane, "Invoke-Expression ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$encoded')))") | Out-Null
    Invoke-Flowmux @('send-key', 'Enter', '--pane', $pane) | Out-Null
    $deadline = (Get-Date).AddSeconds(12)
    do {
        $screen = Invoke-Flowmux @('read-screen', '--surface', $Surface)
        if ($screen.text.Replace("`n", '').Contains($Marker)) { return }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    throw "Marker was not rendered: $Marker"
}
function Load-State([string]$Path) { return ([IO.File]::ReadAllText($Path, [Text.Encoding]::UTF8) | ConvertFrom-Json) }
function Save-Json([string]$Path, $Value) { [IO.File]::WriteAllText($Path, ($Value | ConvertTo-Json -Depth 60), (New-Object Text.UTF8Encoding($false))) }
try {
    $first = Start-Owned @('--cwd', ('"' + $directory + '"'))
    Connect-Owned $first | Out-Null
    Invoke-Flowmux @('new-tab') | Out-Null
    Invoke-Flowmux @('split', 'vertical') | Out-Null
    Invoke-Flowmux @('new-workspace', '--cwd', $directory) | Out-Null
    $before = Connect-Owned $first
    $markers = @{}
    $korean = ([string][char]0xD55C) + [char]0xAE00 + [char]0xAE30 + [char]0xB85D
    foreach ($surface in $before.surfaces) {
        $markers[$surface.id] = "HISTORY-$korean-$($surface.id.Substring(0, 8))"
        Write-Marker $surface.id $markers[$surface.id]
    }
    $before = Invoke-Flowmux @('tree')
    $saved = Invoke-Flowmux @('save-state')
    $path = $saved.path
    $windowId = $saved.window
    $snapshot = Load-State $path
    foreach ($surface in $before.surfaces) {
        $data = $snapshot.screens.($surface.id).data
        if (-not $data.Contains($markers[$surface.id]) -or -not $data.Contains(([string][char]27) + '[92m')) { throw 'Styled Korean history was not saved' }
    }
    $evidence.checks += 'Four terminal histories, including inactive tabs/workspaces, saved with Korean and green SGR'
    $hash = (Get-FileHash $path -Algorithm SHA256).Hash
    $locked = Start-Owned @('--restore-window', $windowId)
    if (-not $locked.WaitForExit(10000) -or $locked.ExitCode -eq 0) { throw 'A second process claimed a live window state' }
    $second = Start-Owned
    $otherTree = Connect-Owned $second
    if ($otherTree.state.window -eq $windowId) { throw 'Default launch reused a live window identity' }
    Stop-Owned $second
    if ((Get-FileHash $path -Algorithm SHA256).Hash -ne $hash) { throw 'Second window overwrote first state' }
    $script:pipeName = (Get-Content -Raw (Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($first.Id).json") | ConvertFrom-Json).pipe
    Stop-Owned $first
    $evidence.checks += 'Live-window lease rejected explicit duplicate restore; another window saved independently'

    # A saved display that looks like a command must never execute on restore.
    $snapshot = Load-State $path
    $sentinel = Join-Path $directory 'must-not-execute.txt'
    $one = $before.surfaces[0].id
    $snapshot.screens.$one.data += "`r`nSet-Content -LiteralPath '$sentinel' -Value executed`r`n" + [char]27 + '[5n'
    Save-Json $path $snapshot
    $restored = Start-Owned @('--restore-window', $windowId)
    $after = Connect-Owned $restored
    if (($before.workspaces | ConvertTo-Json -Depth 60 -Compress) -ne ($after.workspaces | ConvertTo-Json -Depth 60 -Compress) -or $before.active_workspace -ne $after.active_workspace) {
        throw 'Restored layout, focus, titles or working directories differ'
    }
    foreach ($surface in $before.surfaces) {
        $new = $after.surfaces | Where-Object { $_.id -eq $surface.id }
        if (-not $new -or $new.pid -eq $surface.pid) { throw 'Restored surface did not create a fresh shell' }
    }
    $savedAgain = Invoke-Flowmux @('save-state')
    $roundtrip = Load-State $savedAgain.path
    foreach ($surface in $before.surfaces) {
        $data = $roundtrip.screens.($surface.id).data
        if (-not $data.Contains($markers[$surface.id]) -or -not $data.Contains(([string][char]27) + '[92m')) { throw 'Styled history was lost after fresh ConPTY startup' }
    }
    if (Test-Path $sentinel) { throw 'Historical command was executed' }
    $evidence.checks += 'Clean restart kept workspace/pane/surface identity, layout, focus, cwd, Korean and SGR with fresh process IDs'
    $evidence.checks += 'Historical command text and VT status query replayed to display only; sentinel was not created'
    $evidence.hosts += @{ first=$first.Id; restored=$restored.Id; originalShells=@($before.surfaces.pid); restoredShells=@($after.surfaces.pid); window=$windowId }

    # Simulate an atomic replacement failure using a handle that denies deletion.
    $hash = (Get-FileHash $path -Algorithm SHA256).Hash
    $guard = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        $previousErrorPreference = $ErrorActionPreference
        try {
            $ErrorActionPreference = 'Continue'
            $raw = & $cli --pipe $script:pipeName --json quit 2>&1
            $failedExit = $LASTEXITCODE
        } finally { $ErrorActionPreference = $previousErrorPreference }
        if ($failedExit -eq 0 -or $restored.HasExited) { throw 'Failed save closed the window or reported success' }
    } finally { $guard.Dispose() }
    if ((Get-FileHash $path -Algorithm SHA256).Hash -ne $hash) { throw 'Failed replacement damaged the previous checkpoint' }
    $evidence.checks += 'Denied atomic replace retained old checkpoint and kept window/processes alive after quit failed'
    Invoke-Flowmux @('save-state') | Out-Null
    $restored.Kill(); $restored.WaitForExit()
    $crashRestored = Start-Owned @('--restore-window', $windowId)
    $crashTree = Connect-Owned $crashRestored
    if ($crashTree.surfaces.Count -ne 4) { throw 'Crash recovery lost terminals' }
    $automaticMarker = "AUTO-$korean-$([guid]::NewGuid().ToString().Substring(0, 8))"
    Write-Marker $one $automaticMarker
    $deadline = (Get-Date).AddSeconds(40)
    do {
        $automatic = Load-State $path
        if ($automatic.screens.$one.data.Contains($automaticMarker)) { break }
        if ((Get-Date) -gt $deadline) { throw 'Periodic checkpoint did not persist new output' }
        Start-Sleep -Milliseconds 250
    } while ($true)
    $evidence.checks += 'Thirty-second background checkpoint persisted later Korean output without an explicit save'
    Stop-Owned $crashRestored
    $evidence.checks += 'Forced termination released the OS lease; explicit restart recovered the last checkpoint'
    $automaticRestore = Start-Owned
    $automaticTree = Connect-Owned $automaticRestore
    if ($automaticTree.state.window -ne $windowId) { throw 'Default launch did not select the latest closed window' }
    $hash = (Get-FileHash $path -Algorithm SHA256).Hash
    $guard = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        Invoke-Flowmux @('quit', '--discard-state') | Out-Null
        if (-not $automaticRestore.WaitForExit(10000)) { throw 'Explicit close without saving did not close' }
    } finally { $guard.Dispose() }
    if ((Get-FileHash $path -Algorithm SHA256).Hash -ne $hash) { throw 'Discard-state modified the completed checkpoint' }
    $temporary = Start-Owned @('--temporary')
    $temporaryTree = Connect-Owned $temporary
    if ($temporaryTree.state.window -or $temporaryTree.surfaces.Count -ne 1) { throw 'Temporary host restored persistent state' }
    Stop-Owned $temporary
    if ((Get-FileHash $path -Algorithm SHA256).Hash -ne $hash) { throw 'Temporary host modified persistent state' }
    $evidence.checks += 'Default launch restored latest closed window; explicit discard and temporary mode preserved its checkpoint'
    $evidence.finished = (Get-Date).ToString('o')
} catch {
    $evidence.status='failed';$evidence.error=$_.Exception.Message
    Save-Json (Join-Path $directory 'native-state-background.json') $evidence
    throw
} finally {
    foreach ($owned in $hosts) { if (-not $owned.HasExited) { $owned.Kill(); $owned.WaitForExit() } }
}
[ordered]@{status='passed';checks=$evidence.checks.Count}|ConvertTo-Json -Compress
