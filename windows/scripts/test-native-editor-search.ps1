# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden, isolated native editor search verification; outer run-check.ps1 limit120s.
param(
    [string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",
    [ValidateSet('all','startup','find','replace','quick-open','workspace','search-open','late-search-open')][string]$Case='all'
)
if (-not $env:FLOWMUX_TEST_ARTIFACT_ROOT) { throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.' }
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path
$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'EditorFixture.cs')
$directory=Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT ('editor-search-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$fixture=New-Object EditorFixture($directory)
$process=$null;$pipeName=$null;$stdout=$null;$stderr=$null;$hostExitRecorded=$false;$hostForced=$false
$hosts=@();$shells=@();$clients=@();$ownedEditors=@();$cleanupErrors=@();$storageObserved=@()
$caseRoot=$fixture.Root;$suiteClock=[Diagnostics.Stopwatch]::StartNew();$cleaning=$false
$evidence=[ordered]@{started=(Get-Date).ToString('o');mode='background';case=$Case;checks=@();observations=@();clipboardAccess=$false;desktopInput=$false;externalSites=$false;realImeTest=$false;unicodeComparison='ordinal';deferred=@(
    'Physical keyboard/mouse/IME, glyph rendering, native dialogs, clipboard, DPI and accessibility are not exercised.',
    'Monaco ranges are one-based UTF-16; workspace search ranges are zero-based UTF-16. NFD strings are checked ordinally without asserting Unicode normalization equivalence.',
    'In-flight cancellation, supersession, blocked filesystem deadlines and queued close/move races are not forced by these native cases; idle cancellation only verifies the idle command contract.',
    'Guarded stale results are changed before opening; a concurrent filesystem swap between validation and the shared document read is covered separately by deterministic worker tests.',
    'No network/UNC/reparse paths, hostile ACLs, directory floods or exhaustive scanner budget limits are exercised.'
)}

function Same-Text([string]$Left,[string]$Right) {return [string]::Equals($Left,$Right,[StringComparison]::Ordinal)}
function Passed([string]$Name) {$script:evidence.checks+=@{name=$Name;passed=$true};Write-Host ('[check] passed '+$Name)}
function Begin-Probe([string]$File,[string[]]$Arguments) {
    if(-not $script:cleaning -and $suiteClock.ElapsedMilliseconds -ge 110000) {throw 'Search verifier exhausted its110s inner budget'}
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

function Unwrap($Value) {
    if($Value -and $Value.psobject.Properties.Name -contains 'result') {return $Value.result}
    return $Value
}
function Require([bool]$Condition,[string]$Message) {if(-not $Condition) {throw $Message}}
function Assert-Range($Range,[int]$Line,[int]$Column,[int]$EndColumn) {
    Require ($null -ne $Range -and $Range.start_line -eq $Line -and $Range.end_line -eq $Line -and $Range.start_column -eq $Column -and $Range.end_column -eq $EndColumn) ('Wrong Monaco UTF16 range: '+($Range|ConvertTo-Json -Compress))
}
function Find-Text([string]$Surface,[string]$Query,[string[]]$Options=@()) {
    $r=Unwrap (Request (@('editor','find',$Surface,'--query',$Query)+$Options))
    Require (Same-Text $r.query $Query) 'Find did not return the ordinal query'
    Require ($r.count -eq @($r.matches).Count -and $r.truncated -eq $false) 'Small find fixture has inconsistent match count'
    return $r
}
function Search([string]$Surface,[string]$Query,[string[]]$Options=@()) {
    $r=Unwrap (Request (@('editor','search',$Surface,'--query',$Query)+$Options))
    Require ($r.outcome.kind -eq 'workspace' -and $r.token -and $r.request_id) ('Wrong workspace result envelope: '+($r|ConvertTo-Json -Depth 8 -Compress))
    [guid]::Parse($r.token)|Out-Null
    Require (-not $r.outcome.result.cancelled -and -not $r.outcome.result.truncated) 'Small workspace search was cancelled or truncated'
    $script:evidence.observations+=@{kind='search';case=$group;query=$Query;response=$r}
    return $r
}
function Match-Index($Result,[string]$Path) {
    $matches=@($Result.outcome.result.matches)
    for($i=0;$i -lt $matches.Count;$i++) {if(Same-Text $matches[$i].path $Path) {return $i}}
    throw ('Search did not return owned path '+$Path)
}
function Open-Result([string]$Surface,$Result,[int]$Index,[int]$Exit=0) {
    return Request @('editor','search-open',$Surface,'--token',$Result.token,'--index',$Index.ToString()) $Exit
}
function Expect-Error($Result,[string]$Pattern) {
    Require ($Result.error -and [string]$Result.error -match $Pattern) ('Unexpected rejection: '+($Result|ConvertTo-Json -Depth 8 -Compress))
}
function Assert-OwnedWindows {
    Tree|Out-Null
    Require (@([EditorWindowProbe]::VisibleTopLevelWindows($process)).Count -eq 0) 'Owned host has a visible top-level window'
}
function Fixture-Text([string]$Name,[string]$Text,[bool]$Bom=$false,[bool]$CrLf=$false) {
    return $fixture.Write(($group+'/'+$Name),$Text,$Bom,$CrLf)
}

try {
    $doctor=End-Command (Begin-Probe $cli @('doctor'))
    Require ($doctor.background_testing -and $doctor.status -eq 'ok') 'Working hidden debug build required; no host launched'
    $tree=Start-Owned;$script:source=Request @('identify');$script:terminal=$tree.surfaces[0]
    Assert-OwnedWindows
    if($Case -eq 'startup') {Passed 'hidden_owned_editor_search_host_startup'}
    foreach($group in @('find','replace','quick-open','workspace','search-open','late-search-open')) {
        if($Case -ne 'all' -and $Case -ne $group) {continue}
        Clean-Editors;Request @('focus-tab',$terminal.id)|Out-Null
        $script:caseRoot=$fixture.File($group);[IO.Directory]::CreateDirectory($caseRoot)|Out-Null
        switch($group) {
            'find' {
                $text="😀 한글 é`n한글 한 😀`n"
                $path=Fixture-Text 'literal 한글.txt' $text
                $opened=Open-Editor $path;$surface=$opened.surface;$before=Read-Editor $surface
                $found=Find-Text $surface '한글'
                Require ($found.count -eq 2 -and $found.document_id -eq $before.document_id -and $found.version -eq $before.active_version) 'Literal Korean find changed identity/version or lost matches'
                Assert-Range $found.matches[0] 1 4 6;Assert-Range $found.matches[1] 2 1 3
                Require ($null -ne $found.selected) 'Korean find did not select a match'
                $emoji=Find-Text $surface '😀'
                Require ($emoji.count -eq 2) 'Literal emoji find did not find both rendered matches'
                Assert-Range $emoji.matches[0] 1 1 3;Assert-Range $emoji.matches[1] 2 8 10
                $nfd=Find-Text $surface '한';Require ($nfd.count -eq 1) 'Ordinal NFD query did not match its exact fixture'
                Assert-Range $nfd.matches[0] 2 4 7
                $none=Find-Text $surface '.+';Require ($none.count -eq 0 -and $null -eq $none.selected) 'Literal query was interpreted as a regex'
                $backward=Find-Text $surface '한글' @('--backward');Require ($backward.backward -and $null -ne $backward.selected) 'Backward literal find omitted its selection'
                $after=Assert-Text $surface $text $false
                Require ($after.document_id -eq $before.document_id -and $after.active_version -eq $before.active_version) 'Find mutated actual Monaco document identity/version'
                Assert-Bytes $path $text
                $evidence.observations+=@{case=$group;korean=$found;emoji=$emoji;nfd=$nfd;backward=$backward}
                Passed 'literal_korean_emoji_nfd_find_reports_actual_monaco_utf16_ranges_without_editing'
            }
            'replace' {
                $text="😀 한글 é`n한글 한`n";$replacement='바꿈 $1 😀'
                $path=Fixture-Text 'replace 한글.txt' $text $true $true
                $opened=Open-Editor $path;$surface=$opened.surface;$before=Read-Editor $surface
                $found=Find-Text $surface '한글';Assert-Range $found.selected.range 1 4 6
                $first=Unwrap (Request @('editor','replace-match',$surface,'--query','한글','--text',$replacement,'--document-id',$before.document_id,'--version',$before.active_version.ToString()))
                Require ($first.replaced -eq 1 -and $first.count -eq 1 -and $first.version -gt $before.active_version) 'Replace Match did not acknowledge exactly one real Monaco edit'
                $index=$text.IndexOf('한글',[StringComparison]::Ordinal)
                $once=$text.Substring(0,$index)+$replacement+$text.Substring($index+2)
                $read=Assert-Text $surface $once $true;Assert-Bytes $path $text $true $true
                $stale=Request @('editor','replace-all',$surface,'--query','한글','--text','must not appear','--document-id',$before.document_id,'--version',$before.active_version.ToString()) 1
                Expect-Error $stale 'stale|version'
                $unchanged=Assert-Text $surface $once $true
                Require ($unchanged.active_version -eq $read.active_version) 'Rejected stale replacement changed Monaco version'
                $all=Unwrap (Request @('editor','replace-all',$surface,'--query','한글','--text',$replacement,'--document-id',$read.document_id,'--version',$read.active_version.ToString()))
                Require ($all.replaced -eq 1 -and $all.count -eq 0) 'Replace All did not replace the one remaining literal match'
                $expected=$text.Replace('한글',$replacement);Assert-Text $surface $expected $true|Out-Null
                Editor-Command $surface 'undo'|Out-Null;Assert-Text $surface $once $true|Out-Null
                Editor-Command $surface 'redo'|Out-Null;Assert-Text $surface $expected $true|Out-Null
                Editor-Command $surface 'save'|Out-Null;Assert-Text $surface $expected $false|Out-Null;Assert-Bytes $path $expected $true $true
                $evidence.observations+=@{case=$group;first=$first;stale=$stale;all=$all;replacement=$replacement}
                Passed 'literal_replacement_uses_version_guard_monaco_undo_and_exact_bom_crlf_save'
            }
            'quick-open' {
                $base=Fixture-Text 'base.txt' "base`n"
                $name='nested/선택 한 😀.txt';$targetText="quick 선택 😀`n"
                Fixture-Text $name $targetText|Out-Null
                Fixture-Text '.gitignore' "ignored-git.txt`n"|Out-Null
                Fixture-Text '.ignore' "ignored-ignore.txt`n"|Out-Null
                foreach($nameIgnored in @('ignored-git.txt','ignored-ignore.txt','.hidden.txt','node_modules/generated.txt','target/generated.txt')) {Fixture-Text $nameIgnored 'excluded'|Out-Null}
                $opened=Open-Editor $base;$surface=$opened.surface;$initial=Read-Editor $surface
                $result=Unwrap (Request @('editor','quick-open',$surface))
                Require ($result.outcome.kind -eq 'quick_open' -and $result.token -and -not $result.outcome.truncated) 'Quick Open omitted its retained bounded index'
                [guid]::Parse($result.token)|Out-Null;$paths=@($result.outcome.paths)
                Require ($paths.Count -eq 2 -and (Same-Text $paths[0] 'base.txt') -and (Same-Text $paths[1] $name)) ('Quick Open ignore/Unicode index differs: '+($paths|ConvertTo-Json -Compress))
                $bad=Request @('editor','search-open',$surface,'--token',([guid]::NewGuid().ToString()),'--index','0') 1;Expect-Error $bad 'expired|token'
                $bad=Open-Result $surface $result 2 1;Expect-Error $bad 'index|range'
                Require ((Read-Editor $surface).document_id -eq $initial.document_id) 'Rejected result token/index changed active document'
                Open-Result $surface $result 1|Out-Null
                $read=Assert-Text $surface $targetText $false
                Require (Same-Text ($read.path.Replace('\','/')) $name) 'Quick Open selected the wrong actual Unicode document'
                $evidence.observations+=@{case=$group;response=$result;openedPath=$read.path;document=$read.document_id}
                Passed 'quick_open_indexes_unicode_paths_respects_ignore_rules_and_opens_only_retained_entries'
            }
            'workspace' {
                $disk="TOKEN_DISK 😀`n";$dirty="😀 TOKEN_DIRTY 한글`n"
                $base=Fixture-Text 'dirty 한글.txt' $disk
                Fixture-Text 'keep/visible.txt' "😀 TOKEN_KEEP 한글`n" $true $true|Out-Null
                Fixture-Text 'keep/excluded.txt' "TOKEN_EXCLUDED`n"|Out-Null
                Fixture-Text 'ignored.txt' "TOKEN_IGNORED`n"|Out-Null
                Fixture-Text '.gitignore' "ignored.txt`n"|Out-Null
                Fixture-Text '.hidden.txt' "TOKEN_HIDDEN`n"|Out-Null
                $opened=Open-Editor $base;$surface=$opened.surface
                Editor-Command $surface 'replace-text' @('--text',$dirty)|Out-Null
                $before=Assert-Text $surface $dirty $true
                $result=Search $surface 'TOKEN'
                $matches=@($result.outcome.result.matches)
                Require ($matches.Count -eq 3) ('Workspace dirty-buffer/ignore count differs: '+($matches|ConvertTo-Json -Depth 6 -Compress))
                $i=Match-Index $result 'dirty 한글.txt';$match=$matches[$i]
                Require ($match.line -eq 0 -and $match.column -eq 3 -and $match.length -eq 5 -and $match.preview.Contains('TOKEN_DIRTY') -and -not $match.preview.Contains('TOKEN_DISK')) 'Search did not use acknowledged dirty Monaco text with zero-based UTF16 columns'
                $missing=Search $surface 'TOKEN_DISK';Require (@($missing.outcome.result.matches).Count -eq 0) 'Stale disk content leaked through an open-buffer override'
                $filtered=Search $surface 'TOKEN_[A-Z]+' @('--regex','--include','keep/*.txt','--exclude','**/excluded.txt','--case-sensitive')
                $filteredMatches=@($filtered.outcome.result.matches)
                Require ($filteredMatches.Count -eq 1 -and (Same-Text $filteredMatches[0].path 'keep/visible.txt') -and $filteredMatches[0].column -eq 3 -and $filteredMatches[0].length -eq 10) 'Regex/include/exclude search did not return the exact CRLF/BOM fixture match'
                $bad=Request @('editor','search',$surface,'--query','[','--regex') 1;Expect-Error $bad 'regex|pattern'
                $literal=Search $surface '[';Require (@($literal.outcome.result.matches).Count -eq 0) 'Literal bracket search failed after regex rejection'
                $cancel=Request @('editor','search-cancel',$surface);Require ($cancel.ok -eq $true) 'Idle cancellation did not acknowledge its no-op contract'
                $after=Assert-Text $surface $dirty $true
                Require ($after.document_id -eq $before.document_id -and $after.active_version -eq $before.active_version) 'Workspace search changed actual Monaco content/version'
                Assert-Bytes $base $disk
                $evidence.observations+=@{case=$group;idleCancel=$cancel;inFlightCancellationForced=$false}
                Passed 'workspace_search_uses_dirty_buffers_utf16_ranges_ignore_globs_and_regex_without_disk_mutation'
            }
            'late-search-open' {
                $baseText="BASE 😀 untouched`n";$targetText="😀 GOLD 한글`n"
                $base=Fixture-Text 'base.txt' $baseText
                $targetName='late 선택 한 😀.txt';$target=Fixture-Text $targetName $targetText
                $opened=Open-Editor $base;$surface=$opened.surface
                Find-Text $surface 'BASE'|Out-Null
                $retained=Search $surface 'GOLD';$index=Match-Index $retained $targetName
                $before=Read-Editor $surface;Assert-Range $before.selection 1 1 5
                $treeBefore=Tree;$identity=Request @('identify');$beforeStatus=Status $surface
                Require ($identity.pid -eq $process.Id -and $beforeStatus.ready -and -not $beforeStatus.pending -and $before.open_documents -eq 1) 'Late search-open requires the exact idle owned host and one actual model'
                $pause=$null;$deadlineWatch=[Diagnostics.Stopwatch]::StartNew()
                $observation=[ordered]@{case=$group;kind='late-search-open-deadline';pid=$process.Id;surface=$surface;token=$retained.token;index=$index;windowHandle=$treeBefore.window_handle;clientBudgetMs=20000;wholeProcessSuspended=$false;uiThreadSuspended=$false;uiThreadResumed=$false;selectionBefore=$before.selection;documentBefore=$before.document_id}
                $evidence.observations+=$observation
                try {
                    # EditorUiThreadPause is the existing helper compiled above
                    # from EditorFixture.cs: exact owned Process/HWND/PID/class,
                    # hidden/nonforeground checks, previous suspend count zero.
                    $pause=New-Object EditorUiThreadPause($process,([long]$treeBefore.window_handle))
                    $observation.threadId=$pause.ThreadId;$observation.verifiedThreadPid=$pause.ProcessId;$observation.previousSuspendCount=$pause.PreviousSuspendCount;$observation.uiThreadSuspended=$true
                    # The owned host's IPC threads remain alive. Await the real
                    # 15-second server response; there is no timed sleep/retry.
                    $response=End-Command (Begin-Command @('editor','search-open',$surface,'--token',$retained.token,'--index',$index.ToString())) @(1) 20000
                    $observation.response=$response;$observation.responseElapsedMs=$deadlineWatch.ElapsedMilliseconds
                    Require ($response.error -and $response.error.Contains('window did not answer within the IPC command deadline')) 'Late search-open did not reach the actual server command deadline'
                    Require (-not $process.HasExited) 'Owned host exited before its expired request could be processed'
                } finally {
                    if($pause) {
                        try {$pause.Dispose()} finally {$observation.uiThreadResumed=$pause.Resumed;$observation.previousResumeCount=$pause.PreviousResumeCount;$observation.suspendElapsedMs=$deadlineWatch.ElapsedMilliseconds}
                    }
                }
                Require ($observation.uiThreadResumed -and $observation.previousResumeCount -eq 1) 'Late search-open UI suspension was not balanced exactly once'
                $afterStatus=Status $surface
                Require ($afterStatus.ready -and -not $afterStatus.pending -and @($afterStatus.documents).Count -eq 1 -and $afterStatus.active_document_id -eq $before.document_id) 'Expired UI-queued result open started new work, opened a model or left the editor sealed'
                $after=Read-Editor $surface
                Require ((Same-Text $after.content $baseText) -and $after.document_id -eq $before.document_id -and $after.active_version -eq $before.active_version -and $after.open_documents -eq 1 -and -not $after.sealed -and -not $after.replacement_pending -and -not $after.quarantined) 'Expired result open changed or sealed the actual Monaco model'
                Assert-Range $after.selection 1 1 5
                $treeAfter=Tree
                Require ((Same-Text ($treeBefore.workspaces|ConvertTo-Json -Depth 60 -Compress) ($treeAfter.workspaces|ConvertTo-Json -Depth 60 -Compress))) 'Expired result open changed the workspace/pane layout'
                Assert-Terminal;Assert-Bytes $base $baseText;Assert-Bytes $target $targetText
                $observation.documentAfter=$after.document_id;$observation.selectionAfter=$after.selection;$observation.modelsAfter=$after.open_documents;$observation.pendingAfter=$afterStatus.pending;$observation.sealedAfter=$after.sealed
                $fresh=Search $surface 'GOLD';$freshIndex=Match-Index $fresh $targetName
                Require ($fresh.token -ne $retained.token) 'Fresh search did not replace the expired request token'
                Open-Result $surface $fresh $freshIndex|Out-Null
                $selected=Assert-Text $surface $targetText $false
                Require (Same-Text $selected.path $targetName) 'Fresh following result open selected the wrong file'
                Assert-Range $selected.selection 1 4 8
                $observation.followingOpenSucceeded=$true;$observation.followingDocument=$selected.document_id;$observation.followingSelection=$selected.selection
                Passed 'expired_ui_queued_search_open_preserves_model_selection_and_guard_then_fresh_open_succeeds'
            }
            'search-open' {
                $base=Fixture-Text 'base.txt' "active base`n"
                $targetName='nested 한글/선택 한 😀.txt';$original="😀 GOLD 한글`n"
                $target=Fixture-Text $targetName $original $true $true
                $opened=Open-Editor $base;$surface=$opened.surface;$before=Read-Editor $surface
                $staleDisk=Search $surface 'GOLD';$index=Match-Index $staleDisk $targetName
                Fixture-Text $targetName "😀 CHANGED 한글`n" $true $true|Out-Null
                $rejected=Open-Result $surface $staleDisk $index 1;Expect-Error $rejected 'changed|stale|expired'
                $after=Read-Editor $surface
                Require ($after.document_id -eq $before.document_id -and $after.open_documents -eq 1) 'Rejected stale disk result changed the active model or opened a stale result'
                Fixture-Text $targetName $original $true $true|Out-Null
                $current=Search $surface 'GOLD';$index=Match-Index $current $targetName
                $match=@($current.outcome.result.matches)[$index]
                Require ($match.line -eq 0 -and $match.column -eq 3 -and $match.length -eq 4) 'Search result fixture has unexpected zero-based UTF16 range'
                Open-Result $surface $current $index|Out-Null
                $read=Assert-Text $surface $original $false
                Require (Same-Text ($read.path.Replace('\','/')) $targetName) 'Guarded search open activated the wrong nested Unicode path'
                Assert-Range $read.selection 1 4 8
                $staleBuffer=Search $surface 'GOLD';$bufferIndex=Match-Index $staleBuffer $targetName
                $dirty="GOLD edited 😀`n";Editor-Command $surface 'replace-text' @('--text',$dirty)|Out-Null
                $edited=Assert-Text $surface $dirty $true
                $rejected=Open-Result $surface $staleBuffer $bufferIndex 1;Expect-Error $rejected 'stale|version|expired'
                $retained=Assert-Text $surface $dirty $true
                Require ($retained.document_id -eq $edited.document_id -and $retained.active_version -eq $edited.active_version) 'Rejected stale buffer result changed its actual Monaco state'
                $fresh=Search $surface 'GOLD';$freshIndex=Match-Index $fresh $targetName
                Open-Result $surface $fresh $freshIndex|Out-Null
                $selected=Read-Editor $surface;Assert-Range $selected.selection 1 1 5
                Assert-Bytes $target $original $true $true
                $next=Unwrap (Request @('editor','quick-open',$surface))
                $expired=Open-Result $surface $fresh $freshIndex 1;Expect-Error $expired 'expired|token'
                Require ($next.token -ne $fresh.token) 'New search reused the old retained token'
                $evidence.observations+=@{case=$group;staleDisk=$staleDisk.token;opened=$read.path;openedSelection=$read.selection;staleBuffer=$staleBuffer.token;rejection=$rejected;freshSelection=$selected.selection;expired=$expired}
                Passed 'guarded_search_open_selects_actual_utf16_range_and_rejects_changed_disk_buffer_and_expired_tokens'
            }
        }
        Assert-Terminal;Assert-OwnedWindows
    }
    Clean-Editors;Finish-Host
    $evidence.status='passed_background_editor_search_subset'
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
    if ($evidence.status -eq 'failed') {
        $evidence|ConvertTo-Json -Depth 18|Set-Content -Encoding UTF8 (Join-Path $directory 'native-editor-search-background.json')
        Write-Output ('Failure diagnostics: '+$directory)
    }
}
if($cleanupErrors.Count) {throw ($cleanupErrors -join '; ')}
Write-Host ('[check] editor search: '+$evidence.checks.Count+' groups passed')
