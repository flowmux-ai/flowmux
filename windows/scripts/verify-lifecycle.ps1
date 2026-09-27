# SPDX-License-Identifier: GPL-3.0-or-later
param([string]$BuildDirectory = "$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug", [switch]$Interactive)
$ErrorActionPreference = 'Stop'
$OutputEncoding = [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$BuildDirectory = (Resolve-Path $BuildDirectory).Path
$directory = Join-Path $PSScriptRoot '..\dist\evidence\lifecycle'
New-Item -ItemType Directory -Path $directory -Force | Out-Null
$directory = (Resolve-Path $directory).Path
$probe = Join-Path $directory 'process-tree-probe.exe'
if (Test-Path $probe) { Remove-Item $probe }
Add-Type -Path (Join-Path $PSScriptRoot 'ProcessTreeProbe.cs') -OutputAssembly $probe -OutputType ConsoleApplication
Add-Type -Path (Join-Path $PSScriptRoot 'NativeInput.cs')
Add-Type -AssemblyName System.Drawing
$cli = Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor = (& $cli doctor | ConvertFrom-Json)
if ($LASTEXITCODE -ne 0 -or (-not $Interactive -and -not $doctor.background_testing)) {
    throw 'Background verification requires a working debug build; no window was launched.'
}
$previousBackground = $env:FLOWMUX_TEST_BACKGROUND
try {
    $env:FLOWMUX_TEST_BACKGROUND = $(if ($Interactive) { $null } else { '1' })
    $process = Start-Process -FilePath (Join-Path $BuildDirectory 'flowmux.exe') -ArgumentList '--temporary' -PassThru
} finally { $env:FLOWMUX_TEST_BACKGROUND = $previousBackground }
$script:pipeName = $null
$evidence = [ordered]@{ pid = $process.Id; started = (Get-Date).ToString('o'); checks = @(); mode = $(if ($Interactive) { 'interactive' } else { 'background' }) }
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
function Start-Tree([string]$Name) {
    Invoke-Flowmux @('new-tab') | Out-Null
    $tree = Wait-Ready
    $identity = Invoke-Flowmux @('identify')
    $rootPid = ($tree.surfaces | Where-Object { $_.id -eq $identity.surface }).pid
    $file = Join-Path $directory ($Name + '-' + [Guid]::NewGuid().ToString() + '.pids')
    Invoke-Flowmux @('send-keys', $identity.pane, "& '$probe' '$file' 2") | Out-Null
    Invoke-Flowmux @('send-key', 'Enter', '--pane', $identity.pane) | Out-Null
    $deadline = (Get-Date).AddSeconds(15)
    do {
        $children = if (Test-Path $file) { @(Get-Content $file | Where-Object { $_ -match '^\d+$' }) } else { @() }
        if ($children.Count -eq 3) { return @{ surface = $identity.surface; pids = @($rootPid) + @($children | ForEach-Object { [int]$_ }) } }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    throw 'Three-level process probe did not start'
}
function Assert-Stopped([int[]]$Ids) {
    $deadline = (Get-Date).AddSeconds(8)
    do {
        $remaining = @($Ids | Where-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue })
        if ($remaining.Count -eq 0) { return }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    throw "Process tree survived session termination: $remaining"
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
    $window = [IntPtr]::new([long]$initial.window_handle)
    if (-not $Interactive -and (-not $initial.background_testing -or [NativeInput]::IsWindowVisible($window) -or [NativeInput]::GetForegroundWindow() -eq $window)) { throw 'Background host unexpectedly exposed a window' }
    $identity = Invoke-Flowmux @('identify')
    $marker = 'FLOWMUX_NORMAL_EXIT_' + [char]0xD55C + [char]0xAE00
    # The full marker must occur in actual output, never just in the echoed command.
    Invoke-Flowmux @('send-keys', $identity.pane, "Write-Output ('FLOWMUX_' + '$($marker.Substring(8))'); exit 7") | Out-Null
    Invoke-Flowmux @('send-key', 'Enter', '--pane', $identity.pane) | Out-Null
    $deadline = (Get-Date).AddSeconds(12)
    do {
        $tree = Invoke-Flowmux @('tree')
        $surface = $tree.surfaces | Where-Object { $_.id -eq $identity.surface }
        if ($surface.resources_released) { break }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    if (-not $surface.resources_released -or $surface.exit_code -ne 7) { throw 'Normal exit did not release native session resources with exit code 7' }
    $screen = Invoke-Flowmux @('read-screen', $identity.pane)
    if (-not $screen.text.Contains($marker)) { throw 'Final Korean output was lost after process exit' }
    $evidence.checks += @{ name = 'natural_exit_preserves_final_screen_and_exit_code'; passed = $true; surface = $surface }
    if ($Interactive) {
        $process.Refresh()
        $rect = New-Object NativeInput+Rect
        [NativeInput]::GetWindowRect($process.MainWindowHandle, [ref]$rect) | Out-Null
        $bitmap = New-Object Drawing.Bitmap(($rect.Right-$rect.Left), ($rect.Bottom-$rect.Top))
        $graphics = [Drawing.Graphics]::FromImage($bitmap); $dc = $graphics.GetHdc()
        try { [NativeInput]::PrintWindow($process.MainWindowHandle, $dc, 2) | Out-Null }
        finally { $graphics.ReleaseHdc($dc); $graphics.Dispose() }
        $bitmap.Save((Join-Path $directory 'normal-exit.png')); $bitmap.Dispose()
    }
    $childTree = Start-Tree 'tab-close'
    Invoke-Flowmux @('close-tab', $childTree.surface) | Out-Null
    Assert-Stopped $childTree.pids
    $evidence.checks += @{ name = 'tab_close_terminates_root_and_three_descendants'; passed = $true; pids = $childTree.pids }
    $childTree = Start-Tree 'host-crash'
    Stop-Process -Id $process.Id -Force
    $process.WaitForExit()
    Assert-Stopped $childTree.pids
    $evidence.checks += @{ name = 'host_crash_terminates_owned_process_tree'; passed = $true; pids = $childTree.pids }
    $evidence.status = 'passed_lifecycle_subset'
} catch {
    $evidence.status = 'failed'; $evidence.error = $_.Exception.Message
    throw
} finally {
    if (-not $process.HasExited) {
        if ($script:pipeName) { try { Invoke-Flowmux @('quit') | Out-Null } catch {} }
        if (-not $process.WaitForExit(10000)) { Stop-Process -Id $process.Id -Force }
    }
    $evidence.finished = (Get-Date).ToString('o')
    $evidence | ConvertTo-Json -Depth 15 | Set-Content -Encoding UTF8 (Join-Path $directory 'lifecycle.json')
}
$evidence | ConvertTo-Json -Depth 15
