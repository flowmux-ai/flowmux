# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden debug hosts, isolated state and generated PTY commands. No desktop input.
param([string]$BuildDirectory = "$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference = 'Stop'
$OutputEncoding = [Console]::OutputEncoding = New-Object Text.UTF8Encoding($false)
$BuildDirectory = (Resolve-Path $BuildDirectory).Path
$cli = Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor = (& $cli doctor | ConvertFrom-Json)
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) { throw 'A working debug build is required; no window was launched.' }
Add-Type -Path (Join-Path $PSScriptRoot 'NativeInput.cs')
$directory = Join-Path $PSScriptRoot ('..\dist\evidence\cwd-' + [guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory) | Out-Null
$directory = (Resolve-Path $directory).Path
$korean = ([string][char]0xD55C) + [char]0xAE00
$firstCwd = Join-Path $directory ($korean + ' ' + [char]0x1112 + [char]0x1161 + [char]0x11AB + ' e' + [char]0x301 + " space %#;'folder")
$hiddenCwd = Join-Path $directory ($korean + ' hidden')
$inlineCwd = Join-Path $directory ($korean + ' inline')
foreach ($path in @($firstCwd,$hiddenCwd,$inlineCwd)) { [IO.Directory]::CreateDirectory($path) | Out-Null }
$hosts = New-Object 'System.Collections.Generic.List[System.Diagnostics.Process]'
$evidence = [ordered]@{ started=(Get-Date).ToString('o'); mode='background'; checks=@() }
function Quote-PS([string]$Value) { return "'" + $Value.Replace("'", "''") + "'" }
function Start-Owned([string[]]$LaunchArgs) {
    $previousBackground = $env:FLOWMUX_TEST_BACKGROUND; $previousState = $env:FLOWMUX_TEST_STATE_DIR
    try {
        $env:FLOWMUX_TEST_BACKGROUND = '1'; $env:FLOWMUX_TEST_STATE_DIR = Join-Path $directory 'state'
        $process = Start-Process -FilePath (Join-Path $BuildDirectory 'flowmux.exe') -ArgumentList $LaunchArgs -PassThru
        $hosts.Add($process)
        return $process
    } finally { $env:FLOWMUX_TEST_BACKGROUND = $previousBackground; $env:FLOWMUX_TEST_STATE_DIR = $previousState }
}
function Invoke-Flowmux([string[]]$Arguments) {
    $value = & $cli --pipe $script:pipeName --json @Arguments
    if ($LASTEXITCODE -ne 0) { throw "flowmuxctl failed: $Arguments" }
    return (($value -join "`n") | ConvertFrom-Json)
}
function Wait-Ready($Process) {
    $discovery = Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($Process.Id).json"
    $deadline = (Get-Date).AddSeconds(25)
    while (-not (Test-Path $discovery)) {
        if ($Process.HasExited -or (Get-Date) -gt $deadline) { throw 'Host did not start' }
        Start-Sleep -Milliseconds 100
    }
    $script:pipeName = (Get-Content -Raw $discovery | ConvertFrom-Json).pipe
    do {
        $tree = Invoke-Flowmux @('tree')
        if (@($tree.surfaces | Where-Object { -not $_.ready -or -not $_.cwd_reported }).Count -eq 0) {
            $window = [IntPtr]([long]$tree.window_handle)
            if (-not $tree.background_testing -or [NativeInput]::IsWindowVisible($window) -or [NativeInput]::GetForegroundWindow() -eq $window) { throw 'Host became visible or foreground' }
            return $tree
        }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    $tree | ConvertTo-Json -Depth 40 | Set-Content -Encoding UTF8 (Join-Path $directory 'failed-tree.json')
    throw 'Shell prompt did not report its cwd'
}
function Send-Script([string]$Pane, [string]$Source) {
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Source))
    Invoke-Flowmux @('send-keys', $Pane, "Invoke-Expression ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$encoded')))") | Out-Null
    Invoke-Flowmux @('send-key', 'Enter', '--pane', $Pane) | Out-Null
}
function Wait-Cwd([string]$Surface, [string]$Expected) {
    $deadline = (Get-Date).AddSeconds(12)
    do {
        $tree = Invoke-Flowmux @('tree')
        $found = $tree.surfaces | Where-Object { $_.id -eq $Surface }
        if ([string]::Equals($found.cwd, $Expected, [StringComparison]::Ordinal)) { return $tree }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    (Invoke-Flowmux @('read-screen','--surface',$Surface)).text | Set-Content -Encoding UTF8 (Join-Path $directory 'failed-cwd-screen.txt')
    $tree | ConvertTo-Json -Depth 50 | Set-Content -Encoding UTF8 (Join-Path $directory 'failed-cwd-tree.json')
    throw "Cwd mismatch for $Surface : expected $Expected, got $($found.cwd)"
}
function Wait-File([string]$Path) {
    $deadline = (Get-Date).AddSeconds(12)
    while (-not (Test-Path -LiteralPath $Path)) {
        if ((Get-Date) -gt $deadline) { throw "Child did not create $Path" }
        Start-Sleep -Milliseconds 100
    }
}
function Wait-Marker([string]$Surface, [string]$Marker) {
    $deadline = (Get-Date).AddSeconds(12)
    do {
        $screen = Invoke-Flowmux @('read-screen','--surface',$Surface)
        if ($screen.text.Replace("`n", '').Contains($Marker)) { return }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    throw "Output marker missing: $Marker"
}
function Tabs($Node) {
    if ($Node.kind -eq 'leaf') { return $Node.content.surfaces }
    Tabs $Node.first
    Tabs $Node.second
}
try {
    $process = Start-Owned @('--cwd', ('"' + $directory + '"'))
    $tree = Wait-Ready $process
    $origin = Invoke-Flowmux @('identify'); $surface = $origin.surface; $pane = $origin.pane
    Send-Script $pane ("Set-Location -LiteralPath " + (Quote-PS $firstCwd))
    Wait-Cwd $surface $firstCwd | Out-Null
    $evidence.checks += 'Prompt reports Korean, decomposed accent, spaces, percent, hash, semicolon and apostrophe paths through real ConPTY'
    Invoke-Flowmux @('new-tab') | Out-Null
    $tree = Wait-Ready $process
    $other = Invoke-Flowmux @('identify')
    if ($other.cwd -ne $firstCwd) { throw 'New tab did not inherit current directory' }
    $cwdProbe = Join-Path $directory 'child-cwd.txt'
    Send-Script $other.pane ("[IO.File]::WriteAllText(" + (Quote-PS $cwdProbe) + ', $pwd.ProviderPath)')
    Wait-File $cwdProbe
    if ([IO.File]::ReadAllText($cwdProbe) -ne $firstCwd) { throw 'New shell process started in the wrong directory' }
    Invoke-Flowmux @('split','vertical') | Out-Null
    $tree = Wait-Ready $process
    $split = Invoke-Flowmux @('identify')
    if ($split.cwd -ne $firstCwd) { throw 'Split did not inherit current directory' }
    $evidence.checks += 'New tab and split inherit the reported directory; child PowerShell confirms its actual location'

    Invoke-Flowmux @('focus-tab',$surface) | Out-Null
    Send-Script $pane ("Start-Sleep -Milliseconds 350; Set-Location -LiteralPath " + (Quote-PS $hiddenCwd))
    Invoke-Flowmux @('focus-tab',$other.surface) | Out-Null
    $tree = Wait-Cwd $surface $hiddenCwd
    $focused = Invoke-Flowmux @('identify')
    if ($focused.surface -ne $other.surface -or $focused.cwd -ne $firstCwd) { throw 'Hidden cwd update affected active tab or focus' }
    Invoke-Flowmux @('new-workspace','--cwd',$directory) | Out-Null
    $tree = Wait-Ready $process
    $destination = Invoke-Flowmux @('identify')
    Invoke-Flowmux @('move-tab',$surface,'--to-pane',$destination.pane) | Out-Null
    $moved = Invoke-Flowmux @('identify')
    if ($moved.cwd -ne $hiddenCwd) { throw 'Move lost terminal directory' }
    $gate = Join-Path $directory 'release-prompt'; $sentinel = Join-Path $directory 'before-prompt'
    Send-Script $moved.pane ("Set-Location -LiteralPath " + (Quote-PS $inlineCwd) + '; & $env:FLOWMUX_BUNDLED_CLI_PATH new-tab | Out-Null; [IO.File]::WriteAllText(' + (Quote-PS $sentinel) + ", 'done'); `$deadline=(Get-Date).AddSeconds(20); while (-not [IO.File]::Exists(" + (Quote-PS $gate) + ') -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 50 }')
    Wait-File $sentinel
    $tree = Wait-Ready $process
    $inline = Invoke-Flowmux @('identify')
    if ($inline.surface -eq $surface -or $inline.cwd -ne $inlineCwd) { throw 'Same-line native CLI used stale pre-prompt directory' }
    [IO.File]::WriteAllText($gate,'release')
    Wait-Cwd $surface $inlineCwd | Out-Null
    $evidence.checks += 'Hidden tab cwd and focus stay isolated; moved child CLI uses fresh same-line cd context before the next prompt'

    Invoke-Flowmux @('focus-tab',$surface) | Out-Null
    $promptCwd = Join-Path $directory 'prompt-cwd.txt'
    $probe = Join-Path $directory 'prompt-status.txt'; $keys = Join-Path $directory 'readline.json'
    $source = '$global:flowmuxTestPromptValue="before"; $beforeEncoding = [Console]::OutputEncoding.CodePage; $beforeRead = $function:PSConsoleHostReadLine; $beforeKeys = (Get-PSReadLineKeyHandler | ConvertTo-Json -Compress); function global:prompt { $seen=$global:?; $code=$global:LASTEXITCODE; [IO.File]::WriteAllText(' + (Quote-PS $probe) + ', "$seen|$code"); [IO.File]::WriteAllText(' + (Quote-PS $promptCwd) + ', "$flowmuxTestPromptValue|$($pwd.ProviderPath)"); "CUSTOM> " }; $integration = & $env:FLOWMUX_BUNDLED_CLI_PATH shell-integration | Out-String; Invoke-Expression $integration; $wrapper = $function:prompt; Invoke-Expression $integration; @{idempotent=[object]::ReferenceEquals($wrapper,$function:prompt); encoding=($beforeEncoding -eq [Console]::OutputEncoding.CodePage); codePage=$beforeEncoding; readline=[object]::ReferenceEquals($beforeRead,$function:PSConsoleHostReadLine); keys=($beforeKeys -eq (Get-PSReadLineKeyHandler | ConvertTo-Json -Compress))} | ConvertTo-Json | Set-Content -Encoding UTF8 -LiteralPath ' + (Quote-PS $keys) + '; $global:LASTEXITCODE=17; Write-Error "expected test failure" -ErrorAction Ignore'
    Send-Script $moved.pane $source
    Wait-File $probe; Wait-File $keys
    # Fail a top-level command: Invoke-Expression's own success status can hide
    # a nested non-terminating Write-Error, independently of prompt integration.
    Invoke-Flowmux @('send-keys',$moved.pane,'& $env:ComSpec /d /c exit 17') | Out-Null
    Invoke-Flowmux @('send-key','Enter','--pane',$moved.pane) | Out-Null
    $deadline = (Get-Date).AddSeconds(12)
    while ([IO.File]::ReadAllText($probe) -ne 'False|17' -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 100 }
    if ([IO.File]::ReadAllText($probe) -ne 'False|17') { throw 'Wrapper changed custom prompt command status' }
    $keyResult = Get-Content -Raw -LiteralPath $keys | ConvertFrom-Json
    if (-not $keyResult.readline -or -not $keyResult.keys -or -not $keyResult.idempotent -or -not $keyResult.encoding) { throw 'Integration changed PSReadLine function or keys' }
    $evidence.prompt = $keyResult
    $evidence.checks += 'Custom prompt sees failed command status and exit code 17; reinstall is idempotent and PSReadLine handlers/function remain unchanged'
    Send-Script $moved.pane ('$global:flowmuxTestPromptValue="after"; Set-Location -LiteralPath ' + (Quote-PS $hiddenCwd))
    Wait-Cwd $surface $hiddenCwd | Out-Null
    if ([IO.File]::ReadAllText($promptCwd) -cne ('after|' + $hiddenCwd)) { throw 'Custom prompt captured stale variables or cwd' }
    Send-Script $moved.pane ('Set-Location -LiteralPath ' + (Quote-PS $inlineCwd))
    Wait-Cwd $surface $inlineCwd | Out-Null
    $evidence.checks += 'Custom prompt retains dynamic variables and current $pwd after integration and later cd'


    $done = Join-Path $directory 'provider-done'
    Send-Script $moved.pane ('Set-Location HKCU:\; [Console]::Write(([string][char]27) + "]9;9;\\untrusted-host\share" + [char]7); [Console]::Write(([string][char]27) + "]7;file://remote-host/C:/x" + [char]7); [IO.File]::WriteAllText(' + (Quote-PS $done) + ", 'done'); Write-Output ('BAD_META_' + 'DONE')")
    Wait-File $done
    Wait-Marker $surface 'BAD_META_DONE'
    $tree = Invoke-Flowmux @('tree')
    if (($tree.surfaces | Where-Object { $_.id -eq $surface }).cwd -ne $inlineCwd) { throw 'Non-filesystem provider or remote OSC replaced local cwd' }
    $evidence.checks += 'Registry provider retains last filesystem cwd; remote OSC 7/UNC OSC 9 metadata is ignored'
    $saved = Invoke-Flowmux @('save-state')
    Invoke-Flowmux @('quit') | Out-Null
    if (-not $process.WaitForExit(10000)) { throw 'First host did not exit' }
    $state = [IO.File]::ReadAllText($saved.path) | ConvertFrom-Json
    $lockedTitle = $korean + ' locked'
    foreach ($workspace in $state.workspaces) {
        foreach ($tab in (Tabs $workspace.root)) {
            if ($tab.id -eq $surface) { $tab.title=$lockedTitle; $tab.title_locked=$true }
        }
    }
    $state.screens.$surface.data += ([string][char]27) + ']9;9;C:\history-only' + [char]7
    [IO.File]::WriteAllText($saved.path, ($state | ConvertTo-Json -Depth 60), (New-Object Text.UTF8Encoding($false)))
    $restored = Start-Owned @('--restore-window',$saved.window)
    $after = Wait-Ready $restored
    foreach ($old in $tree.surfaces) {
        $new = $after.surfaces | Where-Object { $_.id -eq $old.id }
        if ($new.cwd -ne $old.cwd -or $new.pid -eq $old.pid) { throw 'Restore lost current cwd or did not restart process' }
    }
    Invoke-Flowmux @('focus-tab',$surface) | Out-Null
    $active = Invoke-Flowmux @('identify')
    Send-Script $active.pane '[Console]::Write(([string][char]27) + "]2;overwritten-by-output" + [char]7); Write-Output ("TITLE_" + "DONE")'
    Wait-Marker $surface 'TITLE_DONE'
    $after = Invoke-Flowmux @('tree')
    $labels = [NativeInput]::ButtonTitles([IntPtr]([long]$after.window_handle))
    if ($labels -notcontains (([string][char]0x25CF) + ' ' + $lockedTitle)) { throw 'Native tab label lost locked title or active marker' }
    $evidence.checks += 'Restart restores each current directory with fresh shells; history OSC cannot change cwd; native label retains locked title and active marker'
    $evidence.window=$saved.window; $evidence.first=$process.Id; $evidence.restored=$restored.Id
    $evidence.surfaces=@($after.surfaces | Select-Object id,cwd,cwd_reported,pid)
    Invoke-Flowmux @('quit') | Out-Null
    if (-not $restored.WaitForExit(10000)) { throw 'Restored host did not exit' }
    $evidence.finished=(Get-Date).ToString('o')
    $evidence | ConvertTo-Json -Depth 20 | Set-Content -Encoding UTF8 (Join-Path $directory 'native-cwd-background.json')
    $evidence | ConvertTo-Json -Depth 20
} finally {
    foreach ($owned in $hosts) { if (-not $owned.HasExited) { $owned.Kill(); $owned.WaitForExit() } }
}
