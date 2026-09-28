# SPDX-License-Identifier: GPL-3.0-or-later
# Bounded hidden native Monaco verification. Run through run-check.ps1 (120s).
param(
    [string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",
    [ValidateSet('all','startup','open','async-open','picker-blocked','late-open','close-preparing','edit','encoding','encoding-defaults','conflict','save-as','save-all-many','close','move','restore','restore-errors','missing-root','checkpoint-failure','late-quit','late-quit-empty','recovery','auto-refresh-clean','auto-refresh-inactive','auto-refresh-conflict','auto-refresh-delete-recreate','auto-refresh-stamp','auto-refresh-partial-error','auto-refresh-move-close','auto-refresh-coalescing')][string]$Case='all'
)
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path
$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'EditorFixture.cs'),(Join-Path $PSScriptRoot 'EditorOpenLifetime.cs')
$directory=Join-Path $PSScriptRoot ('..\dist\evidence\editor-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$fixture=New-Object EditorFixture($directory)
$process=$null;$pipeName=$null;$stdout=$null;$stderr=$null;$hostExitRecorded=$false;$hostForced=$false
$hosts=@();$shells=@();$clients=@();$ownedEditors=@();$cleanupErrors=@();$storageObserved=@()
$evidence=[ordered]@{started=(Get-Date).ToString('o');mode='background';case=$Case;checks=@();observations=@();clipboardAccess=$false;desktopInput=$false;externalSites=$false;realImeTest=$false;unicodeComparison='ordinal';deferred=@(
    'Physical keyboard/mouse/IME, glyph fidelity, native dialogs, clipboard, DPI and accessibility are not exercised.',
    'The normal replace-text command synchronizes before replying; closing immediately afterward does not independently force the unsynchronized 150ms edit-debounce race.',
    'Failed checkpoint replacement checks close-error unsealing. No existing checkpoint hold/release hook establishes a close arriving while an earlier checkpoint remains in flight.',
    'Recovery covers acknowledged edits with an observed recovery file and completed checkpoint before forced owned-host termination; unsynchronized edits, power loss and recovery during an interrupted write are not established.',
    'Concurrent Open uses overlapping real CLI processes without delays or hooks; it does not force a particular preparation-completion or cancellation race.',
    'Blocked picker checks its explicit background rejection and owned-host visible top-level HWND snapshots before and after; it does not exercise a native dialog or continuously observe transient windows.',
    'Automatic refresh burst observations do not assume one watcher event per write; partial-error application does not make unknown disk status healthy, and unlock alone may not replay a consumed status transition.',
    'Automatic move/close waits for idle before mutations; post-close absence is bounded observation without a forced queued-callback race.',
    'Concurrent-writer races, network/UNC/reparse-point paths and exhaustive ACL semantics are not established.'
)}

function Same-Text([string]$Left,[string]$Right) {return [string]::Equals($Left,$Right,[StringComparison]::Ordinal)}
function Passed([string]$Name) {$script:evidence.checks+=@{name=$Name;passed=$true};Write-Host ('[check] passed '+$Name)}
function Begin-Probe([string]$File,[string[]]$Arguments) {
    $p=[CliProbe]::Start($File,$Arguments,$directory,$directory)
    $job=[pscustomobject]@{process=$p;pid=$p.Id;file=$File;arguments=$Arguments;output=$p.StandardOutput.ReadToEndAsync();error=$p.StandardError.ReadToEndAsync();completed=$false}
    $script:clients+=$job;return $job
}
function End-Command($Job,[int[]]$AllowedExits=@(0),[ValidateRange(1,20000)][int]$TimeoutMilliseconds=5000) {
    try {
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
function Raw-Tree {
    $peer=New-Object EditorOwnedPipe($process,$pipeName,100)
    try {
        $tree=$peer.Request('{"method":"tree"}',300)|ConvertFrom-Json
        if($tree.error -or -not $tree.background_testing) {throw 'Raw tree did not return the owned hidden host model'}
        Check-Hidden ([long]$tree.window_handle)
        return $tree
    } finally {$peer.Dispose()}
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
        $startup.identityBudgetMs=Remaining-StartupBudget $startupWatch
        $identity=Request @('identify') 0 $startup.identityBudgetMs
        $startup.identityResponse=$identity;$startup.identityElapsedMs=$startupWatch.ElapsedMilliseconds
        Remaining-StartupBudget $startupWatch|Out-Null
        if($identity.pid -ne $process.Id) {throw 'Wrong pipe owner'}
        $startup.stage='terminal-readiness';$startup.treeRequests=0
        do {
            $startup.treeRequests++
            $tree=Tree (Remaining-StartupBudget $startupWatch)
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
function Auto-Document($State,[string]$Document) {
    $matches=@($State.documents|Where-Object {$_.id -eq $Document})
    if($matches.Count -ne 1) {throw ('Automatic refresh lost document identity '+$Document)}
    return $matches[0]
}
function Wait-Automatic([string]$Surface,[scriptblock]$Condition,[bool]$AllowError=$false) {
    $watch=[Diagnostics.Stopwatch]::StartNew();$state=$null
    do {
        $remaining=8000-$watch.ElapsedMilliseconds
        if($remaining -le 0) {
            $script:evidence.observations+=@{kind='automatic-refresh-timeout';surface=$Surface;elapsedMs=$watch.ElapsedMilliseconds;lastStatus=$state}
            throw ('Automatic refresh did not satisfy its bounded condition: '+($state|ConvertTo-Json -Depth 8 -Compress))
        }
        $state=Status $Surface ([int][Math]::Min(5000,$remaining));$auto=$state.automatic_refresh
        if($null -eq $auto) {throw 'Editor omitted automatic_refresh diagnostics'}
        foreach($field in @('mode','ready','pending','generation','applied_generation','completed_count','deferred_count','last_error','phase','timed_out','watcher')) {
            if($auto.psobject.Properties.Name -notcontains $field) {throw ('Automatic refresh omitted '+$field)}
        }
        if($auto.mode -ne 'native') {throw ('Native file watcher is unavailable: '+($auto|ConvertTo-Json -Depth 6 -Compress))}
        if($state.synchronization_failed -or $auto.timed_out) {throw ('Automatic refresh lost synchronization: '+($state|ConvertTo-Json -Depth 8 -Compress))}
        if(-not $AllowError -and $auto.last_error) {throw ('Automatic refresh reported an unexpected error: '+$auto.last_error)}
        if($auto.ready -and $state.ready -and -not $auto.pending -and -not $state.pending -and (& $Condition $state)) {return $state}
        Start-Sleep -Milliseconds 30
    } while($true)
}
function Automatic-Baseline([string]$Surface) {
    return Wait-Automatic $Surface {param($s) $s.automatic_refresh.applied_generation -eq $s.automatic_refresh.generation}
}
function Automatic-Advanced($State,$Baseline) {
    return $State.automatic_refresh.completed_count -gt $Baseline.automatic_refresh.completed_count -and $State.automatic_refresh.applied_generation -gt $Baseline.automatic_refresh.applied_generation
}
function Assert-AutomaticText([string]$Surface,[string]$Expected,[bool]$Dirty,[string]$Document,[long]$Version) {
    # No Flush/Open/CheckDisk here: only read the already acknowledged Monaco model.
    $read=Read-Editor $Surface
    if(-not (Same-Text $read.content $Expected) -or $read.dirty -ne $Dirty -or $read.document_id -ne $Document -or $read.active_version -ne $Version) {throw ('Automatic Monaco content/version differs: '+($read|ConvertTo-Json -Depth 6 -Compress))}
    return $read
}
function Record-Automatic([string]$Group,[string]$Surface,$Before,$After,$Read) {
    $script:evidence.observations+=@{kind='automatic-refresh-result';case=$Group;surface=$Surface;before=$Before.automatic_refresh;after=$After.automatic_refresh;documents=$After.documents;actualModel=$Read;explicitCheckDisk=$false}
}
function Owned-Recovery {
    $root=Join-Path (Join-Path $directory 'state') 'editor-recovery'
    if(-not (Test-Path -LiteralPath $root -PathType Container)) {throw 'Owned editor recovery directory was not created'}
    foreach($file in @(Get-ChildItem -LiteralPath $root -Recurse -File -Filter '*.json')) {
        $record=[IO.File]::ReadAllText($file.FullName,[Text.Encoding]::UTF8)|ConvertFrom-Json
        $identity=[string]$record.identityPath
        # Rust canonical Windows identities may use the verbatim local-drive prefix.
        if($identity.StartsWith('\\?\',[StringComparison]::Ordinal)) {$identity=$identity.Substring(4)}
        [pscustomobject]@{file=$file.FullName;identity=$identity;content=$record.content;version=$record.documentVersion}
    }
}
function Complete-Open($Job) {
    $opened=(End-Command $Job).editor_opened
    if(-not $opened.surface -or -not $opened.pane) {throw 'Editor open omitted pane/surface identity'}
    if($script:ownedEditors -notcontains $opened.surface) {$script:ownedEditors+=$opened.surface}
    return $opened
}
function Open-Editor([string]$Path,[string]$Pane='',[string]$Root='') {
    if(-not $Pane) {$Pane=$script:source.pane}
    if(-not $Root) {$Root=$fixture.Root}
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
function Forget-Editor([string]$Surface) {$script:ownedEditors=@($script:ownedEditors|Where-Object {$_ -ne $Surface})}
function Restore-Owned([string]$Window) {
    $old=$script:terminal
    $tree=Start-Owned $true $Window;$script:source=Request @('identify')
    $current=@($tree.surfaces|Where-Object {$_.id -eq $old.id})
    if($current.Count -ne 1 -or $current[0].pid -eq $old.pid) {throw 'Restored terminal did not retain identity with a fresh process'}
    $script:terminal=$current[0]
    return $tree
}
function Saved-Surface($Pane,[string]$Surface) {
    if($Pane.kind -eq 'split') {Saved-Surface $Pane.first $Surface;Saved-Surface $Pane.second $Surface}
    else {$Pane.content.surfaces|Where-Object {$_.id -eq $Surface}}
}
function Pane-Leaf($Node,[string]$Pane) {
    if($Node.kind -eq 'split') {Pane-Leaf $Node.first $Pane;Pane-Leaf $Node.second $Pane}
    elseif($Node.id -eq $Pane) {$Node}
}
function Checkpoint-Editor([string]$Path,[string]$Surface) {
    $checkpoint=[IO.File]::ReadAllText($Path,[Text.Encoding]::UTF8)|ConvertFrom-Json
    $matches=@($checkpoint.workspaces|ForEach-Object {Saved-Surface $_.root $Surface})
    if($matches.Count -ne 1 -or $matches[0].kind.type -ne 'editor') {throw 'Checkpoint lost its editor surface identity'}
    if($checkpoint.screens.($Surface) -or $checkpoint.shells.($Surface)) {throw 'Editor checkpoint included terminal history or shell metadata'}
    return $matches[0].kind
}
function Assert-SavedSession($Actual,$Expected,[string]$Context) {
    if($null -eq $Actual -or $null -eq $Expected) {throw ($Context+': editor session missing')}
    if(($null -eq $Actual.active_file) -ne ($null -eq $Expected.active_file) -or -not (Same-Text $Actual.active_file $Expected.active_file)) {throw ($Context+': active document path changed')}
    if(($null -eq $Actual.zoom_percent) -ne ($null -eq $Expected.zoom_percent) -or $Actual.zoom_percent -ne $Expected.zoom_percent) {throw ($Context+': editor zoom changed')}
    $actualFiles=@($Actual.open_files|Where-Object {$null -ne $_});$expectedFiles=@($Expected.open_files|Where-Object {$null -ne $_})
    if($actualFiles.Count -ne $expectedFiles.Count) {throw ($Context+': ordered document count changed')}
    for($index=0;$index -lt $expectedFiles.Count;$index++) {
        $actualFile=$actualFiles[$index];$expectedFile=$expectedFiles[$index]
        if(-not (Same-Text $actualFile.path $expectedFile.path) -or $actualFile.cursor_line -ne $expectedFile.cursor_line -or $actualFile.cursor_column -ne $expectedFile.cursor_column -or $actualFile.scroll_top -ne $expectedFile.scroll_top) {throw ($Context+': ordered document path or view state changed at index '+$index)}
    }
}

try {
    $doctor=End-Command (Begin-Probe $cli @('doctor'))
    if(-not $doctor.background_testing -or $doctor.status -ne 'ok') {throw 'Working hidden debug build required; no host launched'}
    $tree=Start-Owned;$script:source=Request @('identify');$script:terminal=$tree.surfaces[0]
    if($Case -eq 'startup') {Passed 'hidden_debug_doctor_and_owned_host_readiness'}
    foreach($group in @('open','async-open','picker-blocked','late-open','close-preparing','edit','encoding','encoding-defaults','conflict','save-as','save-all-many','close','move','restore','restore-errors','missing-root','checkpoint-failure','late-quit','late-quit-empty','recovery','auto-refresh-clean','auto-refresh-inactive','auto-refresh-conflict','auto-refresh-delete-recreate','auto-refresh-stamp','auto-refresh-partial-error','auto-refresh-move-close','auto-refresh-coalescing')) {
        if($Case -ne 'all' -and $Case -ne $group) {continue}
        Clean-Editors;$fixture.ReleaseLocks();Request @('focus-tab',$terminal.id)|Out-Null
        switch($group) {
            'open' {
                $path=$fixture.Write('open 문서 한 😀.txt',[EditorFixture]::Original,$false,$false)
                $opened=Open-Editor $path;$read=Assert-Text $opened.surface ([EditorFixture]::Original) $false
                $expectedName=[IO.Path]::GetFileName($path)
                if(-not (Same-Text $read.path $expectedName) -or $read.language -ne 'plaintext' -or $read.encoding -ne 'UTF-8' -or $read.eol -ne 'LF') {throw 'Opened Monaco document metadata differs'}
                $duplicate=Open-Editor $path $opened.pane
                if($duplicate.surface -ne $opened.surface -or @((Status $opened.surface).documents).Count -ne 1 -or (Read-Editor $opened.surface).document_id -ne $read.document_id) {throw 'Duplicate open did not reuse its existing editor document'}
                $code=$fixture.Write('open syntax.json',"{`"message`":`"한글 😀`"}`n",$false,$false)
                $same=Open-Editor $code $opened.pane
                if($same.surface -ne $opened.surface -or (Read-Editor $opened.surface).language -ne 'json' -or @((Status $opened.surface).documents).Count -ne 2) {throw 'Same-root open did not reuse editor with actual JSON model'}
                $bad=@($fixture.WriteBytes('invalid-utf8.txt',[byte[]]@(0xff)),$fixture.WriteBytes('binary.txt',[byte[]]@(65,0,66)),$fixture.WriteBytes('mixed.txt',[Text.Encoding]::UTF8.GetBytes("one`r`ntwo`n")))
                foreach($invalid in $bad) {Request @('editor','open',$invalid,'--pane',$opened.pane,'--root',$fixture.Root) 1|Out-Null}
                if(@((Status $opened.surface).documents).Count -ne 2) {throw 'Invalid text created an editor document'}
                Request @('editor','status',$terminal.id) 1|Out-Null
                Request @('read-screen','--surface',$opened.surface) 1|Out-Null
                Request @('browser','eval',$opened.pane,'document.title') 1|Out-Null
                Passed 'actual_monaco_unicode_open_duplicate_identity_language_and_invalid_text_isolation'
            }
            'async-open' {
                $firstSource=Request @('identify')
                Request @('split','vertical')|Out-Null;$secondSource=Request @('identify')
                if($firstSource.pane -eq $secondSource.pane -or $firstSource.workspace -ne $secondSource.workspace) {throw 'Concurrent Open requires two distinct captured panes in one workspace'}
                $deadline=(Get-Date).AddSeconds(5)
                do {
                    $tree=Tree;$secondTerminal=@($tree.surfaces|Where-Object {$_.id -eq $secondSource.surface})
                    if($secondTerminal.Count -eq 1 -and $secondTerminal[0].ready -and $secondTerminal[0].running) {break}
                    if((Get-Date) -gt $deadline) {throw 'Second owned terminal did not become ready'}
                    Start-Sleep -Milliseconds 20
                } while($true)
                $secondTerminal=$secondTerminal[0]
                if($shells -notcontains $secondTerminal.pid) {$shells+=$secondTerminal.pid}
                $firstPath=$fixture.Write('async first 한글 한\first é 😀.txt',[EditorFixture]::Original,$false,$false)
                $secondPath=$fixture.Write('async second é 😀\second 한글 한.txt',[EditorFixture]::Edited,$true,$true)
                $requests=@(
                    @{source=$firstSource;path=$firstPath;root=[IO.Path]::GetDirectoryName($firstPath);text=[EditorFixture]::Original;encoding='UTF-8';eol='LF'},
                    @{source=$secondSource;path=$secondPath;root=[IO.Path]::GetDirectoryName($secondPath);text=[EditorFixture]::Edited;encoding='UTF-8 BOM';eol='CRLF'}
                )
                # Start both real clients before waiting for either; no worker delay or test hook.
                $firstJob=Begin-Command @('editor','open',$requests[0].path,'--pane',$requests[0].source.pane,'--root',$requests[0].root)
                $secondJob=Begin-Command @('editor','open',$requests[1].path,'--pane',$requests[1].source.pane,'--root',$requests[1].root)
                $overlapped=-not $firstJob.process.HasExited -and -not $secondJob.process.HasExited
                $evidence.observations+=@{case=$group;kind='concurrent-open-submission';firstClientPid=$firstJob.pid;secondClientPid=$secondJob.pid;overlappingClients=$overlapped;forcedCompletionOrder=$false}
                if(-not $overlapped) {throw 'Open clients did not overlap; concurrent Open coverage was not established'}
                $requests[0].opened=Complete-Open $firstJob;$requests[1].opened=Complete-Open $secondJob
                if($requests[0].opened.surface -eq $requests[1].opened.surface) {throw 'Distinct roots and panes reused one editor surface'}
                foreach($request in $requests) {
                    $opened=$request.opened;$status=Ready $opened.surface;$read=Assert-Text $opened.surface $request.text $false
                    $docs=@($status.documents)
                    if($opened.pane -ne $request.source.pane -or -not (Same-Text $status.workspace_root $request.root) -or -not (Same-Text $status.session.active_file $request.path) -or $docs.Count -ne 1 -or $docs[0].id -ne $read.document_id -or -not (Same-Text $read.path ([IO.Path]::GetFileName($request.path))) -or $read.encoding -ne $request.encoding -or $read.eol -ne $request.eol) {throw 'Concurrent Open returned an incorrect pane, root, path or actual Monaco document'}
                    $tree=Tree;$workspace=@($tree.workspaces|Where-Object {$_.id -eq $request.source.workspace})
                    if($workspace.Count -ne 1) {throw 'Concurrent Open lost its captured workspace'}
                    $leaf=@(Pane-Leaf $workspace[0].root $request.source.pane)
                    if($leaf.Count -ne 1 -or @($leaf[0].content.surfaces|Where-Object {$_.id -eq $opened.surface}).Count -ne 1 -or @($leaf[0].content.surfaces|Where-Object {$_.id -eq $request.source.surface}).Count -ne 1) {throw 'Concurrent Open changed its captured source pane or terminal membership'}
                    $evidence.observations+=@{case=$group;kind='concurrent-open-result';sourcePane=$request.source.pane;sourceSurface=$request.source.surface;surface=$opened.surface;root=$status.workspace_root;activeFile=$status.session.active_file;document=$read.document_id;content=$read.content;encoding=$read.encoding;eol=$read.eol}
                }
                Assert-Terminal;$tree=Tree;$current=@($tree.surfaces|Where-Object {$_.id -eq $secondTerminal.id})
                if($current.Count -ne 1 -or $current[0].pid -ne $secondTerminal.pid -or -not $current[0].running) {throw 'Concurrent Open changed the second original terminal process'}
                Assert-Bytes $firstPath ([EditorFixture]::Original);Assert-Bytes $secondPath ([EditorFixture]::Edited) $true $true
                Clean-Editors;Request @('close-tab',$secondSource.surface)|Out-Null
                Passed 'overlapping_real_cli_opens_keep_two_captured_panes_distinct_unicode_roots_actual_monaco_and_original_terminals'
            }
            'picker-blocked' {
                $path=$fixture.Write('picker retained 한글 한 😀.txt',[EditorFixture]::Original,$false,$false)
                $opened=Open-Editor $path;Assert-Text $opened.surface ([EditorFixture]::Original) $false|Out-Null
                Request @('focus-tab',$terminal.id)|Out-Null
                $before=Tree;$beforeIdentity=Request @('identify')
                $beforeWindows=@([EditorWindowProbe]::VisibleTopLevelWindows($process))
                if($beforeWindows.Count) {throw 'Owned hidden host already has a visible top-level window'}
                $response=Request @('editor','pick','--pane',$source.pane) 1
                $afterWindows=@([EditorWindowProbe]::VisibleTopLevelWindows($process))
                $after=Tree;$afterIdentity=Request @('identify')
                if(-not (Same-Text $response.error 'Open File is unavailable in background mode')) {throw 'Picker did not return its explicit background-mode rejection'}
                if($afterWindows.Count -ne 0 -or -not (Same-Text ($before.workspaces|ConvertTo-Json -Depth 60 -Compress) ($after.workspaces|ConvertTo-Json -Depth 60 -Compress)) -or -not (Same-Text ($before.layout|ConvertTo-Json -Depth 20 -Compress) ($after.layout|ConvertTo-Json -Depth 20 -Compress)) -or $before.active_workspace -ne $after.active_workspace -or @($before.editors).Count -ne @($after.editors).Count -or -not (Same-Text (@($before.editors.id|Sort-Object)|ConvertTo-Json -Compress) (@($after.editors.id|Sort-Object)|ConvertTo-Json -Compress)) -or $beforeIdentity.pane -ne $afterIdentity.pane -or $beforeIdentity.surface -ne $afterIdentity.surface) {throw 'Blocked picker changed the tree, editor identities, logical focus or visible owned windows'}
                if($process.HasExited -or $afterIdentity.pid -ne $process.Id) {throw 'Blocked picker lost its owned host'}
                Assert-Terminal;Assert-Text $opened.surface ([EditorFixture]::Original) $false|Out-Null;Assert-Bytes $path ([EditorFixture]::Original)
                $evidence.observations+=@{case=$group;kind='blocked-picker';response=$response;pid=$process.Id;pane=$source.pane;editorsBefore=@($before.editors).Count;editorsAfter=@($after.editors).Count;visibleOwnedTopLevelBefore=$beforeWindows;visibleOwnedTopLevelAfter=$afterWindows;treeUnchanged=$true;terminalAlive=$true;dialogInteraction=$false;continuousWindowObservation=$false}
                Passed 'background_open_file_picker_rejects_with_unchanged_tree_editor_identity_and_no_visible_owned_top_level_window'
            }
            'late-open' {
                $path=$fixture.Write('late open root 한글 한\expired é 😀.txt',[EditorFixture]::Original,$false,$false)
                $root=[IO.Path]::GetDirectoryName($path);$before=Tree;$identity=Request @('identify')
                if($identity.pid -ne $process.Id -or @($before.editors).Count -or $before.editor_open_pending -ne 0 -or $before.editor_open_admitted -ne 0) {throw 'Late Open requires the exact owned host with no editor or admitted preparation'}
                $pause=$null;$deadlineWatch=[Diagnostics.Stopwatch]::StartNew()
                $observation=[ordered]@{case=$group;kind='late-open-deadline';pid=$process.Id;pane=$identity.pane;windowHandle=$before.window_handle;clientBudgetMs=20000;wholeProcessSuspended=$false;uiThreadSuspended=$false;uiThreadResumed=$false}
                $evidence.observations+=$observation
                try {
                    $pause=New-Object EditorUiThreadPause($process,([long]$before.window_handle))
                    $observation.threadId=$pause.ThreadId;$observation.verifiedThreadPid=$pause.ProcessId;$observation.previousSuspendCount=$pause.PreviousSuspendCount;$observation.uiThreadSuspended=$true
                    # This deliberate pause reaches the real server's 15s deadline,
                    # proving that time already spent in its UI queue is not renewed.
                    $response=End-Command (Begin-Command @('editor','open',$path,'--pane',$identity.pane,'--root',$root)) @(1) 20000
                    $observation.response=$response;$observation.responseElapsedMs=$deadlineWatch.ElapsedMilliseconds
                    if(-not $response.error -or -not $response.error.Contains('window did not answer within the IPC command deadline')) {throw 'Late Open did not observe the actual server command deadline'}
                    if($process.HasExited) {throw 'Owned host exited before the expired Open was processed'}
                } finally {
                    if($pause) {
                        try {$pause.Dispose()} finally {$observation.uiThreadResumed=$pause.Resumed;$observation.previousResumeCount=$pause.PreviousResumeCount;$observation.suspendElapsedMs=$deadlineWatch.ElapsedMilliseconds}
                    }
                }
                if(-not $observation.uiThreadResumed -or $observation.previousResumeCount -ne 1) {throw 'Late Open UI suspension was not balanced exactly once'}
                $after=Tree;$afterIdentity=Request @('identify')
                if(@($after.editors).Count -ne 0 -or $after.editor_open_pending -ne 0 -or $after.editor_open_admitted -ne 0 -or $after.close_accepted -or -not (Same-Text ($before.workspaces|ConvertTo-Json -Depth 60 -Compress) ($after.workspaces|ConvertTo-Json -Depth 60 -Compress)) -or $identity.pane -ne $afterIdentity.pane -or $identity.surface -ne $afterIdentity.surface) {throw 'Expired UI-queued Open created state, retained admission or changed its source identity'}
                Assert-Terminal;Assert-Bytes $path ([EditorFixture]::Original)
                $observation.pendingAfter=$after.editor_open_pending;$observation.admittedAfter=$after.editor_open_admitted;$observation.editorsAfter=@($after.editors).Count;$observation.treeUnchanged=$true
                $opened=Open-Editor $path $identity.pane $root;Assert-Text $opened.surface ([EditorFixture]::Original) $false|Out-Null
                $observation.followingOpenSucceeded=$true;$observation.followingSurface=$opened.surface
                Clean-Editors
                Passed 'expired_ui_queued_open_creates_no_editor_drains_admission_preserves_terminal_and_allows_following_open'
            }
            'close-preparing' {
                # Start the real preparation worker, then leave a terminal-only model.
                $warmPath=$fixture.Write('preparer warm 한글.txt',[EditorFixture]::Original,$false,$false)
                $warm=Open-Editor $warmPath;Assert-Text $warm.surface ([EditorFixture]::Original) $false|Out-Null;Clean-Editors
                $path=$fixture.Write('close preparing root 한 😀\cancelled é 한글.txt',[EditorFixture]::Edited,$true,$true)
                $root=[IO.Path]::GetDirectoryName($path);$before=Tree;$identity=Request @('identify')
                if($identity.pid -ne $process.Id -or @($before.editors).Count -or $before.editor_open_pending -ne 0 -or $before.editor_open_admitted -ne 0) {throw 'Close-preparing requires an idle owned preparation worker and terminal-only model'}
                $pause=$null;$quitPeer=$null;$openJob=$null;$closedPid=$process.Id
                $observation=[ordered]@{case=$group;kind='close-preparing';pid=$closedPid;pane=$identity.pane;windowHandle=$before.window_handle;wholeProcessSuspended=$false;workerSuspended=$false;workerResumed=$false;rawQuitHeld=$false;completedBeforeHostExit=$false}
                $evidence.observations+=$observation
                try {
                    $pause=New-Object EditorOpenWorkerPause($process,([long]$before.window_handle))
                    $observation.threadId=$pause.ThreadId;$observation.verifiedThreadPid=$pause.ProcessId;$observation.threadDescription=$pause.Description;$observation.previousSuspendCount=$pause.PreviousSuspendCount;$observation.workerSuspended=$true
                    $openJob=Begin-Command @('editor','open',$path,'--pane',$identity.pane,'--root',$root)
                    $pendingWatch=[Diagnostics.Stopwatch]::StartNew()
                    do {
                        $pending=Raw-Tree
                        if($pending.editor_open_pending -eq 1 -and $pending.editor_open_admitted -eq 1) {break}
                        if($openJob.process.HasExited -or $pendingWatch.ElapsedMilliseconds -ge 3000) {throw 'Paused worker did not retain exactly one pending/admitted Open'}
                    } while($true)
                    if(@($pending.editors).Count -or $pending.close_accepted) {throw 'Preparation created a tab or accepted close before quit'}
                    $observation.pendingBeforeQuit=$pending.editor_open_pending;$observation.admittedBeforeQuit=$pending.editor_open_admitted
                    $quitPeer=New-Object EditorOwnedPipe($process,$pipeName,300)
                    $quitResponse=$quitPeer.Request('{"method":"quit","discard_state":true}',1000)|ConvertFrom-Json
                    $heldWatch=[Diagnostics.Stopwatch]::StartNew();$observation.rawQuitHeld=$true;$observation.quitResponse=$quitResponse
                    if($quitResponse.ok -ne $true) {throw 'Raw held quit was not accepted'}
                    $accepted=Raw-Tree
                    if(-not $accepted.close_accepted -or $accepted.editor_open_pending -ne 0 -or $accepted.editor_open_admitted -ne 1 -or @($accepted.editors).Count) {throw 'Accepted close did not cancel the pending Open while its paused worker still owned admission'}
                    $observation.closeAccepted=$accepted.close_accepted;$observation.pendingAtAcceptance=$accepted.editor_open_pending;$observation.admittedAtAcceptance=$accepted.editor_open_admitted
                    # The server waits at most two seconds for the quit peer to close.
                    # Resume immediately and use only bounded raw IPC in that interval.
                    $pause.Dispose();$observation.workerResumed=$pause.Resumed;$observation.previousResumeCount=$pause.PreviousResumeCount
                    if(-not $pause.Resumed -or $pause.PreviousResumeCount -ne 1) {throw 'Owned preparation worker suspension was not balanced exactly once'}
                    do {
                        if($process.HasExited) {throw 'Host exited before cancelled preparation admission was observed draining'}
                        $drained=Raw-Tree
                        if(-not $drained.close_accepted -or @($drained.editors).Count -or -not (Same-Text ($before.workspaces|ConvertTo-Json -Depth 60 -Compress) ($drained.workspaces|ConvertTo-Json -Depth 60 -Compress))) {throw 'Late preparation published a tab or changed the accepted-close model'}
                        if($drained.editor_open_pending -eq 0 -and $drained.editor_open_admitted -eq 0) {break}
                        if($heldWatch.ElapsedMilliseconds -ge 1200) {throw 'Cancelled preparation did not drain before the held quit transport deadline'}
                    } while($true)
                    $current=@($drained.surfaces|Where-Object {$_.id -eq $terminal.id})
                    if($process.HasExited -or $current.Count -ne 1 -or $current[0].pid -ne $terminal.pid -or -not $current[0].running) {throw 'Original host/terminal exited before the late preparation result was checked'}
                    $observation.pendingAfterResume=$drained.editor_open_pending;$observation.admittedAfterResume=$drained.editor_open_admitted;$observation.editorsAfterResume=@($drained.editors).Count;$observation.heldMilliseconds=$heldWatch.ElapsedMilliseconds;$observation.completedBeforeHostExit=$true
                } finally {
                    try {
                        if($pause) {
                            try {$pause.Dispose()} finally {$observation.workerResumed=$pause.Resumed;$observation.previousResumeCount=$pause.PreviousResumeCount}
                        }
                    } finally {
                        if($quitPeer) {$quitPeer.Dispose();$observation.rawQuitReleased=$true}
                    }
                }
                $response=End-Command $openJob @(1)
                if(-not (Same-Text $response.error 'window close was accepted before editor Open completed')) {throw 'Preparing Open did not receive its explicit accepted-close cancellation'}
                $observation.openResponse=$response
                if(-not $process.WaitForExit(5000)) {throw 'Accepted close did not exit after the held quit peer was released'}
                Assert-Bytes $path ([EditorFixture]::Edited) $true $true
                Finish-Host;$observation.cleanHostExit=$true
                Passed 'accepted_quit_cancels_paused_owned_preparation_drains_late_result_without_tab_then_exits_when_peer_closes'
                $tree=Start-Owned;$script:source=Request @('identify');$script:terminal=$tree.surfaces[0]
            }
            'auto-refresh-clean' {
                $name='auto clean 한글 한 😀.txt';$path=$fixture.Write($name,[EditorFixture]::Original,$true,$true)
                $opened=Open-Editor $path;$initial=Read-Editor $opened.surface;$before=Automatic-Baseline $opened.surface
                $fixture.Write($name,[EditorFixture]::External,$true,$true)|Out-Null;$writtenTicks=$fixture.LastWriteTicks($name)
                $after=Wait-Automatic $opened.surface {param($s) (Automatic-Advanced $s $before) -and (Auto-Document $s $initial.document_id).version -gt $initial.active_version}
                $doc=Auto-Document $after $initial.document_id;$read=Assert-AutomaticText $opened.surface ([EditorFixture]::External) $false $initial.document_id $doc.version
                if($read.encoding -ne 'UTF-8 BOM' -or $read.eol -ne 'CRLF' -or $after.view_handle -ne $before.view_handle -or -not $doc.disk_status_known -or $doc.external_change -or $read.external_change) {throw 'Clean automatic reload changed identity/encoding or left uncertain/conflicting status'}
                Assert-Bytes $path ([EditorFixture]::External) $true $true
                if($fixture.LastWriteTicks($name) -ne $writtenTicks) {throw 'Automatic detection rewrote the external file timestamp'}
                Record-Automatic $group $opened.surface $before $after $read
                Passed 'native_watcher_clean_reload_updates_actual_unicode_monaco_after_apply_ack_preserving_bom_crlf_and_file_bytes'
            }
            'auto-refresh-inactive' {
                $firstName='auto inactive first 한글.txt';$first=$fixture.Write($firstName,[EditorFixture]::Original,$true,$true)
                $second=$fixture.Write('auto active second 😀.txt',[EditorFixture]::External,$false,$false)
                $opened=Open-Editor $first;$firstRead=Read-Editor $opened.surface
                Open-Editor $second $opened.pane|Out-Null;$active=Read-Editor $opened.surface;$before=Automatic-Baseline $opened.surface
                $beforeTree=(Tree).workspaces|ConvertTo-Json -Depth 60 -Compress
                $fixture.Write($firstName,[EditorFixture]::Edited,$true,$true)|Out-Null
                $after=Wait-Automatic $opened.surface {param($s) (Automatic-Advanced $s $before) -and (Auto-Document $s $firstRead.document_id).version -gt $firstRead.active_version}
                $activeAfter=Assert-AutomaticText $opened.surface ([EditorFixture]::External) $false $active.document_id $active.active_version
                if($after.active_document_id -ne $active.document_id -or $after.view_handle -ne $before.view_handle -or -not (Same-Text $beforeTree ((Tree).workspaces|ConvertTo-Json -Depth 60 -Compress))) {throw 'Inactive automatic reload changed active document, native view or pane/workspace state'}
                $firstAfter=Auto-Document $after $firstRead.document_id
                Editor-Command $opened.surface 'close-document'|Out-Null
                $revealed=Assert-AutomaticText $opened.surface ([EditorFixture]::Edited) $false $firstRead.document_id $firstAfter.version
                if($revealed.encoding -ne 'UTF-8 BOM' -or $revealed.eol -ne 'CRLF') {throw 'Inactive model lost BOM/CRLF metadata'}
                Assert-Bytes $first ([EditorFixture]::Edited) $true $true;Assert-Bytes $second ([EditorFixture]::External)
                Record-Automatic $group $opened.surface $before $after $revealed
                $evidence.observations+=@{kind='inactive-model-proof';case=$group;activeBefore=$active;activeAfter=$activeAfter;revealedExistingModel=$revealed;reopenedFile=$false}
                Passed 'inactive_document_auto_refresh_preserves_active_model_and_exposes_updated_existing_model_without_reopen'
            }
            'auto-refresh-conflict' {
                $name='auto dirty conflict 한글.txt';$path=$fixture.Write($name,[EditorFixture]::Original,$false,$false);$opened=Open-Editor $path
                Editor-Command $opened.surface 'replace-text' @('--text',[EditorFixture]::Edited)|Out-Null
                $dirty=Read-Editor $opened.surface;$before=Automatic-Baseline $opened.surface
                $fixture.Write($name,[EditorFixture]::External,$false,$false)|Out-Null
                $after=Wait-Automatic $opened.surface {param($s) (Automatic-Advanced $s $before) -and (Auto-Document $s $dirty.document_id).external_change}
                $read=Assert-AutomaticText $opened.surface ([EditorFixture]::Edited) $true $dirty.document_id $dirty.active_version
                if(-not $read.external_change) {throw 'Automatic dirty conflict was not applied to the actual Monaco document'}
                $saveFailure=Editor-Command $opened.surface 'save' @() 1
                Assert-Bytes $path ([EditorFixture]::External)
                Editor-Command $opened.surface 'keep-mine'|Out-Null;Assert-Bytes $path ([EditorFixture]::External)
                Editor-Command $opened.surface 'save'|Out-Null;Assert-Bytes $path ([EditorFixture]::Edited)
                Editor-Command $opened.surface 'replace-text' @('--text',[EditorFixture]::Original)|Out-Null
                $secondDirty=Read-Editor $opened.surface;$reloadBefore=Automatic-Baseline $opened.surface
                $fixture.Write($name,[EditorFixture]::External,$false,$false)|Out-Null
                $reloadAfter=Wait-Automatic $opened.surface {param($s) (Automatic-Advanced $s $reloadBefore) -and (Auto-Document $s $secondDirty.document_id).external_change}
                $reloadConflict=Assert-AutomaticText $opened.surface ([EditorFixture]::Original) $true $secondDirty.document_id $secondDirty.active_version
                if(-not $reloadConflict.external_change) {throw 'Second automatic conflict was not applied before explicit Reload'}
                Editor-Command $opened.surface 'reload'|Out-Null
                $reloaded=Read-Editor $opened.surface
                if(-not (Same-Text $reloaded.content ([EditorFixture]::External)) -or $reloaded.dirty -or $reloaded.document_id -ne $dirty.document_id) {throw 'Explicit reload did not resolve the automatic conflict on the same model'}
                Assert-Bytes $path ([EditorFixture]::External)
                Record-Automatic $group $opened.surface $before $after $read
                $evidence.observations+=@{kind='automatic-conflict-actions';case=$group;saveFailure=$saveFailure;reloadRefresh=$reloadAfter.automatic_refresh;explicitReload=$reloaded}
                Passed 'automatic_dirty_conflict_preserves_unsaved_monaco_and_external_bytes_until_explicit_keep_mine_save_or_reload'
            }
            'auto-refresh-delete-recreate' {
                $name='auto deleted 한글 😀.txt';$path=$fixture.Write($name,[EditorFixture]::Original,$false,$false);$opened=Open-Editor $path
                $initial=Read-Editor $opened.surface;$before=Automatic-Baseline $opened.surface
                $fixture.DeleteOwned($name)
                $deleted=Wait-Automatic $opened.surface {param($s) (Automatic-Advanced $s $before) -and (Auto-Document $s $initial.document_id).external_change}
                $retained=Assert-AutomaticText $opened.surface ([EditorFixture]::Original) $false $initial.document_id $initial.active_version
                if(-not $retained.external_change) {throw 'Automatic deletion status was not applied to the retained Monaco document'}
                if(Test-Path -LiteralPath $path) {throw 'Automatic refresh recreated the deleted file'}
                $fixture.Write($name,[EditorFixture]::External,$false,$false)|Out-Null
                $after=Wait-Automatic $opened.surface {param($s) (Automatic-Advanced $s $deleted) -and (Auto-Document $s $initial.document_id).version -gt $initial.active_version -and -not (Auto-Document $s $initial.document_id).external_change}
                $doc=Auto-Document $after $initial.document_id;$read=Assert-AutomaticText $opened.surface ([EditorFixture]::External) $false $initial.document_id $doc.version
                if($before.view_handle -ne $after.view_handle -or $read.external_change) {throw 'Delete/recreate replaced the native editor view or retained a stale conflict'}
                Assert-Bytes $path ([EditorFixture]::External)
                Record-Automatic $group $opened.surface $before $after $read
                $evidence.observations+=@{kind='automatic-deletion';case=$group;missingFileObserved=$true;deletedStatus=$deleted;retainedModel=$retained;implicitRecreation=$false}
                Passed 'automatic_delete_retains_existing_model_without_recreating_file_then_recreated_file_refreshes_same_identity'
            }
            'auto-refresh-stamp' {
                $name='auto same timestamp 한글 한.txt';$path=$fixture.Write($name,[EditorFixture]::Original,$true,$true);$opened=Open-Editor $path
                $initial=Read-Editor $opened.surface;$before=Automatic-Baseline $opened.surface;$ticks=$fixture.LastWriteTicks($name)
                $replacement=[EditorFixture]::Original.Replace('second','SECOND')
                $returnedTicks=$fixture.RewritePreservingTimestamp($name,$replacement,$true,$true)
                if($returnedTicks -ne $ticks -or $fixture.LastWriteTicks($name) -ne $ticks) {throw 'Equal-length fixture rewrite changed timestamp'}
                $after=Wait-Automatic $opened.surface {param($s) (Automatic-Advanced $s $before) -and (Auto-Document $s $initial.document_id).version -gt $initial.active_version}
                $doc=Auto-Document $after $initial.document_id;$read=Assert-AutomaticText $opened.surface $replacement $false $initial.document_id $doc.version
                Assert-Bytes $path $replacement $true $true
                if($fixture.LastWriteTicks($name) -ne $ticks) {throw 'Same-stamp detection rewrote the fixture'}
                Record-Automatic $group $opened.surface $before $after $read
                $evidence.observations+=@{kind='equal-size-restored-timestamp';case=$group;beforeUtcTicks=$ticks;afterUtcTicks=$fixture.LastWriteTicks($name);bytes=[EditorFixture]::Encode($replacement,$true,$true).Length;metadataOnlyPoll=$false}
                Passed 'native_watcher_forced_byte_check_detects_equal_length_external_write_with_exact_restored_timestamp'
            }
            'auto-refresh-partial-error' {
                $firstName='auto partial first 한글.txt';$secondName='auto partial blocked 😀.txt'
                $first=$fixture.Write($firstName,[EditorFixture]::Original,$true,$true);$second=$fixture.Write($secondName,[EditorFixture]::External,$false,$false)
                $opened=Open-Editor $first;$firstRead=Read-Editor $opened.surface
                Open-Editor $second $opened.pane|Out-Null;$secondRead=Read-Editor $opened.surface;$before=Automatic-Baseline $opened.surface
                $fixture.LockAgainstRead($secondName)
                try {
                    $fixture.Write($firstName,[EditorFixture]::Edited,$true,$true)|Out-Null
                    $after=Wait-Automatic $opened.surface {param($s) (Automatic-Advanced $s $before) -and $s.automatic_refresh.last_error -and (Auto-Document $s $firstRead.document_id).version -gt $firstRead.active_version} $true
                    $firstAfter=Auto-Document $after $firstRead.document_id;$blocked=Auto-Document $after $secondRead.document_id
                    if($blocked.disk_status_known -or $after.active_document_id -ne $secondRead.document_id -or $after.view_handle -ne $before.view_handle) {throw 'Partial scan lost active ownership or misreported blocked-file disk certainty'}
                    $retained=Assert-AutomaticText $opened.surface ([EditorFixture]::External) $false $secondRead.document_id $secondRead.active_version
                } finally {$fixture.ReleaseLocks()}
                # Unlock alone does not prove unknown status was repaired: a shared
                # failed poll can consume a status-only transition irreversibly.
                Wait-Automatic $opened.surface {param($s) $true} $true|Out-Null
                Editor-Command $opened.surface 'close-document'|Out-Null
                $revealed=Assert-AutomaticText $opened.surface ([EditorFixture]::Edited) $false $firstRead.document_id $firstAfter.version
                Editor-Command $opened.surface 'replace-text' @('--text',[EditorFixture]::External)|Out-Null
                Editor-Command $opened.surface 'save'|Out-Null
                Assert-Bytes $first ([EditorFixture]::External) $true $true;Assert-Bytes $second ([EditorFixture]::External)
                Record-Automatic $group $opened.surface $before $after $revealed
                $evidence.observations+=@{kind='partial-refresh-error';case=$group;blockedModel=$retained;error=$after.automatic_refresh.last_error;advancedExistingModel=$revealed;advancedDiskStatusKnown=$firstAfter.disk_status_known;repeatedFailedPollMayMakeAdvancedStatusUnknown=$true;unlockAloneProvesHealthy=$false;explicitSaveWithAdvancedVersion=$true;forcedPollOrder=$false}
                Passed 'partial_denied_read_preserves_advanced_clean_model_and_blocked_model_with_error_unknown_and_valid_following_save'
            }
            'auto-refresh-move-close' {
                $name='auto move close 한글.txt';$path=$fixture.Write($name,[EditorFixture]::Original,$false,$false);$opened=Open-Editor $path
                $initial=Read-Editor $opened.surface;$before=Automatic-Baseline $opened.surface
                Request @('focus-tab',$terminal.id)|Out-Null;Request @('split','vertical')|Out-Null;$destination=Request @('identify')
                $tree=Tree;$shells+=@($tree.surfaces|Where-Object {$_.pid -and $shells -notcontains $_.pid}|ForEach-Object {$_.pid})
                Wait-Automatic $opened.surface {param($s) $true}|Out-Null
                Request @('move-tab',$opened.surface,'--to-pane',$destination.pane)|Out-Null
                Request @('focus-tab',$destination.surface)|Out-Null
                $moved=Automatic-Baseline $opened.surface;$active=Request @('identify');$beforeTree=(Tree).workspaces|ConvertTo-Json -Depth 60 -Compress
                $fixture.Write($name,[EditorFixture]::External,$false,$false)|Out-Null
                $after=Wait-Automatic $opened.surface {param($s) (Automatic-Advanced $s $moved) -and (Auto-Document $s $initial.document_id).version -gt $initial.active_version}
                $read=Assert-AutomaticText $opened.surface ([EditorFixture]::External) $false $initial.document_id (Auto-Document $after $initial.document_id).version
                $identity=Request @('identify')
                if($after.view_handle -ne $before.view_handle -or $identity.surface -ne $active.surface -or -not (Same-Text $beforeTree ((Tree).workspaces|ConvertTo-Json -Depth 60 -Compress))) {throw 'Moved inactive refresh changed model/view/focused surface ownership'}
                Wait-Automatic $opened.surface {param($s) $true}|Out-Null
                Request @('close-tab',$opened.surface)|Out-Null;Forget-Editor $opened.surface
                $afterClose=Tree;$fixture.Write($name,[EditorFixture]::Edited,$false,$false)|Out-Null
                $stable=[Diagnostics.Stopwatch]::StartNew()
                do {
                    $tree=Tree
                    if(@($tree.editors|Where-Object {$_.id -eq $opened.surface}).Count -or -not (Same-Text ($afterClose.workspaces|ConvertTo-Json -Depth 60 -Compress) ($tree.workspaces|ConvertTo-Json -Depth 60 -Compress))) {throw 'Closed editor watcher resurrected a view or changed the model'}
                    Assert-Terminal
                    Start-Sleep -Milliseconds 30
                } while($stable.ElapsedMilliseconds -lt 1500)
                Assert-Bytes $path ([EditorFixture]::Edited)
                Record-Automatic $group $opened.surface $before $after $read
                $evidence.observations+=@{kind='automatic-move-close';case=$group;destinationPane=$destination.pane;focusedSurface=$active.surface;postCloseObservationMs=$stable.ElapsedMilliseconds;forcedQueuedCallback=$false}
                Request @('close-tab',$destination.surface)|Out-Null
                Passed 'moved_inactive_editor_refresh_keeps_identity_and_focus_then_closed_owner_stays_absent_after_external_write'
            }
            'auto-refresh-coalescing' {
                $name='auto burst own save 한글.txt';$path=$fixture.Write($name,[EditorFixture]::Original,$false,$false);$opened=Open-Editor $path
                $initial=Read-Editor $opened.surface;$before=Automatic-Baseline $opened.surface;$writes=32
                for($index=0;$index -lt $writes;$index++) {$last=([EditorFixture]::External+'burst '+$index+"`n");$fixture.Write($name,$last,$false,$false)|Out-Null}
                $after=Wait-Automatic $opened.surface {param($s) (Automatic-Advanced $s $before) -and $s.automatic_refresh.applied_generation -eq $s.automatic_refresh.generation -and (Auto-Document $s $initial.document_id).version -gt $initial.active_version}
                $read=Assert-AutomaticText $opened.surface $last $false $initial.document_id (Auto-Document $after $initial.document_id).version
                Assert-Bytes $path $last
                Editor-Command $opened.surface 'replace-text' @('--text',[EditorFixture]::Edited)|Out-Null
                $saveBefore=Automatic-Baseline $opened.surface;Editor-Command $opened.surface 'save'|Out-Null;$saved=Read-Editor $opened.surface
                $settled=Wait-Automatic $opened.surface {param($s) (Automatic-Advanced $s $saveBefore) -and $s.automatic_refresh.applied_generation -eq $s.automatic_refresh.generation}
                $final=Assert-AutomaticText $opened.surface ([EditorFixture]::Edited) $false $initial.document_id $saved.active_version
                if($final.external_change -or (Auto-Document $settled $initial.document_id).external_change -or (Auto-Document $settled $initial.document_id).version -ne $saved.active_version) {throw 'Own-save notification introduced a false conflict or repeated version bump'}
                Assert-Bytes $path ([EditorFixture]::Edited)
                Record-Automatic $group $opened.surface $before $settled $final
                $evidence.observations+=@{kind='automatic-write-burst';case=$group;writes=$writes;generationDelta=($after.automatic_refresh.generation-$before.automatic_refresh.generation);completedDelta=($after.automatic_refresh.completed_count-$before.automatic_refresh.completed_count);oneEventPerWriteAssumed=$false;burstModel=$read;ownSaveVersion=$saved.active_version}
                Passed 'automatic_write_burst_settles_at_final_bytes_and_own_save_notification_preserves_model_version_without_conflict'
            }
            'edit' {
                $path=$fixture.Write('edit 한글.txt',[EditorFixture]::Original,$false,$false);$opened=Open-Editor $path
                $before=Read-Editor $opened.surface
                Editor-Command $opened.surface 'replace-text' @('--text',[EditorFixture]::Edited)|Out-Null
                $edited=Assert-Text $opened.surface ([EditorFixture]::Edited) $true
                if($edited.document_id -ne $before.document_id) {throw 'Editing replaced Monaco document identity'}
                Assert-Bytes $path ([EditorFixture]::Original)
                Editor-Command $opened.surface 'undo'|Out-Null
                Flush $opened.surface;$undo=Read-Editor $opened.surface
                if(-not (Same-Text $undo.content ([EditorFixture]::Original))) {throw 'Actual Monaco undo did not restore original text'}
                Editor-Command $opened.surface 'redo'|Out-Null
                Assert-Text $opened.surface ([EditorFixture]::Edited) $true|Out-Null
                Editor-Command $opened.surface 'save'|Out-Null
                Assert-Text $opened.surface ([EditorFixture]::Edited) $false|Out-Null;Assert-Bytes $path ([EditorFixture]::Edited)
                Passed 'actual_monaco_edit_undo_redo_and_acknowledged_save_preserve_unicode_bytes'
            }
            'encoding' {
                foreach($variant in @(@{name='lf';bom=$false;crlf=$false},@{name='crlf';bom=$false;crlf=$true},@{name='bom-crlf';bom=$true;crlf=$true})) {
                    $path=$fixture.Write(('encoding '+$variant.name+' 한글.txt'),[EditorFixture]::Original,$variant.bom,$variant.crlf)
                    $opened=Open-Editor $path;$read=Assert-Text $opened.surface ([EditorFixture]::Original) $false
                    $encoding=if($variant.bom) {'UTF-8 BOM'} else {'UTF-8'};$eol=if($variant.crlf) {'CRLF'} else {'LF'}
                    if($read.encoding -ne $encoding -or $read.eol -ne $eol) {throw 'Monaco encoding/EOL metadata differs'}
                    Editor-Command $opened.surface 'replace-text' @('--text',[EditorFixture]::Edited)|Out-Null
                    Editor-Command $opened.surface 'save'|Out-Null;Assert-Bytes $path ([EditorFixture]::Edited) $variant.bom $variant.crlf
                    Editor-Command $opened.surface 'close-document'|Out-Null;Open-Editor $path $opened.pane|Out-Null
                    $read=Assert-Text $opened.surface ([EditorFixture]::Edited) $false
                    if($read.encoding -ne $encoding -or $read.eol -ne $eol) {throw 'Reopen lost saved encoding/EOL'}
                    $evidence.observations+=@{case=$group;variant=$variant.name;encoding=$read.encoding;eol=$read.eol;content=$read.content}
                    Clean-Editors
                }
                Passed 'utf8_lf_crlf_and_bom_crlf_round_trip_exact_file_bytes_without_normalizing_unicode'
            }
            'encoding-defaults' {
                # A CRLF encoder flag cannot establish CRLF metadata without any newline bytes.
                # The explicit CRLF control must still keep CRLF on disk after LF model edits.
                foreach($variant in @(
                    @{name='one-line-lf';initial=[EditorFixture]::Unicode;bom=$false;fixtureCrLf=$false;expectedCrLf=$false},
                    @{name='empty';initial='';bom=$false;fixtureCrLf=$false;expectedCrLf=$false},
                    @{name='one-line-bom-requested-crlf';initial=[EditorFixture]::Unicode;bom=$true;fixtureCrLf=$true;expectedCrLf=$false},
                    @{name='explicit-bom-crlf';initial=([EditorFixture]::Unicode+"`n");bom=$true;fixtureCrLf=$true;expectedCrLf=$true}
                )) {
                    $path=$fixture.Write(('encoding-default '+$variant.name+' 한글.txt'),$variant.initial,$variant.bom,$variant.fixtureCrLf)
                    $opened=Open-Editor $path;$initial=Assert-Text $opened.surface $variant.initial $false
                    $encoding=if($variant.bom) {'UTF-8 BOM'} else {'UTF-8'};$eol=if($variant.expectedCrLf) {'CRLF'} else {'LF'}
                    if($initial.encoding -ne $encoding -or $initial.eol -ne $eol) {throw ('Initial newline inference differs: '+$variant.name)}
                    Editor-Command $opened.surface 'replace-text' @('--text',[EditorFixture]::Edited)|Out-Null
                    Assert-Text $opened.surface ([EditorFixture]::Edited) $true|Out-Null
                    Editor-Command $opened.surface 'save'|Out-Null
                    Assert-Bytes $path ([EditorFixture]::Edited) $variant.bom $variant.expectedCrLf
                    Editor-Command $opened.surface 'close-document'|Out-Null;Open-Editor $path $opened.pane|Out-Null
                    $reopened=Assert-Text $opened.surface ([EditorFixture]::Edited) $false
                    if($reopened.encoding -ne $encoding -or $reopened.eol -ne $eol) {throw ('Saved newline inference changed: '+$variant.name)}
                    $evidence.observations+=@{case=$group;variant=$variant.name;fixtureRequestedCrLf=$variant.fixtureCrLf;inferredEol=$initial.eol;savedEol=$reopened.eol;encoding=$reopened.encoding;actualModelContent=$reopened.content}
                    Clean-Editors
                }
                Passed 'newline_less_and_empty_models_use_lf_while_explicit_crlf_disk_encoding_round_trips_without_double_cr'
            }
            'conflict' {
                $name='conflict 한글.txt';$path=$fixture.Write($name,[EditorFixture]::Original,$false,$false);$opened=Open-Editor $path
                $fixture.Write($name,[EditorFixture]::External,$false,$false)|Out-Null
                Request @('editor','check-disk',$opened.surface)|Out-Null
                Assert-Text $opened.surface ([EditorFixture]::External) $false|Out-Null
                Editor-Command $opened.surface 'replace-text' @('--text',[EditorFixture]::Edited)|Out-Null;Flush $opened.surface
                $fixture.Write($name,[EditorFixture]::Original,$false,$false)|Out-Null
                Request @('editor','check-disk',$opened.surface)|Out-Null
                $failure=Editor-Command $opened.surface 'save' @() 1
                Assert-Bytes $path ([EditorFixture]::Original);Assert-Text $opened.surface ([EditorFixture]::Edited) $true|Out-Null
                $evidence.observations+=@{case=$group;saveConflict=$failure}
                Editor-Command $opened.surface 'compare'|Out-Null;Assert-Bytes $path ([EditorFixture]::Original)
                if(-not (Read-Editor $opened.surface).diff_visible) {throw 'Compare did not open the actual Monaco diff view'}
                Editor-Command $opened.surface 'keep-mine'|Out-Null
                Assert-Bytes $path ([EditorFixture]::Original);Assert-Text $opened.surface ([EditorFixture]::Edited) $true|Out-Null
                Editor-Command $opened.surface 'save'|Out-Null;Assert-Bytes $path ([EditorFixture]::Edited)
                Editor-Command $opened.surface 'replace-text' @('--text',[EditorFixture]::Original)|Out-Null;Flush $opened.surface
                $fixture.Write($name,[EditorFixture]::External,$false,$false)|Out-Null
                Request @('editor','check-disk',$opened.surface)|Out-Null;Editor-Command $opened.surface 'reload'|Out-Null
                Assert-Text $opened.surface ([EditorFixture]::External) $false|Out-Null;Assert-Bytes $path ([EditorFixture]::External)
                Passed 'clean_external_reload_dirty_conflict_compare_keep_mine_then_save_and_explicit_reload'
            }
            'save-as' {
                $name='save-as original.txt';$path=$fixture.Write($name,[EditorFixture]::Original,$false,$false);$opened=Open-Editor $path
                Editor-Command $opened.surface 'replace-text' @('--text',[EditorFixture]::Edited)|Out-Null
                $targetName='saved 한글 한 😀.txt';$target=$fixture.File($targetName)
                Editor-Command $opened.surface 'save-as' @('--path',$targetName)|Out-Null
                Assert-Bytes $path ([EditorFixture]::Original);Assert-Bytes $target ([EditorFixture]::Edited)
                if(-not (Same-Text (Read-Editor $opened.surface).path $targetName)) {throw 'Save As did not update Monaco document path'}
                $existingName='overwrite 한글.txt';$existing=$fixture.Write($existingName,[EditorFixture]::External,$false,$false)
                Editor-Command $opened.surface 'save-as' @('--path',$existingName) 1|Out-Null;Assert-Bytes $existing ([EditorFixture]::External)
                Editor-Command $opened.surface 'save-as' @('--path',$existingName,'--overwrite')|Out-Null;Assert-Bytes $existing ([EditorFixture]::Edited)
                Editor-Command $opened.surface 'replace-text' @('--text',[EditorFixture]::Original)|Out-Null
                $other=$fixture.Write('save-all second.txt',[EditorFixture]::External,$false,$false)
                Open-Editor $other $opened.pane|Out-Null
                Editor-Command $opened.surface 'replace-text' @('--text',[EditorFixture]::Edited)|Out-Null
                Editor-Command $opened.surface 'save-all'|Out-Null
                Assert-Bytes $existing ([EditorFixture]::Original);Assert-Bytes $other ([EditorFixture]::Edited)
                if((Status $opened.surface).dirty) {throw 'Save All left owned documents dirty'}
                Editor-Command $opened.surface 'replace-text' @('--text',[EditorFixture]::External)|Out-Null
                $fixture.LockAgainstReplacement('save-all second.txt')
                try {
                    Editor-Command $opened.surface 'save' @() 1|Out-Null
                    Assert-Bytes $other ([EditorFixture]::Edited)
                    Assert-Text $opened.surface ([EditorFixture]::External) $true|Out-Null
                } finally {$fixture.ReleaseLocks()}
                Editor-Command $opened.surface 'save'|Out-Null;Assert-Bytes $other ([EditorFixture]::External)
                $outside=Join-Path $directory 'outside-root.txt'
                Editor-Command $opened.surface 'save-as' @('--path',$outside) 1|Out-Null
                if(Test-Path -LiteralPath $outside) {throw 'Save As escaped the editor root'}
                Passed 'unicode_save_as_overwrite_save_all_replacement_lock_failure_recovery_and_root_boundary'
            }
            'save-all-many' {
                $batch=@();$batchSurface=$null
                foreach($index in 1..10) {
                    $name=('save-all {0:D2} 한글 한 😀.txt' -f $index)
                    $bom=($index % 3 -eq 0);$crlf=($index % 2 -eq 0)
                    $path=$fixture.Write($name,[EditorFixture]::Original,$bom,$crlf)
                    $opened=Open-Editor $path
                    if($batchSurface -and $opened.surface -ne $batchSurface) {throw 'Ten-document Save All setup did not reuse the same editor'}
                    $batchSurface=$opened.surface
                    $text=('batch '+$index+' '+[EditorFixture]::Unicode+"`nline "+$index+"`n")
                    Editor-Command $batchSurface 'replace-text' @('--text',$text)|Out-Null
                    $batch+=@{path=$path;text=$text;bom=$bom;crlf=$crlf}
                }
                $before=Status $batchSurface
                if(@($before.documents).Count -ne 10 -or @($before.documents|Where-Object {$_.dirty}).Count -ne 10) {throw 'Save All queue regression requires ten simultaneously dirty documents'}
                $recovery=@(Owned-Recovery)
                foreach($entry in $batch) {
                    Assert-Bytes $entry.path ([EditorFixture]::Original) $entry.bom $entry.crlf
                    $records=@($recovery|Where-Object {Same-Text $_.identity $entry.path})
                    if($records.Count -ne 1 -or -not (Same-Text $records[0].content $entry.text)) {throw ('Exact edited recovery content was not persisted under the owned root: '+[IO.Path]::GetFileName($entry.path))}
                }
                # CLI save_all invokes the same serialized helper as the editor's Save All button.
                $saved=Editor-Command $batchSurface 'save-all';Flush $batchSurface;$after=Status $batchSurface
                if($saved.saved -ne 10 -or $after.dirty -or @($after.documents).Count -ne 10 -or @($after.documents|Where-Object {$_.dirty}).Count) {throw ('Ten-document Save All did not finish cleanly: '+($saved|ConvertTo-Json -Depth 5 -Compress))}
                foreach($entry in $batch) {
                    Assert-Bytes $entry.path $entry.text $entry.bom $entry.crlf
                    $metadata=@($after.documents|Where-Object {Same-Text $_.path $entry.path})
                    $encoding=if($entry.bom) {'UTF-8 BOM'} else {'UTF-8'};$eol=if($entry.crlf) {'CRLF'} else {'LF'}
                    if($metadata.Count -ne 1 -or $metadata[0].encoding -ne $encoding -or $metadata[0].eol -ne $eol) {throw 'Ten-document Save All changed document identity or encoding metadata'}
                }
                $evidence.observations+=@{case=$group;surface=$batchSurface;dirtyBefore=10;saved=$saved.saved;documentPaths=@($after.documents|ForEach-Object {$_.path});recoveryFiles=@($recovery|ForEach-Object {$_.file});sharedUiSaveAllPath=$true;workerQueueLimit=8}
                Passed 'shared_ui_save_all_serializes_ten_unicode_documents_with_exact_bytes_and_isolated_recovery'
            }
            'close' {
                $path=$fixture.Write('close 한글.txt',[EditorFixture]::Original,$false,$false);$opened=Open-Editor $path
                Request @('new-workspace','--shell=cmd','--cwd',$fixture.Root)|Out-Null;$other=Request @('identify');$tree=Tree
                $shells+=@($tree.surfaces|Where-Object {$_.pid -and $shells -notcontains $_.pid}|ForEach-Object {$_.pid})
                Request @('workspace','focus',$source.workspace)|Out-Null
                $before=Request @('identify')
                Editor-Command $opened.surface 'replace-text' @('--text',[EditorFixture]::Edited)|Out-Null
                # No extra verifier flush/read between edit and close; the command itself already synchronizes.
                Request @('close-tab',$opened.surface) 1|Out-Null;$after=Request @('identify')
                if($before.workspace -ne $after.workspace -or $before.pane -ne $after.pane -or $before.surface -ne $after.surface) {throw 'Rejected dirty close changed logical focus'}
                Assert-Text $opened.surface ([EditorFixture]::Edited) $true|Out-Null;Assert-Bytes $path ([EditorFixture]::Original)
                $close=Editor-Command $opened.surface 'close-document'
                if($close.closed -ne $false -or $close.confirmation_required -ne $true) {throw 'Dirty document close did not require a decision'}
                Assert-Text $opened.surface ([EditorFixture]::Edited) $true|Out-Null
                Request @('workspace','close',$source.workspace) 1|Out-Null
                if(@((Tree).workspaces|Where-Object {$_.id -eq $source.workspace}).Count -ne 1) {throw 'Dirty workspace close removed the source workspace'}
                Request @('quit') 1|Out-Null
                Request @('quit','--discard-state') 1|Out-Null
                if($process.HasExited) {throw 'Dirty quit terminated owned host'}
                Editor-Command $opened.surface 'save'|Out-Null;Request @('close-tab',$opened.surface)|Out-Null;Forget-Editor $opened.surface
                Assert-Bytes $path ([EditorFixture]::Edited)
                $opened=Open-Editor $path
                Editor-Command $opened.surface 'replace-text' @('--text',[EditorFixture]::Original)|Out-Null
                Editor-Command $opened.surface 'discard-document'|Out-Null
                $empty=Read-Editor $opened.surface
                if($null -ne $empty.content -or $empty.dirty -or @((Status $opened.surface).documents).Count) {throw 'Explicit discard left a document or dirty state'}
                Request @('close-tab',$opened.surface)|Out-Null;Forget-Editor $opened.surface;Assert-Bytes $path ([EditorFixture]::Edited)
                Request @('workspace','close',$other.workspace)|Out-Null
                Passed 'dirty_tab_workspace_and_quit_reject_without_focus_change_explicit_save_or_discard_closes'
            }
            'move' {
                $path=$fixture.Write('move 한글.txt',[EditorFixture]::Original,$false,$false);$opened=Open-Editor $path
                Editor-Command $opened.surface 'replace-text' @('--text',[EditorFixture]::Edited)|Out-Null
                $before=Status $opened.surface;$read=Read-Editor $opened.surface
                Request @('focus-tab',$terminal.id)|Out-Null
                Assert-Text $opened.surface ([EditorFixture]::Edited) $true|Out-Null
                Request @('split','vertical')|Out-Null;$destination=Request @('identify');$tree=Tree
                $shells+=@($tree.surfaces|Where-Object {$_.pid -and $shells -notcontains $_.pid}|ForEach-Object {$_.pid})
                Request @('move-tab',$opened.surface,'--to-pane',$destination.pane)|Out-Null
                $after=Status $opened.surface;$moved=Assert-Text $opened.surface ([EditorFixture]::Edited) $true
                if($before.view_handle -ne $after.view_handle -or $moved.document_id -ne $read.document_id) {throw 'Move recreated editor WebView or document'}
                Editor-Command $opened.surface 'undo'|Out-Null;Flush $opened.surface
                if(-not (Same-Text (Read-Editor $opened.surface).content ([EditorFixture]::Original))) {throw 'Move lost Monaco undo history'}
                Editor-Command $opened.surface 'discard-document'|Out-Null
                Request @('close-tab',$opened.surface)|Out-Null;Forget-Editor $opened.surface
                Request @('close-tab',$destination.surface)|Out-Null
                Passed 'hidden_and_moved_editor_keeps_native_view_document_and_actual_monaco_undo_history'
            }
            'restore' {
                Finish-Host
                $tree=Start-Owned $true;$script:source=Request @('identify');$script:terminal=$tree.surfaces[0]
                $first=$fixture.Write('restore first 한글.txt',[EditorFixture]::Original,$true,$true)
                $second=$fixture.Write('restore second 😀.json',"{`"value`":`"한글`"}`n",$false,$false)
                $opened=Open-Editor $first;Open-Editor $second $opened.pane|Out-Null
                $before=Status $opened.surface;$active=Read-Editor $opened.surface
                $saved=Request @('save-state');$window=$saved.window
                $checkpoint=[IO.File]::ReadAllText($saved.path,[Text.Encoding]::UTF8)|ConvertFrom-Json
                if($checkpoint.screens.($opened.surface) -or $checkpoint.shells.($opened.surface)) {throw 'Editor checkpoint included terminal history or shell metadata'}
                Finish-Host $true
                $tree=Start-Owned $true $window;$script:source=Request @('identify')
                $current=@($tree.surfaces|Where-Object {$_.id -eq $terminal.id})
                if($current.Count -ne 1 -or $current[0].pid -eq $terminal.pid) {throw 'Restored terminal did not retain identity with a fresh process'}
                $script:terminal=$current[0];Ready $opened.surface|Out-Null
                $after=Status $opened.surface;$read=Read-Editor $opened.surface
                if(@($after.documents).Count -ne 2 -or $after.dirty -or -not (Same-Text $read.path $active.path) -or -not (Same-Text $read.content $active.content)) {throw 'Editor document list/active content did not restore'}
                Assert-Bytes $first ([EditorFixture]::Original) $true $true
                $evidence.observations+=@{case=$group;window=$window;surface=$opened.surface;beforeDocuments=$before.documents;afterDocuments=$after.documents;activePath=$read.path}
                Passed 'mixed_checkpoint_restores_editor_files_and_active_document_without_terminal_metadata'
            }
            'restore-errors' {
                Finish-Host
                $tree=Start-Owned $true;$script:source=Request @('identify');$script:terminal=$tree.surfaces[0]
                $missingName='restore missing 한글.txt';$awayName=$missingName+'.held'
                $missing=$fixture.Write($missingName,[EditorFixture]::Original,$false,$false)
                $survivor=$fixture.Write('restore surviving 😀.txt',[EditorFixture]::External,$false,$false)
                $opened=Open-Editor $missing;Open-Editor $survivor $opened.pane|Out-Null
                $saved=Request @('save-state');Finish-Host $true
                $fixture.RenameFile($missingName,$awayName)
                try {
                    Restore-Owned $saved.window|Out-Null;Ready $opened.surface|Out-Null
                    $restored=Status $opened.surface
                    if(-not ((@($restored.restore_errors)-join "`n").Contains($missingName)) -or @($restored.documents).Count -ne 1) {throw 'Missing restored file was not reported separately from the surviving document'}
                    Assert-Text $opened.surface ([EditorFixture]::External) $false|Out-Null
                    $newPath=$fixture.Write('after restore error 한글.txt',[EditorFixture]::Edited,$false,$false)
                    $reused=Open-Editor $newPath $opened.pane
                    $after=Status $opened.surface;$read=Assert-Text $opened.surface ([EditorFixture]::Edited) $false
                    if($reused.surface -ne $opened.surface -or @($after.documents).Count -ne 2 -or -not (Same-Text $read.path ([IO.Path]::GetFileName($newPath)))) {throw 'Historical restore error prevented a valid reused Open or changed editor identity'}
                    $evidence.observations+=@{case=$group;surface=$opened.surface;restoreErrors=$restored.restore_errors;afterOpenErrors=$after.restore_errors;documentPaths=@($after.documents|ForEach-Object {$_.path})}
                    Passed 'missing_restored_file_is_reported_and_later_valid_open_reuses_same_editor_successfully'
                } finally {$fixture.RenameFile($awayName,$missingName)}
            }
            'missing-root' {
                Finish-Host
                $tree=Start-Owned $true;$script:source=Request @('identify');$script:terminal=$tree.surfaces[0]
                $rootName='unavailable editor root 한글';$awayName=$rootName+'.held'
                $path=$fixture.Write(($rootName+'\retained 😀.txt'),[EditorFixture]::Original,$true,$true)
                $editorRoot=$fixture.File($rootName);$opened=Open-Editor $path '' $editorRoot
                $saved=Request @('save-state');$expected=Checkpoint-Editor $saved.path $opened.surface
                $expectedSession=$expected.session
                Finish-Host $true;$fixture.RenameDirectory($rootName,$awayName)
                try {
                    Restore-Owned $saved.window|Out-Null
                    $deadline=(Get-Date).AddSeconds(5)
                    do {
                        $failed=Status $opened.surface
                        if($failed.last_error -and -not $failed.ready) {break}
                        if((Get-Date) -gt $deadline) {throw 'Unavailable editor root did not report a bounded initialization failure'}
                        Start-Sleep -Milliseconds 20
                    } while($true)
                    if($failed.initialization_failed -ne $true -or $failed.dirty -or @($failed.documents).Count) {throw 'Failed editor initialization fabricated live documents or did not report its failure'}
                    Assert-SavedSession $failed.session $expectedSession 'Failed editor status'
                    Editor-Command $opened.surface 'read' @() 1|Out-Null;Assert-Terminal
                    $resaved=Request @('save-state');$retained=Checkpoint-Editor $resaved.path $opened.surface
                    if(-not (Same-Text $retained.workspace_root $editorRoot)) {throw 'Checkpoint of failed editor discarded its original root'}
                    Assert-SavedSession $retained.session $expectedSession 'Failed editor checkpoint'
                    Request @('close-tab',$opened.surface)|Out-Null;Forget-Editor $opened.surface
                    if(@((Tree).editors|Where-Object {$_.id -eq $opened.surface}).Count) {throw 'Pristine failed editor could not close'}
                    Assert-Terminal
                    $evidence.observations+=@{case=$group;surface=$opened.surface;failure=$failed.last_error;restoreErrors=$failed.restore_errors;retainedSession=$retained.session;checkpoint=$resaved.path}
                    Passed 'unavailable_editor_root_keeps_mixed_host_alive_preserves_checkpoint_metadata_and_allows_failed_tab_close'
                } finally {$fixture.RenameDirectory($awayName,$rootName)}
                Assert-Bytes $path ([EditorFixture]::Original) $true $true
            }
            'checkpoint-failure' {
                Finish-Host
                $tree=Start-Owned $true;$script:source=Request @('identify');$script:terminal=$tree.surfaces[0]
                $path=$fixture.Write('checkpoint failure 한글.txt',[EditorFixture]::Original,$false,$false);$opened=Open-Editor $path
                $saved=Request @('save-state');$hash=(Get-FileHash -LiteralPath $saved.path -Algorithm SHA256).Hash
                $fixture.LockStateAgainstReplacement($saved.path)
                try {
                    $failure=Request @('quit') 1
                    if($process.HasExited -or (Get-FileHash -LiteralPath $saved.path -Algorithm SHA256).Hash -ne $hash) {throw 'Failed checkpoint close terminated host or changed the previous checkpoint'}
                    $read=Read-Editor $opened.surface
                    if($read.sealed -ne $false) {throw 'Failed checkpoint close left the editor sealed'}
                    # A successful real Monaco mutation proves recovery beyond the reported flag.
                    Editor-Command $opened.surface 'replace-text' @('--text',[EditorFixture]::Edited)|Out-Null
                    Assert-Text $opened.surface ([EditorFixture]::Edited) $true|Out-Null
                    Editor-Command $opened.surface 'undo'|Out-Null;Editor-Command $opened.surface 'save'|Out-Null
                    Assert-Text $opened.surface ([EditorFixture]::Original) $false|Out-Null;Assert-Bytes $path ([EditorFixture]::Original)
                    $evidence.observations+=@{case=$group;quitFailure=$failure;sealedAfterFailure=$read.sealed;previousCheckpointHash=$hash;inFlightOverlapEstablished=$false}
                } finally {$fixture.ReleaseLocks()}
                Request @('save-state')|Out-Null;Finish-Host $true
                Restore-Owned $saved.window|Out-Null;Ready $opened.surface|Out-Null
                Assert-Text $opened.surface ([EditorFixture]::Original) $false|Out-Null
                Passed 'failed_checkpoint_replace_preserves_prior_state_unseals_monaco_and_allows_later_clean_quit'
            }
            {$_ -eq 'late-quit' -or $_ -eq 'late-quit-empty'} {
                $path=$null;$opened=$null
                if($group -eq 'late-quit') {
                    $path=$fixture.Write('late quit clean 한글.txt',[EditorFixture]::Original,$false,$false);$opened=Open-Editor $path
                    Assert-Text $opened.surface ([EditorFixture]::Original) $false|Out-Null
                }
                $tree=Tree;$identity=Request @('identify')
                if($identity.pid -ne $process.Id) {throw 'Refusing UI suspension without exact owned-host identity'}
                if($group -eq 'late-quit-empty' -and @($tree.editors).Count) {throw 'Direct late quit requires a terminal-only model'}
                $closedPid=$process.Id;$pause=$null;$deadlineWatch=[Diagnostics.Stopwatch]::StartNew()
                $observation=[ordered]@{case=$group;kind='late-quit-deadline';pid=$closedPid;surface=$opened.surface;editorCount=@($tree.editors).Count;windowHandle=$tree.window_handle;clientBudgetMs=20000;wholeProcessSuspended=$false;uiThreadSuspended=$false;uiThreadResumed=$false}
                $evidence.observations+=$observation
                try {
                    $pause=New-Object EditorUiThreadPause($process,([long]$tree.window_handle))
                    $observation.threadId=$pause.ThreadId;$observation.verifiedThreadPid=$pause.ProcessId;$observation.previousSuspendCount=$pause.PreviousSuspendCount;$observation.uiThreadSuspended=$true
                    # Deliberately exceed only the server's real 15s command deadline.
                    # Ordinary IPC elsewhere retains its five-second verifier budget.
                    $response=End-Command (Begin-Command @('quit','--discard-state')) @(1) 20000
                    $observation.response=$response;$observation.responseElapsedMs=$deadlineWatch.ElapsedMilliseconds
                    if(-not $response.error -or -not $response.error.Contains('window did not answer within the IPC command deadline')) {throw 'Late quit did not observe the actual server command-deadline response'}
                    if($process.HasExited) {throw 'Host unexpectedly exited before its queued clean editor quit could run'}
                } finally {
                    if($pause) {
                        try {$pause.Dispose()} finally {
                            $observation.uiThreadResumed=$pause.Resumed;$observation.previousResumeCount=$pause.PreviousResumeCount;$observation.suspendElapsedMs=$deadlineWatch.ElapsedMilliseconds
                        }
                    }
                }
                if(-not $observation.uiThreadResumed -or $observation.previousResumeCount -ne 1) {throw 'Owned UI thread suspension was not balanced exactly once'}
                if(-not $process.WaitForExit(5000)) {throw 'Accepted clean editor quit left the host alive after its IPC reply receiver expired'}
                if($path) {Assert-Bytes $path ([EditorFixture]::Original)}
                Finish-Host;if($opened) {Forget-Editor $opened.surface}
                $observation.cleanHostExit=$true
                if($group -eq 'late-quit') {Passed 'late_clean_editor_quit_exits_after_real_ipc_deadline_and_verified_owned_ui_thread_resume'}
                else {Passed 'late_direct_terminal_only_quit_exits_after_real_ipc_deadline_and_verified_owned_ui_thread_resume'}
                # Keep following independently runnable groups on a fresh owned host.
                $tree=Start-Owned;$script:source=Request @('identify');$script:terminal=$tree.surfaces[0]
            }
            'recovery' {
                Finish-Host
                $tree=Start-Owned $true;$script:source=Request @('identify');$script:terminal=$tree.surfaces[0]
                $path=$fixture.Write('recover acknowledged 한글 한 😀.txt',[EditorFixture]::Original,$true,$true)
                $opened=Open-Editor $path
                Editor-Command $opened.surface 'replace-text' @('--text',[EditorFixture]::Edited)|Out-Null
                $dirty=Assert-Text $opened.surface ([EditorFixture]::Edited) $true
                Assert-Bytes $path ([EditorFixture]::Original) $true $true
                $records=@(Owned-Recovery|Where-Object {Same-Text $_.identity $path})
                if($records.Count -ne 1 -or -not (Same-Text $records[0].content ([EditorFixture]::Edited))) {throw 'Acknowledged dirty content was not persisted to the owned recovery root'}
                $saved=Request @('save-state');$checkpoint=Checkpoint-Editor $saved.path $opened.surface
                if(-not (Same-Text $checkpoint.session.active_file $path)) {throw 'Dirty editor checkpoint did not retain its active path'}
                if((Request @('identify')).pid -ne $process.Id) {throw 'Refusing forced termination without exact owned-host identity'}
                Tree|Out-Null
                $crashedPid=$process.Id
                $evidence.observations+=@{case=$group;kind='crash-boundary';pid=$crashedPid;window=$saved.window;surface=$opened.surface;document=$dirty.document_id;checkpoint=$saved.path;recoveryFile=$records[0].file;editAcknowledged=$true;checkpointCompleted=$true;diskStillOriginal=$true;powerLossTest=$false;unsynchronizedEditTest=$false}
                # The Process object belongs to this verifier; no other PID or desktop process is stopped.
                $script:hostForced=$true;$process.Kill();[CliProbe]::WaitAfterKill($process);Record-HostExit
                $process.Dispose();$script:process=$null;$script:pipeName=$null
                Restore-Owned $saved.window|Out-Null;Ready $opened.surface|Out-Null
                $deadline=(Get-Date).AddSeconds(5)
                do {
                    $proposal=Read-Editor $opened.surface
                    if($proposal.recovery_available) {break}
                    if((Get-Date) -gt $deadline) {throw 'Restored editor did not offer acknowledged dirty recovery'}
                    Start-Sleep -Milliseconds 20
                } while($true)
                if($proposal.dirty -or -not (Same-Text $proposal.content ([EditorFixture]::Original))) {throw 'Recovery proposal changed the model before an explicit recovery decision'}
                Assert-Bytes $path ([EditorFixture]::Original) $true $true
                Editor-Command $opened.surface 'recover'|Out-Null
                $recovered=Assert-Text $opened.surface ([EditorFixture]::Edited) $true
                if($recovered.document_id -ne $proposal.document_id -or $recovered.recovery_available) {throw 'Actual Monaco recovery changed the document identity or left its proposal open'}
                Assert-Bytes $path ([EditorFixture]::Original) $true $true
                Editor-Command $opened.surface 'save'|Out-Null
                $clean=Assert-Text $opened.surface ([EditorFixture]::Edited) $false
                Assert-Bytes $path ([EditorFixture]::Edited) $true $true
                if(@(Owned-Recovery|Where-Object {Same-Text $_.identity $path}).Count) {throw 'Successful recovered Save left stale recovery content'}
                Assert-Terminal;Tree|Out-Null
                $evidence.observations+=@{case=$group;kind='recovery-result';crashedPid=$crashedPid;restoredPid=$process.Id;window=$saved.window;surface=$opened.surface;proposalObserved=$proposal.recovery_available;document=$clean.document_id;encoding=$clean.encoding;eol=$clean.eol;diskChangedOnlyAfterSave=$true}
                Finish-Host $true
                Passed 'acknowledged_dirty_recovery_survives_owned_host_termination_and_saves_only_after_explicit_monaco_recover'
            }
        }
        if($process) {Assert-Terminal;Tree|Out-Null}
    }
    $evidence.status='passed_background_editor_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
    $fixture.ReleaseLocks()
    foreach($job in $clients) {
        if(-not $job.completed) {try {if(-not $job.process.HasExited) {$job.process.Kill();[CliProbe]::WaitAfterKill($job.process)};$job.process.Dispose();$job.completed=$true} catch {$cleanupErrors+=$_.Exception.Message}}
    }
    if($process -and -not $process.HasExited -and $pipeName) {
        try {Clean-Editors;Finish-Host} catch {$cleanupErrors+=$_.Exception.Message}
    }
    if($process) {
        try {if(-not $process.HasExited) {$script:hostForced=$true;$process.Kill();[CliProbe]::WaitAfterKill($process)}} catch {$cleanupErrors+=$_.Exception.Message} finally {
            try {Record-HostExit} catch {$cleanupErrors+=('Host exit capture: '+$_.Exception.Message)}
            $process.Dispose()
        }
    }
    $fixture.Dispose()
    if($cleanupErrors.Count) {$evidence.status='failed';$evidence.cleanupErrors=$cleanupErrors}
    $evidence.hosts=$hosts;$evidence.shells=$shells;$evidence.clientPids=@($clients|ForEach-Object {$_.pid});$evidence.finished=(Get-Date).ToString('o')
    $evidence|ConvertTo-Json -Depth 16|Set-Content -Encoding UTF8 (Join-Path $directory 'native-editor-background.json')
    Write-Output ('Evidence: '+$directory)
}
if($cleanupErrors.Count) {throw ($cleanupErrors -join '; ')}
$evidence|ConvertTo-Json -Depth 16
