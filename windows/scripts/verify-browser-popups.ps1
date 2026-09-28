# SPDX-License-Identifier: GPL-3.0-or-later
# Run through run-check.ps1 (120s). Owned hidden host and loopback pages; no desktop input.
param(
    [string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",
    [ValidateSet('all','startup','url','target','blank','history','source-pin','hidden','move','source-close','child-close','manual-close','policy','capacity','flood','final-close')][string]$Case='all'
)
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path
$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'PopupFixture.cs')
$directory=Join-Path $(if ($env:FLOWMUX_TEST_ARTIFACT_ROOT) { $env:FLOWMUX_TEST_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\dist\evidence' }) ('browser-popups-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$fixture=$null;$process=$null;$pipeName=$null;$stdout=$null;$stderr=$null
$hosts=@();$shells=@();$clients=@();$cleanupErrors=@()
$evidence=[ordered]@{started=(Get-Date).ToString('o');mode='background';case=$Case;checks=@();observations=@();clipboardAccess=$false;desktopInput=$false;externalSites=$false;realImeTest=$false;unicodeComparison='ordinal';deferred=@(
    'Native deferral pending during move, hide/re-show, navigation or source close is not deterministically forced by the HTTP fixture.',
    'The eight-request in-flight limit and twelve-second expiry are not saturated or expired deterministically; sampled bounds and eventual drain are checked.',
    'No physical target-link gesture, visible focus, real IME, cross-origin opener policy, external site, renderer crash or shutdown-during-deferral coverage.'
)}

function Same-Text([string]$Left,[string]$Right) {return [string]::Equals($Left,$Right,[StringComparison]::Ordinal)}
function Js([string]$Value) {return ConvertTo-Json -InputObject $Value -Compress}
function Passed([string]$Name) {$script:evidence.checks+=@{name=$Name;passed=$true};Write-Host ('[check] passed '+$Name)}
function Begin-Probe([string]$File,[string[]]$Arguments) {
    $p=[CliProbe]::Start($File,$Arguments,$directory,$directory)
    $job=[pscustomobject]@{process=$p;pid=$p.Id;output=$p.StandardOutput.ReadToEndAsync();error=$p.StandardError.ReadToEndAsync();completed=$false}
    $script:clients+=$job;return $job
}
function End-Command($Job,[int[]]$AllowedExits=@(0)) {
    try {
        if(-not $Job.process.WaitForExit(5000)) {$Job.process.Kill();[CliProbe]::WaitAfterKill($Job.process);throw 'Owned CLI exceeded five seconds; not retried'}
        $outDone=$Job.output.Wait(1000);$errDone=$Job.error.Wait(1000)
        if(-not $outDone -or -not $errDone) {throw 'Owned CLI output did not close'}
        $code=$Job.process.ExitCode;$out=[CliProbe]::Output($Job.output);$err=[CliProbe]::Output($Job.error)
        if($AllowedExits -notcontains $code) {throw ('Owned CLI exit '+$code+': '+$err+' '+$out)}
        if($code -eq 0) {return ($out|ConvertFrom-Json)}
        return ($err|ConvertFrom-Json)
    } finally {
        if(-not $Job.process.HasExited) {$Job.process.Kill();[CliProbe]::WaitAfterKill($Job.process)}
        $Job.completed=$true;$Job.process.Dispose()
    }
}
function Begin-Command([string[]]$Arguments) {
    if(-not $script:pipeName) {throw 'Explicit owned pipe required'}
    return Begin-Probe $cli (@('--pipe',$script:pipeName,'--json')+$Arguments)
}
function Request([string[]]$Arguments,[int]$Exit=0) {return End-Command (Begin-Command $Arguments) @($Exit)}
function Tree {
    $tree=Request @('tree');$window=[IntPtr]([long]$tree.window_handle)
    if(-not $tree.background_testing -or [CliProbe]::IsWindowVisible($window) -or [CliProbe]::GetForegroundWindow() -eq $window) {throw 'Owned host became visible or foreground'}
    foreach($browser in $tree.browsers) {
        foreach($handle in @($browser.view_handle,$browser.chrome_handle,$browser.address_handle)) {
            if($handle -and ([CliProbe]::IsWindowVisible([IntPtr]([long]$handle)) -or [CliProbe]::GetForegroundWindow() -eq [IntPtr]([long]$handle))) {throw 'Owned browser became visible or foreground'}
        }
    }
    if($null -eq $tree.popup -or $tree.popup.limit -ne 16 -or $tree.popup.pending_limit -ne 8 -or $tree.popup.pending -lt 0 -or $tree.popup.pending -gt 8 -or $tree.popup.active -lt 0 -or $tree.popup.active -gt 16) {throw 'Popup diagnostics missing or out of bounds'}
    return $tree
}
function Start-Owned {
    $utc=[DateTime]::UtcNow
    $script:process=[CliProbe]::Start($gui,@('--temporary','--shell=cmd','--cwd',$directory),$directory,$directory)
    $script:hosts+=$process.Id;$script:stdout=$process.StandardOutput.ReadToEndAsync();$script:stderr=$process.StandardError.ReadToEndAsync()
    $file=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($process.Id).json";$deadline=(Get-Date).AddSeconds(20)
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
function Pane-In($Node,[string]$Surface) {
    if($Node.content -and @($Node.content.surfaces|Where-Object {$_.id -eq $Surface}).Count) {return $Node.id}
    foreach($child in @($Node.first,$Node.second)) {if($child) {$found=Pane-In $child $Surface;if($found) {return $found}}}
    return $null
}
function Pane-Of($Tree,[string]$Surface) {
    foreach($workspace in $Tree.workspaces) {$found=Pane-In $workspace.root $Surface;if($found) {return $found}}
    throw ('Surface absent from model: '+$Surface)
}
function Eval-Page([string]$Pane,[string]$Code) {return (Request @('browser','eval',$Pane,$Code)).result}
function Observe([string]$Pane) {
    return Eval-Page $Pane '(()=>{const p=window.popupFixture||{};let openerToken=null,openerError=null,openerClosed=null;try{openerClosed=window.opener?window.opener.closed:null;openerToken=window.opener&&!window.opener.closed&&window.opener.popupFixture?window.opener.popupFixture.sourceToken:null;}catch(e){openerError=String(e);}return {role:p.role,message:document.querySelector("#message")?.textContent,sourceToken:p.sourceToken,hasOpener:window.opener!==null,openerToken,openerClosed,openerError,href:location.href,ipc:typeof window.ipc,host:typeof window.flowmuxHost,identity:typeof window.__flowmuxIdentity,settings:typeof window.__flowmuxSettings,documentFocused:document.hasFocus()};})()'
}
function Assert-Untrusted($Observation) {
    if($Observation.ipc -ne 'undefined' -or $Observation.host -ne 'undefined' -or $Observation.identity -ne 'undefined' -or $Observation.settings -ne 'undefined' -or $Observation.documentFocused -ne $false) {throw ('Popup gained terminal bridge or focus: '+($Observation|ConvertTo-Json -Compress))}
}
function Wait-Page([string]$Pane,[string]$Surface,[string]$Suffix) {
    $deadline=(Get-Date).AddSeconds(8)
    do {
        $status=Request @('browser','status',$Pane)
        if($status.id -ne $Surface) {throw 'Unexpected active browser surface while waiting'}
        if(-not $status.loading -and $status.url.Contains($Suffix) -and (Eval-Page $Pane 'document.readyState') -eq 'complete') {return $status}
        if((Get-Date) -gt $deadline) {throw ('Owned popup document not ready: '+($status|ConvertTo-Json -Compress))}
        Start-Sleep -Milliseconds 25
    } while($true)
}
function Wait-Settled {
    $deadline=(Get-Date).AddSeconds(8)
    do {
        $tree=Tree
        if($tree.popup.pending -eq 0) {
            if($tree.popup.active -ne @($tree.browsers|Where-Object {$_.popup_opener}).Count) {throw 'Settled live popup count differs from browser ownership'}
            return $tree
        }
        if((Get-Date) -gt $deadline) {throw 'Popup deferrals did not settle'}
        Start-Sleep -Milliseconds 25
    } while($true)
}
function Wait-Child([string]$Opener,[string]$Pane,$Before,[string]$Suffix='/child') {
    $deadline=(Get-Date).AddSeconds(8)
    do {
        $tree=Tree;$children=@($tree.browsers|Where-Object {$_.popup_opener -eq $Opener -and $Before -notcontains $_.id})
        if($children.Count -gt 1) {throw 'One popup request created multiple children'}
        if($children.Count -eq 1 -and $tree.popup.pending -eq 0) {
            $child=$children[0]
            if((Pane-Of $tree $child.id) -ne $Pane) {throw 'Popup routed to a pane other than its original source'}
            $status=Wait-Page $Pane $child.id $Suffix
            if($status.popup_opener -ne $Opener) {throw 'Popup status lost original opener identity'}
            return $child
        }
        if((Get-Date) -gt $deadline) {throw ('Native popup child was not attached: '+($tree.popup|ConvertTo-Json -Compress))}
        Start-Sleep -Milliseconds 25
    } while($true)
}
function Wait-Gone([string]$Surface) {
    $deadline=(Get-Date).AddSeconds(5)
    do {$tree=Tree;if(@($tree.browsers|Where-Object {$_.id -eq $Surface}).Count -eq 0) {return $tree};if((Get-Date) -gt $deadline) {throw 'Native window.close left its browser surface present'};Start-Sleep -Milliseconds 25} while($true)
}
function Schedule-Close([string]$Pane) {
    # Closing can invalidate the eval callback; actual model removal is asserted separately.
    $job=Begin-Command @('browser','eval',$Pane,'setTimeout(()=>window.close(),100);true')
    End-Command $job @(0,1)|Out-Null
}
function Assert-Terminal {
    $current=@((Tree).surfaces|Where-Object {$_.id -eq $script:terminal.id})
    if($current.Count -ne 1 -or $current[0].pid -ne $script:terminal.pid -or -not $current[0].running) {throw 'Popup changed original terminal identity or process'}
}
function Reset-Case {
    foreach($browser in @((Tree).browsers)) {Request @('close-tab',$browser.id)|Out-Null}
    $tree=Wait-Settled
    if(@($tree.browsers).Count -ne 0 -or $tree.popup.active -ne 0) {throw 'Previous case left a browser or live popup'}
    Request @('focus-tab',$terminal.id)|Out-Null
    $script:opened=(Request @('browser','open',($origin+'/source'),'--pane',$source.pane)).browser_pane_opened
    $script:domPane=$opened.pane;Wait-Page $domPane $opened.surface '/source'|Out-Null
    $script:sourceObservation=Observe $domPane;Assert-Untrusted $sourceObservation
    if($sourceObservation.hasOpener -or (Request @('browser','status',$domPane)).popup_opener) {throw 'Manually opened source has a popup opener'}
}
function Open-Child([string]$Url) {
    $before=@((Tree).browsers|ForEach-Object {$_.id})
    $script:openResult=Eval-Page $domPane ('popupFixture.openUrl('+(Js $Url)+')')
    if(-not $openResult.returned -or $openResult.error) {throw 'Native popup did not return a WindowProxy'}
    $child=Wait-Child $opened.surface $domPane $before
    $observation=Observe $domPane;Assert-Untrusted $observation
    if(-not $observation.hasOpener -or -not (Same-Text $observation.openerToken $sourceObservation.sourceToken) -or $observation.openerError) {throw 'Child lost its native opener or same-origin access'}
    return $child
}
function Wait-Scheduled([int]$Before) {
    $deadline=(Get-Date).AddSeconds(5)
    while($fixture.CompletedScheduledRequests -le $Before) {if((Get-Date) -gt $deadline) {throw 'Scheduled owned popup did not finish while hidden'};Start-Sleep -Milliseconds 25}
}
function Arm-GatedOpen([string]$Url) {
    $fixture.BlockPopup();$before=$fixture.PopupGateRequests;$events=$fixture.CompletedScheduledRequests
    if(-not (Eval-Page $domPane ('popupFixture.gatedOpen('+(Js $Url)+')')).armed) {throw 'Owned popup gate was not armed'}
    $deadline=(Get-Date).AddSeconds(3)
    while($fixture.PopupGateRequests -le $before) {if((Get-Date) -gt $deadline) {throw 'Owned popup response gate was not reached'};Start-Sleep -Milliseconds 25}
    return $events
}
function Observe-NegativePopup {
    # This is an intentional bounded negative observation interval, not a native-delivery claim.
    $last=Wait-Settled;$timer=[Diagnostics.Stopwatch]::StartNew()
    do {
        Start-Sleep -Milliseconds 25;$next=Wait-Settled
        if($next.popup.opened -ne $last.popup.opened -or @($next.browsers).Count -ne @($last.browsers).Count) {throw 'Rejected popup appeared during negative observation interval'}
        $last=$next
    } while($timer.ElapsedMilliseconds -lt 200)
    return $last
}

try {
    $doctor=End-Command (Begin-Probe $cli @('doctor'))
    if(-not $doctor.background_testing -or $doctor.status -ne 'ok') {throw 'Working hidden build required; no host launched'}
    $tree=Start-Owned;$source=Request @('identify');$script:terminal=$tree.surfaces[0];$shells+=$terminal.pid
    $fixture=New-Object PopupFixture;$origin=$fixture.Origin
    $groups=@('url','target','blank','history','source-pin','hidden','move','source-close','child-close','manual-close','policy','capacity','flood','final-close')
    if($Case -eq 'startup') {Passed 'hidden_doctor_and_owned_host_readiness'}
    foreach($group in $groups) {
        if($Case -ne 'all' -and $Case -ne $group) {continue}
        Reset-Case
        switch($group) {
            'url' {
                $child=Open-Child ($origin+'/child?message='+[uri]::EscapeDataString([PopupFixture]::Unicode))
                $observation=Observe $domPane
                if(-not (Same-Text $observation.message ([PopupFixture]::Unicode))) {throw 'Unicode popup URL payload changed codepoints'}
                Request @('focus-tab',$opened.surface)|Out-Null
                $ref=Eval-Page $domPane ('popupFixture.reference('+$openResult.index+')')
                if(-not $ref.exists -or $ref.closed -or -not (Same-Text (Observe $domPane).sourceToken $sourceObservation.sourceToken)) {throw 'Original browser or returned WindowProxy was replaced'}
                $messages=Eval-Page $domPane 'popupFixture.messages'
                if(@($messages|Where-Object {Same-Text $_.message ([PopupFixture]::Unicode)}).Count -ne 1) {throw 'Child could not write through native opener'}
                $evidence.observations+=@{case=$group;child=$child.id;unicode=$observation.message;windowProxy=$ref}
                Passed 'script_url_creates_same_pane_tab_with_native_opener_unicode_and_no_terminal_bridge'
            }
            'target' {
                $before=@((Tree).browsers|ForEach-Object {$_.id})
                Eval-Page $domPane 'document.getElementById("target-blank").click();true'|Out-Null
                $child=Wait-Child $opened.surface $domPane $before;$observation=Observe $domPane;Assert-Untrusted $observation
                if(-not $observation.hasOpener -or -not (Same-Text $observation.openerToken $sourceObservation.sourceToken) -or -not (Same-Text $observation.message ([PopupFixture]::Unicode))) {throw 'DOM target-blank link lost opener or Unicode'}
                Passed 'dom_target_blank_rel_opener_link_routes_to_browser_tab'
            }
            'blank' {
                $before=@((Tree).browsers|ForEach-Object {$_.id});$result=Eval-Page $domPane 'popupFixture.openBlank()'
                if(-not $result.returned -or -not $result.populated) {throw ('Native blank popup could not be populated synchronously: '+($result|ConvertTo-Json -Compress))}
                $child=Wait-Child $opened.surface $domPane $before 'about:blank';$observation=Observe $domPane;Assert-Untrusted $observation
                if($observation.role -ne 'blank-child' -or -not (Same-Text $observation.message ([PopupFixture]::Unicode)) -or -not (Same-Text $observation.openerToken $sourceObservation.sourceToken)) {throw 'Blank popup document.write or opener was lost'}
                Passed 'native_about_blank_window_proxy_supports_synchronous_unicode_document_write'
            }
            'history' {
                $child=Open-Child ($origin+'/child')
                Request @('browser','navigate',$domPane,($origin+'/next'))|Out-Null;Wait-Page $domPane $child.id '/next'|Out-Null
                Request @('browser','back',$domPane)|Out-Null;Wait-Page $domPane $child.id '/child'|Out-Null
                Request @('browser','forward',$domPane)|Out-Null;Wait-Page $domPane $child.id '/next'|Out-Null
                $observation=Observe $domPane;Assert-Untrusted $observation
                if(-not (Same-Text $observation.openerToken $sourceObservation.sourceToken)) {throw 'History navigation lost native opener'}
                Passed 'popup_child_navigation_back_forward_preserve_opener'
            }
            'source-pin' {
                $before=@((Tree).browsers|ForEach-Object {$_.id})
                $events=Arm-GatedOpen '/child?pin=source'
                Request @('focus-pane',$source.pane)|Out-Null
                if((Request @('identify')).pane -ne $source.pane) {throw 'Logical focus did not switch before gate release'}
                $fixture.ReleasePopup();Wait-Scheduled $events
                $child=Wait-Child $opened.surface $domPane $before;$observation=Observe $domPane;Assert-Untrusted $observation
                if(-not (Same-Text $observation.openerToken $sourceObservation.sourceToken)) {throw 'Logical focus switch rebound popup source'}
                Passed 'delayed_dom_request_uses_source_surface_while_another_pane_is_logically_focused'
            }
            'hidden' {
                $before=Tree;$events=Arm-GatedOpen '/child?hidden=tab'
                $second=(Request @('browser','open',($origin+'/source?other'),'--pane',$source.pane)).browser_pane_opened
                if($second.pane -ne $domPane) {throw 'Hidden fixture did not reuse source pane'}
                if(@((Tree).browsers|Where-Object {$_.id -eq $opened.surface -and -not $_.visible}).Count -ne 1) {throw 'Source tab was not hidden before gate release'}
                $fixture.ReleasePopup()
                Wait-Scheduled $events;$after=Wait-Settled
                if($after.popup.opened -ne $before.popup.opened -or $after.popup.rejected -le $before.popup.rejected -or @($after.browsers).Count -ne 2 -or (Request @('browser','status',$domPane)).id -ne $second.surface) {throw 'Hidden source popup was accepted or changed active tab'}
                Request @('focus-tab',$opened.surface)|Out-Null
                if(-not (Eval-Page $domPane 'popupFixture.scheduled.fired')) {throw 'Hidden source timer did not fire'}
                $before=Tree;$events=Arm-GatedOpen '/child?hidden=workspace'
                Request @('new-workspace','--shell=cmd','--cwd',$directory)|Out-Null;$other=Request @('identify');$otherTree=Tree
                $shells+=@($otherTree.surfaces|Where-Object {$_.pid -and $shells -notcontains $_.pid}|ForEach-Object {$_.pid})
                if($otherTree.active_workspace -ne $other.workspace -or @($otherTree.browsers|Where-Object {$_.id -eq $opened.surface -and -not $_.visible}).Count -ne 1) {throw 'Source workspace was not hidden before gate release'}
                $fixture.ReleasePopup()
                Wait-Scheduled $events;$after=Wait-Settled
                if($after.popup.opened -ne $before.popup.opened -or $after.popup.rejected -le $before.popup.rejected -or $after.active_workspace -ne $other.workspace) {throw 'Hidden workspace popup was accepted or activated its workspace'}
                Request @('workspace','focus',$source.workspace)|Out-Null;Request @('workspace','close',$other.workspace)|Out-Null
                Passed 'delivered_hidden_tab_and_workspace_popup_requests_rejected_without_activation'
            }
            'move' {
                Request @('move-tab',$opened.surface,'--to-pane',$source.pane)|Out-Null;$script:domPane=$source.pane
                if(-not (Same-Text (Observe $domPane).sourceToken $sourceObservation.sourceToken)) {throw 'Moving source recreated its document'}
                $child=Open-Child ($origin+'/child?moved=source')
                if((Pane-Of (Tree) $child.id) -ne $source.pane) {throw 'Moved source popup used stale pane'}
                Passed 'popup_after_source_move_uses_current_pane_and_same_native_opener'
            }
            'source-close' {
                $child=Open-Child ($origin+'/child');$childToken=(Observe $domPane).sourceToken
                Request @('close-tab',$opened.surface)|Out-Null
                $observation=Observe $domPane;Assert-Untrusted $observation
                $actualStatus=Request @('browser','status',$domPane)
                $sourceAbsent=@((Tree).browsers|Where-Object {$_.id -eq $opened.surface}).Count -eq 0
                $evidence.sourceClose=@{observation=$observation;actualStatus=$actualStatus;expectedChild=$child.id;expectedChildToken=$childToken;closedSource=$opened.surface;sourceAbsent=$sourceAbsent}
                $openerCleared=$observation.hasOpener -eq $false -and $null -eq $observation.openerToken -and $null -eq $observation.openerClosed
                $openerClosed=$observation.hasOpener -eq $true -and $observation.openerClosed -eq $true
                if($actualStatus.id -ne $child.id -or $actualStatus.native_closed -or -not $sourceAbsent -or -not (Same-Text $observation.sourceToken $childToken) -or $observation.openerError -or -not ($openerCleared -or $openerClosed)) {throw 'Closing opener cascaded to child or left its native opener open'}
                Passed 'closing_source_preserves_child_and_clears_or_closes_native_opener'
            }
            'child-close' {
                $child=Open-Child ($origin+'/child')
                Schedule-Close $domPane;$after=Wait-Gone $child.id
                if($after.popup.active -ne 0 -or (Request @('browser','status',$domPane)).id -ne $opened.surface) {throw 'Child close removed another surface or left popup active'}
                $ref=Eval-Page $domPane ('popupFixture.reference('+$openResult.index+')')
                if(-not $ref.exists -or $ref.closed -ne $true) {throw 'Closed child WindowProxy does not report closed'}
                Passed 'native_child_window_close_removes_exact_tab_and_closes_its_window_proxy'
            }
            'manual-close' {
                Schedule-Close $domPane
                $deadline=(Get-Date).AddSeconds(1)
                do {$after=Tree;$remaining=@($after.browsers|Where-Object {$_.id -eq $opened.surface});if($remaining.Count -eq 0) {break};Start-Sleep -Milliseconds 25} while((Get-Date) -lt $deadline)
                $nativeCloseObserved=$remaining.Count -eq 0
                if(-not $nativeCloseObserved) {Assert-Untrusted (Observe $domPane)}
                $evidence.observations+=@{case=$group;nativeCloseObserved=$nativeCloseObserved;note='Blink may refuse window.close on a manually opened browser; lack of an event is not a host-close success.'}
                Passed 'manual_browser_close_engine_outcome_recorded_and_original_terminal_preserved'
            }
            'policy' {
                $evidence.policy=@()
                foreach($url in @('file:///C:/flowmux-owned-popup-fixture-missing.html','javascript:window.popupPolicyMarker=true','http://flowmux-terminal.localhost/','https://flowmux-terminal.localhost/')) {
                    $before=Tree;$result=Eval-Page $domPane ('popupFixture.openUrl('+(Js $url)+')');$after=Observe-NegativePopup
                    if(@($after.browsers).Count -ne 1 -or $after.popup.opened -ne $before.popup.opened) {throw 'Rejected popup URL created a browser tab'}
                    if((Eval-Page $domPane 'typeof window.popupPolicyMarker') -ne 'undefined') {throw 'JavaScript popup URL ran in source document'}
                    $evidence.policy+=@{url=$url;windowOpen=$result;hostRejectedDelta=($after.popup.rejected-$before.popup.rejected);note='Zero delta means the engine may have blocked the request before host delivery.'}
                }
                Request @('browser','eval',$source.pane,'window.open("about:blank")') 1|Out-Null
                Passed 'invalid_local_script_and_reserved_origin_popups_create_no_tab_and_terminal_has_no_browser_eval'
            }
            'capacity' {
                $children=@()
                for($index=0;$index -lt 16;$index++) {
                    Request @('focus-tab',$opened.surface)|Out-Null
                    $child=Open-Child ($origin+'/child?capacity='+$index);$children+=$child.id
                }
                Request @('focus-tab',$opened.surface)|Out-Null;$before=Tree
                if($before.popup.active -ne 16) {throw 'Could not reach the documented live popup limit'}
                Eval-Page $domPane 'popupFixture.openUrl("/child?capacity=overflow")'|Out-Null;$after=Wait-Settled
                if($after.popup.active -ne 16 -or @($after.browsers).Count -ne 17 -or $after.popup.opened -ne $before.popup.opened -or $after.popup.rejected -le $before.popup.rejected) {throw 'Seventeenth popup did not reject at the live limit'}
                Request @('close-tab',$children[0])|Out-Null;Request @('focus-tab',$opened.surface)|Out-Null
                Open-Child ($origin+'/child?capacity=recovered')|Out-Null
                if((Tree).popup.active -ne 16) {throw 'Closing a popup did not release live capacity'}
                Passed 'sixteen_live_popup_cap_rejects_seventeenth_and_recovers_after_close'
            }
            'flood' {
                $before=Tree;$results=Eval-Page $domPane 'popupFixture.flood(32)';$after=Wait-Settled
                $new=@($after.browsers|Where-Object {$_.id -ne $opened.surface})
                if(@($results).Count -ne 32 -or $after.popup.rejected -le $before.popup.rejected -or $new.Count -lt 1 -or $new.Count -gt 16 -or @($new|Where-Object {$_.popup_opener -ne $opened.surface}).Count) {throw 'Popup flood did not accept bounded children and reject excess requests'}
                $evidence.observations+=@{case=$group;attempted=32;created=$new.Count;openedDelta=($after.popup.opened-$before.popup.opened);rejectedDelta=($after.popup.rejected-$before.popup.rejected);pending=$after.popup.pending;note='Admission can reject on visibility or pending/live bounds; this does not prove eight requests were simultaneously held.'}
                Passed 'script_popup_flood_has_bounded_children_and_all_deferrals_settle'
            }
            'final-close' {
                $child=Open-Child ($origin+'/child');$pane=$domPane;$workspace=(Tree).active_workspace
                Request @('close-tab',$opened.surface)|Out-Null;Request @('close-tab',$terminal.id)|Out-Null
                $before=Tree
                if(@($before.browsers).Count -ne 1 -or @($before.surfaces).Count -ne 0 -or (Pane-Of $before $child.id) -ne $pane) {throw 'Final popup fixture did not leave exactly one browser'}
                Schedule-Close $pane;$after=Wait-Gone $child.id
                if($after.active_workspace -ne $workspace -or @($after.browsers).Count -ne 1 -or @($after.surfaces).Count -ne 0 -or (Pane-Of $after $after.browsers[0].id) -ne $pane -or $after.popup.active -ne 0) {throw 'Final native close failed to preserve its workspace and pane with a fresh browser'}
                $replacement=$after.browsers[0];Wait-Page $pane $replacement.id 'about:blank'|Out-Null
                if($replacement.id -eq $child.id -or $replacement.popup_opener) {throw 'Final close reused stale surface identity or opener'}
                Assert-Untrusted (Observe $pane)
                Passed 'final_popup_native_close_replaces_closed_surface_with_fresh_blank_browser_in_same_pane'
            }
        }
        if($group -ne 'final-close') {Assert-Terminal}
        $evidence.observations+=@{case=$group;popup=(Wait-Settled).popup}
    }
    Tree|Out-Null;$evidence.status='passed_background_browser_popups_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
    if($fixture) {$fixture.ReleaseSlow();$fixture.ReleasePopup();$evidence.requests=$fixture.RequestLog()}
    foreach($job in $clients) {
        if(-not $job.completed) {try {if(-not $job.process.HasExited) {$job.process.Kill();[CliProbe]::WaitAfterKill($job.process)};$job.process.Dispose();$job.completed=$true} catch {$cleanupErrors+=$_.Exception.Message}}
    }
    if($pipeName -and $process -and -not $process.HasExited) {try {Request @('quit','--discard-state')|Out-Null} catch {$cleanupErrors+=$_.Exception.Message}}
    if($process) {
        try {
            if(-not $process.HasExited -and -not $process.WaitForExit(5000)) {$process.Kill();[CliProbe]::WaitAfterKill($process)}
            if($stderr) {$stderr.Wait(1000)|Out-Null;$evidence.hostStderr=[CliProbe]::Output($stderr)}
            $evidence.hostExitCode=$process.ExitCode;if($process.ExitCode -ne 0) {$cleanupErrors+=('Owned host exited '+$process.ExitCode)}
        } catch {$cleanupErrors+=$_.Exception.Message} finally {$process.Dispose()}
    }
    if($fixture) {$fixture.Dispose()}
    if($cleanupErrors.Count) {$evidence.status='failed';$evidence.cleanupErrors=$cleanupErrors}
    $evidence.hosts=$hosts;$evidence.shells=$shells;$evidence.clientPids=@($clients|ForEach-Object {$_.pid});$evidence.finished=(Get-Date).ToString('o')
    if ($evidence.status -eq 'failed') { $evidence|ConvertTo-Json -Depth 14|Set-Content -Encoding UTF8 (Join-Path $directory 'native-browser-popups-background.json') }
    if ($evidence.status -eq 'failed') { Write-Output ('Evidence: '+$directory) }
}
if($cleanupErrors.Count) {throw ($cleanupErrors -join '; ')}
[ordered]@{status=$evidence.status;checks=$evidence.checks.Count}|ConvertTo-Json -Compress