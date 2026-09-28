# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden owned WebView2 host + loopback fixture only. No foreground, input, clipboard or external sites.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",[ValidateSet("all","files","broken","capacity","close","shutdown")][string]$Case="all")
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path
$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor=& $cli doctor|ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) {throw 'Working debug build required; no host launched'}
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs')
Add-Type -Path (Join-Path $PSScriptRoot 'DownloadFixture.cs')
$directory=Join-Path $(if ($env:FLOWMUX_TEST_ARTIFACT_ROOT) { $env:FLOWMUX_TEST_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\dist\evidence' }) ('browser-downloads-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$fixture=New-Object DownloadFixture;$origin=$fixture.Origin
$pipeName=$null;$process=$null;$hosts=@();$shells=@();$extraFixtures=@()
foreach($file in @('flowmux.exe','flowmuxctl.exe','flowmux-command.exe')) {
    $probe=[CliProbe]::Start((Join-Path $BuildDirectory $file),@('doctor'),$directory,$directory)
    try {
        $out=$probe.StandardOutput.ReadToEndAsync();$err=$probe.StandardError.ReadToEndAsync()
        if(-not $probe.WaitForExit(5000)){$probe.Kill();[CliProbe]::WaitAfterKill($probe);throw 'Owned entry-point doctor timed out'}
        if(-not $out.Wait(3000) -or -not $err.Wait(3000) -or $probe.ExitCode -ne 0){throw ('Entry-point parser failed: '+$file+' '+([CliProbe]::Output($err)))}
        if(-not ($out.Result|ConvertFrom-Json).background_testing){throw 'Wrong entry-point build'}
    } finally {$probe.Dispose()}
}

$evidence=[ordered]@{started=(Get-Date).ToString('o');mode='background';checks=@();clipboardAccess=$false;desktopInput=$false;externalSites=$false;unicodeComparison='ordinal';case=$Case}
function Request([string[]]$Arguments,[int]$Exit=0) {
    if (-not $script:pipeName) {throw 'Owned pipe required'}
    $p=[CliProbe]::Start($cli,(@('--pipe',$script:pipeName,'--json')+$Arguments),$directory,$directory)
    try {
        $out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
        if (-not $p.WaitForExit(5000)) {$p.Kill();[CliProbe]::WaitAfterKill($p);throw 'Owned CLI timed out; not retried'}
        if (-not $out.Wait(3000) -or -not $err.Wait(3000)) {throw 'Owned output did not close'}
        if ($p.ExitCode -ne $Exit) {throw "CLI $Arguments exit $($p.ExitCode): $(([CliProbe]::Output($err))) $($out.Result)"}
        if ($Exit -eq 0) {return ($out.Result|ConvertFrom-Json)}
        return (([CliProbe]::Output($err))|ConvertFrom-Json)
    } finally {$p.Dispose()}
}
function Raw-Request($Body) {
    if (-not $script:pipeName) {throw 'Owned pipe required'}
    $stream=[IO.Pipes.NamedPipeClientStream]::new('.',$script:pipeName.Substring(9),[IO.Pipes.PipeDirection]::InOut,[IO.Pipes.PipeOptions]::Asynchronous,[Security.Principal.TokenImpersonationLevel]::Identification)
    try {
        $stream.Connect(3000)
        $writer=[IO.StreamWriter]::new($stream,(New-Object Text.UTF8Encoding($false)),4096,$true)
        $reader=[IO.StreamReader]::new($stream,(New-Object Text.UTF8Encoding($false)),$false,4096,$true)
        try {
            $writer.WriteLine(($Body|ConvertTo-Json -Depth 10 -Compress));$writer.Flush()
            $read=$reader.ReadLineAsync();if (-not $read.Wait(20000)) {throw 'Owned browser fixture request timed out; not retried'}
            return ($read.Result|ConvertFrom-Json)
        } finally {$writer.Dispose();$reader.Dispose()}
    } finally {$stream.Dispose()}
}
function Tree {
    $tree=Request @('tree');$window=[IntPtr]([long]$tree.window_handle)
    if (-not $tree.background_testing -or [CliProbe]::IsWindowVisible($window) -or [CliProbe]::GetForegroundWindow() -eq $window) {throw 'Owned host became visible or foreground'}
    foreach ($browser in $tree.browsers) {
        if ([CliProbe]::IsWindowVisible([IntPtr]([long]$browser.view_handle)) -or [CliProbe]::IsWindowVisible([IntPtr]([long]$browser.chrome_handle))) {throw 'Owned browser became visible'}
    }
    return $tree
}
function Start-Owned([string[]]$Launch) {
    $script:pipeName=$null;$utc=[DateTime]::UtcNow
    $script:process=[CliProbe]::Start($gui,$Launch,$directory,$directory)
    $script:hosts+= $process.Id
    $script:stdout=$process.StandardOutput.ReadToEndAsync();$script:stderr=$process.StandardError.ReadToEndAsync()
    $file=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($process.Id).json";$deadline=(Get-Date).AddSeconds(40)
    do {
        if ($process.HasExited -or (Get-Date) -gt $deadline) {throw ('Owned startup failed: '+([CliProbe]::Output($stderr)))}
        if ((Test-Path $file) -and (Get-Item $file).LastWriteTimeUtc -ge $utc) {
            $record=Get-Content -Raw $file|ConvertFrom-Json
            if ($record.pid -ne $process.Id) {throw 'Wrong discovery owner'}
            $script:pipeName=$record.pipe;break
        }
        Start-Sleep -Milliseconds 50
    } while ($true)
    if ((Request @('identify')).pid -ne $process.Id) {throw 'Wrong pipe owner'}
    do {
        $tree=Tree
        if (@($tree.surfaces|Where-Object {-not $_.ready}).Count -eq 0) {return $tree}
        if ((Get-Date) -gt $deadline) {throw 'Terminal readiness timed out'}
        Start-Sleep -Milliseconds 50
    } while ($true)
}
function Same-Text([string]$Left,[string]$Right) {return [string]::Equals($Left,$Right,[StringComparison]::Ordinal)}
function Eval-Page([string]$Pane,[string]$Source) {return (Request @('browser','eval',('pane:'+$Pane),$Source)).result}
function Wait-Page([string]$Pane,[string]$Suffix,[string]$Title) {
    $deadline=(Get-Date).AddSeconds(20)
    do {
        $status=Request @('browser','status',('pane:'+$Pane))
        if (-not $status.loading -and $status.url.Contains($Suffix) -and (Same-Text $status.title $Title)) {
            if ((Eval-Page $Pane 'document.readyState') -eq 'complete') {return $status}
        }
        if ((Get-Date) -gt $deadline) {throw ('Page did not load: '+($status|ConvertTo-Json -Compress))}
        Start-Sleep -Milliseconds 50
    } while ($true)
}

function Downloads {
    $result=Request @('downloads','list')
    if ($result.panel_handle) {
        $window=[IntPtr]([long]$result.panel_handle)
        if ([CliProbe]::IsWindowVisible($window) -or [CliProbe]::GetForegroundWindow() -eq $window) {throw 'Download panel became visible or foreground'}
    }
    Tree|Out-Null
    return $result
}
function Start-Download([string]$Suffix,[string]$Base=$origin) {
    $known=@((Downloads).entries|ForEach-Object {$_.id})
    Request @('browser','navigate',$script:domPane,($Base+$Suffix))|Out-Null
    $deadline=(Get-Date).AddSeconds(12)
    do {
        $new=@((Downloads).entries|Where-Object {$known -notcontains $_.id})
        if ($new.Count -eq 1) {return $new[0]}
        if ($new.Count -gt 1) {throw 'One request created multiple download records'}
        if ((Get-Date) -gt $deadline) {$script:evidence.downloadTimeout=Downloads;throw ('DownloadStarting did not produce a record: '+$Base+$Suffix)}
        Start-Sleep -Milliseconds 50
    } while ($true)
}
function Wait-Download([string]$Id,[string]$Phase,[int]$Seconds=20) {
    $deadline=(Get-Date).AddSeconds($Seconds)
    do {
        $entry=(Downloads).entries|Where-Object {$_.id -eq $Id}
        if($entry.phase -eq $Phase) {return $entry}
        if($entry.phase -in @('complete','failed','cancelled') -or (Get-Date) -gt $deadline) {throw ('Unexpected download state: '+($entry|ConvertTo-Json -Compress))}
        Start-Sleep -Milliseconds 100
    } while($true)
}
function Verify-Bytes($Entry,[string]$Expected) {
    if(-not $Entry.path -or -not (Test-Path -LiteralPath $Entry.path)) {throw 'Completed download path is missing'}
    if(-not (Same-Text ([IO.File]::ReadAllText($Entry.path,[Text.Encoding]::UTF8)) $Expected)) {throw 'Downloaded Unicode bytes differ'}
    $bytes=[Text.Encoding]::UTF8.GetBytes($Expected)
    if($Entry.received -ne $bytes.Length -or (Get-Item -LiteralPath $Entry.path).Length -ne $bytes.Length) {throw 'Downloaded byte count differs'}
}
try {
    $tree=Start-Owned @('--temporary','--shell=cmd','--cwd',$directory)
    $source=Request @('identify');$terminal=$tree.surfaces[0];$script:shells+=$terminal.pid
    $opened=(Request @('browser','open',($origin+'/dom'),'--pane',$source.pane)).browser_pane_opened
    $script:domPane=$opened.pane
    Wait-Page $domPane '/dom' 'Downloads 한글'|Out-Null
    if ($Case -in @('all','files')) {
    $blocker=Join-Path $directory 'state\downloads'
    [IO.File]::WriteAllText($blocker,'owned destination blocker',[Text.Encoding]::UTF8)
    $blocked=Start-Download '/one';$blocked=Wait-Download $blocked.id 'failed'
    if(-not $blocked.error -or -not (Same-Text ([IO.File]::ReadAllText($blocker)) 'owned destination blocker')) {throw 'Failed preparation altered the blocker or lost its error'}
    Remove-Item -LiteralPath $blocker
    $evidence.checks+=@{name='unavailable_destination_fails_before_native_transfer_preserves_blocker_and_recovers';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)
    $entry=Start-Download '/one';$first=Wait-Download $entry.id 'complete'
    Verify-Bytes $first ([DownloadFixture]::Text)
    if(-not (Same-Text $first.filename ([DownloadFixture]::Name)) -or $first.surface -ne $opened.surface) {throw 'Download filename/surface differs'}
    if(-not $first.path.Contains($directory)) {throw 'Download escaped the isolated test root'}
    $evidence.first=$first
    $evidence.checks+=@{name='native_attachment_exact_unicode_filename_payload_and_original_browser_surface';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)

    $entry=Start-Download '/one';$second=Wait-Download $entry.id 'complete';Verify-Bytes $second ([DownloadFixture]::Text)
    if(-not (Same-Text ([IO.Path]::GetFileName($second.path)) '한 글 한 é 😀 (1).txt')) {throw ('Collision suffix differs: '+$second.path)}
    Verify-Bytes $first ([DownloadFixture]::Text)
    $entry=Start-Download '/empty';$empty=Wait-Download $entry.id 'complete';Verify-Bytes $empty ''
    $entry=Start-Download '/unknown';$unknown=Wait-Download $entry.id 'complete';Verify-Bytes $unknown ([DownloadFixture]::Text)
    $evidence.checks+=@{name='collision_preserves_existing_file_empty_and_unknown_length_downloads';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)

    $panel=Request @('downloads','show')
    $panel=Downloads
    if(-not $panel.panel_handle -or $panel.panel_rows -ne $panel.entries.Count) {throw 'Hidden download list did not reflect records'}
    Request @('downloads','remove',$first.id)|Out-Null
    if(-not (Test-Path -LiteralPath $first.path) -or @((Downloads).entries|Where-Object {$_.id -eq $first.id}).Count -ne 0) {throw 'Remove deleted a downloaded file or kept its record'}
    $evidence.checks+=@{name='hidden_native_list_and_remove_keep_downloaded_files';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)

    }
    if ($Case -eq 'all') {
    $slow=Start-Download '/slow';$slow=Wait-Download $slow.id 'downloading'
    Request @('downloads','remove',$slow.id) 1|Out-Null
    Request @('downloads','clear')|Out-Null
    if(@((Downloads).entries).Count -ne 1 -or -not (Test-Path -LiteralPath $second.path)) {throw 'Clear removed active work or completed files'}
    $cancel=Request @('downloads','cancel',$slow.id)
    if(-not $cancel.changed) {throw 'Active download was not cancelled'}
    $cancelled=Wait-Download $slow.id 'cancelled'
    if($cancelled.path -or (Test-Path -LiteralPath $slow.path)) {throw 'Cancelled download retained partial file'}
    if((Request @('downloads','cancel',$slow.id)).changed) {throw 'Repeated cancellation was not idempotent'}
    $evidence.checks+=@{name='active_clear_remove_guard_native_cancellation_cleanup_and_idempotence';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)

    }
    if ($Case -in @('all','broken')) {
    $broken=Start-Download '/broken'
    $deadline=(Get-Date).AddSeconds(5)
    do {
        $broken=(Downloads).entries|Where-Object {$_.id -eq $broken.id}
        if($broken.phase -eq 'complete') {throw 'Truncated response was published as complete'}
        if($broken.phase -eq 'failed' -or (Get-Date) -gt $deadline) {break}
        Start-Sleep -Milliseconds 100
    } while($true)
    $evidence.truncatedBeforeCancel=$broken
    if($broken.phase -ne 'failed') {Request @('downloads','cancel',$broken.id)|Out-Null;$broken=Wait-Download $broken.id 'cancelled'}
    if($broken.path -or (Test-Path -LiteralPath (Join-Path $directory 'state\downloads\broken.txt'))) {throw 'Truncated transfer left published/partial data after cancellation'}
    $evidence.brokenRequestsAfterCancel=$fixture.BrokenRequests
    $evidence.afterBrokenCancel=Downloads
    if(@($evidence.afterBrokenCancel.entries|Where-Object {$_.uri -eq $broken.uri -and $_.phase -in @('preparing','downloading','cancelling','finalizing')}).Count -gt 0){throw 'Cancelled broken transfer left another active native record'}
    $evidence.checks+=@{name='truncated_body_not_published_native_retry_count_recorded_then_cancelled';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)

    }
    if ($Case -in @('all','capacity')) {
    $pending=@()
    for($i=0;$i -lt 8;$i++) {$server=New-Object DownloadFixture;$extraFixtures+=$server;$entry=Start-Download '/slow' $server.Origin;$pending+=Wait-Download $entry.id 'downloading'}
    $before=Downloads
    if($before.active -ne 8) {throw 'Expected eight concurrently active native downloads'}
    $server=New-Object DownloadFixture;$extraFixtures+=$server
    Request @('browser','navigate',$domPane,($server.Origin+'/slow'))|Out-Null
    $deadline=(Get-Date).AddSeconds(3)
    do {
        $after=Downloads
        if($after.rejected_at_capacity -gt $before.rejected_at_capacity) {break}
        if((Get-Date) -gt $deadline) {throw 'Download admission rejection was not recorded'}
        Start-Sleep -Milliseconds 25
    } while($true)
    if($after.active -ne 8 -or $after.entries.Count -ne $before.entries.Count) {throw 'Ninth download bypassed native admission bound'}
    foreach($entry in $pending) {Request @('downloads','cancel',$entry.id)|Out-Null}
    foreach($entry in $pending) {Wait-Download $entry.id 'cancelled'|Out-Null}
    $evidence.checks+=@{name='eight_concurrent_native_downloads_ninth_rejected_and_all_cancelled';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)

    }
    if ($Case -in @('all','capacity','close')) {
    $slow=Start-Download '/slow';$slow=Wait-Download $slow.id 'downloading'
    Request @('close-tab',$opened.surface)|Out-Null
    $cancelled=Wait-Download $slow.id 'cancelled'
    if($cancelled.path) {throw 'Source close published an unfinished download'}
    $current=(Tree).surfaces|Where-Object {$_.id -eq $terminal.id}
    if($current.pid -ne $terminal.pid -or -not $current.running) {throw 'Downloads changed original terminal identity'}
    $evidence.checks+=@{name='source_tab_close_cancels_owned_download_and_preserves_original_terminal';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)
    }
    if($Case -in @('all','broken') -and $fixture.BrokenRequests -ne $evidence.brokenRequestsAfterCancel) {throw 'Cancelled transfer issued later HTTP requests'}
    if ($Case -eq 'shutdown') {
        $slow=Start-Download '/slow';$slow=Wait-Download $slow.id 'downloading'
        $evidence.activeAtShutdown=$slow
        Request @('quit','--discard-state')|Out-Null
        if(-not $process.WaitForExit(5000) -or $process.ExitCode -ne 0){throw 'Host shutdown with an active download failed'}
        $evidence.checks+=@{name='host_shutdown_releases_download_objects_before_webview_controllers';passed=$true}
        Write-Host ("[check] passed "+$evidence.checks[-1].name)
    } else {
    $evidence.final=Downloads
    $downloadRoot=Join-Path $directory 'state\downloads'
    if(@(Get-ChildItem -LiteralPath $downloadRoot -Force -Directory -Filter '.flowmux-download-*').Count -ne 0) {throw 'Finished downloads left owned staging directories'}
    }
    $evidence.status='passed_background_browser_downloads_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
    if ($pipeName -and $process -and -not $process.HasExited) {
        try {foreach($entry in (Downloads).entries) {if($entry.phase -in @('preparing','downloading')) {Request @('downloads','cancel',$entry.id)|Out-Null}}} catch {}
        try {Request @('quit','--discard-state')|Out-Null} catch {}
    }
    if ($process) {if (-not $process.HasExited -and -not $process.WaitForExit(10000)) {$process.Kill();[CliProbe]::WaitAfterKill($process)};if ($stderr -and $stderr.Wait(3000)) {$evidence.hostStderr=([CliProbe]::Output($stderr))};$evidence.hostExitCode=$process.ExitCode;$process.Dispose()}
    $evidence.brokenRequests=$fixture.BrokenRequests;$evidence.concurrentFixtureOrigins=@($extraFixtures|ForEach-Object {$_.Origin});foreach($server in $extraFixtures){$server.Dispose()};$fixture.Dispose();$evidence.hosts=$hosts;$evidence.shells=$shells;$evidence.finished=(Get-Date).ToString('o')
    if ($evidence.status -eq 'failed') { $evidence|ConvertTo-Json -Depth 12|Set-Content -Encoding UTF8 (Join-Path $directory 'native-browser-downloads-background.json') }
    if ($evidence.status -eq 'failed') { Write-Output ('Evidence: '+$directory) }
}
[ordered]@{status=$evidence.status;checks=$evidence.checks.Count}|ConvertTo-Json -Compress