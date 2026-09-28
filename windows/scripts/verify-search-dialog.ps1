# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden owned modal dialog + actual terminal search; no desktop input or clipboard.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
if (-not $env:FLOWMUX_TEST_ARTIFACT_ROOT) { throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.' }
$ErrorActionPreference='Stop'
$budget=[Diagnostics.Stopwatch]::StartNew()
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path
$cli=Join-Path $BuildDirectory 'flowmuxctl.exe';$gui=Join-Path $BuildDirectory 'flowmux.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs')
$directory=Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT ('search-dialog-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$evidence=[ordered]@{started=(Get-Date).ToUniversalTime().ToString('o');mode='hidden-native-search-dialog';checks=@();observations=@();desktopInput=$false;clipboardAccess=$false;physicalIme=$false}
$process=$null;$pipeName=$null;$hostOut=$null;$hostErr=$null;$cleaningUp=$false
$needle='SEARCH_DIALOG_한글_한_😀'
function Assert-Budget {
    if($budget.Elapsed.TotalSeconds -ge 55){throw 'Search-dialog verification exceeded its 55-second work budget'}
}
function Invoke-Owned([string[]]$Arguments) {
    if(-not $script:cleaningUp){Assert-Budget}
    $child=[CliProbe]::Start($cli,$Arguments,$directory,$directory)
    $out=$child.StandardOutput.ReadToEndAsync();$err=$child.StandardError.ReadToEndAsync()
    try {
        $timeout=if($script:cleaningUp){5000}else{[Math]::Max(1,[Math]::Min(5000,55000-[int]$budget.ElapsedMilliseconds))}
        if(-not $child.WaitForExit($timeout)){throw 'Owned CLI exceeded 5 seconds; no retry'}
        if(-not $out.Wait(1000) -or -not $err.Wait(1000)){throw 'Owned CLI pipes did not close'}
        if($child.ExitCode -ne 0){throw ('Owned CLI failed: '+[CliProbe]::Output($err))}
        return ([CliProbe]::Output($out)|ConvertFrom-Json)
    } finally {if(-not $child.HasExited){$child.Kill();[CliProbe]::WaitAfterKill($child)};$child.Dispose()}
}
function Request([string[]]$Arguments) {
    if(-not $script:pipeName){throw 'Explicit owned pipe required'}
    return (Invoke-Owned (@('--pipe',$script:pipeName,'--json')+$Arguments))
}
function Tree {
    $tree=Request @('tree');$owner=[IntPtr]([long]$tree.window_handle)
    if(-not $tree.background_testing -or [CliProbe]::IsWindowVisible($owner) -or [CliProbe]::GetForegroundWindow() -eq $owner){throw 'Owned host appeared or gained foreground'}
    if($tree.search_dialog){$dialog=[IntPtr]([long]$tree.search_dialog.window);if([CliProbe]::IsWindowVisible($dialog) -or [CliProbe]::GetForegroundWindow() -eq $dialog){throw 'Owned dialog appeared or gained foreground'}}
    return $tree
}
function Wait-Dialog([scriptblock]$Condition,[string]$Failure) {
    $deadline=(Get-Date).AddSeconds(8)
    do {
        Assert-Budget
        $tree=Tree
        if(& $Condition $tree.search_dialog){return $tree.search_dialog}
        if((Get-Date) -ge $deadline){$evidence.observations+=@{kind='condition-timeout';message=$Failure;tree=$tree};throw $Failure}
        Start-Sleep -Milliseconds 20
    }while($true)
}
function Wait-Search($Result) {
    $deadline=(Get-Date).AddSeconds(8)
    while($Result.pending){if((Get-Date) -ge $deadline){throw 'Owned search exceeded 8 seconds'};Start-Sleep -Milliseconds 20;$Result=Request @('search-results',$Result.search)}
    if($Result.cancelled -or $Result.unavailable.Count -ne 0){throw 'Owned fixture search cancelled or unavailable'}
    return $Result
}
try {
    $doctor=Invoke-Owned @('doctor')
    if(-not $doctor.background_testing){throw 'Debug background host required'}
    $probe=Join-Path $directory 'search-dialog-probe.exe'
    Add-Type -Path (Join-Path $PSScriptRoot 'SearchDialogFixture.cs') -OutputAssembly $probe -OutputType ConsoleApplication
    [Reflection.Assembly]::LoadFrom($probe)|Out-Null
    $process=[CliProbe]::Start($gui,@('--temporary',('--shell='+$probe)),$directory,$directory)
    $hostOut=$process.StandardOutput.ReadToEndAsync();$hostErr=$process.StandardError.ReadToEndAsync()
    $discovery=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($process.Id).json";$deadline=(Get-Date).AddSeconds(8)
    do {
        if($process.HasExited -or (Get-Date) -ge $deadline){throw 'Owned host startup exceeded 8 seconds'}
        if(Test-Path $discovery){$record=Get-Content -Raw $discovery|ConvertFrom-Json;if($record.pid -ne $process.Id){throw 'Discovery owner mismatch'};$script:pipeName=$record.pipe;break}
        Start-Sleep -Milliseconds 30
    }while($true)
    if((Request @('identify')).pid -ne $process.Id){throw 'Wrong explicit pipe target'}
    do {
        Assert-Budget
        $tree=Tree
        if($tree.surfaces[0].ready){$screen=Request @('read-screen','--surface',$tree.surfaces[0].id,'--recent');if($screen.text.Contains('SEARCH_DIALOG_PROBE_READY')){break}}
        if((Get-Date) -ge $deadline){throw 'Fixture output did not parse within startup budget'}
        Start-Sleep -Milliseconds 30
    }while($true)
    $surface=$tree.surfaces[0].id;$owner=[IntPtr]([long]$tree.window_handle)
    $evidence.host=@{pid=$process.Id;surface=$surface;probePid=$tree.surfaces[0].pid;window=$owner.ToInt64()}
    $page=Wait-Search (Request @('search-all',$needle,'--match-case','--offset','500'))
    if($page.offset -ne 500 -or $page.hits.Count -ne 205 -or $page.total -ne 705 -or $page.page_size -ne 500){throw 'Existing CLI paging contract changed'}
    $page=Wait-Search (Request @('search-all',$needle,'--match-case'))
    if($page.hits.Count -ne 500 -or $page.total -ne 705 -or $page.panel_rows -ne 500 -or -not [SearchDialogFixture]::IsWindowEnabled($owner)){throw 'CLI search changed modality or initial page'}
    $tree=Tree
    $searchButtons=@($tree.chrome.controls|Where-Object {$_.kind -eq 'search_all'})
    if($searchButtons.Count -ne 1){throw 'Expected exactly one native search entry'}
    $button=[IntPtr]([long]$searchButtons[0].handle)
    [SearchDialogFixture]::Click($button,$process.Id)
    $dialog=Wait-Dialog {param($d)$d.open -and -not $d.owner_enabled} 'Native Show did not disable only its owner'
    $hwnd=[IntPtr]([long]$dialog.window);$dpi=[SearchDialogFixture]::GetDpiForWindow($hwnd)
    $evidence.observations+=@{kind='modal-open';dialog=$dialog;owner=[SearchDialogFixture]::GetWindow($hwnd,4).ToInt64();nativeOwnerEnabled=[SearchDialogFixture]::IsWindowEnabled($owner);dpi=$dpi}
    if(-not $dialog.modal -or [SearchDialogFixture]::GetWindow($hwnd,4) -ne $owner -or [SearchDialogFixture]::IsWindowEnabled($owner) -or $dialog.rect.width -ne [Math]::Round(760*$dpi/96) -or $dialog.rect.height -ne [Math]::Round(520*$dpi/96)){throw 'Search is not the intended 760x520 modal owned dialog'}
    if($dialog.query -cne $needle -or [SearchDialogFixture]::Text([IntPtr]([long]$dialog.query_handle),$process.Id) -cne $needle -or -not $dialog.more_visible){throw 'Original query or Show more state differs'}
    $nativeLabel=[SearchDialogFixture]::RowText([IntPtr]([long]$dialog.list_handle),0,$process.Id)
    if($nativeLabel -cne $dialog.labels[0] -or -not $nativeLabel.Contains("`n") -or -not $nativeLabel.Contains($needle)){throw 'Native descriptive result label differs from its original Unicode text'}
    $evidence.checks+=@{name='cli_paging_unchanged_and_hidden_owned_modal_with_descriptive_rows';passed=$true}
    $oldSearch=$dialog.search
    [SearchDialogFixture]::Click([IntPtr]([long]$dialog.more_handle),$process.Id)
    $dialog=Wait-Dialog {param($d)$d.search -ne $oldSearch -and -not $d.pending -and $d.rows -eq 700} 'Show more did not retain a fresh bounded larger result set'
    $expanded=Request @('search-results',$dialog.search)
    if($expanded.hits.Count -ne 700 -or $expanded.total -ne 705 -or $expanded.result_limit -ne 700 -or $dialog.more_visible -or -not $dialog.status.Contains('700-result display limit')){throw 'Result bound is missing or silently truncates UI'}
    $evidence.checks+=@{name='show_more_refreshes_valid_hit_ids_and_explicit_700_result_bound';passed=$true;dialog=$dialog}
    [SearchDialogFixture]::ActivateRow([IntPtr]([long]$dialog.list_handle),699,$process.Id)
    $closed=Wait-Dialog {param($d)-not $d.open -and $d.owner_enabled} 'Result activation did not close and re-enable owner'
    $selected=Request @('selection','--surface',$surface,'read')
    if($selected.result.text -cne $needle -or -not [SearchDialogFixture]::IsWindowEnabled($owner)){throw 'Expanded result selected stale/wrong terminal cells or kept owner disabled'}
    $evidence.checks+=@{name='actual_native_result_activation_selects_same_original_unicode_and_reenables_owner';passed=$true;selection=$selected.result}
    $tree=Tree
    $searchButtons=@($tree.chrome.controls|Where-Object {$_.kind -eq 'search_all'})
    if($searchButtons.Count -ne 1){throw 'Expected exactly one native search entry'}
    $button=[IntPtr]([long]$searchButtons[0].handle)
    [SearchDialogFixture]::Click($button,$process.Id)
    $dialog=Wait-Dialog {param($d)$d.open -and -not $d.owner_enabled} 'Reopening search did not restore modality'
    $missing='없는결과_한_😀'
    [SearchDialogFixture]::SetText([IntPtr]([long]$dialog.query_handle),$missing,$process.Id)
    $dialog=Wait-Dialog {param($d)$d.query -ceq $missing -and -not $d.pending -and $d.rows -eq 0 -and $d.status.Contains('0 of 0')} 'Live native query change did not show empty results'
    if([SearchDialogFixture]::Text([IntPtr]([long]$dialog.query_handle),$process.Id) -cne $missing){throw 'Native query normalization changed original text'}
    [SearchDialogFixture]::SetText([IntPtr]([long]$dialog.query_handle),('x'*1100),$process.Id)
    $dialog=Wait-Dialog {param($d)$d.status.Contains('at most 1024')} 'Invalid query did not report bounded validation error'
    [SearchDialogFixture]::Close([IntPtr]([long]$dialog.window),$process.Id)
    $dialog=Wait-Dialog {param($d)-not $d.open -and $d.owner_enabled} 'Closing error state failed to release modal owner'
    $evidence.checks+=@{name='native_query_empty_error_and_close_preserve_text_and_release_owner';passed=$true;dialog=$dialog}
    Tree|Out-Null
    $evidence.status='passed_hidden_search_dialog_subset'
    $evidence.deferred=@('Physical keyboard/IME candidate and composition acceptance, accessibility, per-monitor DPI, real activation/focus restoration, and composed desktop/WebView screenshots remain unverified.')
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
    $script:cleaningUp=$true
    if($process){
        try {
            if(-not $process.HasExited -and $pipeName){Request @('quit','--discard-state')|Out-Null}
            if(-not $process.WaitForExit(5000)){
                $process.Kill();[CliProbe]::WaitAfterKill($process)
                throw 'Owned host required forced cleanup'
            }
        } catch {
            $evidence.status='failed';$evidence.cleanupError=$_.Exception.Message
            try {if(-not $process.HasExited){$process.Kill();[CliProbe]::WaitAfterKill($process)}}
            catch {$evidence.cleanupTerminationError=$_.Exception.Message}
        }
        $outDone=if($hostOut){$hostOut.Wait(1000)}else{$false};$errDone=if($hostErr){$hostErr.Wait(1000)}else{$false}
        $exitCode=if($process.HasExited){$process.ExitCode}else{$null}
        if(-not $process.HasExited -or $exitCode -ne 0 -or -not $outDone -or -not $errDone){
            $evidence.status='failed'
            if(-not $evidence.cleanupError){$evidence.cleanupError='Owned host did not exit cleanly with both output pipes closed'}
        }
        $evidence.observations+=@{kind='host-exit';pid=$process.Id;exitCode=$exitCode;stdoutComplete=$outDone;stderrComplete=$errDone;stdout=[CliProbe]::Output($hostOut);stderr=[CliProbe]::Output($hostErr)}
        $process.Dispose()
    }
    $evidence.finished=(Get-Date).ToUniversalTime().ToString('o')
    $evidence.elapsedMs=$budget.ElapsedMilliseconds
    $evidencePath=Join-Path $directory 'native-search-dialog-background.json'
    if($evidence.status -eq 'failed'){$evidence|ConvertTo-Json -Depth 18|Set-Content -Encoding UTF8 $evidencePath;Write-Output ('Evidence: '+$evidencePath)}
    [ordered]@{status=$evidence.status;checkCount=$evidence.checks.Count;elapsedMs=$evidence.elapsedMs}|ConvertTo-Json -Compress
}
if($evidence.status -ne 'passed_hidden_search_dialog_subset'){
    throw ('Search dialog verification failed: '+$evidence.error+' '+$evidence.cleanupError)
}
