# SPDX-License-Identifier: GPL-3.0-or-later
# Defaults to a hidden debug host. -Interactive explicitly enables desktop input.
param([string]$BuildDirectory = "$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug", [switch]$Interactive)
if (-not $env:FLOWMUX_TEST_ARTIFACT_ROOT) { throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.' }
$ErrorActionPreference = 'Stop'
$OutputEncoding = [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$BuildDirectory = (Resolve-Path $BuildDirectory).Path
$directory = Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT 'tab-move'
New-Item -ItemType Directory -Path $directory -Force | Out-Null
$directory = (Resolve-Path $directory).Path
$probe = Join-Path $directory 'input-probe.exe'
if (Test-Path $probe) { Remove-Item $probe }
Add-Type -Path (Join-Path $PSScriptRoot 'InputProbe.cs') -OutputAssembly $probe -OutputType ConsoleApplication
Add-Type -Path (Join-Path $PSScriptRoot 'NativeInput.cs')
Add-Type -AssemblyName System.Drawing
$cli = Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor = (& $cli doctor | ConvertFrom-Json)
if ($LASTEXITCODE -ne 0) { throw 'Native runtime check failed' }
if (-not $Interactive -and -not $doctor.background_testing) {
    throw 'Background verification requires a debug build; no window was launched.'
}
$trace = Join-Path $directory 'before-conpty.jsonl'
foreach ($path in @($trace, ($trace + '.keys.jsonl'))) { if (Test-Path $path) { Remove-Item $path } }
$previousTrace = $env:FLOWMUX_TEST_INPUT_TRACE
$previousBackground = $env:FLOWMUX_TEST_BACKGROUND
try {
    $env:FLOWMUX_TEST_INPUT_TRACE = $trace
    $env:FLOWMUX_TEST_BACKGROUND = $(if ($Interactive) { $null } else { '1' })
    $process = Start-Process -FilePath (Join-Path $BuildDirectory 'flowmux.exe') -ArgumentList '--temporary' -PassThru
} finally {
    $env:FLOWMUX_TEST_INPUT_TRACE = $previousTrace
    $env:FLOWMUX_TEST_BACKGROUND = $previousBackground
}
$script:pipeName = $null
$evidence = [ordered]@{ pid = $process.Id; started = (Get-Date).ToString('o'); mode = $(if ($Interactive) { 'interactive' } else { 'background' }); checks = @() }
function Invoke-Flowmux([string[]]$Arguments) {
    $output = & $cli --pipe $script:pipeName --json @Arguments
    if ($LASTEXITCODE -ne 0) { throw "flowmuxctl failed: $Arguments" }
    return (($output -join "`n") | ConvertFrom-Json)
}
function Wait-Ready {
    $deadline = (Get-Date).AddSeconds(20)
    do {
        $tree = Invoke-Flowmux @('tree')
        if (@($tree.surfaces | Where-Object { -not $_.ready }).Count -eq 0) { return $tree }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    throw 'Terminal readiness timed out'
}
function Send-Line([string]$Pane, [string]$Line) {
    Invoke-Flowmux @('send-keys', $Pane, $Line) | Out-Null
    Invoke-Flowmux @('send-key', 'Enter', '--pane', $Pane) | Out-Null
}
function Wait-Screen([string]$Pane, [string]$Marker) {
    $deadline = (Get-Date).AddSeconds(12)
    do {
        $screen = Invoke-Flowmux @('read-screen', $Pane)
        if ($screen.text.Contains($Marker)) { return $screen }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    $screen.text | Set-Content -Encoding UTF8 (Join-Path $directory 'failed-screen.txt')
    throw "Screen marker missing: $Marker (pane $Pane)"
}
function Bytes([string]$Path) {
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
    try { $memory = New-Object IO.MemoryStream; $stream.CopyTo($memory); return ,$memory.ToArray() }
    finally { $stream.Dispose() }
}
function Press([int]$Key) {
    [NativeInput]::Key($script:window, $Key, $false)
    Start-Sleep -Milliseconds 80
}
function Save-Window([string]$Name, [IntPtr]$Handle = $script:window) {
    $rect = New-Object NativeInput+Rect
    [NativeInput]::GetWindowRect($Handle, [ref]$rect) | Out-Null
    $bitmap = New-Object Drawing.Bitmap(($rect.Right-$rect.Left), ($rect.Bottom-$rect.Top))
    $graphics = [Drawing.Graphics]::FromImage($bitmap); $dc = $graphics.GetHdc()
    try { [NativeInput]::PrintWindow($Handle, $dc, 2) | Out-Null }
    finally { $graphics.ReleaseHdc($dc); $graphics.Dispose() }
    $bitmap.Save((Join-Path $directory $Name)); $bitmap.Dispose()
}
try {
    $discovery = Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($process.Id).json"
    $deadline = (Get-Date).AddSeconds(20)
    while (-not (Test-Path $discovery)) {
        if ($process.HasExited -or (Get-Date) -gt $deadline) { throw 'Host did not start' }
        Start-Sleep -Milliseconds 100
    }
    $script:pipeName = (Get-Content -Raw $discovery | ConvertFrom-Json).pipe
    $initial = Wait-Ready
    $script:window = [IntPtr]::new([long]$initial.window_handle)
    if (-not $Interactive -and (-not $initial.background_testing -or [NativeInput]::IsWindowVisible($script:window) -or [NativeInput]::GetForegroundWindow() -eq $script:window)) { throw 'Background host unexpectedly exposed a window' }
    $original = Invoke-Flowmux @('identify')
    $originalPid = $initial.surfaces[0].pid
    Send-Line $original.pane "Write-Output ('FLOWMUX_' + 'MOVE_HISTORY')"
    Wait-Screen $original.pane 'FLOWMUX_MOVE_HISTORY' | Out-Null
    Invoke-Flowmux @('split', 'vertical') | Out-Null
    Wait-Ready | Out-Null
    $destination = Invoke-Flowmux @('identify')
    Invoke-Flowmux @('move-tab', $original.surface, '--to-pane', $destination.pane) | Out-Null
    $tree = Invoke-Flowmux @('tree')
    $surface = $tree.surfaces | Where-Object { $_.id -eq $original.surface }
    if ($surface.pid -ne $originalPid -or -not $surface.running) { throw 'Moving tab restarted its shell' }
    Wait-Screen $destination.pane 'FLOWMUX_MOVE_HISTORY' | Out-Null
    $evidence.checks += @{ name = 'pane_move_preserves_pid_surface_and_history'; passed = $true; pid = $originalPid }
    # The child still inherits its original pane ID. Its CLI must resolve the stable
    # surface even when another tab becomes focused while the command is delayed.
    $context = Join-Path $directory ('context-' + [Guid]::NewGuid().ToString() + '.json')
    $line = 'Start-Sleep -Milliseconds 800; $i = & $env:FLOWMUX_BUNDLED_CLI_PATH --json identify | ConvertFrom-Json; $s = & $env:FLOWMUX_BUNDLED_CLI_PATH --json read-screen | ConvertFrom-Json; @{{ identity=$i; screen=$s; cwd=$PWD.Path; inheritedPane=$env:FLOWMUX_PANE_ID }} | ConvertTo-Json -Depth 5 | Set-Content -Encoding UTF8 ''{0}''' -f $context
    Send-Line $destination.pane $line
    Invoke-Flowmux @('focus-tab', $destination.surface) | Out-Null
    $deadline = (Get-Date).AddSeconds(12)
    while (-not (Test-Path $context)) {
        if ((Get-Date) -gt $deadline) { throw 'CLI from moved hidden tab did not finish' }
        Start-Sleep -Milliseconds 100
    }
    $actual = Get-Content -Raw $context | ConvertFrom-Json
    if ($actual.identity.surface -ne $original.surface -or $actual.identity.pane -ne $destination.pane -or $actual.screen.surface -ne $original.surface -or $actual.inheritedPane -ne $original.pane -or $actual.cwd -ne $initial.workspaces[0].cwd) { throw 'Caller routing or cwd changed after move' }
    $evidence.checks += @{ name = 'hidden_moved_child_cli_uses_stable_surface_and_preserves_cwd'; passed = $true; context = $actual.identity }
    Invoke-Flowmux @('focus-tab', $original.surface) | Out-Null
    Invoke-Flowmux @('new-workspace') | Out-Null
    Wait-Ready | Out-Null
    $other = Invoke-Flowmux @('identify')
    Invoke-Flowmux @('move-tab', $original.surface, '--to-pane', $other.pane) | Out-Null
    Wait-Screen $other.pane 'FLOWMUX_MOVE_HISTORY' | Out-Null
    $tree = Invoke-Flowmux @('tree')
    if (($tree.surfaces | Where-Object { $_.id -eq $original.surface }).pid -ne $originalPid) { throw 'Workspace move restarted shell' }
    $evidence.checks += @{ name = 'workspace_move_preserves_pid_and_history'; passed = $true }
    if (-not $Interactive) {
        Invoke-Flowmux @('move-tab', $original.surface, '--to-pane', $other.pane, '--index', '0') | Out-Null
        $tree = Invoke-Flowmux @('tree')
        $ws = $tree.workspaces | Where-Object { $_.id -eq $other.workspace }
        if ($ws.root.content.surfaces[0].id -ne $original.surface) { throw 'CLI tab reorder failed' }
        for ($cycle = 0; $cycle -lt 20; $cycle++) {
            Invoke-Flowmux @('move-tab', $original.surface, '--to-pane', $destination.pane) | Out-Null
            Invoke-Flowmux @('move-tab', $original.surface, '--to-pane', $other.pane) | Out-Null
        }
        $tree = Invoke-Flowmux @('tree')
        if (($tree.surfaces | Where-Object { $_.id -eq $original.surface }).pid -ne $originalPid -or $tree.surfaces.Count -ne 3) { throw 'Repeated move lost/restarted a session' }
        Wait-Screen $other.pane 'FLOWMUX_MOVE_HISTORY' | Out-Null
        if ([NativeInput]::IsWindowVisible($script:window) -or [NativeInput]::GetForegroundWindow() -eq $script:window) { throw 'Background test took desktop focus' }
        $evidence.checks += @{ name = 'hidden_host_reorder_and_40_workspace_moves_preserve_sessions_and_output'; passed = $true; cycles = 20 }
        $evidence.interactive = 'Native menu and real IME remain pending; desktop input was not injected.'
        $evidence.status = 'passed_background_move_subset'
    } else {
        # Exercise the actual native menu: the moved tab is at index 1, so its first
        # menu item moves left. Mouse/key input remains confined to this HWND.
        $process.Refresh(); $script:window = $process.MainWindowHandle
        Start-Sleep -Milliseconds 500
        [NativeInput]::Foreground($script:window)
        [NativeInput]::ClickButton($script:window, ('Move tab' + [char]0x2026))
        $deadline = (Get-Date).AddSeconds(3)
        while ([NativeInput]::MenuWindow($script:window) -eq [IntPtr]::Zero) {
            if ((Get-Date) -gt $deadline) { throw 'Native move menu did not open after button click' }
            Start-Sleep -Milliseconds 50
        }
        $evidence.menuOpened = $true
        Save-Window 'native-move-menu.png' ([NativeInput]::MenuWindow($script:window))
        $evidence.menuFocus = [NativeInput]::Focused($script:window).ToInt64()
        $evidence.window = $script:window.ToInt64()
        Press 0x28
        $evidence.menuAfterDown = [NativeInput]::MenuWindow($script:window).ToInt64()
        Save-Window 'native-move-menu-selected.png' ([NativeInput]::MenuWindow($script:window))
        Press 0x0D
        $evidence.menuAfterEnter = [NativeInput]::MenuWindow($script:window).ToInt64()
        $tree = Invoke-Flowmux @('tree')
        $evidence.menuTree = $tree
        $ws = $tree.workspaces | Where-Object { $_.id -eq $other.workspace }
        if ($ws.root.content.surfaces[0].id -ne $original.surface) { throw 'Native Move left menu did not reorder tab' }
        $evidence.checks += @{ name = 'native_menu_reorders_live_tab'; passed = $true }
        # Two independent raw console probes detect cross-surface IME delivery.
        $raw = Join-Path $directory 'moved-input.bin'
        $otherRaw = Join-Path $directory 'other-input.bin'
        Send-Line $other.pane "& '$probe' '$raw'"
        Wait-Screen $other.pane 'INPUT_PROBE_READY' | Out-Null
        Invoke-Flowmux @('focus-tab', $destination.surface) | Out-Null
        Send-Line $destination.pane "& '$probe' '$otherRaw'"
        Wait-Screen $destination.pane 'INPUT_PROBE_READY' | Out-Null
        Invoke-Flowmux @('focus-tab', $original.surface) | Out-Null
        [NativeInput]::Foreground($script:window)
        $evidence.imeSetup = [NativeInput]::Korean($script:window)
        Press 0x47; Press 0x4B; Press 0x53
        if ((Bytes $raw).Length -ne 0) { throw 'Composition reached PTY before commit' }
        Invoke-Flowmux @('move-tab', $original.surface, '--to-pane', $destination.pane) | Out-Null
        Start-Sleep -Milliseconds 200
        Save-Window 'composition-after-workspace-move.png'
        Press 0x0D
        Start-Sleep -Milliseconds 300
        $hex = [BitConverter]::ToString((Bytes $raw))
        if ($hex -ne 'ED-95-9C-0D' -or (Bytes $otherRaw).Length -ne 0) { throw "IME crossed surfaces or lost/repeated commit after move: $hex" }
        $tree = Invoke-Flowmux @('tree')
        if (($tree.surfaces | Where-Object { $_.id -eq $original.surface }).pid -ne $originalPid) { throw 'IME move restarted process' }
        $evidence.checks += @{ name = 'real_ime_composition_move_commits_once_to_same_process'; passed = $true; actualHex = $hex; otherSurfaceBytes = (Bytes $otherRaw).Length }
        Save-Window 'after-move-enter.png'
        $evidence.status = 'passed_same_window_move_subset'
    }
} catch {
    $evidence.status = 'failed'; $evidence.error = $_.Exception.Message
    throw
} finally {
    if ($script:pipeName) { try { Invoke-Flowmux @('quit') | Out-Null } catch {} }
    if (-not $process.HasExited -and -not $process.WaitForExit(10000)) { Stop-Process -Id $process.Id -Force }
    $evidence.finished = (Get-Date).ToString('o')
    if ($evidence.status -eq 'failed') {
        $evidence | ConvertTo-Json -Depth 15 | Set-Content -Encoding UTF8 (Join-Path $directory 'tab-move.json')
        Write-Output ('Failure diagnostics: '+$directory)
    }
}
Write-Host ('[check] tab move: '+$evidence.checks.Count+' groups passed')
