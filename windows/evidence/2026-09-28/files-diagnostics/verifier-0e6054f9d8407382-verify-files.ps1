# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden owned Files panel/native LISTBOX + real Monaco verification.
# Run under run-check.ps1 with a120s Job limit; ordinary CLI calls cap at5s.
param(
    [string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",
    [ValidateSet('all','startup','nested','late-show','paging','selection','refresh','confinement','owner')][string]$Case='all'
)
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path
$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'EditorFixture.cs'),(Join-Path $PSScriptRoot 'FilesFixture.cs')
$directory=Join-Path $PSScriptRoot ('..\dist\evidence\files-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$fixture=New-Object FilesFixture($directory)
$process=$null;$pipeName=$null;$stdout=$null;$stderr=$null;$hostExitRecorded=$false;$hostForced=$false
$hosts=@();$shells=@();$clients=@();$ownedEditors=@();$cleanupErrors=@();$storageObserved=@()
$caseRoot=$fixture.Root;$suiteClock=[Diagnostics.Stopwatch]::StartNew();$cleaning=$false;$group='startup'
$evidence=[ordered]@{started=(Get-Date).ToString('o');mode='background';case=$Case;checks=@();observations=@();clipboardAccess=$false;desktopInput=$false;externalSites=$false;realImeTest=$false;unicodeComparison='ordinal';deferred=@(
    'No physical keyboard/mouse/IME, glyph rendering, clipboard, native dialogs, DPI or accessibility checks.',
    'Commands exercise retained native model state; LISTBOX observations are read-only bounded system messages, not simulated mouse/keyboard input.',
    'Paging verifies1105 entries, not every entry/byte/depth/cache/service cap. Blocked directory I/O, in-flight cancellation and queued-close races are not forced. late-show alone deliberately waits for the real15s IPC server expiry while only the owned UI thread is paused.',
    'Reparse confinement uses an owned junction to an owned sibling directory. Hostile concurrent reparse swaps, ACL denial, malformed UTF16 names and network roots are not exercised.',
    'Files is read-only. Explicit refresh and in-memory selection are checked; filesystem mutations, automatic tree watching and persisted tree restoration are outside this verifier.'
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
    $deadline=(Get-Date).AddSeconds(5)
    do {$r=Status $Surface;if($r.ready) {return $r};if((Get-Date) -gt $deadline) {throw ('Editor not ready: '+($r|ConvertTo-Json -Compress -Depth 6))};Start-Sleep -Milliseconds 20} while($true)
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

try {
    $doctor=End-Command (Begin-Probe $cli @('doctor'))
    Require ($doctor.background_testing -and $doctor.status -eq 'ok') 'Working hidden debug build required; no host launched'
    $tree=Start-Owned;$script:source=Request @('identify');$script:terminal=$tree.surfaces[0]
    Assert-OwnedWindows
    if($Case -eq 'startup') {Passed 'hidden_owned_files_host_startup'}
    foreach($group in @('nested','late-show','paging','selection','refresh','confinement','owner')) {
        if($Case -ne 'all' -and $Case -ne $group) {continue}
        Clean-Editors;Request @('focus-tab',$terminal.id)|Out-Null
        $script:caseRoot=$fixture.Directory($group)
        switch($group) {
            'nested' {
                $script:caseRoot=$fixture.LongRoot('nested/long-root')
                Require ($caseRoot.Length -gt 300) 'Long root must exceed300 UTF16 units before any nested path'
                $first='중첩 한글';$second=$first+'/안쪽 한 😀';$relative=$second+'/문서 é 😀.txt'
                $text="한글 한 é 😀`nsecond line`n";$path=Write-InRoot $relative $text $true $true
                $secondRelative=$second+'/두번째 선택 한글 한 😀.txt';$secondText="second retained file 😀 한`n"
                $secondPath=Write-InRoot $secondRelative $secondText
                $s=Show-Files $source.pane $caseRoot
                Require ($s.rendered_rows -eq 1 -and (Row $s $first).directory) 'Long-root first snapshot did not contain its Unicode directory'
                Native-Snapshot $s|Out-Null
                $s=Expand-Row $s $first;Native-Snapshot $s|Out-Null
                $s=Expand-Row $s $second
                $target=Row $s $relative;Require (-not $target.directory -and -not $target.reparse -and $target.depth -eq 2 -and $target.path.Contains('/')) 'Nested retained file path did not use slash-separated identities'
                $s=Select-Row $s $relative;Native-Snapshot $s|Out-Null
                Require-Paths $s.selected_paths @($relative) 'Nested Unicode file was not logically selected'
                $opened=Open-Row $s $relative;$read=Assert-Text $opened.surface $text $false
                Require (Same-Text ($read.path.Replace('\','/')) $relative) 'Retained Files Open selected the wrong actual nested Unicode Monaco document'
                Assert-Bytes $path $text $true $true
                $after=Settled $source.pane;Require ($after.source -eq $terminal.id -and (Same-Text $after.root $caseRoot) -and $after.token -eq $s.token) 'Opening an editor silently rebound the Files source/root or replaced its retained snapshot'
                Require (@((Status $opened.surface).documents).Count -eq 1) 'First retained Open created unexpected editor documents'
                # Use the exact earlier retained snapshot/token, without refresh or
                # another Show, to exercise the existing editor's reuse barrier.
                $reused=Open-Row $s $secondRelative
                Require ($reused.surface -eq $opened.surface -and $reused.pane -eq $opened.pane -and $reused.placement_strategy -eq 'reuse_tab') 'Second retained Files Open did not reuse the original Monaco editor surface'
                $secondRead=Assert-Text $reused.surface $secondText $false
                Require ((Same-Text ($secondRead.path.Replace('\','/')) $secondRelative) -and $secondRead.document_id -ne $read.document_id -and @((Status $reused.surface).documents).Count -eq 2) 'Reused Files Open did not activate exactly the second actual Unicode Monaco document'
                Assert-Bytes $secondPath $secondText;Assert-Bytes $path $text $true $true
                $afterReuse=Settled $source.pane;Require ($afterReuse.token -eq $s.token -and $afterReuse.source -eq $terminal.id) 'Reused Open changed retained Files ownership/token'
                Native-Snapshot $afterReuse|Out-Null
                $evidence.observations+=@{case=$group;rootUtf16=$caseRoot.Length;fullPathUtf16=$path.Length;retainedPath=$target.path;retainedToken=$s.token;opened=$opened;monaco=$read;reused=$reused;secondRetainedPath=$secondRelative;secondMonaco=$secondRead}
                Passed 'nested_unicode_long_root_listbox_and_retained_slash_path_opens_reach_new_and_reused_monaco_documents'
            }
            'late-show' {
                $script:caseRoot=$fixture.Directory('late-show/original-root')
                Write-InRoot 'chosen 한글 한 😀.txt' "original root selection`n"|Out-Null
                Write-InRoot 'untouched.txt' "original sibling`n"|Out-Null
                $replacementRoot=$fixture.Directory('late-show/replacement-root')
                $fixture.Write('late-show/replacement-root/new é 😀.txt',"replacement root`n",$false,$false)|Out-Null
                $before=Show-Files $source.pane $caseRoot;$before=Select-Row $before 'chosen 한글 한 😀.txt'
                $nativeBefore=Native-Snapshot $before;$treeBefore=Tree;$identity=Request @('identify')
                Require ($identity.pid -eq $process.Id -and -not $before.loading -and -not $before.stale -and $before.visible) 'Late Show requires the exact idle owned host and a current selected Files snapshot'
                $pause=$null;$deadlineClock=[Diagnostics.Stopwatch]::StartNew()
                $observation=[ordered]@{case=$group;kind='late-show-actual-server-expiry';pid=$process.Id;pane=$source.pane;windowHandle=$treeBefore.window_handle;clientBudgetMs=20000;intentionalServerDeadlineWait=$true;inferredReceiptTimestamp=$false;wholeProcessSuspended=$false;uiThreadSuspended=$false;uiThreadResumed=$false;rootBefore=$before.root;tokenBefore=$before.token;selectedBefore=$before.selected_paths;replacementRoot=$replacementRoot}
                $evidence.observations+=$observation
                try {
                    # Existing EditorFixture.cs helper validates exact owned
                    # Process/HWND/PID/class, hidden state and suspend count zero.
                    # IPC workers remain live. No clock-based receipt inference.
                    $pause=New-Object EditorUiThreadPause($process,([long]$treeBefore.window_handle))
                    $observation.threadId=$pause.ThreadId;$observation.verifiedThreadPid=$pause.ProcessId;$observation.previousSuspendCount=$pause.PreviousSuspendCount;$observation.uiThreadSuspended=$true
                    $response=End-Command (Begin-Command @('files','show','--pane',$source.pane,'--root',$replacementRoot)) @(1) 20000
                    $observation.response=$response;$observation.serverResponseElapsedMs=$deadlineClock.ElapsedMilliseconds
                    Require ($response.error -and $response.error.Contains('window did not answer within the IPC command deadline')) 'Late Show did not reach the actual server command deadline'
                    Require (-not $process.HasExited) 'Owned host exited before its expired Show could be processed'
                } finally {
                    if($pause) {
                        try {$pause.Dispose()} finally {$observation.uiThreadResumed=$pause.Resumed;$observation.previousResumeCount=$pause.PreviousResumeCount;$observation.suspendElapsedMs=$deadlineClock.ElapsedMilliseconds}
                    }
                }
                Require ($observation.uiThreadResumed -and $observation.previousResumeCount -eq 1) 'Late Show suspension was not resumed exactly once'
                # The server already dispatched Show before its observed expiry;
                # this new status is queued after it and observes the rejected UI operation.
                $after=Settled $source.pane
                Require ((Same-Text $after.root $before.root) -and (Same-Text $after.captured_root $before.captured_root) -and $after.token -eq $before.token -and $after.source -eq $before.source -and $after.visible -and -not $after.stale -and -not $after.loading -and -not $after.last_error) 'Expired queued Show changed the prior root, token, ownership or settled state'
                Require ((Same-Text ($after.current_owner|ConvertTo-Json -Depth 5 -Compress) ($before.current_owner|ConvertTo-Json -Depth 5 -Compress))) 'Expired queued Show advanced the Files generation'
                Require-Paths $after.selected_paths @('chosen 한글 한 😀.txt') 'Expired queued Show changed retained selection'
                Require-Paths $after.visible_selected_paths @('chosen 한글 한 😀.txt') 'Expired queued Show changed visible selection'
                Require ($after.panel_handle -eq $before.panel_handle -and $after.list_handle -eq $before.list_handle) 'Expired queued Show replaced native Files controls'
                $nativeAfter=Native-Snapshot $after
                Require ((Same-Text ($nativeAfter.Text|ConvertTo-Json -Compress) ($nativeBefore.Text|ConvertTo-Json -Compress))) 'Expired queued Show changed actual LISTBOX text'
                $treeAfter=Tree
                Require (@($treeAfter.editors).Count -eq @($treeBefore.editors).Count -and (Same-Text ($treeAfter.workspaces|ConvertTo-Json -Depth 60 -Compress) ($treeBefore.workspaces|ConvertTo-Json -Depth 60 -Compress))) 'Expired queued Show changed the editor count or workspace layout'
                $observation.rootAfter=$after.root;$observation.tokenAfter=$after.token;$observation.selectedAfter=$after.selected_paths;$observation.actualNativeCountAfter=$nativeAfter.Count
                $fresh=Show-Files $source.pane $replacementRoot;Native-Snapshot $fresh|Out-Null
                Require ($fresh.token -ne $before.token -and $fresh.selected_count -eq 0 -and (Row $fresh 'new é 😀.txt').name -eq 'new é 😀.txt') 'A fresh following Show did not load the new root with clean selection'
                $observation.followingShowSucceeded=$true;$observation.followingRoot=$fresh.root
                Passed 'expired_ui_queued_files_show_preserves_root_token_selection_and_controls_then_fresh_show_succeeds'
            }
            'paging' {
                $fixture.Populate($fixture.Relative($caseRoot),1105)
                $s=Show-Files $source.pane $caseRoot
                Require ($s.known_rows -eq 1105 -and $s.snapshot_rows -eq 1105 -and $s.rendered_rows -eq 500 -and $s.more_available) 'Initial1105-row fixture was not paged to500'
                Native-Snapshot $s|Out-Null
                foreach($expected in @(1000,1105)) {
                    Request @('files','more','--pane',$source.pane,'--token',$s.token)|Out-Null;$s=Settled $source.pane
                    Require ($s.rendered_rows -eq $expected -and $s.known_rows -eq 1105 -and $s.more_available -eq ($expected -lt 1105)) 'More did not reveal exactly the next500 retained rows'
                    $native=Native-Snapshot $s;$rows=@(All-Rows $s)
                    for($i=0;$i -lt $expected;$i++) {Require (Same-Text $rows[$i].path ('entry-'+$i.ToString('D4')+' 한글.txt')) 'Paging changed ordinal path order or lost an entry'}
                }
                $last=Row $s 'entry-1104 한글.txt';Require ($last.index -eq 1104) 'Final page returned a page-local row index'
                $s=Select-Row $s $last.path;Native-Snapshot $s|Out-Null
                $opened=Open-Row $s $last.path;Assert-Text $opened.surface "row 1104`n" $false|Out-Null
                Passed 'files_more_and_status_paging_preserve1105_native_rows_and_absolute_retained_indices'
            }
            'selection' {
                foreach($name in @('A 한글/one.txt','A 한글/two.txt','B 한/three.txt','C 😀.txt')) {Write-InRoot $name ($name+"`n")|Out-Null}
                $s=Show-Files $source.pane $caseRoot;$s=Expand-Row $s 'A 한글';$s=Expand-Row $s 'B 한'
                $s=Select-Row $s 'A 한글/one.txt';$s=Select-Row $s 'A 한글/two.txt' 'toggle'
                Require-Paths $s.selected_paths @('A 한글/one.txt','A 한글/two.txt') 'Toggle did not preserve both path identities';Native-Snapshot $s|Out-Null
                Row-Action 'collapse' $s 'A 한글'|Out-Null;$s=Settled $source.pane
                Require ($s.selected_count -eq 2 -and $s.hidden_selected_count -eq 2) 'Collapse dropped logical descendant selections'
                Require-Paths $s.visible_selected_paths @() 'Collapsed descendants remained visibly selected';Native-Snapshot $s|Out-Null
                $s=Expand-Row $s 'A 한글';Require ($s.hidden_selected_count -eq 0) 'Re-expansion did not restore visible selection';Native-Snapshot $s|Out-Null
                $s=Select-Row $s 'A 한글/one.txt';$s=Select-Row $s 'B 한/three.txt' 'range'
                Require-Paths $s.selected_paths @('A 한글/one.txt','A 한글/two.txt','B 한','B 한/three.txt') 'Range did not follow current visible preorder';Native-Snapshot $s|Out-Null
                Passed 'files_replace_toggle_range_and_collapsed_descendant_selection_match_native_listbox'
            }
            'refresh' {
                Write-InRoot 'b 선택 한.txt' "chosen`n"|Out-Null;Write-InRoot 'c vanished.txt' "vanished`n"|Out-Null
                $s=Show-Files $source.pane $caseRoot;$s=Select-Row $s 'b 선택 한.txt';$s=Select-Row $s 'c vanished.txt' 'toggle'
                $old=$s;$oldIndex=(Row $s 'b 선택 한.txt').index
                Write-InRoot 'a new 😀.txt' "new first`n"|Out-Null;$fixture.DeleteOwned(($fixture.Relative($caseRoot)+'/c vanished.txt'))
                Request @('files','refresh','--pane',$source.pane)|Out-Null;$s=Settled $source.pane
                Require ($s.token -ne $old.token -and -not $s.last_error -and -not $s.stale) 'Refresh did not replace the retained generation'
                Require-Paths $s.selected_paths @('b 선택 한.txt') 'Refresh moved selection by row index or retained a vanished path'
                Require ((Row $s 'b 선택 한.txt').index -ne $oldIndex) 'Refresh fixture did not actually move the selected row index';Native-Snapshot $s|Out-Null
                $before=Tree
                $bad=Request @('files','open','--pane',$source.pane,'--token',$old.token,'--index',$oldIndex.ToString()) 1;Expect-Error $bad 'stale|expired|token'
                $after=Tree;Require (@($after.editors).Count -eq @($before.editors).Count) 'Stale retained row opened an editor'
                $bad=Request @('files','open','--pane',$source.pane,'--token',$s.token,'--index','999999') 1;Expect-Error $bad 'index|row|range|rendered'
                $opened=Open-Row $s 'b 선택 한.txt';Assert-Text $opened.surface "chosen`n" $false|Out-Null
                Passed 'files_refresh_retains_selection_by_unicode_path_and_rejects_stale_or_out_of_range_open'
            }
            'confinement' {
                Write-InRoot 'inside.txt' "inside`n"|Out-Null
                $outside=$fixture.Write('outside-owned/secret 한글.txt',"outside fixture sentinel`n",$false,$false)
                $fixture.Junction(($fixture.Relative($caseRoot)+'/junction 밖'), 'outside-owned')|Out-Null
                $s=Show-Files $source.pane $caseRoot;$leaf=Row $s 'junction 밖'
                Require ($leaf.reparse -and $leaf.directory -and -not $leaf.expanded) 'Owned junction was not classified as an unsupported reparse leaf';Native-Snapshot $s|Out-Null
                $before=Tree
                foreach($action in @('expand','open')) {$bad=Row-Action $action $s 'junction 밖' @() 1;Expect-Error $bad 'reparse|unsupported|regular|directory'}
                $after=Settled $source.pane;Require ($after.snapshot_rows -eq 2 -and @((All-Rows $after)|Where-Object {$_.path -like '*secret*'}).Count -eq 0) 'Reparse target contents leaked into Files snapshot'
                foreach($invalid in @('\\localhost\flowmux-never-contacted','\\?\C:\flowmux-never-contacted',($caseRoot+'\inside.txt:stream'),($caseRoot+'\NUL'))) {
                    $bad=Request @('files','show','--pane',$source.pane,'--root',$invalid) 1;Expect-Error $bad 'UNC|device|alternate|reserved|invalid|unsupported'
                }
                Require (@((Tree).editors).Count -eq @($before.editors).Count) 'Rejected confinement operation opened an editor'
                Require ($fixture.BytesEqual($outside,[EditorFixture]::Encode("outside fixture sentinel`n",$false,$false))) 'Confinement checks altered the outside-root owned sentinel'
                $s=Show-Files $source.pane $caseRoot;$opened=Open-Row $s 'inside.txt';Assert-Text $opened.surface "inside`n" $false|Out-Null
                Passed 'files_reparse_leaves_and_invalid_roots_reject_activation_without_target_listing_or_file_mutation'
            }
            'owner' {
                $relative='owner 한글.txt';Write-InRoot $relative "pinned owner 😀`n"|Out-Null
                $s=Show-Files $source.pane $caseRoot;$s=Select-Row $s $relative;Native-Snapshot $s|Out-Null
                $originalSource=$s.source;$originalPane=$s.pane;$originalWorkspace=$s.owner.workspace
                Request @('split','vertical')|Out-Null;$second=Request @('identify');$tree=Tree
                Require ($second.pane -ne $source.pane) 'Owner fixture did not create a different active pane'
                $readyClock=[Diagnostics.Stopwatch]::StartNew()
                do {
                    $left=5000-$readyClock.ElapsedMilliseconds;Require ($left -gt 0) 'Second owned terminal exceeded five-second readiness budget'
                    $tree=Tree ([int]$left);$secondCandidates=@($tree.surfaces|Where-Object {$_.id -eq $second.surface})
                    if($secondCandidates.Count -eq 1 -and $secondCandidates[0].ready -and $secondCandidates[0].running) {break}
                    Start-Sleep -Milliseconds ([int][Math]::Min(20,[Math]::Max(1,5000-$readyClock.ElapsedMilliseconds)))
                } while($true)
                $secondTerminal=$secondCandidates[0]
                $script:shells+=@($tree.surfaces|Where-Object {$_.pid -and $script:shells -notcontains $_.pid}|ForEach-Object {$_.pid})
                $pinned=Settled $source.pane
                Require ($pinned.source -eq $originalSource -and $pinned.pane -eq $originalPane -and $pinned.owner.workspace -eq $originalWorkspace -and (Same-Text $pinned.root $caseRoot)) 'Switching active pane rebound Files ownership'
                $opened=Open-Row $pinned $relative;Require ($opened.pane -eq $originalPane) 'Files Open targeted the currently active pane instead of its original source pane'
                Assert-Text $opened.surface "pinned owner 😀`n" $false|Out-Null
                $live=@((Tree).surfaces|Where-Object {$_.id -eq $secondTerminal.id});Require ($live.Count -eq 1 -and $live[0].pid -eq $secondTerminal.pid -and $live[0].running) 'Pinned Open changed the unrelated terminal identity'
                $hidden=Hide-Files $source.pane
                $bad=Request @('files','open','--pane',$source.pane,'--token',$pinned.token,'--index',(Row $pinned $relative).index.ToString()) 1;Expect-Error $bad 'hidden|stale|token|show|visible|closed'
                $shown=Show-Files $source.pane $caseRoot;Native-Snapshot $shown|Out-Null
                Require ($shown.owner.workspace -eq $originalWorkspace -and $shown.pane -eq $originalPane) 'Re-show changed Files workspace/pane identity'
                Hide-Files $source.pane|Out-Null;Clean-Editors;Request @('close-pane',$second.pane)|Out-Null;Request @('focus-tab',$terminal.id)|Out-Null
                $evidence.observations+=@{case=$group;originalSource=$originalSource;originalPane=$originalPane;otherPane=$second.pane;opened=$opened;hidden=$hidden;reshownSource=$shown.source}
                Passed 'files_show_hide_and_retained_open_preserve_original_pane_and_unrelated_terminal'
            }
        }
        Hide-Files $source.pane|Out-Null;Assert-Terminal;Assert-OwnedWindows
    }
    Clean-Editors;Finish-Host
    $evidence.status='passed_background_files_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
    $script:cleaning=$true;$fixture.ReleaseLocks()
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
    $evidence|ConvertTo-Json -Depth 22|Set-Content -Encoding UTF8 (Join-Path $directory 'native-files-background.json')
    Write-Output ('Evidence: '+$directory)
}
if($cleanupErrors.Count) {throw ($cleanupErrors -join '; ')}
# Full observations stay in the evidence file; bounded console output must not
# consume run-check's output cap or glue its runner marker onto a truncated line.
[ordered]@{status=$evidence.status;case=$Case;checks=$evidence.checks.Count;elapsedMs=$evidence.elapsedMs}|ConvertTo-Json -Compress
