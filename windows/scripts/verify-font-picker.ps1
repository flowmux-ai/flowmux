# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden owned font picker; 50s work + bounded cleanup, outer Job60s.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
if (-not $env:FLOWMUX_TEST_ARTIFACT_ROOT) { throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.' }
$ErrorActionPreference='Stop';$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path;$cli=Join-Path $BuildDirectory 'flowmuxctl.exe';$gui=Join-Path $BuildDirectory 'flowmux.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'OptionsFixture.cs')
Add-Type -ReferencedAssemblies System.Drawing -Path (Join-Path $PSScriptRoot 'ChromeFixture.cs')
$directory=Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT ('font-picker-'+[guid]::NewGuid());[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$clock=[Diagnostics.Stopwatch]::StartNew();$owned=$null;$pipeName=$null;$clients=@();$shells=@();$cleaning=$false;$cleanupErrors=@();$hostOut=$null;$hostErr=$null
$evidence=[ordered]@{started=[DateTime]::UtcNow.ToString('o');mode='hidden-native-font-picker';hosts=@();checks=@();observations=@();artifacts=@();desktopInput=$false;clipboardAccess=$false;imeGuardMessageSimulation=$true;physicalIme=$false;deferred='Owned WM_IME_START/END messages exercise application guards only, not OS Korean IME or TSF. Hidden HWND dispatch does not establish physical focus, keyboard routing, per-monitor DPI, accessibility or composed font glyph rendering. Font family strings, native control ownership and renderer settings ACK are asserted.'}
function Require([bool]$Condition,[string]$Message){if(-not $Condition){throw $Message}}
function Budget([int]$Maximum=5000){if($cleaning){return $Maximum};$left=50000-$clock.ElapsedMilliseconds;Require ($left -gt 0) 'Font picker work budget expired';return [int][Math]::Min($Maximum,$left)}
function Probe([string[]]$Arguments,[int]$Maximum=5000){
    Budget|Out-Null;$p=[CliProbe]::Start($cli,$Arguments,$directory,$directory);$script:clients+=,$p.Id;$out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
    try {Require ($p.WaitForExit((Budget $Maximum))) 'Owned CLI deadline exceeded; no retry';Require ($out.Wait(500) -and $err.Wait(500)) 'CLI output did not close';Require ($p.ExitCode -eq 0) ('CLI failed: '+[CliProbe]::Output($err));return ([CliProbe]::Output($out)|ConvertFrom-Json)}
    catch {$evidence.observations+=@{kind='cli-failure';arguments=$Arguments;stdout=[CliProbe]::Output($out);stderr=[CliProbe]::Output($err)};throw}
    finally {if(-not $p.HasExited){$p.Kill();[CliProbe]::WaitAfterKill($p)};$p.Dispose()}
}
function Request([string[]]$Arguments,[int]$Maximum=5000){Require ([bool]$pipeName) 'Explicit owned pipe required';return Probe (@('--pipe',$pipeName,'--json')+$Arguments) $Maximum}
function Tree([int]$Maximum=5000){$t=Request @('tree') $Maximum;Require ($t.background_testing) 'Host is not in background mode';[OptionsFixture]::Describe([long]$t.window_handle,$owned.Id)|Out-Null;return $t}
function Identities($Tree){return (@($Tree.surfaces|Sort-Object id|ForEach-Object {$_.id.ToString()+':'+$_.pid.ToString()}) -join ',')}
function Ack($Status){return @($Status.surfaces).Count -gt 0 -and @($Status.surfaces|Where-Object {-not $_.applied -or $_.applied.revision -ne $Status.document.revision -or $_.applied.terminal.font_family -cne $Status.document.terminal.font_family}).Count -eq 0}
function Await([scriptblock]$Condition){$wait=[Diagnostics.Stopwatch]::StartNew();$last=$null;do{$left=5000-$wait.ElapsedMilliseconds;if($left -le 0){$evidence.observations+=@{kind='condition-timeout';settings=$last};throw 'Font picker condition exceeded five seconds'};$s=Request @('settings','show') ([int]$left);if($s.options){[OptionsFixture]::Describe([long]$s.options.window,$owned.Id)|Out-Null};$last=$s;if(& $Condition $s){return $s};Start-Sleep -Milliseconds 20}while($true)}
function Click($Status,[long]$Control){[OptionsFixture]::Click([long]$Status.options.window,$Control,$owned.Id)}
function Field($Status){$rows=@($Status.options.controls|Where-Object {$_.key -ceq 'font_family'});Require ($rows.Count -eq 1) 'Font family row missing';return $rows[0]}
function Parent([long]$Control){return [OptionsFixture]::Parent($Control,$owned.Id)}
function Native-Click([long]$Control){[OptionsFixture]::Click((Parent $Control),$Control,$owned.Id)}
function Search($Status,[string]$Text){$picker=$Status.options.font_picker;[OptionsFixture]::SetTextAndNotify([long]$picker.window,[long]$picker.search,$owned.Id,$Text)}
function Raw-Query($Status){return [OptionsFixture]::Text([long]$Status.options.font_picker.search,$owned.Id)}
function Choose($Status,[int]$Choice){$picker=$Status.options.font_picker;$indices=@($picker.filtered);$index=-1;for($i=0;$i -lt $indices.Count;$i++){if([int]$indices[$i] -eq $Choice){$index=$i;break}};Require ($index -ge 0) 'Target font choice is not in the filtered list';[OptionsFixture]::ListSelect([long]$picker.window,[long]$picker.list,$owned.Id,$index);return Await {param($s) $null -ne $s.options.font_picker.selected -and $s.options.font_picker.selected -eq $Choice}}
function Find-Choice($Status,[string]$Kind){$choices=@($Status.options.font_picker.choices);for($i=0;$i -lt $choices.Count;$i++){if($choices[$i].kind -ceq $Kind){return $i}};throw ('No '+$Kind+' font choice available')}
function Open-Picker($Status){Native-Click ([long]$Status.options.font_picker.entrybutton);return Await {param($s) if($s.options.font_picker.catalog_state -in @('error','timeout')){Record 'catalog-failure' $s;throw ('Installed font catalog failed: '+$s.options.font_picker.catalog_state)};$s.options.font_picker.open -and $s.options.font_picker.catalog_state -in @('ready','truncated')}}
function Settled($Status){return -not $Status.options.pending -and $Status.options.queued -eq 0 -and (Ack $Status)}
function Record([string]$Name,$Status){
    $picker=$Status.options.font_picker;$row=Field $Status
    $summary=[ordered]@{open=$picker.open;window=$picker.window;owner=$picker.owner;search=$picker.search;list=$picker.list;choose=$picker.choose;cancel=$picker.cancel;catalog_state=$picker.catalog_state;choice_count=@($picker.choices).Count;filtered=@($picker.filtered);selected=$picker.selected;query=$picker.query;composing=$picker.composing;error=$picker.error;source_raw=$picker.source_raw}
    $evidence.observations+=@{name=$Name;revision=$Status.document.revision;font=$Status.document.terminal.font_family;row=@{input=$row.input;parent=$row.parent;value=$row.value;baseline=$row.baseline;error=$row.draft_error};picker=$summary;acks=@($Status.surfaces|ForEach-Object {@{surface=$_.surface;revision=$_.applied.revision;font=$_.applied.terminal.font_family}})}
}
function Capture-Picker($Status){
    $before=Request @('settings','show');$pickerBefore=$before.options.font_picker|ConvertTo-Json -Depth 12 -Compress;$raw=(Field $before).value;$handle=[long]$before.options.font_picker.window
    $bitmap=Join-Path $directory 'font-picker.bmp';$png=Join-Path $directory 'font-picker.png';$capture=Request @('chrome-capture',$bitmap);Require ($capture.root_handle -eq $handle) 'Native capture selected the wrong owned font popup';[ChromeFixture]::Png($bitmap,$png)
    $picture=[Drawing.Image]::FromFile($png);try{$width=$picture.Width;$height=$picture.Height}finally{$picture.Dispose()};Require ($width -eq $capture.width -and $height -eq $capture.height -and $width -gt 0 -and $height -gt 0) 'Font PNG dimensions differ from production capture'
    $after=Request @('settings','show');$unchanged=$after.document.revision -eq $before.document.revision -and (Field $after).value -ceq $raw -and ($after.options.font_picker|ConvertTo-Json -Depth 12 -Compress) -ceq $pickerBefore
    $evidence.artifacts+=@{name='font-picker';path=$png;width=$width;height=$height;bytes=(Get-Item -LiteralPath $png).Length;sha256=(Get-FileHash -Algorithm SHA256 -LiteralPath $png).Hash.ToLowerInvariant();stateUnchanged=$unchanged;capture=$capture;scope='Actual owned native client control paint with requested WM_PRINTCLIENT for EDIT and LISTBOX; hidden EDIT pixels may be absent. Root visually reviews content; this assertion covers geometry and unchanged state, not glyph correctness. No titlebar, GPU composition or desktop capture.'}
    Require ($unchanged) 'Native popup capture changed revision, raw font field or picker state';Require ((Get-Item -LiteralPath $png).Length -lt 500KB) 'Font PNG exceeds small artifact limit'
}
function Popup($Status){
    $picker=$Status.options.font_picker;$window=[OptionsFixture]::Describe([long]$picker.window,$owned.Id);$options=[OptionsFixture]::Describe([long]$Status.options.window,$owned.Id);$scale=[Math]::Max(96,$window.Dpi)/96.0
    Require ($window.Owner -eq [long]$Status.options.window -and -not $window.OwnerEnabled -and $window.Enabled -and -not $options.Enabled -and -not [OptionsFixture]::Describe($options.Owner,$owned.Id).Enabled) 'Font picker modal owner chain differs'
    Require ([Math]::Abs($window.Width-460*$scale) -le 3 -and [Math]::Abs($window.Height-420*$scale) -le 3) 'Font picker expected 460x420 DIP outer bounds'
    $size=[ChromeFixture]::Size([long]$picker.window,$owned.Id);$controls=@();foreach($name in @('search','list','choose','cancel')){$handle=[long]$picker.$name;$bounds=[OptionsFixture]::RelativeBounds([long]$picker.window,$handle,$owned.Id);Require ($bounds.X -ge 0 -and $bounds.Y -ge 0 -and $bounds.Width -gt 0 -and $bounds.Height -gt 0 -and $bounds.X+$bounds.Width -le $size[0] -and $bounds.Y+$bounds.Height -le $size[1]) ('Font picker control clipped: '+$name);$controls+=@{name=$name;handle=$handle;bounds=$bounds}}
    $evidence.observations+=@{name='modal-owner-and-native-client-bounds';window=$window;client=$size;controls=$controls}
}
try {
    $doctor=Probe @('doctor');Require ($doctor.background_testing -and $doctor.status -eq 'ok') 'Working hidden debug build required'
    $started=[DateTime]::UtcNow;$startup=[Diagnostics.Stopwatch]::StartNew();$owned=[CliProbe]::Start($gui,@('--temporary','--shell=cmd','--cwd',$directory),$directory,$directory);$hostOut=$owned.StandardOutput.ReadToEndAsync();$hostErr=$owned.StandardError.ReadToEndAsync();$evidence.hosts+=,$owned.Id
    $file=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($owned.Id).json"
    do {Require (-not $owned.HasExited -and $startup.ElapsedMilliseconds -lt 8000) 'Host discovery exceeded eight seconds or exited';if((Test-Path -LiteralPath $file) -and (Get-Item -LiteralPath $file).LastWriteTimeUtc -ge $started){$record=Get-Content -Raw -LiteralPath $file|ConvertFrom-Json;Require ($record.pid -eq $owned.Id -and [bool]$record.pipe) 'Wrong discovery owner';$pipeName=$record.pipe;break};Start-Sleep -Milliseconds 20}while($true)
    $left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup budget exhausted';Require ((Request @('identify') ([int][Math]::Min(5000,$left))).pid -eq $owned.Id) 'Pipe owner mismatch'
    do {$left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup readiness exceeded eight seconds';$tree=Tree ([int][Math]::Min(5000,$left));if(@($tree.surfaces).Count -eq 1 -and $tree.surfaces[0].ready -and $tree.surfaces[0].running){break};Start-Sleep -Milliseconds 20}while($true)
    $evidence.observations+=@{name='startup';elapsedMs=$startup.ElapsedMilliseconds;pipe=$pipeName};$shells=@($tree.surfaces.pid);$originalPid=$tree.surfaces[0].pid;$identities=Identities $tree;$active=(Request @('identify')).surface


    $status=Await {param($s) Ack $s};$entry=@($tree.chrome.controls|Where-Object {$_.kind -eq 'settings'});Require ($entry.Count -eq 1) 'Options entry missing';Native-Click ([long]$entry[0].handle)
    $status=Await {param($s) $s.options -and $s.options.open -and $s.options.font_picker};Require ($status.options.page -eq 'general') 'Font picker belongs on General page'
    $defaultFont=$status.document.terminal.font_family;$custom='"Current 한 é 😀 &", "Malgun Gothic", monospace'
    Request @('settings','set','font-family',$custom)|Out-Null;$status=Await {param($s) $s.document.terminal.font_family -ceq $custom -and (Field $s).value -ceq $custom -and (Settled $s)}
    $status=Open-Picker $status;Popup $status;$before=$status.document.revision
    $currentIndex=Find-Choice $status 'current';$defaultIndex=Find-Choice $status 'default';$installedIndex=Find-Choice $status 'installed';$installed=$status.options.font_picker.choices[$installedIndex]
    Require ($status.options.font_picker.choices[$currentIndex].value -ceq $custom -and [bool]$installed.family -and [bool]$installed.value) 'Current custom raw value or real installed catalog entry missing'
    $evidence.observations+=@{name='catalog-choice-examples';catalog=$status.options.font_picker.catalog;current=$status.options.font_picker.choices[$currentIndex];default=$status.options.font_picker.choices[$defaultIndex];installed=$installed;catalog_scope='Actual GDI installed font enumeration, not a font rendering or glyph coverage assertion'}
    Record 'current-fallback-and-installed-catalog' $status;[OptionsFixture]::PostEnter([long]$status.options.font_picker.cancel,$owned.Id);$status=Await {param($s) -not $s.options.font_picker.open}
    Require ($status.document.revision -eq $before -and (Field $status).value -ceq $custom -and [OptionsFixture]::Describe([long]$status.options.window,$owned.Id).Enabled) 'Enter Cancel saved or lost the current raw fallback value'
    $evidence.checks+=@{name='installed_catalog_current_custom_fallbacks_owned_modal_and_Enter_Cancel';passed=$true}

    $status=Open-Picker $status;$currentIndex=Find-Choice $status 'current';$currentLabel=$status.options.font_picker.choices[$currentIndex].label
    foreach($query in @('한 É','한 é','😀 &')){
        Search $status $query;$status=Await {param($s) $s.options.font_picker.query -ceq $query}
        Require (@($status.options.font_picker.filtered) -contains $currentIndex) ('Canonically equivalent font query missed the current custom choice: '+$query)
        Require ([string]::Equals((Raw-Query $status),$query,[StringComparison]::Ordinal) -and [string]::Equals($status.options.font_picker.choices[$currentIndex].label,$currentLabel,[StringComparison]::Ordinal) -and [string]::Equals($status.options.font_picker.choices[$currentIndex].value,$custom,[StringComparison]::Ordinal) -and [string]::Equals((Field $status).value,$custom,[StringComparison]::Ordinal) -and $status.document.revision -eq $before) 'Font filtering rewrote raw Unicode input, label or settings'
    }
    $evidence.checks+=@{name='canonical_Hangul_and_accent_search_preserves_raw_query_label_and_font';passed=$true}
    $installedIndex=Find-Choice $status 'installed';$installed=$status.options.font_picker.choices[$installedIndex];$query=$installed.family.Substring(0,[Math]::Min(3,$installed.family.Length))
    Search $status $query;$status=Await {param($s) $s.options.font_picker.query -ceq $query -and @($s.options.font_picker.filtered).Count -gt 0}
    foreach($index in @($status.options.font_picker.filtered)){Require ($status.options.font_picker.choices[$index].label.IndexOf($query,[StringComparison]::OrdinalIgnoreCase) -ge 0) 'Font search returned a nonmatching choice'}
    $status=Choose $status $installedIndex;Capture-Picker $status
    Search $status 'FLOWMUX_NO_SUCH_FONT_7e71f5';$status=Await {param($s) $s.options.font_picker.query -ceq 'FLOWMUX_NO_SUCH_FONT_7e71f5' -and @($s.options.font_picker.filtered).Count -eq 0}
    Require (-not [OptionsFixture]::Describe([long]$status.options.font_picker.choose,$owned.Id).Enabled -and $status.document.revision -eq $before) 'No-results search enabled Use font or persisted settings'
    Record 'substring-and-no-results-do-not-save' $status;Native-Click ([long]$status.options.font_picker.cancel);$status=Await {param($s) -not $s.options.font_picker.open};$evidence.checks+=@{name='native_search_substring_and_empty_results_do_not_save';passed=$true}

    $status=Open-Picker $status;$picker=$status.options.font_picker;$rawQuery='한글 한 é 😀 &'
    [OptionsFixture]::CompositionGuard([long]$picker.window,[long]$picker.search,$owned.Id,$true);Search $status $rawQuery
    $status=Await {param($s) $s.options.font_picker.composing -and (Raw-Query $s) -ceq $rawQuery};Request @('settings','set','font-size','17')|Out-Null;$status=Await {param($s) $s.document.terminal.font_size -eq 17 -and (Ack $s)}
    Require ($status.options.font_picker.composing -and (Raw-Query $status) -ceq $rawQuery -and $status.document.terminal.font_family -ceq $custom) 'Watcher update replaced the Unicode composing search or changed font'
    Record 'raw-Unicode-NFD-query-during-composition-and-watcher' $status;[OptionsFixture]::CompositionGuard([long]$picker.window,[long]$picker.search,$owned.Id,$false);$status=Await {param($s) -not $s.options.font_picker.composing -and (Raw-Query $s) -ceq $rawQuery}
    Native-Click ([long]$status.options.font_picker.cancel);$status=Await {param($s) -not $s.options.font_picker.open};$evidence.checks+=@{name='raw_Unicode_NFD_search_survives_controlled_composition_and_external_other_setting';passed=$true;scope='Owned HWND application guard messages only, not physical Korean IME'}

    $status=Open-Picker $status;$installedIndex=Find-Choice $status 'installed';$installed=$status.options.font_picker.choices[$installedIndex];$status=Choose $status $installedIndex;$selectedValue=$installed.value;Require ($selectedValue.EndsWith(', "Malgun Gothic", monospace',[StringComparison]::Ordinal)) 'Installed font replacement lost the original fallback tail'
    [OptionsFixture]::PostEnter([long]$status.options.font_picker.choose,$owned.Id);$status=Await {param($s) -not $s.options.font_picker.open -and $s.document.terminal.font_family -ceq $selectedValue -and (Settled $s)}
    Require ((Field $status).value -ceq $selectedValue -and (Identities (Tree)) -ceq $identities) 'Installed selection did not update row or recreated terminal';Record 'installed-selection-renderer-ack' $status;$evidence.checks+=@{name='native_LISTBOX_selection_Enter_Use_font_persists_renderer_ACK_and_stable_terminal_PID';passed=$true}

    $status=Open-Picker $status;$cancelWinner='"Cancel winner 한", monospace';Request @('settings','set','font-family',$cancelWinner)|Out-Null
    $status=Await {param($s) $s.document.terminal.font_family -ceq $cancelWinner -and (Ack $s)};$winnerRevision=$status.document.revision;[OptionsFixture]::PostEnter([long]$status.options.font_picker.cancel,$owned.Id)
    $status=Await {param($s) -not $s.options.font_picker.open -and (Field $s).value -ceq $cancelWinner -and (Settled $s)};Require ($status.document.revision -eq $winnerRevision) 'Cancel wrote a stale snapshot over the external font winner';Record 'Cancel-refreshes-untouched-external-winner' $status
    $status=Open-Picker $status;$currentIndex=Find-Choice $status 'current';$status=Choose $status $currentIndex;$currentWinner='"Current no-op winner 한", monospace';Request @('settings','set','font-family',$currentWinner)|Out-Null
    $status=Await {param($s) $s.document.terminal.font_family -ceq $currentWinner -and (Ack $s)};$winnerRevision=$status.document.revision;Native-Click ([long]$status.options.font_picker.choose)
    $status=Await {param($s) -not $s.options.font_picker.open -and (Field $s).value -ceq $currentWinner -and (Settled $s)};Require ($status.document.revision -eq $winnerRevision -and -not (Field $status).draft_error) 'Choosing unchanged Current saved stale source or failed to refresh the winning value';Record 'Current-identical-source-no-op-refreshes-winner' $status
    $evidence.checks+=@{name='Cancel_and_identical_Current_refresh_untouched_font_row_after_external_write_without_saving';passed=$true}

    # Controlled messages to the disabled owner's exact child exercise stale-row
    # and composing guards; this is not a claim of possible physical modal input.
    $status=Open-Picker $status;$candidateIndex=Find-Choice $status 'default';$status=Choose $status $candidateIndex;$row=Field $status;$sourceRaw=$row.value;$guardRevision=$status.document.revision
    [OptionsFixture]::CompositionGuard([long]$row.parent,[long]$row.input,$owned.Id,$true);Native-Click ([long]$status.options.font_picker.choose)
    $status=Await {param($s) $s.options.font_picker.error -match 'original font field changed or is composing'};Require ($status.options.font_picker.open -and $status.document.revision -eq $guardRevision -and (Field $status).value -ceq $sourceRaw) 'Use font replaced a composing original row';Record 'original-row-composition-guard' $status
    [OptionsFixture]::CompositionGuard([long]$row.parent,[long]$row.input,$owned.Id,$false);$status=Await {param($s) -not $s.options.composing};$status=Choose $status $candidateIndex;$status=Await {param($s) -not $s.options.font_picker.error}
    [OptionsFixture]::SetTextAndNotify([long]$row.parent,[long]$row.input,$owned.Id,'');Native-Click ([long]$status.options.font_picker.choose)
    $status=Await {param($s) $s.options.font_picker.error -match 'original font field changed or is composing' -and (Field $s).value -ceq ''};Require ($status.options.font_picker.open -and $status.document.revision -eq $guardRevision) 'Use font overwrote or saved a changed original row';Record 'stale-original-row-draft-preserved' $status
    [OptionsFixture]::SetTextAndNotify([long]$row.parent,[long]$row.input,$owned.Id,$sourceRaw);Native-Click ([long]$status.options.font_picker.cancel);$status=Await {param($s) -not $s.options.font_picker.open -and (Settled $s)}
    Require ($status.document.revision -eq $guardRevision -and (Field $status).value -ceq $sourceRaw) 'Guard cleanup changed persisted font';$evidence.checks+=@{name='controlled_original_row_composition_and_stale_raw_draft_block_picker_replacement';passed=$true;scope='Owned native messages to disabled parent controls; no physical input'}

    $status=Open-Picker $status;$candidateIndex=Find-Choice $status 'default';$status=Choose $status $candidateIndex;$candidate=$status.options.font_picker.choices[$candidateIndex].value;$external='"External winner 한", monospace'
    Request @('settings','set','font-family',$external)|Out-Null;$status=Await {param($s) $s.document.terminal.font_family -ceq $external -and (Ack $s)};Native-Click ([long]$status.options.font_picker.choose)
    $status=Await {param($s) -not $s.options.pending -and (Field $s).draft_error -match 'changed elsewhere|conflict'}
    Require ($status.document.terminal.font_family -ceq $external -and (Field $status).value -ceq $candidate) 'Stale picker overwrote the external winner or lost its candidate draft';Record 'picker-source-CAS-conflict' $status
    if($status.options.font_picker.open){Native-Click ([long]$status.options.font_picker.cancel);$status=Await {param($s) -not $s.options.font_picker.open}}
    Native-Click ([long]$status.options.reload);$status=Await {param($s) (Field $s).value -ceq $external -and -not (Field $s).draft_error};$evidence.checks+=@{name='stale_picker_source_CAS_preserves_external_winner_and_local_candidate';passed=$true}

    $status=Open-Picker $status;$defaultIndex=Find-Choice $status 'default';$status=Choose $status $defaultIndex;Native-Click ([long]$status.options.font_picker.choose)
    $status=Await {param($s) -not $s.options.font_picker.open -and $s.document.terminal.font_family -ceq $defaultFont -and (Settled $s)}
    Require ($status.document.terminal.font_size -eq 17 -and (Identities (Tree)) -ceq $identities -and [OptionsFixture]::Describe([long]$status.options.window,$owned.Id).Enabled) 'Default font changed other settings, recreated terminal or left owner disabled'
    Record 'default-font-chain-and-clean-owner' $status;$evidence.checks+=@{name='explicit_default_restores_default_font_chain_only';passed=$true}

    $row=Field $status;Require ($row.baseline -ceq $defaultFont -and $row.value -ceq $defaultFont) 'Baseline-equal conflict requires a saved default font'
    [OptionsFixture]::SetTextAndNotify([long]$row.parent,[long]$row.input,$owned.Id,'');$status=Await {param($s) (Field $s).value -ceq '' -and [bool](Field $s).draft_error -and (Settled $s)}
    Require ((Field $status).baseline -ceq $defaultFont -and $status.document.terminal.font_family -ceq $defaultFont) 'Invalid empty draft changed its saved baseline'
    $status=Open-Picker $status;Require ($status.options.font_picker.source_raw -ceq '') 'Picker did not preserve the invalid empty source draft';$defaultIndex=Find-Choice $status 'default';$status=Choose $status $defaultIndex
    $baselineWinner='"Baseline conflict winner 한", monospace';Request @('settings','set','font-family',$baselineWinner)|Out-Null;$status=Await {param($s) $s.document.terminal.font_family -ceq $baselineWinner -and (Ack $s)};$winnerRevision=$status.document.revision;Native-Click ([long]$status.options.font_picker.choose)
    $status=Await {param($s) -not $s.options.font_picker.open -and (Field $s).draft_error -match 'changed elsewhere|conflict' -and (Settled $s)}
    Require ((Field $status).value -ceq $defaultFont -and (Field $status).baseline -ceq $defaultFont -and $status.document.terminal.font_family -ceq $baselineWinner -and $status.document.revision -eq $winnerRevision) 'Explicit old-baseline choice lost its conflict draft or overwrote the external winner';$heldError=(Field $status).draft_error;Record 'explicit-old-baseline-conflict' $status
    Request @('settings','set','font-size','18')|Out-Null;$status=Await {param($s) $s.document.terminal.font_size -eq 18 -and (Settled $s)}
    Require ((Field $status).value -ceq $defaultFont -and (Field $status).baseline -ceq $defaultFont -and (Field $status).draft_error -ceq $heldError -and $status.document.terminal.font_family -ceq $baselineWinner) 'Unrelated settings update overwrote an errored draft equal to its old baseline';Record 'baseline-equal-error-survives-unrelated-write' $status
    Native-Click ([long]$status.options.reload);$status=Await {param($s) (Field $s).value -ceq $baselineWinner -and (Field $s).baseline -ceq $baselineWinner -and -not (Field $s).draft_error}
    Request @('settings','set','font-family',$defaultFont)|Out-Null;Request @('settings','set','font-size','17')|Out-Null;$status=Await {param($s) $s.document.terminal.font_family -ceq $defaultFont -and $s.document.terminal.font_size -eq 17 -and (Field $s).value -ceq $defaultFont -and (Settled $s)}
    Require ((Identities (Tree)) -ceq $identities) 'Baseline conflict or cleanup recreated the terminal';$evidence.checks+=@{name='explicit_old_baseline_conflict_draft_error_survives_unrelated_settings_update_until_Reload';passed=$true};$evidence.finalSettings=$status;$evidence.status='passed'

} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
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
    if($cleanupErrors.Count){$evidence.status='failed';$evidence.cleanupErrors=$cleanupErrors};$evidence.clientPids=$clients;$evidence.shells=$shells;$evidence.elapsedMs=$clock.ElapsedMilliseconds;$evidence.finished=[DateTime]::UtcNow.ToString('o');if ($evidence.status -eq 'failed') { $evidence|ConvertTo-Json -Depth 50|Set-Content -Encoding UTF8 (Join-Path $directory 'native-font-picker-background.json') };if ($evidence.status -eq 'failed') { Write-Output ('Evidence: '+$directory) }
}
if($cleanupErrors.Count){throw ($cleanupErrors -join '; ')}
[ordered]@{status=$evidence.status;checks=$evidence.checks.Count;elapsedMs=$evidence.elapsedMs}|ConvertTo-Json -Compress
