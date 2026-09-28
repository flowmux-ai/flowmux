# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden owned hosts only. Selection is driven through IPC; clipboard is never accessed.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path
$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor=& $cli doctor|ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) {throw 'A working debug build is required; no host was launched.'}
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs')
$directory=Join-Path $(if ($env:FLOWMUX_TEST_ARTIFACT_ROOT) { $env:FLOWMUX_TEST_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\dist\evidence' }) ('selection-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$probe=Join-Path $directory 'selection-probe.exe';$control=Join-Path $directory 'control.txt'
Add-Type -Path (Join-Path $PSScriptRoot 'SelectionProbe.cs') -OutputAssembly $probe -OutputType ConsoleApplication
$pipeName=$null;$process=$null
$evidence=[ordered]@{started=(Get-Date).ToString('o');mode='background';checks=@();clipboardAccess=$false;desktopInput=$false}
function Request([string[]]$Arguments,[int]$Exit=0) {
    if (-not $script:pipeName) {throw 'Owned pipe required; refusing discovery fallback'}
    $p=[CliProbe]::Start($cli,(@('--pipe',$script:pipeName,'--json')+$Arguments),$directory,$directory)
    try {
        $out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
        if (-not $p.WaitForExit(30000)) {$p.Kill();$p.WaitForExit();throw 'Owned CLI timed out; not retried'}
        if (-not $out.Wait(3000) -or -not $err.Wait(3000)) {throw 'Owned output pipes did not close'}
        if ($p.ExitCode -ne $Exit) {throw "CLI exit $($p.ExitCode): $($err.Result)"}
        if ($Exit -eq 0) {return ($out.Result|ConvertFrom-Json)}
        return ($err.Result|ConvertFrom-Json)
    } finally {$p.Dispose()}
}
function Tree {
    $tree=Request @('tree');$window=[IntPtr]([long]$tree.window_handle)
    if (-not $tree.background_testing -or [CliProbe]::IsWindowVisible($window) -or [CliProbe]::GetForegroundWindow() -eq $window) {throw 'Owned host became visible or foreground'}
    return $tree
}
function Wait-Screen([string]$Text) {
    $deadline=(Get-Date).AddSeconds(20)
    do {
        $screen=Request @('read-screen','--surface',$script:surface)
        if ($screen.text.Contains($Text)) {return $screen}
        if ((Get-Date) -gt $deadline) {throw ('Missing probe output: '+$Text)}
        Start-Sleep -Milliseconds 30
    } while ($true)
}
function Control([string]$Action) {
    $id=[guid]::NewGuid().ToString('N')
    [IO.File]::WriteAllText(($control+'.tmp'),($id+':'+$Action),[Text.Encoding]::UTF8)
    if (-not [CliProbe]::MoveFileEx(($control+'.tmp'),$control,1)) {throw ('Owned control replacement failed: '+[Runtime.InteropServices.Marshal]::GetLastWin32Error())}
    if ($Action -ne 'exit') {return (Wait-Screen ('SELECTION_CONTROL_'+$id)).sequence}
}
function Selection([string[]]$Arguments=@('read')) {
    $reply=Request (@('selection','--surface',$script:surface)+$Arguments)
    if ($reply.surface -ne $script:surface) {throw 'Wrong selection target'}
    return $reply
}
function Exact($Reply,[string]$Text,[string]$Source='') {
    if ($Reply.result.error -or -not [string]::Equals($Reply.result.text,$Text,[StringComparison]::Ordinal)) {throw "Selection text differs: $($Reply|ConvertTo-Json -Depth 10 -Compress)"}
    if ($Source -and $Reply.result.source -ne $Source) {throw "Wrong selection source: $($Reply.result.source)"}
}
function Find([string]$Text) {
    $reply=Request @('find',$Text,'--surface',$script:surface,'--match-case')
    if (-not $reply.result.found -or -not [string]::Equals($reply.result.selection,$Text,[StringComparison]::Ordinal)) {throw 'Probe text was not found exactly'}
    return $reply.result
}
try {
    $process=[CliProbe]::Start($gui,@('--temporary',('--shell='+$probe),('--shell-arg='+$control)),$directory,$directory)
    $stdout=$process.StandardOutput.ReadToEndAsync();$stderr=$process.StandardError.ReadToEndAsync()
    $discovery=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($process.Id).json";$deadline=(Get-Date).AddSeconds(25)
    do {
        if ($process.HasExited -or (Get-Date) -gt $deadline) {throw 'Owned host startup failed'}
        if (Test-Path $discovery) {
            $record=Get-Content -Raw $discovery|ConvertFrom-Json
            if ($record.pid -ne $process.Id) {throw 'Wrong discovery owner'}
            $script:pipeName=$record.pipe;break
        }
        Start-Sleep -Milliseconds 50
    } while ($true)
    if ((Request @('identify')).pid -ne $process.Id) {throw 'Wrong pipe target'}
    do {
        $tree=Tree
        if ($tree.surfaces[0].ready) {break}
        if ((Get-Date) -gt $deadline) {throw 'Owned terminal not ready'}
        Start-Sleep -Milliseconds 50
    } while ($true)
    $script:surface=$tree.surfaces[0].id;$probePid=$tree.surfaces[0].pid
    $evidence.host=@{pid=$process.Id;probePid=$probePid;surface=$surface}
    Wait-Screen 'SELECTION_PROBE_READY'|Out-Null
    $original='SELECT_한글_한_😀_END'
    $found=Find $original
    $selected=Selection;Exact $selected $original 'live'
    $evidence.checks+=@{name='unicode_selection_keeps_composed_decomposed_and_emoji_codepoints';passed=$true;selection=$selected}
    $after=Control 'rewrite';$selected=Selection;Exact $selected $original 'retained'
    if ($selected.sequence -lt $after) {throw 'Selection preceded redraw parsing'}
    $evidence.checks+=@{name='same_cells_repaint_keep_original_copy_snapshot';passed=$true;selection=$selected}
    Control 'clear'|Out-Null;$selected=Selection;Exact $selected $original 'retained'
    $evidence.checks+=@{name='erase_display_retains_the_selected_snapshot';passed=$true;selection=$selected}
    Selection @('clear')|Out-Null;Exact (Selection) '' 'none'
    $evidence.checks+=@{name='explicit_clear_forgets_live_and_retained_selection';passed=$true}
    Find 'CLEARED_SCREEN'|Out-Null
    Request @('find','--surface',$surface,'--close')|Out-Null;Exact (Selection) '' 'none'
    Find 'CLEARED_SCREEN'|Out-Null
    $miss=Request @('find','NO_SUCH_SELECTION_428697','--surface',$surface)
    if ($miss.result.found) {throw 'Missing query unexpectedly matched'}
    Exact (Selection) '' 'none'
    $evidence.checks+=@{name='closing_or_missing_search_cannot_resurrect_a_previous_copy';passed=$true}
    Find 'CLEARED_SCREEN'|Out-Null
    Control 'alt_on'|Out-Null;Exact (Selection) '' 'none'
    $found=Find 'ALTERNATE_한글_😀';$start=$found.position.start;$end=$found.position.end
    Selection @('clear')|Out-Null
    $selected=Selection @('range',([string]$start.y),([string]$start.x),([string]($end.x-$start.x)))
    Exact $selected 'ALTERNATE_한글_😀' 'live'
    if ($selected.result.buffer -ne 'alternate') {throw 'Wrong selected buffer'}
    $evidence.checks+=@{name='cell_range_in_alternate_buffer_preserves_wide_characters';passed=$true;selection=$selected}
    $invalid=Selection @('range','4294967295','0','1')
    if (-not $invalid.result.error.Contains('outside')) {throw 'Invalid cell range was accepted'}
    Exact (Selection) 'ALTERNATE_한글_😀'
    Control 'alt_off'|Out-Null;Exact (Selection) '' 'none'
    $evidence.checks+=@{name='invalid_range_is_atomic_and_buffer_switch_discards_old_selection';passed=$true}
    Find 'BACK_TO_NORMAL'|Out-Null
    Request @('new-tab','--shell=cmd')|Out-Null
    $active=(Request @('identify')).surface
    Exact (Selection) 'BACK_TO_NORMAL'
    if ((Request @('identify')).surface -ne $active) {throw 'Selection read changed active tab'}
    Request @('new-workspace','--shell=cmd')|Out-Null
    $target=Request @('identify')
    Request @('move-tab',$surface,'--to-pane',$target.pane)|Out-Null
    Exact (Selection) 'BACK_TO_NORMAL'
    $moved=(Tree).surfaces|Where-Object {$_.id -eq $surface}
    if ($moved.pid -ne $probePid) {throw 'Moving the selection replaced its process'}
    $evidence.checks+=@{name='inactive_and_moved_surface_keep_selection_without_restarting';passed=$true}
    $all=Selection @('all')
    if ($all.result.error -or -not $all.result.text.Contains('BACK_TO_NORMAL')) {throw 'Select all omitted retained text'}
    Selection @('clear')|Out-Null
    Control 'large'|Out-Null
    $large=Selection @('all')
    if (-not $large.result.error.Contains('128 KiB') -or $large.result.text.Length -ne 0) {throw 'Oversized selection returned truncated or old text'}
    $evidence.checks+=@{name='select_all_rejects_oversize_without_truncating_or_falling_back';passed=$true;error=$large.result.error}
    Selection @('clear')|Out-Null
    Find 'LONG_SELECTION_2499_한글_'|Out-Null
    Control 'exit'
    $deadline=(Get-Date).AddSeconds(15)
    do {
        $stopped=(Tree).surfaces|Where-Object {$_.id -eq $surface}
        if ($stopped.resources_released -and $stopped.exit_code -eq 7) {break}
        if ((Get-Date) -gt $deadline) {throw 'Owned renderer did not exit'}
        Start-Sleep -Milliseconds 30
    } while ($true)
    Exact (Selection) 'LONG_SELECTION_2499_한글_'
    Selection @('clear')|Out-Null;Exact (Selection) '' 'none'
    $evidence.checks+=@{name='exited_terminal_remains_selectable_and_clearable';passed=$true}
    $evidence.status='passed_background_selection_subset'
} catch {
    $evidence.status='failed';$evidence.error=$_.Exception.Message
    throw
} finally {
    if ($pipeName) {try {Request @('quit','--discard-state')|Out-Null} catch {}}
    if ($process) {
        if (-not $process.HasExited -and -not $process.WaitForExit(10000)) {$process.Kill();$process.WaitForExit()}
        $process.Dispose()
    }
    $evidence.finished=(Get-Date).ToString('o')
    $evidence|ConvertTo-Json -Depth 12|Set-Content -Encoding UTF8 (Join-Path $directory 'native-selection-background.json')
    Write-Output ('Evidence: '+$directory)
}
$evidence|ConvertTo-Json -Depth 12
