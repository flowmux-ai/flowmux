# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden owned Keybindings; 50s work + bounded cleanup, outer Job60s.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
if (-not $env:FLOWMUX_TEST_ARTIFACT_ROOT) { throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.' }
$ErrorActionPreference='Stop';$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path;$cli=Join-Path $BuildDirectory 'flowmuxctl.exe';$gui=Join-Path $BuildDirectory 'flowmux.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'OptionsFixture.cs')
Add-Type -ReferencedAssemblies System.Drawing -Path (Join-Path $PSScriptRoot 'ChromeFixture.cs')
. (Join-Path $PSScriptRoot 'OptionsEvidence.ps1')
$directory=Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT ('keybindings-dialog-'+[guid]::NewGuid());[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$clock=[Diagnostics.Stopwatch]::StartNew();$owned=$null;$pipeName=$null;$clients=@();$shells=@();$cleaning=$false;$cleanupErrors=@();$hostOut=$null;$hostErr=$null
$evidence=[ordered]@{started=[DateTime]::UtcNow.ToString('o');mode='hidden-native-keybindings-dialog';hosts=@();checks=@();observations=@();artifacts=@();desktopInput=$false;clipboardAccess=$false;imeGuardMessageSimulation=$true;physicalIme=$false;deferred='Owned WM_IME_START/END messages exercise only application guards; they do not establish OS Korean IME/TSF correctness. The renderer hook is synthetic: physical keyboard/focus/IME and WebView/OS accelerator routing, per-monitor DPI, accessibility and composed visual acceptance are not established. PNG capture requests native EDIT client paint, but hidden EDIT pixels may be absent; nonclient titlebar pixels are excluded.'}
function Require([bool]$Condition,[string]$Message){if(-not $Condition){throw $Message}}
function Budget([int]$Maximum=5000){if($cleaning){return $Maximum};$left=50000-$clock.ElapsedMilliseconds;Require ($left -gt 0) 'Keybindings work budget expired';return [int][Math]::Min($Maximum,$left)}
function Probe([string[]]$Arguments,[int]$Maximum=5000){
    Budget|Out-Null;$p=[CliProbe]::Start($cli,$Arguments,$directory,$directory);$script:clients+=,$p.Id;$out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
    try {Require ($p.WaitForExit((Budget $Maximum))) 'Owned CLI deadline exceeded; no retry';Require ($out.Wait(500) -and $err.Wait(500)) 'CLI output did not close';Require ($p.ExitCode -eq 0) ('CLI failed: '+[CliProbe]::Output($err));return ([CliProbe]::Output($out)|ConvertFrom-Json)}
    catch {$evidence.observations+=@{kind='cli-failure';arguments=$Arguments;stdout=[CliProbe]::Output($out);stderr=[CliProbe]::Output($err)};throw}
    finally {if(-not $p.HasExited){$p.Kill();[CliProbe]::WaitAfterKill($p)};$p.Dispose()}
}
function Request([string[]]$Arguments,[int]$Maximum=5000){Require ([bool]$pipeName) 'Explicit owned pipe required';return Probe (@('--pipe',$pipeName,'--json')+$Arguments) $Maximum}
function Tree([int]$Maximum=5000){$t=Request @('tree') $Maximum;Require ($t.background_testing) 'Host is not in background mode';[OptionsFixture]::Describe([long]$t.window_handle,$owned.Id)|Out-Null;return $t}
function Identities($Tree){return (@($Tree.surfaces|Sort-Object id|ForEach-Object {$_.id.ToString()+':'+$_.pid.ToString()}) -join ',')}
function Ack($Status){return @($Status.surfaces).Count -gt 0 -and @($Status.surfaces|Where-Object {-not $_.applied -or $_.applied.revision -ne $Status.document.revision}).Count -eq 0}
function Await([scriptblock]$Condition){$wait=[Diagnostics.Stopwatch]::StartNew();$last=$null;do{$left=5000-$wait.ElapsedMilliseconds;if($left -le 0){$evidence.observations+=@{kind='condition-timeout';settings=$last};throw 'Keybindings condition exceeded five seconds'};$s=Request @('settings','show') ([int]$left);if($s.options){[OptionsFixture]::Describe([long]$s.options.window,$owned.Id)|Out-Null};$last=$s;if(& $Condition $s){return $s};Start-Sleep -Milliseconds 20}while($true)}
function Click($Status,[long]$Control){[OptionsFixture]::Click([long]$Status.options.window,$Control,$owned.Id)}
function Record([string]$Name,$Status){
    # Keep every existing owned HWND read/guard; compact only the saved evidence.
    $window=[OptionsFixture]::Describe([long]$Status.options.window,$owned.Id);$controls=@([ChromeFixture]::Read([long]$Status.options.viewport,$owned.Id))
    $record=Get-OptionsEvidenceSummary $Name $Status 'keybindings';$record.window=$window;$record.nativeCounts=@{viewportControls=$controls.Count}
    if(-not $script:optionsEvidenceInventoryRecorded){$record.catalog=$Status.options.keybindings.actions;$record.controls=$controls;$script:optionsEvidenceInventoryRecorded=$true}
    if($Status.options.keybindings.editor){$record.editorWindow=[OptionsFixture]::Describe([long]$Status.options.keybindings.editor.window,$owned.Id);$record.editorControls=@([ChromeFixture]::Read([long]$Status.options.keybindings.editor.window,$owned.Id));if($Status.options.keybindings.editor.capture_dialog){$record.captureWindow=[OptionsFixture]::Describe([long]$Status.options.keybindings.editor.capture_dialog.window,$owned.Id)}}
    $evidence.observations+=$record;$evidence.finalSettings=$Status
}

function Capture([string]$Name){
    $before=Request @('settings','show');$snapshot=$before.options|ConvertTo-Json -Depth 40 -Compress;$editor=$before.options.keybindings.editor
    $handle=if($editor -and $editor.capture_dialog){[long]$editor.capture_dialog.window}elseif($editor){[long]$editor.window}else{[long]$before.options.window};[OptionsFixture]::Describe($handle,$owned.Id)|Out-Null
    $bitmap=Join-Path $directory ($Name+'.bmp');$png=Join-Path $directory ($Name+'.png');$capture=Request @('chrome-capture',$bitmap);Require ($capture.root_handle -eq $handle) 'Native capture selected the wrong owned popup';[ChromeFixture]::Png($bitmap,$png)
    $picture=[Drawing.Image]::FromFile($png);try{$width=$picture.Width;$height=$picture.Height}finally{$picture.Dispose()};Require ($width -eq $capture.width -and $height -eq $capture.height -and $width -gt 0 -and $height -gt 0) 'PNG dimensions differ from production capture'
    $after=Request @('settings','show');$unchanged=$after.document.revision -eq $before.document.revision -and ($after.options|ConvertTo-Json -Depth 40 -Compress) -ceq $snapshot
    $evidence.artifacts+=@{name=$Name;path=$png;width=$width;height=$height;bytes=(Get-Item -LiteralPath $png).Length;sha256=(Get-FileHash -Algorithm SHA256 -LiteralPath $png).Hash.ToLowerInvariant();capture=$capture;stateUnchanged=$unchanged;scope='Production native client BUTTON/STATIC painters and requested EDIT/LISTBOX WM_PRINTCLIENT on an owned offscreen DIB; hidden EDIT pixels may be absent; nonclient titlebar pixels are absent; not a composed GPU or desktop screenshot'}
    Require ($unchanged) 'Capturing native controls changed Options or popup state';Require ((Get-Item -LiteralPath $png).Length -lt 500KB) 'Native PNG exceeds the small evidence artifact limit'
}

function Row($Status,[string]$Action){$rows=@($Status.options.keybindings.actions|Where-Object {$_.action -ceq $Action});Require ($rows.Count -eq 1) ('Missing shortcut row '+$Action);return $rows[0]}
function Parent([long]$Control){return [OptionsFixture]::Parent($Control,$owned.Id)}
function Native-Click([long]$Control){[OptionsFixture]::Click((Parent $Control),$Control,$owned.Id)}
function Editor-Text($Status){return [OptionsFixture]::Text([long]$Status.options.keybindings.editor.input,$owned.Id)}
function Draft($Status,[string]$Text){$input=[long]$Status.options.keybindings.editor.input;[OptionsFixture]::SetTextAndNotify((Parent $input),$input,$owned.Id,$Text)}
function Edit-Click($Status,[string]$Name){Native-Click ([long]$Status.options.keybindings.editor.$Name)}
function Edit-Enter($Status,[string]$Name){$handle=[long]$Status.options.keybindings.editor.$Name;Require ((Parent $handle) -eq [long]$Status.options.keybindings.editor.window) 'Enter target belongs to another popup';[OptionsFixture]::PostEnter($handle,$owned.Id)}
function Bound($Status,[string]$Action,[string]$Code,[bool]$Ctrl,[bool]$Alt,[bool]$Shift){return @($Status.surfaces[0].applied.bindings|Where-Object {$_.action -ceq $Action -and $_.chord.code -ceq $Code -and $_.chord.ctrl -eq $Ctrl -and $_.chord.alt -eq $Alt -and $_.chord.shift -eq $Shift}).Count -eq 1}
function Open-Editor($Status,[string]$Action){
    $wait=[Diagnostics.Stopwatch]::StartNew();$row=Row $Status $Action
    do{Require ($wait.ElapsedMilliseconds -lt 5000) 'Shortcut row could not be reached within five seconds';$bounds=[OptionsFixture]::RelativeBounds([long]$Status.options.viewport,[long]$row.edit,$owned.Id);$size=[ChromeFixture]::Size([long]$Status.options.viewport,$owned.Id);if($bounds.Y -ge 0 -and $bounds.Y+$bounds.Height -le $size[1]){break};if([Math]::Abs($bounds.Y) -ge $size[1]){[OptionsFixture]::ScrollPage([long]$Status.options.viewport,$owned.Id,($bounds.Y -gt 0))}else{[OptionsFixture]::ScrollLine([long]$Status.options.viewport,$owned.Id,($bounds.Y -gt 0))};$Status=Request @('settings','show');$row=Row $Status $Action}while($true)
    Native-Click ([long]$row.edit);return Await {param($s) $s.options.keybindings.editor -and $s.options.keybindings.editor.action -ceq $Action}
}
function Popup($Status,[bool]$Capture){$edit=$Status.options.keybindings.editor;$panel=if($Capture){$edit.capture_dialog}else{$edit};$window=[OptionsFixture]::Describe([long]$panel.window,$owned.Id);$owner=if($Capture){[long]$edit.window}else{[long]$Status.options.window};$width=if($Capture){320}else{420};$height=if($Capture){140}else{220};$scale=[Math]::Max(96,$window.Dpi)/96.0;Require ($window.Owner -eq $owner -and -not $window.OwnerEnabled -and $window.Enabled) 'Modal popup owner chain or enabled state differs';$options=[OptionsFixture]::Describe([long]$Status.options.window,$owned.Id);Require (-not $options.Enabled -and -not [OptionsFixture]::Describe($options.Owner,$owned.Id).Enabled) 'Edit modal did not disable both Options and the main owner';Require ([Math]::Abs($window.Width-$width*$scale) -le 3 -and [Math]::Abs($window.Height-$height*$scale) -le 3) 'Modal popup size differs';return $window}
function Editor-Layout($Status){
    $editor=$Status.options.keybindings.editor;$size=[ChromeFixture]::Size([long]$editor.window,$owned.Id);$controls=@()
    foreach($name in @('input','capture','reset','unbind','cancel','ok')){$handle=[long]$editor.$name;$bounds=[OptionsFixture]::RelativeBounds([long]$editor.window,$handle,$owned.Id);$controls+=,@{name=$name;handle=$handle;bounds=$bounds}}
    $evidence.observations+=@{name='editor-input-buttons-bounds-and-nonoverlap';client=$size;controls=$controls;scope='Actual owned native client geometry and overlap checks only; label fit requires root review of production native PNGs, with no nonclient titlebar raster assertion'}
    foreach($control in $controls){$r=$control.bounds;Require ($r.X -ge 0 -and $r.Y -ge 0 -and $r.Width -gt 0 -and $r.Height -gt 0 -and $r.X+$r.Width -le $size[0] -and $r.Y+$r.Height -le $size[1]) ('Editor control is clipped: '+$control.name)}
    for($a=0;$a -lt $controls.Count;$a++){for($b=$a+1;$b -lt $controls.Count;$b++){$x=$controls[$a].bounds;$y=$controls[$b].bounds;Require (-not ($x.X -lt $y.X+$y.Width -and $y.X -lt $x.X+$x.Width -and $x.Y -lt $y.Y+$y.Height -and $y.Y -lt $x.Y+$x.Height)) ('Editor controls overlap: '+$controls[$a].name+' / '+$controls[$b].name)}}
}

function Capture-Key($Status,[int]$Key,[int]$Scan,[bool]$Up=$false,[bool]$Extended=$false,[bool]$System=$false,[bool]$Repeat=$false){[OptionsFixture]::KeyMessage([long]$Status.options.keybindings.editor.capture_dialog.window,$owned.Id,$Key,$Scan,$Up,$Extended,$System,$Repeat)}
function Capture-Unchanged($Status,[bool]$Composing,[string]$Name){Record $Name $Status;Require ($Status.options.keybindings.editor.capture_dialog -and $Status.options.keybindings.editor.capture_dialog.composing -eq $Composing -and (Editor-Text $Status) -ceq $captureOriginal -and $Status.document.revision -eq $captureRevision -and -not $Status.options.keybindings.editor.error -and $Status.options.keybindings.editor.capture_dialog.message -ceq $captureMessage -and -not $Status.options.keybindings.editor.pending -and -not $Status.options.keybindings.queued) ('Capture guard changed draft, saved revision or error state: '+$Name)}
function Shortcut([string]$Surface,[hashtable]$Event,[bool]$Forwarded){$result=Request @('test-shortcut',$Surface,($Event|ConvertTo-Json -Compress));$evidence.observations+=@{name='renderer-shortcut-hook';event=$Event;response=$result};Require ($result.surface -eq $Surface -and $result.forwarded -eq $Forwarded) 'Renderer shortcut forwarding differed'}
function Await-Tree([int]$Count){$wait=[Diagnostics.Stopwatch]::StartNew();$tree=$null;do{$left=5000-$wait.ElapsedMilliseconds;if($left -le 0){$evidence.observations+=@{kind='tree-timeout';tree=$tree};throw 'Terminal creation exceeded five seconds'};$tree=Tree ([int]$left);if(@($tree.surfaces).Count -eq $Count -and @($tree.surfaces|Where-Object {-not $_.ready -or -not $_.running}).Count -eq 0){$script:shells=@($script:shells+@($tree.surfaces.pid)|Sort-Object -Unique);return $tree};Start-Sleep -Milliseconds 20}while($true)}
try {
    $doctor=Probe @('doctor');Require ($doctor.background_testing -and $doctor.status -eq 'ok') 'Working hidden debug build required'
    $started=[DateTime]::UtcNow;$startup=[Diagnostics.Stopwatch]::StartNew();$owned=[CliProbe]::Start($gui,@('--temporary','--shell=cmd','--cwd',$directory),$directory,$directory);$hostOut=$owned.StandardOutput.ReadToEndAsync();$hostErr=$owned.StandardError.ReadToEndAsync();$evidence.hosts+=,$owned.Id
    $file=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($owned.Id).json"
    do {Require (-not $owned.HasExited -and $startup.ElapsedMilliseconds -lt 8000) 'Host discovery exceeded eight seconds or exited';if((Test-Path -LiteralPath $file) -and (Get-Item -LiteralPath $file).LastWriteTimeUtc -ge $started){$record=Get-Content -Raw -LiteralPath $file|ConvertFrom-Json;Require ($record.pid -eq $owned.Id -and [bool]$record.pipe) 'Wrong discovery owner';$pipeName=$record.pipe;break};Start-Sleep -Milliseconds 20}while($true)
    $left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup budget exhausted';Require ((Request @('identify') ([int][Math]::Min(5000,$left))).pid -eq $owned.Id) 'Pipe owner mismatch'
    do {$left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup readiness exceeded eight seconds';$tree=Tree ([int][Math]::Min(5000,$left));if(@($tree.surfaces).Count -eq 1 -and $tree.surfaces[0].ready -and $tree.surfaces[0].running){break};Start-Sleep -Milliseconds 20}while($true)
    $evidence.observations+=@{name='startup';elapsedMs=$startup.ElapsedMilliseconds;pipe=$pipeName};$shells=@($tree.surfaces.pid);$originalPid=$tree.surfaces[0].pid;$identities=Identities $tree;$active=(Request @('identify')).surface


    $status=Await {param($s) Ack $s}
    $defaultEvent=@{code='KeyT';key='t';ctrlKey=$true;shiftKey=$true};Shortcut $active $defaultEvent $false
    $tree=Await-Tree 2;Require (@($tree.surfaces|Where-Object {$_.id -eq $active -and $_.pid -eq $originalPid}).Count -eq 1) 'Default shortcut replaced its original terminal';$active=(Request @('identify')).surface;$identities=Identities $tree
    $status=Await {param($s) Ack $s};$entry=@($tree.chrome.controls|Where-Object {$_.kind -eq 'settings'});Require ($entry.Count -eq 1) 'Options entry missing';Native-Click ([long]$entry[0].handle)
    $status=Await {param($s) $s.options -and $s.options.open};Require ($status.options.page -eq 'general' -and $status.options.auto_apply -and $status.options.error_or_status -match 'save automatically') 'General page no longer describes automatic saving'
    Click $status ([long]$status.options.tabs[2].handle);$status=Await {param($s) $s.options.page -eq 'keybindings'};Require (-not $status.options.keybindings.auto_apply -and $status.options.error_or_status -match 'save after OK' -and $status.options.error_or_status -notmatch 'automatically') 'Keybindings footer misstates the explicit OK save flow'
    Require (@($status.options.keybindings.actions).Count -eq 35 -and @($status.options.keybindings.actions|Where-Object {$_.supported}).Count -eq 32) 'Expected 35 rows including 32 supported actions'
    $unsupported=@($status.options.keybindings.actions|Where-Object {-not $_.supported});Require ($unsupported.Count -eq 3) 'Unsupported action count differs';foreach($row in $unsupported){Require (-not [OptionsFixture]::Describe([long]$row.edit,$owned.Id).Enabled) ('Unavailable action Edit is enabled: '+$row.action)}
    $row=Row $status 'new-surface';Require ([OptionsFixture]::Text([long]$row.edit,$owned.Id) -ceq 'Edit' -and [OptionsFixture]::Text([long]$row.accel_handle,$owned.Id) -ceq (@($row.accels) -join ', ')) 'Actual row Edit or current binding chip differs';Record 'row-list-and-default-shortcut' $status;Require ($status.options.scroll_offset -eq 0) 'Initial rows capture must start at scroll top';Capture 'keybindings-rows'
    $evidence.checks+=@{name='native_action_rows_current_chips_edit_controls_and_existing_default_shortcut';passed=$true}

    $status=Open-Editor $status 'new-surface';Popup $status $false|Out-Null;Editor-Layout $status;$before=$status.document.revision;$originalDraft=Editor-Text $status
    Draft $status 'Ctrl+Alt+Y';Require ((Editor-Text $status) -ceq 'Ctrl+Alt+Y') 'Edit draft was not accepted';Record 'owned-edit-before-cancel' $status;Capture 'keybindings-edit';Edit-Enter $status 'cancel';$status=Await {param($s) -not $s.options.keybindings.editor}
    Require ($status.document.revision -eq $before -and [OptionsFixture]::Describe([long]$status.options.window,$owned.Id).Enabled) 'Cancel persisted a draft or left Options disabled'
    $status=Open-Editor $status 'new-surface';Edit-Enter $status 'unbind';$status=Await {param($s) (Editor-Text $s) -ceq ''};Require ($status.document.revision -eq $before) 'Unbind saved before OK'
    Edit-Enter $status 'reset';$status=Await {param($s) (Editor-Text $s) -ceq $originalDraft};Require ($status.document.revision -eq $before) 'Reset saved before OK';Record 'reset-and-unbind-remain-drafts' $status
    Edit-Click $status 'cancel';$status=Await {param($s) -not $s.options.keybindings.editor};$evidence.checks+=@{name='owned_420x220_modal_Enter_targets_Cancel_Reset_Unbind_without_early_save';passed=$true}

    $status=Open-Editor $status 'new-surface';Draft $status 'Ctrl+Alt+Y, Ctrl+Alt+U';Edit-Enter $status 'ok'
    $status=Await {param($s) -not $s.options.keybindings.editor -and (Ack $s) -and (Bound $s 'new-surface' 'KeyY' $true $true $false) -and (Bound $s 'new-surface' 'KeyU' $true $true $false)}
    $row=Row $status 'new-surface';Require ([OptionsFixture]::Text([long]$row.accel_handle,$owned.Id) -ceq 'Ctrl+Alt+Y, Ctrl+Alt+U') 'Committed row chip did not show comma-separated bindings';Record 'multi-accelerator-commit-and-renderer-ack' $status
    Shortcut $active $defaultEvent $true;Require ((Identities (Tree)) -ceq $identities) 'Old default still dispatched after rebind'
    Shortcut $active @{code='KeyY';key='y';ctrlKey=$true;altKey=$true} $false;$tree=Await-Tree 3;$active=(Request @('identify')).surface;$status=Await {param($s) Ack $s}
    Shortcut $active @{code='KeyU';key='u';ctrlKey=$true;altKey=$true} $false;$tree=Await-Tree 4;$active=(Request @('identify')).surface;$status=Await {param($s) Ack $s};$identities=Identities $tree
    $evidence.checks+=@{name='comma_separated_Enter_OK_updates_row_and_renderer_both_chords_create_one_terminal_each';passed=$true}

    # Keep the renderer guards separate from the native capture-popup guards.
    $rebound=@{code='KeyY';key='y';ctrlKey=$true;altKey=$true};Require (Bound $status 'new-surface' 'KeyY' $true $true $false) 'Renderer guard checks require an actually bound chord'
    foreach($guardEvent in @(@{repeat=$true;forwarded=$false},@{isComposing=$true;forwarded=$true},@{keyCode=229;forwarded=$true},@{altGraph=$true;forwarded=$true},@{key='Dead';forwarded=$true},@{metaKey=$true;forwarded=$true})){
        $event=$rebound.Clone();$expected=$guardEvent.forwarded;foreach($name in $guardEvent.Keys){if($name -ne 'forwarded'){$event[$name]=$guardEvent[$name]}}
        Shortcut $active $event $expected;Require ((Identities (Tree)) -ceq $identities) 'Guarded renderer shortcut dispatched or changed a terminal PID'
    }
    Shortcut $active @{code='AltRight';key='Alt';altKey=$true} $true;Shortcut $active $rebound $true;Shortcut $active @{type='keyup';code='AltRight';key='Alt'} $true;Require ((Identities (Tree)) -ceq $identities) 'Right-Alt renderer guard dispatched a shortcut'
    $evidence.checks+=@{name='actual_renderer_repeat_composition_229_altgraph_dead_meta_rightalt_guards_do_not_dispatch';passed=$true;scope='controlled renderer hook, not physical keyboard routing or OS IME'}

    $status=Open-Editor $status 'new-surface';$edit=$status.options.keybindings.editor;$raw='Ctrl+Alt+Q, 한글 한 é 😀 &';[OptionsFixture]::CompositionGuard([long]$edit.window,[long]$edit.input,$owned.Id,$true);Draft $status 'Ctrl+Alt+Q'
    $status=Await {param($s) $s.options.keybindings.editor.composing -and (Editor-Text $s) -ceq 'Ctrl+Alt+Q'};$composingRevision=$status.document.revision;Edit-Click $status 'ok';$status=Request @('settings','show')
    Require ($status.options.keybindings.editor -and $status.options.keybindings.editor.id -eq $edit.id -and $status.options.keybindings.editor.composing -and -not $status.options.keybindings.editor.pending -and -not $status.options.keybindings.queued -and -not $status.options.keybindings.editor.error -and $status.document.revision -eq $composingRevision -and (Editor-Text $status) -ceq 'Ctrl+Alt+Q') 'OK committed, queued or rejected a valid draft during composition';Record 'valid-draft-OK-blocked-while-composing' $status;Draft $status $raw
    $status=Await {param($s) $s.options.keybindings.editor.composing -and (Editor-Text $s) -ceq $raw};Request @('settings','set','font-size','17')|Out-Null;$status=Await {param($s) $s.document.terminal.font_size -eq 17 -and (Ack $s)}
    Require ($status.options.keybindings.editor.composing -and (Editor-Text $status) -ceq $raw -and (Bound $status 'new-surface' 'KeyY' $true $true $false)) 'Unrelated update committed or replaced the composing draft';Record 'owned-editor-ime-guard' $status
    [OptionsFixture]::CompositionGuard([long]$edit.window,[long]$edit.input,$owned.Id,$false);$status=Await {param($s) -not $s.options.keybindings.editor.composing};Edit-Click $status 'ok';$status=Await {param($s) $s.options.keybindings.editor -and [bool]$s.options.keybindings.editor.error -and -not $s.options.keybindings.editor.pending}
    Require ((Editor-Text $status) -ceq $raw -and (Bound $status 'new-surface' 'KeyY' $true $true $false)) 'Invalid Unicode shortcut draft was discarded or committed';Record 'invalid-unicode-draft-retained' $status;Edit-Click $status 'cancel';$status=Await {param($s) -not $s.options.keybindings.editor}
    $evidence.checks+=@{name='owned_IME_guard_preserves_raw_unicode_draft_during_other_write_and_invalid_OK';passed=$true;scope='application guard only; not OS Korean IME'}

    $status=Open-Editor $status 'new-surface';Draft $status 'Ctrl+Alt+Q';Request @('settings','keybindings','set','new-surface','Ctrl+Alt+J')|Out-Null;$status=Await {param($s) (Ack $s) -and (Bound $s 'new-surface' 'KeyJ' $true $true $false)}
    Require ((Editor-Text $status) -ceq 'Ctrl+Alt+Q') 'External write replaced active edit draft';Edit-Click $status 'ok';$status=Await {param($s) $s.options.keybindings.editor.error -match 'changed elsewhere|conflict'}
    Require ((Editor-Text $status) -ceq 'Ctrl+Alt+Q' -and (Bound $status 'new-surface' 'KeyJ' $true $true $false)) 'Stale dialog OK overwrote the external winner';Record 'modal-edit-cas-conflict' $status;Edit-Click $status 'cancel';$status=Await {param($s) -not $s.options.keybindings.editor};$evidence.checks+=@{name='modal_OK_rechecks_keybinding_CAS_and_preserves_winner_and_draft';passed=$true}

    $status=Open-Editor $status 'new-surface';$captureOriginal=Editor-Text $status;$captureRevision=$status.document.revision;Edit-Enter $status 'capture';$status=Await {param($s) $s.options.keybindings.editor.capture_dialog};Popup $status $true|Out-Null
    $captureMessage=$status.options.keybindings.editor.capture_dialog.message
    Capture-Key $status 0x11 0x1D;Capture-Key $status 0x11 0x1D $true;$status=Request @('settings','show');Capture-Unchanged $status $false 'capture-pure-modifier-ignored'
    # Every guard below receives valid Ctrl+Alt+Y; an unmodified Y would fail
    # parsing even without the guard and could produce a false-positive pass.
    Capture-Key $status 0x11 0x1D;Capture-Key $status 0x12 0x38 $false $false $true;Capture-Key $status 0x59 0x15 $false $false $true $true
    $status=Request @('settings','show');Capture-Unchanged $status $false 'capture-valid-chord-repeat-ignored'
    Capture-Key $status 0x59 0x15 $true $false $true;Capture-Key $status 0x12 0x38 $true $false $true;Capture-Key $status 0x11 0x1D $true
    Capture-Key $status 0xE5 0;Capture-Key $status 0x11 0x1D;Capture-Key $status 0x12 0x38 $false $false $true;Capture-Key $status 0x59 0x15 $false $false $true
    $status=Request @('settings','show');Capture-Unchanged $status $false 'capture-valid-chord-blocked-after-process-229'
    Capture-Key $status 0xE5 0 $true;Capture-Key $status 0x59 0x15 $true $false $true;Capture-Key $status 0x12 0x38 $true $false $true;Capture-Key $status 0x11 0x1D $true
    $captureWindow=[long]$status.options.keybindings.editor.capture_dialog.window;[OptionsFixture]::WindowCompositionGuard($captureWindow,$owned.Id,$true)
    # START clears tracked modifiers, so hold Ctrl+Alt again after START.
    Capture-Key $status 0x11 0x1D;Capture-Key $status 0x12 0x38 $false $false $true;Capture-Key $status 0x59 0x15 $false $false $true
    $status=Request @('settings','show');Capture-Unchanged $status $true 'capture-valid-chord-blocked-during-IME-guard'
    [OptionsFixture]::WindowCompositionGuard($captureWindow,$owned.Id,$false);Capture-Key $status 0x59 0x15 $true $false $true;Capture-Key $status 0x12 0x38 $true $false $true;Capture-Key $status 0x11 0x1D $true
    $status=Request @('settings','show');Capture-Unchanged $status $false 'owned-capture-guards';Capture 'keybindings-capture';Capture-Key $status 0x1B 0x01;$status=Await {param($s) $s.options.keybindings.editor -and -not $s.options.keybindings.editor.capture_dialog}
    Require ((Editor-Text $status) -ceq $captureOriginal -and $status.document.revision -eq $captureRevision -and [OptionsFixture]::Describe([long]$status.options.keybindings.editor.window,$owned.Id).Enabled -and -not [OptionsFixture]::Describe([long]$status.options.window,$owned.Id).Enabled) 'Escape changed draft or closed/disabled wrong modal owner';$evidence.checks+=@{name='Enter_Capture_opens_owned_320x140_popup_with_modifier_repeat_229_IME_guards_and_Escape';passed=$true;scope='controlled owned HWND messages only'}
    Edit-Click $status 'capture';$status=Await {param($s) $s.options.keybindings.editor.capture_dialog};Capture-Key $status 0x11 0x1D;Capture-Key $status 0x12 0x38 $false $false $true;Capture-Key $status 0x59 0x15 $false $false $true
    $status=Await {param($s) $s.options.keybindings.editor -and -not $s.options.keybindings.editor.capture_dialog};$captured=Editor-Text $status;Require ($captured -match '(?i)ctrl|control' -and $captured -match '(?i)alt' -and $captured -match '(?i)y$' -and $status.document.revision -eq $captureRevision) 'Captured chord did not update only the edit draft';Record 'captured-chord-before-OK' $status
    Edit-Enter $status 'input';$status=Await {param($s) -not $s.options.keybindings.editor -and (Ack $s) -and (Bound $s 'new-surface' 'KeyY' $true $true $false)}
    Shortcut $active @{code='KeyY';key='y';ctrlKey=$true;altKey=$true} $false;$tree=Await-Tree 5;$status=Await {param($s) Ack $s};$identities=Identities $tree;$evidence.checks+=@{name='owned_capture_chord_changes_draft_then_input_Enter_persists_and_actual_renderer_dispatches';passed=$true}

    $status=Open-Editor $status 'new-surface';Edit-Click $status 'unbind';Edit-Click $status 'ok';$status=Await {param($s) -not $s.options.keybindings.editor -and (Ack $s) -and @($s.surfaces[0].applied.bindings|Where-Object {$_.action -eq 'new-surface'}).Count -eq 0};Require ([OptionsFixture]::Text([long](Row $status 'new-surface').accel_handle,$owned.Id) -ceq '(unbound)') 'Unbound row chip differs'
    $status=Open-Editor $status 'new-surface';Edit-Click $status 'reset';Edit-Click $status 'ok';$status=Await {param($s) -not $s.options.keybindings.editor -and (Ack $s) -and (Bound $s 'new-surface' 'KeyT' $true $false $true)}
    Require ($status.document.terminal.font_size -eq 17 -and [OptionsFixture]::Describe([long]$status.options.window,$owned.Id).Enabled -and (Identities (Tree)) -ceq $identities) 'Unbind/Reset changed unrelated setting or left owner disabled';Record 'final-unbind-reset-modal-cleanup' $status;$evidence.checks+=@{name='OK_commits_unbind_reset_and_reenables_owner_without_terminal_recreation';passed=$true}
    $generalBefore=$status.document.terminal|ConvertTo-Json -Depth 10 -Compress
    Request @('settings','keybindings','set','toggle-pane-zoom','Ctrl+Alt+L')|Out-Null;$status=Await {param($s) (Ack $s) -and (Bound $s 'toggle-pane-zoom' 'KeyL' $true $true $false)}
    Require ([OptionsFixture]::Text([long]$status.options.keybindings.reset,$owned.Id) -ceq 'Reset all keybindings to defaults') 'Reset-all native control differs';Native-Click ([long]$status.options.keybindings.reset)
    $status=Await {param($s) -not $s.options.keybindings.pending -and -not $s.options.keybindings.queued -and (Ack $s) -and (Bound $s 'toggle-pane-zoom' 'KeyM' $true $true $false) -and (Bound $s 'new-surface' 'KeyT' $true $false $true) -and @($s.surfaces[0].applied.bindings).Count -eq 32}
    Require (($status.document.terminal|ConvertTo-Json -Depth 10 -Compress) -ceq $generalBefore -and (Identities (Tree)) -ceq $identities) 'Native Reset all changed general settings or terminal PIDs'
    Click $status ([long]$status.options.close);$status=Await {param($s) -not $s.options.open};$tree=Tree;$entry=@($tree.chrome.controls|Where-Object {$_.kind -eq 'settings'});Require ($entry.Count -eq 1) 'Options reopen entry is missing';Native-Click ([long]$entry[0].handle)
    $status=Await {param($s) $s.options.open};Click $status ([long]$status.options.tabs[2].handle);$status=Await {param($s) $s.options.page -eq 'keybindings' -and (Ack $s)}
    Require (($status.document.terminal|ConvertTo-Json -Depth 10 -Compress) -ceq $generalBefore -and $status.document.terminal.font_size -eq 17 -and @($status.surfaces[0].applied.bindings).Count -eq 32 -and (Bound $status 'toggle-pane-zoom' 'KeyM' $true $true $false) -and (Identities (Tree)) -ceq $identities -and -not $status.options.keybindings.editor) 'Reset-all/close/reopen changed general settings, renderer bindings or surviving terminal PIDs'
    Record 'reset-all-close-reopen' $status;$evidence.checks+=@{name='native_reset_all_and_options_reopen_preserve_general_settings_and_terminal_pids';passed=$true};$evidence.status='passed'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;$evidence.failureSettings=$status;throw}
finally {
    $cleaning=$true
    if($owned){
        try{if(-not $owned.HasExited){$stop=[Diagnostics.Stopwatch]::StartNew();if($pipeName){Request @('quit','--discard-state') 2500|Out-Null};if(-not $owned.WaitForExit([int][Math]::Max(1,5000-$stop.ElapsedMilliseconds))){throw 'Owned host cleanup deadline exceeded'}}}catch{$cleanupErrors+=$_.Exception.Message}
        if(-not $owned.HasExited){try{$owned.Kill();[CliProbe]::WaitAfterKill($owned)}catch{$cleanupErrors+=('Owned host termination failed: '+$_.Exception.Message)}}
        $exitCode=$null;if($owned.HasExited){$exitCode=$owned.ExitCode;if($exitCode -ne 0){$cleanupErrors+=('Owned host exited with nonzero code: '+$exitCode)}}else{$cleanupErrors+='Owned host is still running after bounded cleanup'}
        $stdoutComplete=$false;$stderrComplete=$false
        try{$stdoutComplete=$hostOut.Wait(500)}catch{$cleanupErrors+=('Host stdout read failed: '+$_.Exception.Message)};if(-not $stdoutComplete){$cleanupErrors+='Host stdout did not complete within 500ms'}
        try{$stderrComplete=$hostErr.Wait(500)}catch{$cleanupErrors+=('Host stderr read failed: '+$_.Exception.Message)};if(-not $stderrComplete){$cleanupErrors+='Host stderr did not complete within 500ms'}
        $evidence.observations+=@{name='host-exit';pid=$owned.Id;exitCode=$exitCode;stdoutComplete=$stdoutComplete;stderrComplete=$stderrComplete;stdout=[CliProbe]::Output($hostOut);stderr=[CliProbe]::Output($hostErr)};$owned.Dispose()
    }
    if($cleanupErrors.Count){$evidence.status='failed';$evidence.cleanupErrors=$cleanupErrors};$evidence.clientPids=$clients;$evidence.shells=$shells;$evidence.elapsedMs=$clock.ElapsedMilliseconds;$evidence.finished=[DateTime]::UtcNow.ToString('o');if($evidence.status -eq 'failed'){$evidence|ConvertTo-Json -Depth 50|Set-Content -Encoding UTF8 (Join-Path $directory 'native-keybindings-dialog-background.json')};if($evidence.status -eq 'failed'){Write-Output ('Evidence: '+$directory)}
}
if($cleanupErrors.Count){throw ($cleanupErrors -join '; ')}
[ordered]@{status=$evidence.status;checks=$evidence.checks.Count;elapsedMs=$evidence.elapsedMs}|ConvertTo-Json -Compress
