# SPDX-License-Identifier: GPL-3.0-or-later
# Run through run-check.ps1 (120s). Owned hidden host, loopback pages and no desktop input.
param(
    [string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",
    [ValidateSet('all','startup','basic','unicode','panel','surface','validation','queued-close','navigation','close')][string]$Case='all'
)
if (-not $env:FLOWMUX_TEST_ARTIFACT_ROOT) { throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.' }
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path
$gui=Join-Path $BuildDirectory 'flowmux.exe'
$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'FindFixture.cs')
$directory=Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT ('browser-find-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null
$directory=(Resolve-Path $directory).Path
$fixture=$null;$process=$null;$pipeName=$null;$stdout=$null;$stderr=$null
$hosts=@();$shells=@();$clients=@();$cleanupErrors=@()
$evidence=[ordered]@{started=(Get-Date).ToString('o');mode='background';case=$Case;checks=@();deferred=@();clipboardAccess=$false;desktopInput=$false;externalSites=$false;unicodeComparison='ordinal';realImeTest=$false}

function Same-Text([string]$Left,[string]$Right) {return [string]::Equals($Left,$Right,[StringComparison]::Ordinal)}
function Passed([string]$Name) {
    $script:evidence.checks+=@{name=$Name;passed=$true}
    Write-Host ('[check] passed '+$Name)
}
function Begin-Probe([string]$File,[string[]]$Arguments,[bool]$Json=$true) {
    $p=[CliProbe]::Start($File,$Arguments,$directory,$directory)
    $job=[pscustomobject]@{process=$p;pid=$p.Id;output=$p.StandardOutput.ReadToEndAsync();error=$p.StandardError.ReadToEndAsync();completed=$false;json=$Json}
    $script:clients+=$job
    return $job
}
function End-Command($Job,[int[]]$AllowedExits=@(0)) {
    try {
        if(-not $Job.process.WaitForExit(5000)) {
            $Job.process.Kill();[CliProbe]::WaitAfterKill($Job.process)
            throw 'Owned CLI exceeded five seconds; not retried'
        }
        $outDone=$Job.output.Wait(1000);$errDone=$Job.error.Wait(1000)
        if(-not $outDone -or -not $errDone) {throw 'Owned CLI output did not close'}
        $code=$Job.process.ExitCode
        $out=[CliProbe]::Output($Job.output);$err=[CliProbe]::Output($Job.error)
        if($AllowedExits -notcontains $code) {throw ('Owned CLI exit '+$code+': '+$err+' '+$out)}
        if(-not $Job.json) {return $out.TrimEnd("`r","`n")}
        if($code -eq 0) {return ($out|ConvertFrom-Json)}
        return ($err|ConvertFrom-Json)
    } finally {
        if(-not $Job.process.HasExited) {$Job.process.Kill();[CliProbe]::WaitAfterKill($Job.process)}
        $Job.completed=$true;$Job.process.Dispose()
    }
}
function Begin-Command([string[]]$Arguments,[bool]$Json=$true) {
    if(-not $script:pipeName) {throw 'Explicit owned pipe required'}
    $prefix=@('--pipe',$script:pipeName)
    if($Json) {$prefix+='--json'}
    return Begin-Probe $cli ($prefix+$Arguments) $Json
}
function Request([string[]]$Arguments,[int]$Exit=0) {return End-Command (Begin-Command $Arguments) @($Exit)}
function Plain([string[]]$Arguments) {return End-Command (Begin-Command $Arguments $false)}
function Raw-Request($Body) {
    if(-not $script:pipeName) {throw 'Explicit owned pipe required'}
    $stream=[IO.Pipes.NamedPipeClientStream]::new('.',$script:pipeName.Substring(9),[IO.Pipes.PipeDirection]::InOut,[IO.Pipes.PipeOptions]::Asynchronous,[Security.Principal.TokenImpersonationLevel]::Identification)
    try {
        $stream.Connect(3000)
        $writer=[IO.StreamWriter]::new($stream,(New-Object Text.UTF8Encoding($false)),4096,$true)
        $reader=[IO.StreamReader]::new($stream,(New-Object Text.UTF8Encoding($false)),$false,4096,$true)
        try {
            $write=$writer.WriteLineAsync(($Body|ConvertTo-Json -Depth 10 -Compress))
            if(-not $write.Wait(5000) -or -not $writer.FlushAsync().Wait(5000)) {throw 'Owned raw write timed out'}
            $read=$reader.ReadLineAsync()
            if(-not $read.Wait(5000)) {throw 'Owned raw request exceeded five seconds; not retried'}
            return ([CliProbe]::Output($read)|ConvertFrom-Json)
        } finally {$writer.Dispose();$reader.Dispose()}
    } finally {$stream.Dispose()}
}
function Tree {
    $tree=Request @('tree');$window=[IntPtr]([long]$tree.window_handle)
    if(-not $tree.background_testing -or [CliProbe]::IsWindowVisible($window) -or [CliProbe]::GetForegroundWindow() -eq $window) {throw 'Owned host became visible or foreground'}
    foreach($browser in $tree.browsers) {
        foreach($handle in @($browser.view_handle,$browser.chrome_handle)) {
            if($handle -and [CliProbe]::IsWindowVisible([IntPtr]([long]$handle))) {throw 'Owned browser became visible'}
        }
    }
    return $tree
}
function Start-Owned {
    $utc=[DateTime]::UtcNow
    $script:process=[CliProbe]::Start($gui,@('--temporary','--shell=cmd','--cwd',$directory),$directory,$directory)
    $script:hosts+=$process.Id
    $script:stdout=$process.StandardOutput.ReadToEndAsync();$script:stderr=$process.StandardError.ReadToEndAsync()
    $file=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($process.Id).json"
    $deadline=(Get-Date).AddSeconds(20)
    do {
        if($process.HasExited -or (Get-Date) -gt $deadline) {throw ('Owned startup failed: '+[CliProbe]::Output($stderr))}
        if((Test-Path -LiteralPath $file) -and (Get-Item -LiteralPath $file).LastWriteTimeUtc -ge $utc) {
            $record=Get-Content -Raw -LiteralPath $file|ConvertFrom-Json
            if($record.pid -ne $process.Id) {throw 'Wrong discovery owner'}
            $script:pipeName=$record.pipe;break
        }
        Start-Sleep -Milliseconds 25
    } while($true)
    if((Request @('identify')).pid -ne $process.Id) {throw 'Wrong pipe owner'}
    do {
        $tree=Tree
        if(@($tree.surfaces|Where-Object {-not $_.ready}).Count -eq 0) {return $tree}
        if((Get-Date) -gt $deadline) {throw 'Owned terminal readiness timed out'}
        Start-Sleep -Milliseconds 25
    } while($true)
}
function Eval-Page([string]$Source) {return (Request @('browser','eval',$script:domPane,$Source)).result}
function Status {return Request @('browser','status',$script:domPane)}
function Wait-Page([string]$Suffix) {
    $deadline=(Get-Date).AddSeconds(8)
    do {
        $status=Status
        if(-not $status.loading -and $status.url.Contains($Suffix) -and (Same-Text $status.title 'Find fixture')) {
            if((Eval-Page 'document.readyState') -eq 'complete') {return $status}
        }
        if((Get-Date) -gt $deadline) {throw ('Owned page not ready: '+($status|ConvertTo-Json -Compress))}
        Start-Sleep -Milliseconds 25
    } while($true)
}
function Reset-Selection {
    Eval-Page '(()=>{const s=getSelection(),r=document.createRange();r.setStart(document.getElementById("start").firstChild,0);r.collapse(true);s.removeAllRanges();s.addRange(r);return true;})()'|Out-Null
}
function Selection {
    return Eval-Page '(()=>{const s=getSelection(),n=s.anchorNode,e=n&&(n.nodeType===1?n:n.parentElement);return {text:String(s),id:e&&e.closest("[id]")?e.closest("[id]").id:null};})()'
}
function Find([string]$Query,[string[]]$Options=@()) {
    $result=Request (@('browser','find',$script:domPane,$Query)+$Options)
    if(-not (Same-Text $result.query $Query) -or $result.surface -ne $script:expectedSurface -or $result.found -isnot [bool]) {throw 'Find query, surface or result type differs'}
    if($result.backward -ne ($Options -contains '--backward') -or $result.case_sensitive -ne ($Options -contains '--case-sensitive') -or $result.wrap -ne ($Options -notcontains '--no-wrap')) {throw 'Find options differ'}
    if((Eval-Page 'document.hasFocus()') -ne $false) {throw 'Hidden find document gained focus'}
    return $result
}
function Assert-Hit($Result,[string]$Id,[string]$Text) {
    $selection=Selection
    if(-not $Result.found -or $selection.id -ne $Id -or -not (Same-Text $selection.text $Text) -or -not (Same-Text $Result.selection $selection.text)) {throw ('Unexpected native find selection: '+($selection|ConvertTo-Json -Compress))}
}
function Assert-Panel([string]$Query) {
    $find=(Status).find
    if(-not $find.panel_handle -or -not $find.panel_query_handle -or -not (Same-Text $find.query $Query)) {throw 'Find panel/query state missing'}
    foreach($handle in @($find.panel_handle,$find.panel_query_handle)) {
        $window=[IntPtr]([long]$handle)
        if([CliProbe]::IsWindowVisible($window) -or [CliProbe]::GetForegroundWindow() -eq $window) {throw 'Owned find panel became visible or foreground'}
    }
    if(-not (Same-Text ([FindFixture]::ReadText([long]$find.panel_query_handle)) $Query)) {throw 'Native query control changed Unicode codepoints'}
    if($find.busy -or $null -eq $find.panel_status) {throw 'Finished find panel has no settled native status'}
    return $find
}
function Begin-PendingFind {
    $busy=Begin-Command @('browser','eval',$script:domPane,'(()=>{const until=performance.now()+2500;while(performance.now()<until){};return "owned busy loop done";})()')
    Start-Sleep -Milliseconds 100
    $find=Begin-Command @('browser','find',$script:domPane,'needle')
    $deadline=(Get-Date).AddSeconds(2)
    do {
        if((Status).find.busy) {return @{busy=$busy;find=$find}}
        if($find.process.HasExited -or (Get-Date) -gt $deadline) {throw 'Could not observe pending find behind owned renderer work'}
        Start-Sleep -Milliseconds 10
    } while($true)
}
try {
    $doctor=End-Command (Begin-Probe $cli @('doctor'))
    if(-not $doctor.background_testing -or $doctor.status -ne 'ok') {throw 'Working debug build required; no host launched'}
    $tree=Start-Owned;$source=Request @('identify');$terminal=$tree.surfaces[0];$shells+=$terminal.pid
    if($Case -eq 'startup') {
        Passed 'hidden_debug_doctor_and_owned_host_readiness'
    } else {
    $fixture=New-Object FindFixture;$origin=$fixture.Origin
    $opened=(Request @('browser','open',($origin+'/find'),'--pane',$source.pane)).browser_pane_opened
    $script:domPane=$opened.pane;$script:expectedSurface=$opened.surface
    Wait-Page '/find'|Out-Null
    if((Eval-Page 'document.hasFocus()') -ne $false) {throw 'Owned hidden document unexpectedly has focus'}

    if($Case -in @('all','basic')) {
    Reset-Selection
    foreach($id in @('one','two','three')) {Assert-Hit (Find 'needle' @('--no-wrap')) $id 'needle'}
    if((Find 'needle' @('--no-wrap')).found) {throw 'No-wrap advanced past the last match'}
    Assert-Hit (Find 'needle') 'one' 'needle'
    Assert-Hit (Find 'needle' @('--backward')) 'three' 'needle'
    Reset-Selection
    Assert-Hit (Find 'casetoken') 'upper' 'CASEtoken'
    Reset-Selection
    Assert-Hit (Find 'casetoken' @('--case-sensitive')) 'lower' 'casetoken'
    if((Find 'owned-no-such-text').found) {throw 'Absent text matched'}
    if((Plain @('browser','find',$domPane,'needle')) -cne 'true' -or (Plain @('browser','find',$domPane,'owned-no-such-text')) -cne 'false') {throw 'Plain find output is not true/false'}
    Passed 'native_forward_backward_wrap_case_no_hit_and_plain_boolean'
    }

    if($Case -in @('all','unicode')) {
    $evidence.unicodeMatches=@()
    foreach($query in @('한글','한','é','😀',[FindFixture]::Unicode)) {
        Reset-Selection;$result=Find $query
        if(-not $result.found) {throw ('Known rendered Unicode query was not found: '+$query)}
        $selection=Selection
        if(-not (Same-Text $result.selection $selection.text)) {throw 'Find selection differs from actual DOM selection'}
        $evidence.unicodeMatches+=@{query=$query;found=$result.found;selection=$result.selection}
    }
    Reset-Selection
    $evidence.canonicalEquivalenceObservation=Find '한 é'
    # Chromium's matching equivalence is recorded, not asserted to be ordinal.
    Reset-Selection
    Assert-Hit (Find ([FindFixture]::Injection)) 'injection' ([FindFixture]::Injection)
    if((Eval-Page 'typeof window.findInjected') -ne 'undefined') {throw 'Find query executed as JavaScript'}
    Passed 'exact_unicode_query_transport_engine_matching_observation_and_literal_injection_text'
    }

    if($Case -in @('all','panel')) {
    Request @('browser','find-show',$domPane)|Out-Null
    if(-not (Find ([FindFixture]::Unicode)).found) {throw 'Known compound Unicode query was not found before native panel inspection'}
    $evidence.panel=Assert-Panel ([FindFixture]::Unicode)
    Reset-Selection;Assert-Hit (Find 'needle') 'one' 'needle'
    $closed=Request @('browser','find-close',$domPane)
    if(-not $closed.ok -or $closed.surface -ne $opened.surface -or -not $closed.cleared) {throw 'Find close did not report its owned selection cleared'}
    if(-not (Same-Text (Selection).text '')) {throw 'Find close kept its owned match selection'}
    $closedStatus=(Status).find
    if($closedStatus.panel_handle -or $closedStatus.panel_query_handle) {throw 'Find close left its panel bound'}
    Request @('browser','find-show',$domPane)|Out-Null
    Assert-Panel 'needle'|Out-Null
    Reset-Selection;Find 'needle'|Out-Null
    Eval-Page '(()=>{const r=document.createRange();r.selectNodeContents(document.getElementById("manual"));const s=getSelection();s.removeAllRanges();s.addRange(r);return String(s);})()'|Out-Null
    $closed=Request @('browser','find-close',$domPane)
    if(-not $closed.ok -or $closed.cleared) {throw 'Find close reported clearing a changed user selection'}
    if(-not (Same-Text (Selection).text 'user selection stays')) {throw 'Find close removed a selection changed by the page/user'}
    Passed 'hidden_native_panel_exact_query_and_close_clears_only_owned_selection'
    }

    if($Case -in @('all','surface')) {
    Reset-Selection
    Find 'needle'|Out-Null
    Request @('browser','find-show',$domPane)|Out-Null
    $firstQuery=(Status).find.query
    $firstPanel=Assert-Panel $firstQuery
    $second=(Request @('browser','open',($origin+'/second'),'--pane',$source.pane)).browser_pane_opened
    if($second.pane -ne $domPane) {throw 'Fixture did not reuse the browser pane'}
    $script:expectedSurface=$second.surface;Wait-Page '/second'|Out-Null
    Reset-Selection;Assert-Hit (Find 'second surface') 'other' 'second surface'
    if(-not (Same-Text ([FindFixture]::ReadText([long]$firstPanel.panel_query_handle)) $firstQuery)) {throw 'Tab switch or another surface find implicitly rebound the native panel'}
    Request @('browser','find-show',$domPane)|Out-Null
    Assert-Panel 'second surface'|Out-Null
    Request @('close-tab',$second.surface)|Out-Null
    $script:expectedSurface=$opened.surface
    if((Status).id -ne $opened.surface -or -not (Same-Text (Status).find.query $firstQuery)) {throw 'Find state crossed browser surfaces'}
    Request @('new-workspace','--shell=cmd','--cwd',$directory)|Out-Null
    $other=Request @('identify');$otherTree=Tree
    $shells+=@($otherTree.surfaces|Where-Object {$_.pid -and $shells -notcontains $_.pid}|ForEach-Object {$_.pid})
    Request @('browser','find',$domPane,'needle') 1|Out-Null
    Request @('browser','find-show',$domPane) 1|Out-Null
    if((Request @('identify')).workspace -ne $other.workspace) {throw 'Hidden find request activated another workspace'}
    Request @('workspace','focus',$source.workspace)|Out-Null
    Request @('workspace','close',$other.workspace)|Out-Null
    Passed 'find_state_pinned_to_surface_and_hidden_workspace_requests_rejected'
    }

    if($Case -in @('all','validation')) {
    Request @('browser','find',$source.pane,'needle') 1|Out-Null
    foreach($bad in @('',("bad"+[char]0+"query"),('x'*4097))) {
        $r=Raw-Request @{method='browser';op=@{kind='find';pane=$domPane;query=$bad;backward=$false;case_sensitive=$false;no_wrap=$false}}
        if(-not $r.error) {throw 'Invalid raw find query accepted'}
    }
    $evidence.newlineQuery=Find "line one`r`nline two"
    Request @('browser','navigate',$domPane,($origin+'/slow'))|Out-Null
    $loadingDeadline=(Get-Date).AddSeconds(2)
    while(-not (Status).loading) {
        if((Get-Date) -gt $loadingDeadline) {throw 'Owned slow response did not keep navigation loading'}
        Start-Sleep -Milliseconds 10
    }
    Request @('browser','find',$domPane,'needle') 1|Out-Null
    $fixture.ReleaseSlow();Wait-Page '/slow'|Out-Null
    Passed 'terminal_invalid_query_and_loading_document_rejections'
    }

    if($Case -in @('all','queued-close')) {
    Reset-Selection;Assert-Hit (Find 'needle') 'one' 'needle'
    Request @('browser','find-show',$domPane)|Out-Null
    $panel=Assert-Panel 'needle'
    $pending=Begin-PendingFind
    # This is only WM_CLOSE to the verified hidden panel; no keyboard, pointer or focus APIs.
    [FindFixture]::PostClose([long]$panel.panel_handle,$process.Id)
    $deadline=(Get-Date).AddSeconds(2)
    do {
        $findStatus=(Status).find
        if(-not $findStatus.panel_handle) {break}
        if((Get-Date) -gt $deadline) {throw 'Queued UI close did not hide and unpin the owned panel'}
        Start-Sleep -Milliseconds 10
    } while($true)
    # The read-only eval finishes before the find callback. It must not consume the deferred close.
    End-Command $pending.busy|Out-Null
    $pendingResult=End-Command $pending.find
    if(-not $pendingResult.found -or $pendingResult.surface -ne $opened.surface) {throw 'Queued close prevented the pending find from completing'}
    $deadline=(Get-Date).AddSeconds(3)
    do {
        $findStatus=(Status).find
        if(-not $findStatus.busy) {
            $selection=Selection
            if(Same-Text $selection.text '') {break}
        }
        if((Get-Date) -gt $deadline) {throw 'Deferred UI close lost the pending find selection or remained busy'}
        Start-Sleep -Milliseconds 10
    } while($true)
    if($findStatus.panel_handle -or (Eval-Page 'document.hasFocus()') -ne $false) {throw 'Queued close rebound the panel or focused the hidden document'}
    $evidence.queuedClose=@{message='WM_CLOSE';panelHandle=$panel.panel_handle;find=$pendingResult;selectionAfter=$selection.text}
    Passed 'queued_owned_panel_close_waits_for_find_after_readonly_eval_and_clears_selection'
    }

    if($Case -in @('all','navigation')) {
    $pending=Begin-PendingFind
    Request @('browser','navigate',$domPane,($origin+'/find?after-pending'))|Out-Null
    $evidence.navigationCancellation=End-Command $pending.find @(1)
    End-Command $pending.busy @(0,1)|Out-Null
    Wait-Page '/find?after-pending'|Out-Null
    if((Status).find.busy) {throw 'Navigation left find busy'}
    Reset-Selection;Assert-Hit (Find 'needle') 'one' 'needle'
    Passed 'navigation_cancels_pending_find_and_next_document_recovers'
    }

    if($Case -in @('all','close')) {
    $pending=Begin-PendingFind
    Request @('close-tab',$opened.surface)|Out-Null
    $evidence.closeCancellation=End-Command $pending.find @(1)
    End-Command $pending.busy @(0,1)|Out-Null
    $tree=Tree;$current=@($tree.surfaces|Where-Object {$_.id -eq $terminal.id})
    if($current.Count -ne 1 -or $current[0].pid -ne $terminal.pid -or -not $current[0].running) {throw 'Browser find changed original terminal identity'}
    Passed 'source_close_cancels_pending_find_and_preserves_original_terminal'
    }
    }
    Tree|Out-Null
    $evidence.status='passed_background_browser_find_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
    if($fixture) {$fixture.ReleaseSlow()}
    foreach($job in $clients) {
        if(-not $job.completed) {
            try {if(-not $job.process.HasExited) {$job.process.Kill();[CliProbe]::WaitAfterKill($job.process)};$job.process.Dispose();$job.completed=$true}
            catch {$cleanupErrors+=$_.Exception.Message}
        }
    }
    if($pipeName -and $process -and -not $process.HasExited) {try {Request @('quit','--discard-state')|Out-Null} catch {$cleanupErrors+=$_.Exception.Message}}
    if($process) {
        try {
            if(-not $process.HasExited -and -not $process.WaitForExit(5000)) {$process.Kill();[CliProbe]::WaitAfterKill($process)}
            if($stderr) {$stderr.Wait(1000)|Out-Null;$evidence.hostStderr=[CliProbe]::Output($stderr)}
            $evidence.hostExitCode=$process.ExitCode
            if($process.ExitCode -ne 0) {$cleanupErrors+=('Owned host exited '+$process.ExitCode)}
        } catch {$cleanupErrors+=$_.Exception.Message} finally {$process.Dispose()}
    }
    if($fixture) {$fixture.Dispose()}
    if($cleanupErrors.Count) {$evidence.status='failed';$evidence.cleanupErrors=$cleanupErrors}
    $evidence.hosts=$hosts;$evidence.shells=$shells;$evidence.clientPids=@($clients|ForEach-Object {$_.pid})
    $evidence.finished=(Get-Date).ToString('o')
    if ($evidence.status -eq 'failed') { $evidence|ConvertTo-Json -Depth 12|Set-Content -Encoding UTF8 (Join-Path $directory 'native-browser-find-background.json') }
    if ($evidence.status -eq 'failed') { Write-Output ('Evidence: '+$directory) }
}
if($cleanupErrors.Count) {throw ($cleanupErrors -join '; ')}
[ordered]@{status=$evidence.status;checks=$evidence.checks.Count}|ConvertTo-Json -Compress