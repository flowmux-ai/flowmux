# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden owned Files copy/rename/move + native LISTBOX + real Monaco verification.
# Run under run-check.ps1 with a120s Job limit; ordinary CLI calls cap at5s.
param(
    [string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",
    [ValidateSet('all','startup','roundtrip','copy-bytes','rejections','editor-guard','stale-token')][string]$Case='all'
)
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path
$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'EditorFixture.cs'),(Join-Path $PSScriptRoot 'FilesFixture.cs'),(Join-Path $PSScriptRoot 'FilesActionsFixture.cs')
$directory=Join-Path $PSScriptRoot ('..\dist\evidence\files-actions-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$fixture=New-Object FilesFixture($directory)
$process=$null;$pipeName=$null;$stdout=$null;$stderr=$null;$hostExitRecorded=$false;$hostForced=$false
$hosts=@();$shells=@();$clients=@();$ownedEditors=@();$cleanupErrors=@();$storageObserved=@()
$activeOperations=@();$caseRoot=$fixture.Root;$suiteClock=[Diagnostics.Stopwatch]::StartNew();$cleaning=$false;$group='startup'
$evidence=[ordered]@{started=(Get-Date).ToString('o');mode='background';case=$Case;checks=@();observations=@();clipboardAccess=$false;desktopInput=$false;externalSites=$false;realImeTest=$false;unicodeComparison='ordinal';deferred=@(
    'No physical keyboard/mouse/IME, glyph rendering, clipboard, native dialogs, DPI or accessibility checks.',
    'Commands exercise retained native model state; LISTBOX observations are read-only bounded system messages, not simulated mouse/keyboard input.',
    'No operation is timed by a fixed dwell: status is polled immediately with a ten-second condition bound; ordinary CLI calls cap at five seconds.',
    'Cancellation/blocked I/O, admission/close races, every resource cap, hostile reparse swaps, ACL denial and crash recovery are not forced by these cases.',
    'File actions use owned fixture files only. Exact byte checks cover UTF8 BOM/CRLF and binary NUL; operation status does not substitute for disk or native control observations.'
)}

function Same-Text([string]$Left,[string]$Right) {return [string]::Equals($Left,$Right,[StringComparison]::Ordinal)}
function Passed([string]$Name) {$script:evidence.checks+=@{name=$Name;passed=$true};Write-Host ('[check] passed '+$Name)}
function Begin-Probe([string]$File,[string[]]$Arguments) {
    if(-not $script:cleaning -and $suiteClock.ElapsedMilliseconds -ge 110000) {throw 'Files verifier exhausted its110s inner budget'}
    $p=[CliProbe]::Start($File,$Arguments,$directory,$directory)
    $job=[pscustomobject]@{process=$p;pid=$p.Id;file=$File;arguments=$Arguments;output=$p.StandardOutput.ReadToEndAsync();error=$p.StandardError.ReadToEndAsync();completed=$false}
    $script:clients+=$job;return $job
}
function End-Command($Job,[int[]]$AllowedExits=@(0),[ValidateRange(1,20000)][int]$TimeoutMilliseconds=5000) {
    try {
        if(-not $script:cleaning) {$TimeoutMilliseconds=[int][Math]::Max(1,[Math]::Min($TimeoutMilliseconds,110000-$suiteClock.ElapsedMilliseconds))}
        if(-not $Job.process.WaitForExit($TimeoutMilliseconds)) {$Job.process.Kill();[CliProbe]::WaitAfterKill($Job.process);throw ('Owned CLI exceeded '+$TimeoutMilliseconds+'ms; not retried')}
        $outDone=$Job.output.Wait(1000);$errDone=$Job.error.Wait(1000)
        if(-not $outDone -or -not $errDone) {throw 'Owned CLI output did not close'}
        $code=$Job.process.ExitCode;$out=[CliProbe]::Output($Job.output);$err=[CliProbe]::Output($Job.error)
        if($AllowedExits -notcontains $code) {throw ('Owned CLI exit '+$code+': '+$err+' '+$out)}
        if($code -eq 0) {return ($out|ConvertFrom-Json)}
        return ($err|ConvertFrom-Json)
    } catch {
        $reason=$_.Exception.Message
        $script:evidence.observations+=@{kind='command-failure';pid=$Job.pid;file=$Job.file;arguments=$Job.arguments;reason=$reason;stdout=[CliProbe]::Output($Job.output);stderr=[CliProbe]::Output($Job.error)}
        throw ('Owned command failed: '+$Job.file+' '+($Job.arguments|ConvertTo-Json -Compress)+'; '+$reason)
    } finally {
        if(-not $Job.process.HasExited) {$Job.process.Kill();[CliProbe]::WaitAfterKill($Job.process)}
        $Job.completed=$true;$Job.process.Dispose()
    }
}
function Begin-Command([string[]]$Arguments) {
    if(-not $script:pipeName) {throw 'Explicit owned pipe required'}
    return Begin-Probe $cli (@('--pipe',$script:pipeName,'--json')+$Arguments)
}
function Request([string[]]$Arguments,[int]$Exit=0,[ValidateRange(1,20000)][int]$TimeoutMilliseconds=5000) {return End-Command (Begin-Command $Arguments) @($Exit) $TimeoutMilliseconds}
function Check-Hidden([long]$Handle) {
    if($Handle -and ([CliProbe]::IsWindowVisible([IntPtr]$Handle) -or [CliProbe]::GetForegroundWindow() -eq [IntPtr]$Handle)) {throw 'Owned window became visible or foreground'}
}
function Tree([ValidateRange(1,20000)][int]$TimeoutMilliseconds=5000) {
    $tree=Request @('tree') 0 $TimeoutMilliseconds
    if(-not $tree.background_testing) {throw 'Host is not in hidden debug mode'}
    Check-Hidden ([long]$tree.window_handle)
    foreach($view in @($tree.browsers)+@($tree.editors)) {
        if($view) {foreach($handle in @($view.view_handle,$view.chrome_handle)) {if($handle) {Check-Hidden ([long]$handle)}}}
    }
    return $tree
}
function Remaining-StartupBudget([Diagnostics.Stopwatch]$Clock) {
    $remaining=8000-$Clock.ElapsedMilliseconds
    if($remaining -le 0) {throw 'Owned host exceeded its total eight-second startup budget'}
    return [int]$remaining
}
function Start-Owned([bool]$Persistent=$false,[string]$Restore='') {
    $arguments=@('--temporary','--shell=cmd','--cwd',$fixture.Root)
    if($Persistent) {$arguments=@('--new-window','--shell=cmd','--cwd',$fixture.Root)}
    if($Restore) {$arguments=@('--restore-window',$Restore)}
    $utc=[DateTime]::UtcNow;$startupWatch=[Diagnostics.Stopwatch]::StartNew()
    $startup=[ordered]@{kind='host-startup';pid=$null;restoring=[bool]$Restore;restoreWindow=$Restore;budgetMs=8000;started=$utc.ToString('o');status='starting';stage='launch';identityResponse=$null}
    $script:evidence.observations+=$startup
    try {
        $script:process=[CliProbe]::Start($gui,$arguments,$directory,$directory)
        $script:hostExitRecorded=$false;$script:hostForced=$false
        $script:hosts+=$process.Id;$script:stdout=$process.StandardOutput.ReadToEndAsync();$script:stderr=$process.StandardError.ReadToEndAsync();$script:pipeName=$null
        $startup.pid=$process.Id;$startup.stage='discovery'
        $file=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($process.Id).json"
        do {
            Remaining-StartupBudget $startupWatch|Out-Null
            if($process.HasExited) {throw ('Owned startup failed: '+[CliProbe]::Output($stderr))}
            if((Test-Path -LiteralPath $file) -and (Get-Item -LiteralPath $file).LastWriteTimeUtc -ge $utc) {
                $record=Get-Content -Raw -LiteralPath $file|ConvertFrom-Json
                if($record.pid -ne $process.Id) {throw 'Wrong discovery owner'}
                $script:pipeName=$record.pipe;break
            }
            Start-Sleep -Milliseconds ([Math]::Min(20,(Remaining-StartupBudget $startupWatch)))
        } while($true)
        $startup.discoveryElapsedMs=$startupWatch.ElapsedMilliseconds;$startup.stage='identify'
        $startup.identityBudgetMs=[Math]::Min(5000,(Remaining-StartupBudget $startupWatch))
        $identity=Request @('identify') 0 $startup.identityBudgetMs
        $startup.identityResponse=$identity;$startup.identityElapsedMs=$startupWatch.ElapsedMilliseconds
        Remaining-StartupBudget $startupWatch|Out-Null
        if($identity.pid -ne $process.Id) {throw 'Wrong pipe owner'}
        $startup.stage='terminal-readiness';$startup.treeRequests=0
        do {
            $startup.treeRequests++
            $tree=Tree ([Math]::Min(5000,(Remaining-StartupBudget $startupWatch)))
            Remaining-StartupBudget $startupWatch|Out-Null
            if(@($tree.surfaces|Where-Object {-not $_.ready}).Count -eq 0) {
                $script:shells+=@($tree.surfaces|Where-Object {$_.pid -and $script:shells -notcontains $_.pid}|ForEach-Object {$_.pid})
                $startup.stage='complete';$startup.status='ready'
                return $tree
            }
            Start-Sleep -Milliseconds ([Math]::Min(20,(Remaining-StartupBudget $startupWatch)))
        } while($true)
    } catch {
        $startup.status='failed';$startup.error=$_.Exception.Message
        throw
    } finally {
        $startup.elapsedMs=$startupWatch.ElapsedMilliseconds;$startup.finished=[DateTime]::UtcNow.ToString('o')
    }
}
function Record-HostExit {
    if(-not $script:process -or $script:hostExitRecorded) {return}
    $outDone=$false;$errDone=$false
    try {$outDone=$stdout.Wait(1000)} catch {$script:cleanupErrors+=('Host stdout capture: '+$_.Exception.Message)}
    try {$errDone=$stderr.Wait(1000)} catch {$script:cleanupErrors+=('Host stderr capture: '+$_.Exception.Message)}
    $exited=$process.HasExited;$code=if($exited) {$process.ExitCode} else {$null}
    $script:evidence.observations+=@{kind='host-exit';pid=$process.Id;exited=$exited;exitCode=$code;forced=$script:hostForced;stdoutComplete=$outDone;stderrComplete=$errDone;stdout=[CliProbe]::Output($stdout);stderr=[CliProbe]::Output($stderr)}
    $script:hostExitRecorded=$true
}
function Finish-Host([bool]$Save=$false) {
    if(-not $script:process) {return}
    if(-not $process.HasExited) {
        if($Save) {Request @('quit')|Out-Null} else {Request @('quit','--discard-state')|Out-Null}
    }
    if(-not $process.WaitForExit(5000)) {$script:hostForced=$true;$process.Kill();[CliProbe]::WaitAfterKill($process);throw 'Owned host did not exit within five seconds'}
    Record-HostExit
    if($process.ExitCode -ne 0) {throw ('Owned host exited '+$process.ExitCode)}
    $process.Dispose();$script:process=$null;$script:pipeName=$null
}
function Status([string]$Surface,[ValidateRange(1,20000)][int]$TimeoutMilliseconds=5000) {
    $r=Request @('editor','status',$Surface) 0 $TimeoutMilliseconds
    if($r.view_handle) {Check-Hidden ([long]$r.view_handle)}
    $expectedRoot=Join-Path $directory 'state';$expectedProfile=Join-Path $expectedRoot 'editor-profile';$expectedRecovery=Join-Path $expectedRoot 'editor-recovery'
    if(-not (Same-Text $r.storage_root $expectedRoot) -or -not (Same-Text $r.profile_path $expectedProfile) -or -not (Same-Text $r.recovery_root $expectedRecovery)) {throw ('Editor storage/profile/recovery escaped the explicit owned root: '+($r|ConvertTo-Json -Depth 5 -Compress))}
    $key=$process.Id.ToString()+'/'+$Surface
    if($script:storageObserved -notcontains $key) {
        $script:storageObserved+=$key
        $script:evidence.observations+=@{kind='editor-storage';pid=$process.Id;surface=$Surface;storageRoot=$r.storage_root;profilePath=$r.profile_path;recoveryRoot=$r.recovery_root}
    }
    return $r
}
function Ready([string]$Surface) {
    $clock=[Diagnostics.Stopwatch]::StartNew()
    do {
        $remaining=5000-$clock.ElapsedMilliseconds;Require ($remaining -gt 0) 'Editor exceeded total five-second readiness budget'
        $r=Status $Surface ([int]$remaining);if($r.ready) {return $r}
        Start-Sleep -Milliseconds ([int][Math]::Min(20,[Math]::Max(1,5000-$clock.ElapsedMilliseconds)))
    } while($true)
}
function Editor-Command([string]$Surface,[string]$Action,[string[]]$Options=@(),[int]$Exit=0) {
    $r=Request (@('editor','command',$Surface,$Action)+$Options) $Exit
    if($r -and $r.psobject.Properties.Name -contains 'result') {return $r.result}
    return $r
}
function Read-Editor([string]$Surface) {
    $r=Editor-Command $Surface 'read'
    if($r.document_focused -ne $false) {throw 'Owned editor document gained focus or read omitted focus state'}
    if($r.content_truncated) {throw 'Small editor fixture unexpectedly truncated'}
    return $r
}
function Flush([string]$Surface) {Request @('editor','flush',$Surface)|Out-Null}
function Assert-Text([string]$Surface,[string]$Expected,[bool]$Dirty) {
    Flush $Surface;$r=Read-Editor $Surface;$status=Status $Surface
    if(-not (Same-Text $r.content $Expected) -or $r.dirty -ne $Dirty -or $status.dirty -ne $Dirty) {throw ('Editor content/dirty differs: '+($r|ConvertTo-Json -Compress -Depth 6))}
    return $r
}
function Assert-Bytes([string]$Path,[string]$Text,[bool]$Bom=$false,[bool]$CrLf=$false) {
    if(-not $fixture.BytesEqual($Path,[EditorFixture]::Encode($Text,$Bom,$CrLf))) {throw ('Exact file bytes differ: '+[IO.Path]::GetFileName($Path))}
}
function Complete-Open($Job) {
    $opened=(End-Command $Job).editor_opened
    if(-not $opened.surface -or -not $opened.pane) {throw 'Editor open omitted pane/surface identity'}
    if($script:ownedEditors -notcontains $opened.surface) {$script:ownedEditors+=$opened.surface}
    return $opened
}
function Open-Editor([string]$Path,[string]$Pane='',[string]$Root='') {
    if(-not $Pane) {$Pane=$script:source.pane}
    if(-not $Root) {$Root=$script:caseRoot}
    $opened=Complete-Open (Begin-Command @('editor','open',$Path,'--pane',$Pane,'--root',$Root))
    Ready $opened.surface|Out-Null;Tree|Out-Null
    return $opened
}
function Assert-Terminal {
    $current=@((Tree).surfaces|Where-Object {$_.id -eq $script:terminal.id})
    if($current.Count -ne 1 -or $current[0].pid -ne $script:terminal.pid -or -not $current[0].running) {throw 'Editor changed original terminal process identity'}
}
function Clean-Editors {
    foreach($surface in @($script:ownedEditors)) {
        $status=Status $surface
        $limit=@($status.documents).Count+1
        while(@($status.documents).Count -gt 0 -and $limit -gt 0) {
            Editor-Command $surface 'discard-document'|Out-Null;Flush $surface;$status=Status $surface;$limit--
        }
        if(@($status.documents).Count) {throw 'Owned editor cleanup did not discard its documents'}
        Request @('close-tab',$surface)|Out-Null
    }
    $script:ownedEditors=@()
}

function Require([bool]$Condition,[string]$Message) {if(-not $Condition) {throw $Message}}
function Expect-Error($Result,[string]$Pattern) {Require ($Result.error -and [string]$Result.error -match $Pattern) ('Unexpected rejection: '+($Result|ConvertTo-Json -Depth 8 -Compress))}
function Assert-OwnedWindows {
    Tree|Out-Null
    Require (@([EditorWindowProbe]::VisibleTopLevelWindows($process)).Count -eq 0) 'Owned host has a visible top-level window'
}
function Files-Status([string]$Pane,[int]$Offset=0,[int]$TimeoutMilliseconds=5000) {
    $r=Request @('files','status','--pane',$Pane,'--offset',$Offset.ToString()) 0 $TimeoutMilliseconds
    Require ($r.pane -eq $Pane) 'Files status returned another pane'
    foreach($handle in @($r.panel_handle,$r.list_handle)) {if($handle) {Check-Hidden ([long]$handle)}}
    Require (@($r.rows).Count -le 500) 'Files status exceeded500 row response cap'
    Require ([Text.Encoding]::UTF8.GetByteCount(($r|ConvertTo-Json -Compress -Depth 20)) -le 1048576) 'Files status exceeded one-MiB response cap'
    return $r
}
function Settled([string]$Pane) {
    $clock=[Diagnostics.Stopwatch]::StartNew()
    do {
        $left=5000-$clock.ElapsedMilliseconds;Require ($left -gt 0) 'Files did not settle within five seconds'
        $r=Files-Status $Pane 0 ([int]$left)
        if(-not $r.loading) {return $r}
        Start-Sleep -Milliseconds ([int][Math]::Min(20,[Math]::Max(1,5000-$clock.ElapsedMilliseconds)))
    } while($true)
}
function Show-Files([string]$Pane,[string]$Root) {
    Request @('files','show','--pane',$Pane,'--root',$Root)|Out-Null
    $r=Settled $Pane
    Require ($r.visible -and $r.token -and -not $r.stale -and -not $r.last_error -and -not $r.truncated) ('Files show did not produce a complete current snapshot: '+($r|ConvertTo-Json -Depth 6 -Compress))
    Require (Same-Text $r.root $Root) 'Files root changed its ordinal Unicode path'
    Require ($r.panel_handle -and $r.list_handle) 'Files omitted real native child handles'
    return $r
}
function All-Rows($Status) {
    $rows=@($Status.rows);$next=$Status.next_offset;$pages=1
    while($null -ne $next) {
        Require ($pages -lt 40) 'Files status pagination exceeded snapshot cap'
        $page=Files-Status $Status.pane ([int]$next)
        Require ($page.token -eq $Status.token -and $page.offset -eq $next -and $page.rendered_rows -eq $Status.rendered_rows) 'Files token/order changed while paging'
        if($null -ne $page.next_offset) {Require ($page.next_offset -gt $next) 'Files status next_offset did not advance'}
        $rows+=@($page.rows);$next=$page.next_offset;$pages++
    }
    Require ($rows.Count -eq $Status.rendered_rows) 'Files status pages differ from rendered-row count'
    return $rows
}
function Native-Snapshot($Status) {
    $tree=Tree;$rows=@(All-Rows $Status)
    $native=[FilesListProbe]::Read($process,([long]$tree.window_handle),([long]$Status.panel_handle),([long]$Status.list_handle))
    Require ($native.Count -eq $Status.native_count -and $native.Count -eq $Status.rendered_rows -and $native.Count -eq $rows.Count) 'Retained rows differ from actual native LISTBOX count'
    $selected=@()
    for($i=0;$i -lt $rows.Count;$i++) {
        $row=$rows[$i];$marker='    '
        if($row.reparse) {$marker='[!] '} elseif($row.directory) {if($row.expanded) {$marker='[-] '} else {$marker='[+] '}}
        $label=('  '*[int]$row.depth)+$marker+$row.name
        Require (Same-Text $native.Text[$i] $label) ('Actual LISTBOX Unicode label differs at visible index '+$i+': '+($native.Text[$i]|ConvertTo-Json -Compress)+' expected '+($label|ConvertTo-Json -Compress))
        if($row.selected) {$selected+=,$i}
    }
    Require ((Same-Text ($selected|ConvertTo-Json -Compress) (@($native.Selected)|ConvertTo-Json -Compress))) 'Retained visible selection differs from actual native LB_GETSELITEMS'
    Require ((Same-Text (@($Status.native_selected_indices)|ConvertTo-Json -Compress) (@($native.Selected)|ConvertTo-Json -Compress))) 'Native status selection differs from independent LISTBOX observation'
    $script:evidence.observations+=@{case=$group;kind='native-listbox';status=$Status;native=$native}
    return $native
}
function Row($Status,[string]$Path) {
    $rows=@(All-Rows $Status|Where-Object {Same-Text $_.path $Path})
    Require ($rows.Count -eq 1) ('Expected exactly one rendered retained row for '+$Path)
    return $rows[0]
}
function Row-Action([string]$Action,$Status,[string]$Path,[string[]]$Options=@(),[int]$Exit=0) {
    $row=Row $Status $Path
    return Request (@('files',$Action,'--pane',$Status.pane,'--token',$Status.token,'--index',$row.index.ToString())+$Options) $Exit
}
function Select-Row($Status,[string]$Path,[string]$Mode='replace') {
    Row-Action 'select' $Status $Path @('--mode',$Mode)|Out-Null
    return Settled $Status.pane
}
function Expand-Row($Status,[string]$Path) {
    Row-Action 'expand' $Status $Path|Out-Null
    $r=Settled $Status.pane;Require (-not $r.last_error -and -not $r.stale) 'Directory expansion failed';return $r
}
function Require-Paths($Actual,[string[]]$Expected,[string]$Message) {
    $values=@($Actual);Require ($values.Count -eq $Expected.Count) $Message
    for($i=0;$i -lt $values.Count;$i++) {Require (Same-Text $values[$i] $Expected[$i]) $Message}
}
function Open-Row($Status,[string]$Path) {
    $opened=(Row-Action 'open' $Status $Path).editor_opened
    Require ($opened.surface -and $opened.pane -eq $Status.pane) 'Files Open omitted the pinned editor identity'
    if($script:ownedEditors -notcontains $opened.surface) {$script:ownedEditors+=$opened.surface}
    Ready $opened.surface|Out-Null
    return $opened
}
function Write-InRoot([string]$Relative,[string]$Text,[bool]$Bom=$false,[bool]$CrLf=$false) {
    return $fixture.Write(($fixture.Relative($caseRoot)+'/'+$Relative),$Text,$Bom,$CrLf)
}
function Hide-Files([string]$Pane) {
    Request @('files','hide','--pane',$Pane)|Out-Null
    $r=Files-Status $Pane
    Require (-not $r.visible -and -not $r.loading) 'Files Hide left the panel visible or loading'
    return $r
}

function Action-Path([string]$Relative) {return $fixture.File(($fixture.Relative($caseRoot)+'/'+$Relative))}
function Assert-Exists([string]$Relative,[bool]$Expected) {
    Require ([FilesActionsFixture]::Exists($fixture,(Action-Path $Relative)) -eq $Expected) ('Owned filesystem existence differs: '+$Relative)
}
function Refresh-Files {
    Request @('files','refresh','--pane',$source.pane)|Out-Null
    $s=Settled $source.pane
    Require ($s.token -and -not $s.loading -and -not $s.stale -and -not $s.last_error) 'Files action refresh did not settle successfully'
    return $s
}
function Reveal-Path($Status,[string]$Path) {
    $parts=$Path.Split('/');$parent='';$s=$Status
    for($i=0;$i -lt $parts.Length-1;$i++) {
        $parent=if($parent) {$parent+'/'+$parts[$i]} else {$parts[$i]}
        $row=Row $s $parent
        Require ($row.directory -and -not $row.reparse) 'Reveal parent is not an ordinary retained directory'
        if(-not $row.expanded) {$s=Expand-Row $s $parent}
    }
    Row $s $Path|Out-Null
    return $s
}
function Operation-Record($Response,[string]$ExpectedId='') {
    $op=$Response.operation;$id=[guid]::Empty
    Require ($op -and [guid]::TryParse([string]$op.id,[ref]$id) -and $id -ne [guid]::Empty) 'Files action omitted a valid retained operation id'
    if($ExpectedId) {Require ($op.id -eq $ExpectedId) 'Files operation status changed operation identity'}
    Require (@('accepted','preparing','succeeded','failed','cancelled') -contains $op.status) ('Unknown Files operation status: '+$op.status)
    return $op
}
function Await-Operation($Accepted) {
    $op=Operation-Record $Accepted;$id=[string]$op.id
    if($script:activeOperations -notcontains $id) {$script:activeOperations+=,$id}
    $clock=[Diagnostics.Stopwatch]::StartNew();$polls=0
    do {
        if(@('succeeded','failed','cancelled') -contains $op.status) {
            $script:activeOperations=@($script:activeOperations|Where-Object {$_ -ne $id})
            $script:evidence.observations+=@{case=$group;kind='file-operation';accepted=$Accepted.operation;operation=$op;polls=$polls;elapsedMs=$clock.ElapsedMilliseconds}
            return $op
        }
        $remaining=10000-$clock.ElapsedMilliseconds;Require ($remaining -gt 0) ('Files operation exceeded ten-second condition budget: '+$id)
        $response=Request @('files','operation-status','--id',$id) 0 ([int][Math]::Min(5000,$remaining));$polls++
        $op=Operation-Record $response $id
        if(@('accepted','preparing') -contains $op.status) {
            Start-Sleep -Milliseconds ([int][Math]::Min(20,[Math]::Max(1,10000-$clock.ElapsedMilliseconds)))
        }
    } while($true)
}
function Mutation([string]$Action,$Status,[string]$SourcePath,[string]$Destination) {
    $row=Row $Status $SourcePath;$flag=if($Action -eq 'rename') {'--name'} else {'--destination'}
    $arguments=@('files',$Action,'--pane',$Status.pane,'--token',$Status.token,'--index',$row.index.ToString(),$flag,$Destination)
    $accepted=Request $arguments
    $op=Operation-Record $accepted
    Require ($op.status -eq 'accepted' -and $op.kind -eq $Action -and (Same-Text $op.source $SourcePath)) 'Mutation acceptance did not preserve action/source identity'
    $result=Await-Operation $accepted
    Require ($result.status -eq 'succeeded') ('File action failed without retry: '+($result|ConvertTo-Json -Depth 10 -Compress))
    return $result
}
function Rejected-Mutation([string]$Action,$Status,[string]$SourcePath,[string]$Destination,[string]$Pattern) {
    $row=Row $Status $SourcePath;$flag=if($Action -eq 'rename') {'--name'} else {'--destination'}
    $arguments=@('files',$Action,'--pane',$Status.pane,'--token',$Status.token,'--index',$row.index.ToString(),$flag,$Destination)
    $response=End-Command (Begin-Command $arguments) @(0,1)
    if($response.error) {
        Expect-Error $response $Pattern
        $script:evidence.observations+=@{case=$group;kind='rejected-file-operation';action=$Action;source=$SourcePath;destination=$Destination;response=$response}
        return $response
    }
    $result=Await-Operation $response
    Require ($result.status -eq 'failed' -and $result.error -and ([string]$result.error -match $Pattern)) ('Expected rejected file action, got: '+($result|ConvertTo-Json -Depth 10 -Compress))
    return $result
}
function Assert-Raw([string]$Relative,[byte[]]$Bytes) {
    Require ($fixture.BytesEqual((Action-Path $Relative),$Bytes)) ('Exact source/destination bytes differ: '+$Relative)
    $script:evidence.observations+=@{case=$group;kind='exact-file-bytes';path=$Relative;bytes=$Bytes.Length;sha256=[FilesActionsFixture]::Sha256($Bytes)}
}

try {
    $doctor=End-Command (Begin-Probe $cli @('doctor'))
    Require ($doctor.background_testing -and $doctor.status -eq 'ok') 'Working hidden debug build required; no host launched'
    $tree=Start-Owned;$script:source=Request @('identify');$script:terminal=$tree.surfaces[0]
    Assert-OwnedWindows
    if($Case -eq 'startup') {Passed 'hidden_owned_files_actions_host_startup'}
    foreach($group in @('roundtrip','copy-bytes','rejections','editor-guard','stale-token')) {
        if($Case -ne 'all' -and $Case -ne $group) {continue}
        Clean-Editors;Request @('focus-tab',$terminal.id)|Out-Null
        $script:caseRoot=$fixture.Directory($group)
        switch($group) {
            'roundtrip' {
                $script:caseRoot=$fixture.LongRoot('roundtrip/long-root')
                Require ($caseRoot.Length -gt 300) 'Action roundtrip root must exceed300 UTF16 units'
                $src='원본 한글/안쪽 한 😀/문서 é.txt';$copy='원본 한글/안쪽 한 😀/복사 😀.txt'
                $renamed='원본 한글/안쪽 한 😀/이름 변경 한.txt';$dest='이동 대상 한/최종 é 😀.txt'
                $text="원본 한글 한 😀`nsecond line é`n";$bytes=[EditorFixture]::Encode($text,$true,$true)
                Write-InRoot $src $text $true $true|Out-Null;$fixture.Directory(($fixture.Relative($caseRoot)+'/이동 대상 한'))|Out-Null
                $s=Reveal-Path (Show-Files $source.pane $caseRoot) $src;Native-Snapshot $s|Out-Null
                Mutation 'copy' $s $src $copy|Out-Null;Assert-Raw $src $bytes;Assert-Raw $copy $bytes
                $s=Reveal-Path (Refresh-Files) $copy;Native-Snapshot $s|Out-Null
                Mutation 'rename' $s $copy '이름 변경 한.txt'|Out-Null;Assert-Exists $copy $false;Assert-Raw $renamed $bytes
                $s=Reveal-Path (Refresh-Files) $renamed;Native-Snapshot $s|Out-Null
                Mutation 'move' $s $renamed $dest|Out-Null;Assert-Exists $renamed $false;Assert-Raw $src $bytes;Assert-Raw $dest $bytes
                $s=Reveal-Path (Refresh-Files) $dest;Native-Snapshot $s|Out-Null
                $opened=Open-Row $s $dest;$read=Assert-Text $opened.surface $text $false
                Require (Same-Text ($read.path.Replace('\','/')) $dest) 'Moved Unicode file opened a different Monaco document'
                Assert-Raw $dest $bytes
                $evidence.observations+=@{case=$group;kind='roundtrip-monaco';rootUtf16=$caseRoot.Length;destination=$dest;opened=$opened;monaco=$read}
                Passed 'long_nested_unicode_copy_rename_move_preserve_bom_crlf_bytes_and_reach_actual_listbox_and_monaco'
            }
            'copy-bytes' {
                $script:caseRoot=$fixture.LongRoot('copy-bytes/long-root')
                $text="한글 한 é 😀`nline two`n";$textBytes=[EditorFixture]::Encode($text,$true,$true)
                $binary=[byte[]]@(0,255,0,13,10,128,1,2,0,239,187,191,240,159,152,128,0)
                $src='중첩 한글/텍스트 한.txt';$bin='중첩 한글/바이너리 😀.bin'
                Write-InRoot $src $text $true $true|Out-Null;$fixture.WriteBytes(($fixture.Relative($caseRoot)+'/'+$bin),$binary)|Out-Null
                $fixture.Directory(($fixture.Relative($caseRoot)+'/복사 대상 é'))|Out-Null
                $s=Reveal-Path (Show-Files $source.pane $caseRoot) $src
                Mutation 'copy' $s $src '복사 대상 é/텍스트 😀.txt'|Out-Null
                $s=Reveal-Path (Refresh-Files) $bin
                Mutation 'copy' $s $bin '복사 대상 é/바이너리 한.bin'|Out-Null
                Assert-Raw $src $textBytes;Assert-Raw '복사 대상 é/텍스트 😀.txt' $textBytes;Assert-Raw $bin $binary;Assert-Raw '복사 대상 é/바이너리 한.bin' $binary
                $s=Reveal-Path (Refresh-Files) '복사 대상 é/바이너리 한.bin';Native-Snapshot $s|Out-Null
                Require (@((Tree).editors).Count -eq 0) 'Raw copy unnecessarily opened a text editor'
                Passed 'long_nested_unicode_copy_preserves_exact_utf8_bom_crlf_and_binary_nul_bytes_without_editor'
            }
            'rejections' {
                $src='원본 한글.txt';$collision='존재 한.txt';$text="original 😀`n";$other="collision sentinel`n"
                Write-InRoot $src $text|Out-Null;Write-InRoot $collision $other|Out-Null
                $outside=$fixture.Write('outside-actions/sentinel.txt',"outside sentinel`n",$false,$false)
                $fixture.Junction(($fixture.Relative($caseRoot)+'/junction 밖'),'outside-actions')|Out-Null
                $s=Show-Files $source.pane $caseRoot;Native-Snapshot $s|Out-Null
                foreach($action in @('copy','move','rename')) {$s=Refresh-Files;Rejected-Mutation $action $s $src $collision 'exist|collision|overwrite|destination|no-replacement'|Out-Null}
                foreach($dest in @('없는 부모/target.txt','../escape.txt','/absolute.txt','NUL','stream.txt:ads','trailing.','junction 밖/new.txt')) {
                    $s=Refresh-Files
                    Rejected-Mutation 'copy' $s $src $dest 'parent|directory|exist|path|relative|reserved|device|alternate|stream|invalid|reparse|unsupported|denied|ordinary|trailing|outside|root|contain|escape|identity'|Out-Null
                }
                $s=Refresh-Files;Rejected-Mutation 'rename' $s $src 'child/leaf.txt' 'leaf|name|separator|relative|path|component'|Out-Null
                $s=Refresh-Files;Rejected-Mutation 'move' $s 'junction 밖' 'renamed-junction' 'reparse|directory|regular|unsupported|file'|Out-Null
                Assert-Raw $src ([EditorFixture]::Encode($text,$false,$false));Assert-Raw $collision ([EditorFixture]::Encode($other,$false,$false))
                Require ($fixture.BytesEqual($outside,[EditorFixture]::Encode("outside sentinel`n",$false,$false))) 'Rejected operation changed outside-root sentinel'
                Require (-not [FilesActionsFixture]::Exists($fixture,$fixture.File('outside-actions/new.txt'))) 'Rejected destination traversed the owned junction'
                Assert-Exists '없는 부모' $false;Assert-Exists 'renamed-junction' $false
                $after=Refresh-Files;Require ($after.snapshot_rows -eq 3) 'Rejected operations added or removed root entries';Native-Snapshot $after|Out-Null
                Passed 'collisions_missing_parent_invalid_names_and_reparse_paths_reject_without_source_destination_or_outside_changes'
            }
            'editor-guard' {
                $src='열린 한글 한.txt';$text="disk clean 😀`n";$edited="unsaved 한글 😀`n"
                Write-InRoot $src $text|Out-Null;$s=Show-Files $source.pane $caseRoot;$opened=Open-Row $s $src
                $before=Assert-Text $opened.surface $text $false
                foreach($dirty in @($false,$true)) {
                    if($dirty) {Editor-Command $opened.surface 'replace-text' @('--text',$edited)|Out-Null}
                    $expected=if($dirty) {$edited} else {$text};$current=Assert-Text $opened.surface $expected $dirty
                    foreach($action in @('copy','rename','move')) {
                        $s=Refresh-Files;$dest=$action+'-blocked.txt';Rejected-Mutation $action $s $src $dest 'open|editor|document|buffer|dirty|close'|Out-Null
                        Assert-Exists $dest $false
                    }
                    $after=Assert-Text $opened.surface $expected $dirty
                    Require ($after.document_id -eq $before.document_id -and $after.active_version -eq $current.active_version) 'Rejected action changed actual Monaco identity/version'
                    Assert-Raw $src ([EditorFixture]::Encode($text,$false,$false))
                    $evidence.observations+=@{case=$group;kind='editor-source-guard';dirty=$dirty;before=$current;after=$after}
                }
                Native-Snapshot (Files-Status $source.pane)|Out-Null
                Passed 'copy_rename_and_move_reject_clean_or_dirty_open_editor_source_preserving_disk_and_actual_monaco'
            }
            'stale-token' {
                $src='retained 한글.txt';$text="retained 😀`n";Write-InRoot $src $text|Out-Null
                $old=Show-Files $source.pane $caseRoot;Write-InRoot 'new first.txt' "new row`n"|Out-Null;$current=Refresh-Files
                Require ($current.token -ne $old.token) 'Stale fixture did not replace its retained token'
                foreach($action in @('copy','rename','move')) {
                    $dest=$action+'-stale.txt';Rejected-Mutation $action $old $src $dest 'stale|token|expired|generation'|Out-Null;Assert-Exists $dest $false
                }
                $bad=Request @('files','copy','--pane',$source.pane,'--token',$current.token,'--index','999999','--destination','invalid-index.txt') 1
                Expect-Error $bad 'index|row|range|rendered';Assert-Exists 'invalid-index.txt' $false
                Assert-Raw $src ([EditorFixture]::Encode($text,$false,$false));Native-Snapshot (Files-Status $source.pane)|Out-Null
                Passed 'stale_tokens_and_out_of_range_indices_reject_all_file_actions_without_mutation'
            }
        }
        Hide-Files $source.pane|Out-Null;Assert-Terminal;Assert-OwnedWindows
    }
    Clean-Editors;Finish-Host
    $evidence.status='passed_background_files_actions_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
    $script:cleaning=$true;$fixture.ReleaseLocks()
    if($process -and -not $process.HasExited -and $pipeName) {
        foreach($id in @($activeOperations)) {
            try {
                $cancel=Request @('files','operation-cancel','--id',$id)
                $result=Await-Operation $cancel
                $evidence.observations+=@{case=$group;kind='cleanup-operation';operation=$result}
            } catch {$cleanupErrors+=('Owned operation cleanup: '+$_.Exception.Message)}
        }
    }
    foreach($job in $clients) {
        if(-not $job.completed) {try {if(-not $job.process.HasExited) {$job.process.Kill();[CliProbe]::WaitAfterKill($job.process)};$job.process.Dispose();$job.completed=$true} catch {$cleanupErrors+=$_.Exception.Message}}
    }
    if($process -and -not $process.HasExited -and $pipeName) {try {Clean-Editors;Finish-Host} catch {$cleanupErrors+=$_.Exception.Message}}
    if($process) {
        try {if(-not $process.HasExited) {$script:hostForced=$true;$process.Kill();[CliProbe]::WaitAfterKill($process)}} catch {$cleanupErrors+=$_.Exception.Message} finally {
            try {Record-HostExit} catch {$cleanupErrors+=('Host exit capture: '+$_.Exception.Message)}
            $process.Dispose()
        }
    }
    $fixture.Dispose()
    if($cleanupErrors.Count) {$evidence.status='failed';$evidence.cleanupErrors=$cleanupErrors}
    $evidence.hosts=$hosts;$evidence.shells=$shells;$evidence.clientPids=@($clients|ForEach-Object {$_.pid});$evidence.elapsedMs=$suiteClock.ElapsedMilliseconds;$evidence.finished=(Get-Date).ToString('o')
    $evidence|ConvertTo-Json -Depth 22|Set-Content -Encoding UTF8 (Join-Path $directory 'native-files-actions-background.json')
    Write-Output ('Evidence: '+$directory)
}
if($cleanupErrors.Count) {throw ($cleanupErrors -join '; ')}
# Full observations stay in the evidence file; bounded console output must not
# consume run-check's output cap or glue its runner marker onto a truncated line.
[ordered]@{status=$evidence.status;case=$Case;checks=$evidence.checks.Count;elapsedMs=$evidence.elapsedMs}|ConvertTo-Json -Compress
