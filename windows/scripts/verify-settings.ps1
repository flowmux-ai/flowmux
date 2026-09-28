# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden owned hosts, isolated shared settings/state, no desktop input.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
if (-not $env:FLOWMUX_TEST_ARTIFACT_ROOT) { throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.' }
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path; $cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor=(& $cli doctor | ConvertFrom-Json)
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) { throw 'A working debug build is required; no host was launched.' }
Add-Type -Path (Join-Path $PSScriptRoot 'NativeInput.cs')
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class SettingsFile {
    [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)]
    public static extern bool MoveFileEx(string from,string to,uint flags);
}
'@
$directory=Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT ('settings-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory) | Out-Null
$directory=(Resolve-Path $directory).Path
$configDirectory=Join-Path $directory 'config'; $configPath=Join-Path $configDirectory 'config.json'
$hosts=New-Object 'System.Collections.Generic.List[object]'
$evidence=[ordered]@{ started=(Get-Date).ToString('o'); mode='background'; hosts=@(); checks=@() }
function Invoke-Owned($Owned,[string[]]$Arguments) {
    if (-not $Owned.pipe) { throw 'No owned pipe; refusing fallback' }
    $value=& $cli --pipe $Owned.pipe --json @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Owned request failed: $Arguments" }
    return (($value -join "`n") | ConvertFrom-Json)
}
function Same($Actual,[hashtable]$Expected) {
    if (-not $Actual) { return $false }
    foreach ($key in $Expected.Keys) {
        if ($Actual.$key -is [string]) { if (-not [string]::Equals($Actual.$key,[string]$Expected[$key],[StringComparison]::Ordinal)) { return $false } }
        elseif ($Actual.$key -ne $Expected[$key]) { return $false }
    }
    return $true
}
function Wait-Settings($Owned,[hashtable]$Expected,[string]$Revision='') {
    $deadline=(Get-Date).AddSeconds(15)
    do {
        $status=Invoke-Owned $Owned @('settings','show')
        $good=(Same $status.document.terminal $Expected) -and (-not $Revision -or $status.document.revision -eq $Revision)
        foreach ($surface in $status.surfaces) {
            $good=$good -and $surface.applied -and $surface.applied.revision -eq $status.document.revision -and (Same $surface.applied.terminal $Expected)
        }
        if ($good -and $status.surfaces.Count -gt 0) { return $status }
        Start-Sleep -Milliseconds 50
    } while ((Get-Date) -lt $deadline)
    $status | ConvertTo-Json -Depth 20 | Set-Content -Encoding UTF8 (Join-Path $directory 'failed-settings.json')
    throw 'Settings did not reach all terminal instances'
}
function Tree($Owned) {
    $tree=Invoke-Owned $Owned @('tree'); $window=[IntPtr]([long]$tree.window_handle)
    if (-not $tree.background_testing -or [NativeInput]::IsWindowVisible($window) -or [NativeInput]::GetForegroundWindow() -eq $window) { throw 'Host became visible or foreground' }
    return $tree
}
function Start-Owned([string[]]$Arguments) {
    $launch=(Get-Date).ToUniversalTime()
    $oldBackground=$env:FLOWMUX_TEST_BACKGROUND; $oldConfig=$env:FLOWMUX_TEST_CONFIG_DIR; $oldState=$env:FLOWMUX_TEST_STATE_DIR
    try {
        $env:FLOWMUX_TEST_BACKGROUND='1'; $env:FLOWMUX_TEST_CONFIG_DIR=$configDirectory; $env:FLOWMUX_TEST_STATE_DIR=Join-Path $directory 'state'
        $process=Start-Process -FilePath (Join-Path $BuildDirectory 'flowmux.exe') -ArgumentList $Arguments -PassThru
    } finally { $env:FLOWMUX_TEST_BACKGROUND=$oldBackground; $env:FLOWMUX_TEST_CONFIG_DIR=$oldConfig; $env:FLOWMUX_TEST_STATE_DIR=$oldState }
    $owned=@{process=$process;pipe=$null}; $hosts.Add($owned)
    $discovery=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($process.Id).json"; $deadline=(Get-Date).AddSeconds(25)
    do {
        if ($process.HasExited -or (Get-Date) -gt $deadline) { throw 'Owned host failed to start' }
        if ((Test-Path $discovery) -and (Get-Item $discovery).LastWriteTimeUtc -ge $launch) {
            $record=Get-Content -Raw $discovery | ConvertFrom-Json
            if ($record.pid -ne $process.Id) { throw 'Wrong discovery process' }
            $owned.pipe=$record.pipe; break
        }
        Start-Sleep -Milliseconds 50
    } while ($true)
    if ((Invoke-Owned $owned @('identify')).pid -ne $process.Id) { throw 'Connected to another host' }
    do {
        $tree=Tree $owned
        if (@($tree.surfaces | Where-Object { -not $_.ready }).Count -eq 0) { break }
        if ((Get-Date) -gt $deadline) { throw 'Terminals did not start' }
        Start-Sleep -Milliseconds 50
    } while ($true)
    Wait-Settings $owned @{} | Out-Null
    $evidence.hosts+=@{pid=$process.Id;pipe=$owned.pipe;launchedUtc=$launch.ToString('o')}
    return $owned
}
function Stop-Owned($Owned) { Invoke-Owned $Owned @('quit') | Out-Null; if (-not $Owned.process.WaitForExit(10000)) { throw 'Owned host did not quit' } }
function Set-Setting($Owned,[string]$Key,[string]$Value) { return (Invoke-Owned $Owned @('settings','set',$Key,$Value)) }
function Reject($Owned,[string[]]$Arguments) {
    $old=$ErrorActionPreference
    try { $ErrorActionPreference='Continue'; $result=& $cli --pipe $Owned.pipe --json @Arguments 2>&1; $status=$LASTEXITCODE }
    finally { $ErrorActionPreference=$old }
    if ($status -eq 0) { throw "Expected settings rejection: $Arguments" }
}
function Send-Script($Owned,[string]$Pane,[string]$Source) {
    $encoded=[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Source))
    Invoke-Owned $Owned @('send-keys',$Pane,"Invoke-Expression ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$encoded')))") | Out-Null
    Invoke-Owned $Owned @('send-key','Enter','--pane',$Pane) | Out-Null
}
function Wait-Find($Owned,[string]$Surface,[string]$Query) {
    $deadline=(Get-Date).AddSeconds(12)
    do {
        $found=Invoke-Owned $Owned @('find',$Query,'--match-case','--surface',$Surface)
        if ($found.result.found -and $found.result.selection -ceq $Query) { return }
        Start-Sleep -Milliseconds 50
    } while ((Get-Date) -lt $deadline)
    throw "Terminal output not retained: $Query"
}
function Write-Config([string]$Text) {
    $temporary=$configPath+'.'+[guid]::NewGuid()+'.tmp'
    [IO.File]::WriteAllText($temporary,$Text,(New-Object Text.UTF8Encoding($false)))
    if (-not [SettingsFile]::MoveFileEx($temporary,$configPath,9)) { throw 'Test could not atomically replace owned settings' }
}
try {
    $a=Start-Owned @('--cwd',('"'+$directory+'"'))
    Invoke-Owned $a @('new-tab') | Out-Null; Invoke-Owned $a @('split','horizontal') | Out-Null
    $b=Start-Owned @('--new-window','--cwd',('"'+$directory+'"'))
    $identity=Invoke-Owned $a @('identify'); $before=Tree $a
    $pids=(@($before.surfaces | Sort-Object id | ForEach-Object { "$($_.id):$($_.pid)" }) -join ',')
    $marker='SETTINGS-'+[char]0xD55C+[char]0xAE00+'-'+[guid]::NewGuid().ToString().Substring(0,8)
    Send-Script $a $identity.pane ("Write-Output '"+$marker+"'"); Wait-Find $a $identity.surface $marker
    $changed=Set-Setting $a 'font-size' '24'
    Wait-Settings $a @{font_size=24} $changed.document.revision | Out-Null
    Wait-Settings $b @{font_size=24} $changed.document.revision | Out-Null
    $after=Tree $a
    if ((@($after.surfaces | Sort-Object id | ForEach-Object { "$($_.id):$($_.pid)" }) -join ',') -ne $pids -or (Invoke-Owned $a @('identify')).surface -ne $identity.surface) { throw 'Settings restarted a shell or changed focus' }
    $oldSurface=$before.surfaces | Where-Object { $_.id -eq $identity.surface }; $newSurface=$after.surfaces | Where-Object { $_.id -eq $identity.surface }
    if ($newSurface.cols -ge $oldSurface.cols -or $newSurface.rows -ge $oldSurface.rows) { throw 'Larger font did not resize the terminal grid' }
    $sizeFile=Join-Path $directory 'console-size.json'
    Send-Script $a $identity.pane ('@{cols=[Console]::WindowWidth;rows=[Console]::WindowHeight} | ConvertTo-Json | Set-Content -Encoding UTF8 '''+$sizeFile+'''')
    $deadline=(Get-Date).AddSeconds(10); while (-not (Test-Path $sizeFile)) { if ((Get-Date) -gt $deadline) { throw 'Console size probe timed out' }; Start-Sleep -Milliseconds 50 }
    $console=Get-Content -Raw $sizeFile | ConvertFrom-Json
    if ($console.cols -ne $newSurface.cols -or $console.rows -ne $newSurface.rows) { throw 'ConPTY dimensions differ from the configured xterm grid' }
    Wait-Find $a $identity.surface $marker
    $evidence.checks+=@{name='font_size_updates_all_visible_and_hidden_tabs_and_another_window_without_restarting_or_focusing'; previousGrid=@($oldSurface.cols,$oldSurface.rows); newGrid=@($newSurface.cols,$newSurface.rows); console=$console; surfacePids=$pids}

    # CSS accepts single-quoted families; preserve them through PowerShell 5's
    # legacy native argument marshalling (which removes embedded double quotes).
    $font="'"+[char]0xD55C+[char]0xAE00+' '+[char]0x1112+[char]0x1161+[char]0x11AB+' '+[char]::ConvertFromUtf32(0x1F600)+"', Consolas, monospace"
    Set-Setting $b 'font-family' $font | Out-Null; Set-Setting $b 'theme' 'light' | Out-Null
    Set-Setting $a 'cursor-blink' 'false' | Out-Null; Set-Setting $a 'cursor-style' 'bar' | Out-Null
    $wanted=@{font_family=$font;font_size=24;theme='light';cursor_blink=$false;cursor_style='bar'}
    $sa=Wait-Settings $a $wanted; $sb=Wait-Settings $b $wanted
    if (@($sa.surfaces | Where-Object { $_.applied.background -cne '#ffffff' -or $_.applied.foreground -cne '#202124' }).Count) { throw 'xterm did not apply the light colors' }
    if (-not [string]::Equals((Get-Content -Raw -Encoding UTF8 $configPath | ConvertFrom-Json).terminal.font_family,$font,[StringComparison]::Ordinal)) { throw 'Stored Unicode font list changed' }
    $evidence.checks+=@{name='unicode_font_list_theme_and_cursor_survive_cross_window_field_merges'; terminal=$sa.document.terminal}

    $hash=(Get-FileHash $configPath).Hash
    Reject $a @('settings','set','font-size','5'); Reject $a @('settings','set','font-size','24.5')
    Reject $a @('settings','set','scrollback','100001'); Reject $a @('settings','set','font-family',"bad`nfont")
    Reject $a @('settings','set','theme','unknown'); Reject $a @('settings','set','font-size','20','--expected','14')
    if ((Get-FileHash $configPath).Hash -ne $hash) { throw 'Invalid or stale edit changed settings' }
    $guard=[IO.File]::Open($configPath,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
    try { Reject $a @('settings','set','font-size','20') } finally { $guard.Dispose() }
    if ((Get-FileHash $configPath).Hash -ne $hash) { throw 'Failed atomic save changed the previous file' }
    Wait-Settings $a $wanted | Out-Null
    $evidence.checks+=@{name='invalid_stale_and_denied_writes_preserve_file_and_live_terminal_options'}

    $external=Get-Content -Raw -Encoding UTF8 $configPath | ConvertFrom-Json; $external.terminal.font_size=18
    Write-Config ($external | ConvertTo-Json -Depth 10)
    $wanted.font_size=18; Wait-Settings $a $wanted | Out-Null; Wait-Settings $b $wanted | Out-Null
    $evidence.checks+=@{name='external_atomic_edit_reloads_even_when_revision_is_unchanged'}

    $saved=Invoke-Owned $a @('save-state'); $window=(Tree $a).state.window
    Stop-Owned $a; $a=Start-Owned @('--restore-window',$window)
    Wait-Settings $a $wanted | Out-Null; Wait-Find $a $identity.surface $marker
    if ((Tree $a).surfaces.Count -ne 3) { throw 'Settings restore changed the saved layout' }
    $evidence.checks+=@{name='restart_restores_three_tabs_and_korean_history_with_persisted_terminal_settings'; window=$window}

    Set-Setting $a 'scrollback' '100' | Out-Null; Wait-Settings $a @{scrollback=100} | Out-Null
    $tail='TAIL-'+[guid]::NewGuid().ToString().Substring(0,8)
    Send-Script $a $identity.pane ("1..600 | ForEach-Object { Write-Output ('line-'+`$_) }; Write-Output '"+$tail+"'")
    Wait-Find $a $identity.surface $tail
    if ((Invoke-Owned $a @('find',$marker,'--match-case','--surface',$identity.surface)).result.found) { throw 'Reduced scrollback retained evicted history' }
    $evidence.checks+=@{name='scrollback_limit_evicts_old_history_and_keeps_new_output'}

    Write-Config 'invalid settings preserved'
    $deadline=(Get-Date).AddSeconds(6)
    do { $errorState=Invoke-Owned $a @('settings','show'); if ($errorState.config_error) { break }; Start-Sleep -Milliseconds 100 } while ((Get-Date) -lt $deadline)
    if (-not $errorState.config_error -or $errorState.document.terminal.font_size -ne 18) { throw 'Invalid external file replaced live settings or stayed silent' }
    $c=Start-Owned @('--temporary')
    if (-not (Invoke-Owned $c @('settings','show')).config_error) { throw 'Invalid startup settings were not reported' }
    Reject $a @('settings','set','font-size','20')
    if ((Get-Content -Raw $configPath) -cne 'invalid settings preserved') { throw 'Invalid file was automatically overwritten' }
    $resetRequest=Invoke-Owned $a @('settings','reset')
    foreach ($hostItem in @($a,$b,$c)) { $reset=Wait-Settings $hostItem @{font_size=14;theme='dark';scrollback=10000;cursor_blink=$true;cursor_style='block'} $resetRequest.document.revision; if ($reset.config_error) { throw 'Explicit reset left a settings error' } }
    $evidence.checks+=@{name='corrupt_settings_keep_last_good_runtime_defaults_on_new_host_and_require_explicit_reset'}
    foreach ($hostItem in @($a,$b,$c)) { Tree $hostItem | Out-Null; Stop-Owned $hostItem }
    $evidence.status='passed_settings_subset'
} catch { $evidence.status='failed';$evidence.error=$_.Exception.Message;throw }
finally {
    foreach ($hostItem in $hosts) { if (-not $hostItem.process.HasExited) { $hostItem.process.Kill(); $hostItem.process.WaitForExit() } }
    $evidence.finished=(Get-Date).ToString('o')
    if($evidence.status -eq 'failed'){$evidence | ConvertTo-Json -Depth 30 | Set-Content -Encoding UTF8 (Join-Path $directory 'settings.json')}
}
[ordered]@{status=$evidence.status;checks=$evidence.checks.Count}|ConvertTo-Json -Compress
