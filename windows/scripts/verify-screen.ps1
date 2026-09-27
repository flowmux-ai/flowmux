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
$directory=Join-Path $PSScriptRoot ('..\dist\evidence\screen-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$probe=Join-Path $directory 'screen-probe.exe';$control=Join-Path $directory 'control.txt'
Add-Type -Path (Join-Path $PSScriptRoot 'ScreenProbe.cs') -OutputAssembly $probe -OutputType ConsoleApplication
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
    if ($Action -ne 'exit') {return (Wait-Screen ('SCREEN_CONTROL_'+$id)).sequence}
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

    Wait-Screen 'SCREEN_PROBE_READY'|Out-Null
    $initial=Request @('read-screen','--surface',$surface)
    if (-not $initial.text.Contains('INITIAL_한글_한_é_😀') -or -not $initial.text.Contains('GREEN_한글') -or $initial.text.Contains([string][char]27)) {throw 'Unicode or parsed color text differs'}
    if ($initial.screen.row_count -ne $initial.screen.rows -or $initial.screen.mode -ne 'viewport') {throw 'Wrong viewport metadata'}
    $capture=Request @('capture-pane','--surface',$surface)
    if (-not [string]::Equals($capture.text,$initial.text,[StringComparison]::Ordinal)) {throw 'Capture alias differs from read-screen'}
    $evidence.checks+=@{name='capture_alias_reads_exact_unicode_and_parsed_plain_text';passed=$true;screen=$initial.screen}
    $after=Control 'rewrite'
    $rewritten=Request @('capture-pane','--surface',$surface)
    if ($rewritten.sequence -lt $after -or -not $rewritten.text.Contains('REPLACED_다른') -or -not $rewritten.text.Contains('CURRENT_한글') -or $rewritten.text.Contains('INITIAL_') -or $rewritten.text.Contains('OBSOLETE')) {throw 'Read returned old transcript instead of rewritten cells'}
    $evidence.checks+=@{name='overwrite_carriage_return_and_erase_are_read_from_completed_grid';passed=$true}
    Control 'history'|Out-Null
    $recent=Request @('read-screen','--surface',$surface,'--recent')
    if ($recent.screen.row_count -ne 80 -or $recent.screen.first_row -ne ($recent.screen.buffer_rows-80) -or -not $recent.text.Contains('HISTORY_0399_한글') -or $recent.text.Contains('HISTORY_0000')) {throw 'Recent normal range differs'}
    $found=Request @('find','HISTORY_0000_한글','--surface',$surface,'--match-case')
    if (-not $found.result.found) {throw 'Old history not retained'}
    $selectionBefore=Request @('selection','--surface',$surface,'read')
    $scrolled=Request @('read-screen','--surface',$surface)
    if (-not $scrolled.text.Contains('HISTORY_0000_한글') -or $scrolled.screen.viewport_row -ge $scrolled.screen.base_row) {throw 'Find did not scroll to old history'}
    $recentAgain=Request @('capture-pane','--surface',$surface,'--recent')
    $scrolledAgain=Request @('read-screen','--surface',$surface)
    $selectionAfter=Request @('selection','--surface',$surface,'read')
    if (-not [string]::Equals($recentAgain.text,$recent.text,[StringComparison]::Ordinal) -or $scrolledAgain.screen.viewport_row -ne $scrolled.screen.viewport_row -or -not [string]::Equals($selectionAfter.result.text,$selectionBefore.result.text,[StringComparison]::Ordinal)) {throw 'Recent read changed scroll position or selection'}
    $evidence.checks+=@{name='recent_80_rows_ignore_scrolled_view_without_changing_selection_or_viewport';passed=$true;recent=$recentAgain.screen;viewport=$scrolled.screen}
    Control 'wrap'|Out-Null
    $wrapped=Request @('read-screen','--surface',$surface,'--recent')
    if (-not $wrapped.text.Replace("`n",'').Contains('한글_WRAP_한_😀') -or $wrapped.text.Contains('한글_WRAP_한_😀')) {throw 'Physical wrap or Unicode content differs'}
    $evidence.checks+=@{name='physical_wrap_breaks_preserve_korean_jamo_and_emoji';passed=$true}
    Control 'cursor_top'|Out-Null
    $cursor=Request @('read-screen','--surface',$surface,'--recent')
    if ($cursor.screen.cursor.row -ne $cursor.screen.base_row -or -not $cursor.text.Contains('HISTORY_0399_한글')) {throw 'Recent range followed the cursor instead of buffer bottom'}
    $evidence.checks+=@{name='normal_recent_range_includes_rows_below_cursor';passed=$true;screen=$cursor.screen}
    Control 'alt_on'|Out-Null
    $alt=Request @('read-screen','--surface',$surface,'--recent')
    if ($alt.screen.buffer -ne 'alternate' -or $alt.screen.first_row -ne 0 -or $alt.screen.row_count -ne $alt.screen.rows -or $alt.screen.cursor.row -ne 0 -or -not $alt.text.Contains('ALT_TOP_한글') -or -not $alt.text.Contains('FOOTER_한글_😀') -or $alt.text.Contains('HISTORY_')) {throw 'Alternate read omitted footer or mixed normal history'}
    $evidence.checks+=@{name='alternate_recent_reads_whole_screen_including_footer_below_cursor';passed=$true;screen=$alt.screen}
    Control 'alt_off'|Out-Null
    $normal=Request @('capture-pane','--surface',$surface,'--recent')
    if ($normal.screen.buffer -ne 'normal' -or -not $normal.text.Contains('BACK_TO_NORMAL') -or $normal.text.Contains('FOOTER_')) {throw 'Normal restore mixed alternate cells'}
    $evidence.checks+=@{name='alternate_exit_restores_separate_normal_buffer';passed=$true}
    Request @('new-tab','--shell=cmd')|Out-Null
    $active=(Request @('identify')).surface
    Control 'history'|Out-Null
    $hidden=Request @('read-screen','--surface',$surface,'--recent')
    if (-not $hidden.text.Contains('HISTORY_0399_한글') -or (Request @('identify')).surface -ne $active) {throw 'Hidden screen read changed active tab or stopped parsing'}
    Request @('new-workspace','--shell=cmd')|Out-Null
    $target=Request @('identify')
    Request @('move-tab',$surface,'--to-pane',$target.pane)|Out-Null
    $moved=Request @('capture-pane','--surface',$surface,'--recent')
    $movedSurface=(Tree).surfaces|Where-Object {$_.id -eq $surface}
    if ($movedSurface.pid -ne $probePid -or -not $moved.text.Contains('HISTORY_0399_한글')) {throw 'Moved screen lost its process or recent rows'}
    $evidence.checks+=@{name='inactive_and_moved_surfaces_parse_and_read_without_process_restart';passed=$true}
    $invalid=Request @('read-screen','--surface',([guid]::NewGuid().ToString()),'--recent') 1
    if (-not $invalid.error) {throw 'Missing surface did not fail'}
    if (-not (Request @('read-screen','--surface',$surface,'--recent')).text.Contains('HISTORY_0399_한글')) {throw 'Invalid target damaged live reads'}
    $evidence.checks+=@{name='invalid_surface_returns_error_without_changing_live_terminal';passed=$true}
    Control 'exit'
    $deadline=(Get-Date).AddSeconds(15)
    do {
        $stopped=(Tree).surfaces|Where-Object {$_.id -eq $surface}
        if ($stopped.resources_released -and $stopped.exit_code -eq 7) {break}
        if ((Get-Date) -gt $deadline) {throw 'Owned renderer did not exit'}
        Start-Sleep -Milliseconds 30
    } while ($true)
    $exited=Request @('capture-pane','--surface',$surface,'--recent')
    if (-not $exited.text.Contains('SCREEN_EXITED')) {throw 'Exited screen became unreadable'}
    $evidence.checks+=@{name='exited_terminal_keeps_its_last_parsed_screen';passed=$true}
    $evidence.status='passed_background_screen_subset'
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
    $evidence|ConvertTo-Json -Depth 12|Set-Content -Encoding UTF8 (Join-Path $directory 'native-screen-background.json')
    Write-Output ('Evidence: '+$directory)
}
$evidence|ConvertTo-Json -Depth 12
