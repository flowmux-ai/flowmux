# SPDX-License-Identifier: GPL-3.0-or-later
# Bounded hidden native Monaco verification. Run through run-check.ps1 (120s).
param(
    [string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",
    [ValidateSet('all','startup','open','edit','encoding','encoding-defaults','conflict','save-as','save-all-many','close','move','restore','restore-errors','missing-root','checkpoint-failure','late-quit','recovery')][string]$Case='all'
)
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path
$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'EditorFixture.cs')
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
function Request([string[]]$Arguments,[int]$Exit=0) {return End-Command (Begin-Command $Arguments) @($Exit)}
function Check-Hidden([long]$Handle) {
    if($Handle -and ([CliProbe]::IsWindowVisible([IntPtr]$Handle) -or [CliProbe]::GetForegroundWindow() -eq [IntPtr]$Handle)) {throw 'Owned window became visible or foreground'}
}
function Tree {
    $tree=Request @('tree')
    if(-not $tree.background_testing) {throw 'Host is not in hidden debug mode'}
    Check-Hidden ([long]$tree.window_handle)
    foreach($view in @($tree.browsers)+@($tree.editors)) {
        if($view) {foreach($handle in @($view.view_handle,$view.chrome_handle)) {if($handle) {Check-Hidden ([long]$handle)}}}
    }
    return $tree
}
function Start-Owned([bool]$Persistent=$false,[string]$Restore='') {
    $arguments=@('--temporary','--shell=cmd','--cwd',$fixture.Root)
    if($Persistent) {$arguments=@('--new-window','--shell=cmd','--cwd',$fixture.Root)}
    if($Restore) {$arguments=@('--restore-window',$Restore)}
    $utc=[DateTime]::UtcNow
    $script:process=[CliProbe]::Start($gui,$arguments,$directory,$directory)
    $script:hostExitRecorded=$false;$script:hostForced=$false
    $script:hosts+=$process.Id;$script:stdout=$process.StandardOutput.ReadToEndAsync();$script:stderr=$process.StandardError.ReadToEndAsync();$script:pipeName=$null
    $file=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($process.Id).json";$deadline=(Get-Date).AddSeconds(8)
    do {
        if($process.HasExited -or (Get-Date) -gt $deadline) {throw ('Owned startup failed: '+[CliProbe]::Output($stderr))}
        if((Test-Path -LiteralPath $file) -and (Get-Item -LiteralPath $file).LastWriteTimeUtc -ge $utc) {
            $record=Get-Content -Raw -LiteralPath $file|ConvertFrom-Json
            if($record.pid -ne $process.Id) {throw 'Wrong discovery owner'}
            $script:pipeName=$record.pipe;break
        }
        Start-Sleep -Milliseconds 20
    } while($true)
    if((Request @('identify')).pid -ne $process.Id) {throw 'Wrong pipe owner'}
    do {
        $tree=Tree
        if(@($tree.surfaces|Where-Object {-not $_.ready}).Count -eq 0) {
            $script:shells+=@($tree.surfaces|Where-Object {$_.pid -and $script:shells -notcontains $_.pid}|ForEach-Object {$_.pid})
            return $tree
        }
        if((Get-Date) -gt $deadline) {throw 'Owned terminal readiness timed out'}
        Start-Sleep -Milliseconds 20
    } while($true)
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
function Status([string]$Surface) {
    $r=Request @('editor','status',$Surface)
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
function Open-Editor([string]$Path,[string]$Pane='',[string]$Root='') {
    if(-not $Pane) {$Pane=$script:source.pane}
    if(-not $Root) {$Root=$fixture.Root}
    $opened=(Request @('editor','open',$Path,'--pane',$Pane,'--root',$Root)).editor_opened
    if(-not $opened.surface -or -not $opened.pane) {throw 'Editor open omitted pane/surface identity'}
    if($script:ownedEditors -notcontains $opened.surface) {$script:ownedEditors+=$opened.surface}
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
    foreach($group in @('open','edit','encoding','encoding-defaults','conflict','save-as','save-all-many','close','move','restore','restore-errors','missing-root','checkpoint-failure','late-quit','recovery')) {
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
            'late-quit' {
                $path=$fixture.Write('late quit clean 한글.txt',[EditorFixture]::Original,$false,$false);$opened=Open-Editor $path
                Assert-Text $opened.surface ([EditorFixture]::Original) $false|Out-Null
                $tree=Tree;$identity=Request @('identify')
                if($identity.pid -ne $process.Id) {throw 'Refusing UI suspension without exact owned-host identity'}
                $closedPid=$process.Id;$pause=$null;$deadlineWatch=[Diagnostics.Stopwatch]::StartNew()
                $observation=[ordered]@{case=$group;kind='late-quit-deadline';pid=$closedPid;surface=$opened.surface;windowHandle=$tree.window_handle;clientBudgetMs=20000;wholeProcessSuspended=$false;uiThreadSuspended=$false;uiThreadResumed=$false}
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
                Assert-Bytes $path ([EditorFixture]::Original)
                Finish-Host;Forget-Editor $opened.surface
                $observation.cleanHostExit=$true
                Passed 'late_clean_editor_quit_exits_after_real_ipc_deadline_and_verified_owned_ui_thread_resume'
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
