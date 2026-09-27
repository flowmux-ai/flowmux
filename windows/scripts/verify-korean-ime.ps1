# SPDX-License-Identifier: GPL-3.0-or-later
# Uses the real Microsoft Korean IME and virtual-key SendInput, not JS text injection.
param([string]$BuildDirectory = "$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference = 'Stop'
$OutputEncoding = [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$BuildDirectory = (Resolve-Path $BuildDirectory).Path
$directory = Join-Path $PSScriptRoot '..\dist\evidence\ime'
New-Item -ItemType Directory -Path $directory -Force | Out-Null
$directory = (Resolve-Path $directory).Path
Add-Type -Path (Join-Path $PSScriptRoot 'NativeInput.cs')
Add-Type -AssemblyName System.Drawing
$probe = Join-Path $directory 'input-probe.exe'
if (Test-Path $probe) { Remove-Item $probe }
Add-Type -Path (Join-Path $PSScriptRoot 'InputProbe.cs') -OutputAssembly $probe -OutputType ConsoleApplication
$cli = Join-Path $BuildDirectory 'flowmuxctl.exe'
$trace = Join-Path $directory 'before-conpty.jsonl'
if (Test-Path $trace) { Remove-Item $trace }
$previousTrace = $env:FLOWMUX_TEST_INPUT_TRACE
try {
    $env:FLOWMUX_TEST_INPUT_TRACE = $trace
    $process = Start-Process -FilePath (Join-Path $BuildDirectory 'flowmux.exe') -PassThru
} finally { $env:FLOWMUX_TEST_INPUT_TRACE = $previousTrace }
$discovery = Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($process.Id).json"
$script:pipeName = $null
$evidence = [ordered]@{ pid = $process.Id; started = (Get-Date).ToString('o'); mechanism = 'Microsoft Korean IME + SendInput virtual keys'; cases = @() }
function Invoke-Flowmux([string[]]$Arguments) {
    $output = & $cli --pipe $script:pipeName --json @Arguments
    if ($LASTEXITCODE -ne 0) { throw "flowmuxctl failed: $Arguments" }
    return (($output -join "`n") | ConvertFrom-Json)
}
function Read-Bytes([string]$Path) {
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
    try { $memory = New-Object IO.MemoryStream; $stream.CopyTo($memory); return ,$memory.ToArray() }
    finally { $stream.Dispose() }
}
function Save-Window([IntPtr]$Window, [string]$Name) {
    $rectangle = New-Object NativeInput+Rect
    [NativeInput]::GetWindowRect($Window, [ref]$rectangle) | Out-Null
    $bitmap = New-Object Drawing.Bitmap(($rectangle.Right - $rectangle.Left), ($rectangle.Bottom - $rectangle.Top))
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    $dc = $graphics.GetHdc()
    try { [NativeInput]::PrintWindow($Window, $dc, 2) | Out-Null }
    finally { $graphics.ReleaseHdc($dc); $graphics.Dispose() }
    $bitmap.Save((Join-Path $directory $Name)); $bitmap.Dispose()
}
function Press([int]$Key, [bool]$Shift = $false) {
    [NativeInput]::Key($script:testWindow, $Key, $Shift)
    Start-Sleep -Milliseconds 65
}
function Type-Han { Press 0x47; Press 0x4B; Press 0x53 }
function Case([string]$Name, [scriptblock]$Keys, [string]$Expected) {
    $before = (Read-Bytes $script:rawPath).Length
    $traceCount = if (Test-Path $script:trace) { @(Get-Content $script:trace).Count } else { 0 }
    & $Keys
    Start-Sleep -Milliseconds 250
    $all = Read-Bytes $script:rawPath
    $bytes = New-Object byte[] ($all.Length - $before)
    [Array]::Copy($all, $before, $bytes, 0, $bytes.Length)
    $actual = [BitConverter]::ToString($bytes)
    $expectedHex = [BitConverter]::ToString([Text.Encoding]::UTF8.GetBytes($Expected))
    $row = @{ name = $Name; actualHex = $actual; expectedHex = $expectedHex; passed = ($actual -eq $expectedHex) }
    if (Test-Path $script:trace) {
        $wireBytes = New-Object 'System.Collections.Generic.List[byte]'
        @(Get-Content $script:trace) | Select-Object -Skip $traceCount | ForEach-Object {
            $wireBytes.AddRange([byte[]](($_ | ConvertFrom-Json).bytes))
        }
        $row.beforeConptyHex = [BitConverter]::ToString($wireBytes.ToArray())
        $row.beforeConptyPassed = ($row.beforeConptyHex -eq $expectedHex)
    }
    $script:evidence.cases += $row
}
try {
    $deadline = (Get-Date).AddSeconds(25)
    while (-not (Test-Path $discovery)) {
        if ($process.HasExited -or (Get-Date) -gt $deadline) { throw 'Native host startup failed' }
        Start-Sleep -Milliseconds 100
    }
    $script:pipeName = (Get-Content -Raw $discovery | ConvertFrom-Json).pipe
    do {
        $tree = Invoke-Flowmux @('tree')
        if ($tree.surfaces[0].ready) { break }
        if ((Get-Date) -gt $deadline) { throw 'Terminal not ready' }
        Start-Sleep -Milliseconds 100
    } while ($true)
    $pane = (Invoke-Flowmux @('identify')).pane
    $process.Refresh(); $window = $process.MainWindowHandle
    $script:testWindow = $window
    if ($window -eq [IntPtr]::Zero) { throw 'No native window handle' }
    [NativeInput]::SetForegroundWindow($window) | Out-Null
    Start-Sleep -Milliseconds 200
    Invoke-Flowmux @('focus-pane', $pane) | Out-Null
    Start-Sleep -Milliseconds 200
    $raw = Join-Path $directory 'composition-enter.bin'
    $script:rawPath = $raw
    Invoke-Flowmux @('send-keys', $pane, "& '$probe' '$raw'") | Out-Null
    Invoke-Flowmux @('send-key', 'Enter', '--pane', $pane) | Out-Null
    $deadline = (Get-Date).AddSeconds(10)
    do {
        $screen = Invoke-Flowmux @('read-screen', $pane)
        if ((Test-Path $raw) -and $screen.text.Contains('INPUT_PROBE_READY')) { break }
        if ((Get-Date) -gt $deadline) { throw 'Console byte probe did not start' }
        Start-Sleep -Milliseconds 100
    } while ($true)
    [NativeInput]::Foreground($window)
    $evidence.imeSetup = [NativeInput]::Korean($window)
    Start-Sleep -Milliseconds 200
    # g, k, s on the Korean two-set keyboard produce the composing syllable U+D55C.
    foreach ($key in @(0x47, 0x4B, 0x53)) {
        [NativeInput]::Key($window, $key, $false)
        Start-Sleep -Milliseconds 100
    }
    Start-Sleep -Milliseconds 300
    $preedit = Read-Bytes $raw
    Save-Window $window 'hidden-cursor-preedit.png'
    [NativeInput]::Key($window, 0x0D, $false)
    Start-Sleep -Milliseconds 500
    $committed = Read-Bytes $raw
    $expected = [Text.Encoding]::UTF8.GetBytes(([string][char]0xD55C) + "`r")
    $actualHex = [BitConverter]::ToString($committed)
    $expectedHex = [BitConverter]::ToString($expected)
    $passed = ($preedit.Length -eq 0 -and $actualHex -eq $expectedHex)
    $evidence.cases += @{ name = 'hidden_cursor_composition_then_enter'; preeditHex = [BitConverter]::ToString($preedit); actualHex = $actualHex; expectedHex = $expectedHex; passed = $passed }
    Save-Window $window 'after-enter.png'
    $han = [string][char]0xD55C; $ha = [string][char]0xD558
    Case 'composition_backspace_decomposes' { Type-Han; Press 0x08; Press 0x0D } ($ha + "`r")
    Case 'composition_three_backspaces_cancel' { Type-Han; Press 0x08; Press 0x08; Press 0x08; Press 0x0D } "`r"
    Case 'consonant_moves_to_next_syllable' { Press 0x52; Press 0x4B; Press 0x53; Press 0x4B; Press 0x0D } (([string][char]0xAC00) + [char]0xB098 + "`r")
    Case 'continuous_hangul' { Type-Han; Press 0x52; Press 0x4D; Press 0x46; Press 0x0D } ($han + [char]0xAE00 + "`r")
    Case 'composition_shift_enter' { Type-Han; Press 0x0D $true } ($han + [char]27 + "`r")
    Case 'composition_enter_three_times' {
        Type-Han
        [NativeInput]::Key($window, 0x0D, $false)
        [NativeInput]::Key($window, 0x0D, $false)
        [NativeInput]::Key($window, 0x0D, $false)
    } ($han + "`r`r`r")
    Case 'composition_space_number_comma' { Type-Han; Press 0x20; Press 0x31; Press 0xBC; Press 0x0D } ($han + " 1,`r")
    Case 'composition_arrow_left' { Type-Han; Press 0x25 } ($han + [char]27 + '[D')
    Case 'composition_shift_arrow_left' { Type-Han; Press 0x25 $true } ($han + [char]27 + '[1;2D')
    Case 'committed_shift_arrow_left' { Press 0x25 $true } (([string][char]27) + '[1;2D')
    $evidence.status = $(if (@($evidence.cases | Where-Object { -not $_.passed }).Count -eq 0) { 'passed_ime_subset_only' } else { 'failed' })
} catch {
    $evidence.status = 'blocked_or_failed'; $evidence.error = $_.Exception.Message
    throw
} finally {
    if ($script:pipeName) { try { Invoke-Flowmux @('quit') | Out-Null } catch {} }
    if (-not $process.HasExited) { if (-not $process.WaitForExit(10000)) { Stop-Process -Id $process.Id -Force } }
    $evidence.finished = (Get-Date).ToString('o')
    $evidence | ConvertTo-Json -Depth 10 | Set-Content -Encoding UTF8 (Join-Path $directory 'native-ime.json')
}
$evidence | ConvertTo-Json -Depth 10
if ($evidence.status -ne 'passed_ime_subset_only') { throw 'One or more native IME checks failed; see native-ime.json.' }
