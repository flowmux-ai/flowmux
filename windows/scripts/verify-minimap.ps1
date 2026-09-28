# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden owned hosts only. Read parsed screens through IPC; never touch desktop input or clipboard.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path
$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor=& $cli doctor|ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) {throw 'A working debug build is required; no host was launched.'}
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs')
$directory=Join-Path $(if ($env:FLOWMUX_TEST_ARTIFACT_ROOT) { $env:FLOWMUX_TEST_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\dist\evidence' }) ('minimap-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$probe=Join-Path $directory 'minimap-probe.exe';$control=Join-Path $directory 'control.txt'
Add-Type -Path (Join-Path $PSScriptRoot 'MinimapProbe.cs') -OutputAssembly $probe -OutputType ConsoleApplication
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
        $screen=Request @('read-screen','--surface',$script:surface,'--recent')
        if ($screen.text.Contains($Text)) {return $screen}
        if ((Get-Date) -gt $deadline) {throw ('Missing probe output: '+$Text)}
        Start-Sleep -Milliseconds 30
    } while ($true)
}
function Control([string]$Action) {
    $id=[guid]::NewGuid().ToString('N')
    [IO.File]::WriteAllText(($control+'.tmp'),($id+':'+$Action),[Text.Encoding]::UTF8)
    if (-not [CliProbe]::MoveFileEx(($control+'.tmp'),$control,1)) {throw ('Owned control replacement failed: '+[Runtime.InteropServices.Marshal]::GetLastWin32Error())}
    if ($Action -ne 'exit') {return (Wait-Screen ('MINIMAP_CONTROL_'+$id)).sequence}
}

function Map([string[]]$Arguments=@('read'),[int]$Exit=0) {
    $reply=Request (@('minimap','--surface',$script:surface)+$Arguments) $Exit
    if ($Exit -ne 0) {return $reply}
    if ($reply.surface -ne $script:surface) {throw 'Wrong minimap target'}
    $script:lastMap=$reply.result
    return $reply.result
}
function Set-Map([string]$Key,[string]$Value) {
    $changed=Request @('settings','set',$Key,$Value)
    $deadline=(Get-Date).AddSeconds(15)
    do {
        $status=Request @('settings','show')
        $applied=@($status.surfaces|Where-Object { -not $_.applied -or $_.applied.revision -ne $status.document.revision })
        if ($status.document.revision -eq $changed.document.revision -and $applied.Count -eq 0) {return $status}
        if ((Get-Date) -gt $deadline) {throw 'Minimap settings did not apply'}
        Start-Sleep -Milliseconds 50
    } while ($true)
}
function Dims($Map) {
    Control 'dims'|Out-Null
    Wait-Screen ('DIMS_'+$Map.cols+'_'+$Map.rows)|Out-Null
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


    Wait-Screen 'MINIMAP_PROBE_READY'|Out-Null
    $first=Map
    if (-not $first.enabled -or -not $first.visible -or $first.width -ne 40 -or $first.opacity -ne 50 -or $first.gutter -ne 40 -or -not $first.raster_hash -or $first.wide_cells -le 0 -or -not ($first.colors|Where-Object {$_.color -eq '#112233'})) {throw 'Initial raster/defaults/truecolor/wide cells differ'}
    Dims $first
    $evidence.checks+=@{name='default_minimap_renders_native_unicode_cells_and_truecolor';passed=$true;map=$first}
    $before=Map;Control 'rewrite'|Out-Null;$rewrite=Map
    if ($rewrite.raster_hash -eq $before.raster_hash -or -not ($rewrite.colors|Where-Object {$_.color -eq '#445566'})) {throw 'In-place redraw did not update raster'}
    $evidence.checks+=@{name='same_cell_rewrite_updates_real_canvas';passed=$true}
    Control 'history'|Out-Null
    $history=Map
    if ($history.buffer_rows -lt 2500 -or $history.preview_top -le 0 -or $history.preview_rows -gt 2048 -or $history.cells -gt ($history.preview_rows*$history.cols)) {throw 'History raster not bounded to preview'}
    $preview=Map @('preview','-25')
    if ($preview.preview_top -ne ($history.preview_top-25) -or $preview.viewport_row -ne $history.viewport_row) {throw 'Preview scrolled terminal or used wrong row step'}
    $evidence.checks+=@{name='bounded_preview_scroll_moves_25_rows_without_terminal_scroll';passed=$true;history=$history;preview=$preview}
    $seek=Map @('seek','10')
    $screen=Request @('read-screen','--surface',$surface)
    if ($seek.viewport_row -ne 0 -or -not $screen.text.Contains('HISTORY_0000_한글')) {throw 'Seek did not move actual viewport to old output'}
    $find=Request @('find','HISTORY_0000_한글','--surface',$surface,'--match-case')
    if (-not $find.result.found) {throw 'Probe history lost'}
    $selected=(Request @('selection','--surface',$surface,'read')).result.text
    Map @('preview','-25')|Out-Null;Map @('seek','100')|Out-Null
    if ((Request @('selection','--surface',$surface,'read')).result.text -cne $selected) {throw 'Minimap navigation changed selection'}
    $evidence.checks+=@{name='seek_scrolls_actual_viewport_and_preserves_selection';passed=$true}
    $normal=Map;Control 'alt_on'|Out-Null;$alt=Map
    if ($alt.visible -or $alt.buffer -ne 'alternate' -or $alt.gutter -ne $normal.gutter -or $alt.cols -ne $normal.cols -or $alt.rows -ne $normal.rows -or $alt.cells -ne 0) {throw 'Alternate transition resized terminal or retained raster work'}
    $reject=Map @('seek','0') 1
    if (-not $reject.error.Contains('unavailable')) {throw 'Alternate minimap navigation accepted'}
    Control 'alt_off'|Out-Null;$back=Map
    if (-not $back.visible -or $back.cols -ne $normal.cols -or $back.gutter -ne $normal.gutter -or $back.ink_cells -le 0) {throw 'Normal minimap not restored'}
    Dims $back
    $evidence.checks+=@{name='alternate_transition_keeps_gutter_columns_and_pty_dimensions';passed=$true;alternate=$alt;normal=$back}
    Set-Map 'minimap-width' '96'|Out-Null;$wide=Map;Dims $wide
    if ($wide.width -ne 96 -or $wide.gutter -ne 96 -or $wide.cols -ge $back.cols) {throw 'Width was not applied to gutter and ConPTY columns'}
    Set-Map 'minimap-opacity' '0'|Out-Null
    if ((Map).opacity -ne 0) {throw 'Zero opacity not accepted'}
    Set-Map 'minimap-opacity' '100'|Out-Null
    Set-Map 'minimap-enabled' 'false'|Out-Null;$off=Map;Dims $off
    if ($off.visible -or $off.gutter -ne 0 -or $off.cols -le $wide.cols -or $off.cells -ne 0 -or $off.preview_offset -ne 0) {throw 'Disabled minimap did not release gutter and raster'}
    Set-Map 'minimap-width' '12'|Out-Null
    Set-Map 'minimap-enabled' 'true'|Out-Null;$on=Map;Dims $on
    if (-not $on.visible -or $on.width -ne 12 -or $on.gutter -ne 12 -or $on.opacity -ne 100) {throw 'Minimum width and full opacity not applied'}
    $config=Get-Content -Raw -Encoding UTF8 (Join-Path $directory 'config\config.json')|ConvertFrom-Json
    if ($config.terminal.minimap_width -ne 12 -or $config.terminal.minimap_opacity -ne 100 -or -not $config.terminal.minimap_enabled) {throw 'Settings were not persisted'}
    $evidence.checks+=@{name='live_width_opacity_toggle_persist_and_resize_only_on_settings_change';passed=$true;wide=$wide;disabled=$off;enabled=$on}
    $hash=(Get-FileHash (Join-Path $directory 'config\config.json')).Hash
    Request @('settings','set','minimap-width','11') 1|Out-Null
    Request @('settings','set','minimap-opacity','101') 1|Out-Null
    Map @('seek','4294967295') 1|Out-Null
    if ((Get-FileHash (Join-Path $directory 'config\config.json')).Hash -ne $hash) {throw 'Invalid values changed persistent settings'}
    $evidence.checks+=@{name='invalid_settings_and_stale_row_are_atomic_errors';passed=$true}
    Request @('new-tab','--shell=cmd')|Out-Null
    $active=(Request @('identify')).surface;$hidden=Map
    Control 'history'|Out-Null;$hiddenAfter=Map
    if ($hiddenAfter.visible -or $hiddenAfter.cells -ne 0 -or $hiddenAfter.renders -ne $hidden.renders -or (Request @('identify')).surface -ne $active) {throw 'Inactive surface painted or read changed focus'}
    if (-not (Request @('read-screen','--surface',$surface,'--recent')).text.Contains('HISTORY_2499_한글')) {throw 'Hidden parsing stopped'}
    Request @('focus-tab',$surface)|Out-Null;$shown=Map
    if (-not $shown.visible -or $shown.renders -le $hidden.renders -or $shown.ink_cells -le 0) {throw 'Returning to tab did not repaint latest buffer'}
    $evidence.checks+=@{name='hidden_tab_keeps_parsing_without_minimap_paint_and_rebuilds_on_return';passed=$true}
    Control 'clear_history'|Out-Null;$cleared=Map
    if ($cleared.preview_top -ne 0 -or $cleared.preview_offset -ne 0 -or $cleared.buffer_rows -ge $shown.buffer_rows -or $cleared.raster_hash -eq $shown.raster_hash) {throw 'Cleared history retained stale preview rows'}
    Request @('new-workspace','--shell=cmd')|Out-Null;$target=Request @('identify')
    Request @('move-tab',$surface,'--to-pane',$target.pane)|Out-Null;$moved=Map
    $movedSurface=(Tree).surfaces|Where-Object {$_.id -eq $surface}
    if ($movedSurface.pid -ne $probePid -or -not $moved.visible -or $moved.ink_cells -le 0) {throw 'Moved minimap lost process or view'}
    Control 'exit';$deadline=(Get-Date).AddSeconds(15)
    do {
        $stopped=(Tree).surfaces|Where-Object {$_.id -eq $surface}
        if ($stopped.resources_released -and $stopped.exit_code -eq 7) {break}
        if ((Get-Date) -gt $deadline) {throw 'Owned renderer did not exit'}
        Start-Sleep -Milliseconds 30
    } while ($true)
    if ((Map).ink_cells -le 0) {throw 'Exited terminal lost minimap'}
    $evidence.checks+=@{name='clear_history_move_and_process_exit_keep_minimap_current';passed=$true}
    $evidence.status='passed_background_minimap_subset'
} catch {
    $evidence.status='failed';$evidence.error=$_.Exception.Message;$evidence.lastMap=$script:lastMap
    throw
} finally {
    if ($pipeName) {try {Request @('quit','--discard-state')|Out-Null} catch {}}
    if ($process) {
        if (-not $process.HasExited -and -not $process.WaitForExit(10000)) {$process.Kill();$process.WaitForExit()}
        $process.Dispose()
    }
    $evidence.finished=(Get-Date).ToString('o')
    if($evidence.status -eq 'failed'){$evidence|ConvertTo-Json -Depth 12|Set-Content -Encoding UTF8 (Join-Path $directory 'native-minimap-background.json')}
    if($evidence.status -eq 'failed'){Write-Output ('Evidence: '+$directory)}
}
[ordered]@{status=$evidence.status;checks=$evidence.checks.Count}|ConvertTo-Json -Compress
