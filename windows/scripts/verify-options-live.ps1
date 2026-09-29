# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden owned live Options; 50s work + bounded cleanup, outer Job60s.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",[ValidateSet('all','about','focus','cursor')][string]$Case='all',[switch]$Capture)
if (-not $env:FLOWMUX_TEST_ARTIFACT_ROOT) { throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.' }
$ErrorActionPreference='Stop';$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path;$cli=Join-Path $BuildDirectory 'flowmuxctl.exe';$gui=Join-Path $BuildDirectory 'flowmux.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'OptionsFixture.cs')
Add-Type -ReferencedAssemblies System.Drawing -Path (Join-Path $PSScriptRoot 'ChromeFixture.cs')
. (Join-Path $PSScriptRoot 'OptionsEvidence.ps1')
$directory=Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT ('options-live-'+[guid]::NewGuid());[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$clock=[Diagnostics.Stopwatch]::StartNew();$owned=$null;$pipeName=$null;$clients=@();$shells=@();$cleaning=$false;$cleanupErrors=@();$hostOut=$null;$hostErr=$null
$evidence=[ordered]@{started=[DateTime]::UtcNow.ToString('o');mode='hidden-native-options-live';hosts=@();checks=@();observations=@();desktopInput=$false;clipboardAccess=$false;imeGuardMessageSimulation=$true;physicalIme=$false;deferred='Owned WM_IME_START/END messages exercise only application guards; they do not establish OS Korean IME/TSF correctness. Physical keyboard/focus/IME, per-monitor DPI, accessibility and composed visual acceptance are not established.'}
function Require([bool]$Condition,[string]$Message){if(-not $Condition){throw $Message}}
function Budget([int]$Maximum=5000){if($cleaning){return $Maximum};$left=50000-$clock.ElapsedMilliseconds;Require ($left -gt 0) 'Options work budget expired';return [int][Math]::Min($Maximum,$left)}
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
function Await([scriptblock]$Condition){$wait=[Diagnostics.Stopwatch]::StartNew();$last=$null;do{$left=5000-$wait.ElapsedMilliseconds;if($left -le 0){$evidence.observations+=@{kind='condition-timeout';settings=$last};throw 'Live Options condition exceeded five seconds'};$s=Request @('settings','show') ([int]$left);if($s.options){[OptionsFixture]::Describe([long]$s.options.window,$owned.Id)|Out-Null};$last=$s;if(& $Condition $s){return $s};Start-Sleep -Milliseconds 20}while($true)}
function Field($Status,[string]$Key){$rows=@($Status.options.controls|Where-Object {$_.key -eq $Key});Require ($rows.Count -eq 1) ('Missing Options field '+$Key);return $rows[0]}
function Click($Status,[long]$Control){[OptionsFixture]::Click([long]$Status.options.window,$Control,$owned.Id)}
function Record([string]$Name,$Status){
    # Keep every existing owned HWND read/guard; compact only the saved evidence.
    $window=[OptionsFixture]::Describe([long]$Status.options.window,$owned.Id);$controls=@([ChromeFixture]::Read([long]$Status.options.window,$owned.Id));$viewport=@([ChromeFixture]::Read([long]$Status.options.viewport,$owned.Id))
    $record=Get-OptionsEvidenceSummary $Name $Status 'options';$record.window=$window;$record.nativeCounts=@{controls=$controls.Count;viewportControls=$viewport.Count}
    if(-not $script:optionsEvidenceInventoryRecorded){$record.catalog=$Status.options.keybindings.actions;$record.controls=$controls;$record.viewportControls=$viewport;$script:optionsEvidenceInventoryRecorded=$true}
    $evidence.observations+=$record;$evidence.finalSettings=$Status
}

function Parent-Of($Status,$Row){Require ([bool]$Row.parent) 'Live Options input parent diagnostic missing';Require ([OptionsFixture]::Parent([long]$Row.input,$owned.Id) -eq [long]$Row.parent) 'Options input parent mismatch';return [long]$Row.parent}
function Edit($Status,[string]$Key,[string]$Value){$row=Field $Status $Key;[OptionsFixture]::SetText((Parent-Of $Status $row),[long]$row.input,$owned.Id,$Value)}
function Guard($Status,[string]$Key,[bool]$Active){$row=Field $Status $Key;[OptionsFixture]::CompositionGuard((Parent-Of $Status $row),[long]$row.input,$owned.Id,$Active)}
function Select-Field($Status,[string]$Key,[int]$Index){$row=Field $Status $Key;[OptionsFixture]::Select((Parent-Of $Status $row),[long]$row.input,$owned.Id,$Index)}
function Reveal-Field($Status,[string]$Key){
    $wait=[Diagnostics.Stopwatch]::StartNew();$row=Field $Status $Key;$size=[ChromeFixture]::Size([long]$Status.options.viewport,$owned.Id)
    do {
        $bounds=[OptionsFixture]::RelativeBounds([long]$row.parent,[long]$row.input,$owned.Id)
        if($bounds.Y -ge 0 -and $bounds.Y+$bounds.Height -le $size[1]){return $Status}
        Require ($wait.ElapsedMilliseconds -lt 3000) ('General field cannot be reached: '+$Key)
        [OptionsFixture]::ScrollLine([long]$Status.options.viewport,$owned.Id,($bounds.Y -ge 0));$Status=Request @('settings','show')
    }while($true)
}
function Capture-Options($Status,[string]$Name){
    if(-not $Capture){return}
    $bitmap=Join-Path $directory ($Name+'.bmp');$result=Request @('chrome-capture',$bitmap)
    Require ($result.root_handle -eq $Status.options.window) 'Capture did not target the owned Options window'
    # Hidden native combo captures omit selected text; verify its real HWND text separately.
    $choice=Field $Status $(if($Name -match 'bottom'){'cursor_style'}else{'persist_browser_session'})
    $selected=[OptionsFixture]::Text([long]$choice.input,$owned.Id)
    Require ($selected -ceq $(if($Name -match 'bottom'){'Block'}else{$choice.label})) ('Native choice display text differs: '+$selected)
    [ChromeFixture]::Png($bitmap,(Join-Path $directory ($Name+'.png')));Remove-Item -LiteralPath $bitmap
}
function Verify-Cursor($Status){
    $Status=Reveal-Field $Status 'cursor_blink_interval_ms';$key='cursor_blink_interval_ms'
    foreach($ms in @(100,2000,530)){
        Edit $Status $key ([string]$ms)
        $Status=Await {param($s) $s.document.terminal.cursor_blink_interval_ms -eq $ms -and -not $s.options.pending -and (Ack $s)}
        Require (@($Status.surfaces|Where-Object {$_.applied.cursor_animation_duration_ms -ne 2*$ms}).Count -eq 0) 'Actual cursor CSS duration did not follow the interval'
    }
    $evidence.checks+=@{name='native_cursor_interval_boundaries_update_actual_WebView_cursor_CSS_duration';passed=$true}
    foreach($value in @('99','2001','530.5','한글 한 é 😀')){
        Edit $Status $key $value;$Status=Await {param($s) (Field $s $key).draft_error -and -not $s.options.pending}
        Require ($Status.document.terminal.cursor_blink_interval_ms -eq 530 -and (Field $Status $key).value -ceq $value) 'Invalid cursor interval changed settings or lost its Unicode draft'
    }
    Guard $Status $key $true;Edit $Status $key '250';$Status=Request @('settings','show')
    Require ($Status.options.composing -and $Status.document.terminal.cursor_blink_interval_ms -eq 530 -and (Field $Status $key).value -ceq '250') 'Cursor interval committed during composition guard'
    Guard $Status $key $false;$Status=Await {param($s) -not $s.options.composing -and -not $s.options.pending -and $s.document.terminal.cursor_blink_interval_ms -eq 250 -and (Ack $s)}
    $evidence.checks+=@{name='cursor_interval_rejects_out_of_range_and_Unicode_drafts_and_defers_during_owned_IME_guard';passed=$true}
    foreach($shape in @('block','underline','bar')){
        Request @('settings','set','cursor-style',$shape)|Out-Null
        foreach($blink in @($false,$true)){
            Request @('settings','set','cursor-blink',([string]$blink).ToLowerInvariant())|Out-Null
            $Status=Await {param($s) (Ack $s) -and $s.document.terminal.cursor_style -eq $shape -and $s.document.terminal.cursor_blink -eq $blink}
            Require (@($Status.surfaces|Where-Object {$_.applied.cursor_animation_duration_ms -ne 500}).Count -eq 0) 'Cursor shape or blink toggle reset its CSS duration'
        }
    }
    Require ((Identities (Tree)) -ceq $identities) 'Cursor setting restarted the terminal'
    $evidence.checks+=@{name='cursor_shapes_and_blink_toggle_keep_interval_and_original_shell_identity';passed=$true;scope='computed duration on hidden cursor; physical focus and animation cadence not established'}
    return $Status
}
function Verify-Focus($Status){
    $color=Field $Status 'focus_border_color';$opacity=Field $Status 'focus_border_opacity'
    foreach($row in @($color,$opacity)){Require ((([OptionsFixture]::Describe([long]$row.input,$owned.Id)).Style -band 0x10000000) -ne 0) 'General focus field is hidden'}
    $picker=[long]$Status.options.focus_color_picker;Require ([OptionsFixture]::Parent($picker,$owned.Id) -eq $Status.options.viewport) 'Focus color picker belongs to another viewport'
    $bounds=[OptionsFixture]::RelativeBounds([long]$color.parent,[long]$color.input,$owned.Id);$button=[OptionsFixture]::RelativeBounds([long]$color.parent,$picker,$owned.Id)
    Require ($button.X -ge $bounds.X+$bounds.Width -and $button.Y -eq $bounds.Y) 'Focus hex input overlaps its native chooser'
    $revision=$Status.document.revision;[OptionsFixture]::Click([long]$color.parent,$picker,$owned.Id);$Status=Await {param($s) $s.options.error_or_status -match 'disabled during hidden verification'}
    Require ($Status.document.revision -eq $revision) 'Hidden native color picker changed settings'
    Edit $Status 'focus_border_color' '#한글 한';$Status=Await {param($s) (Field $s 'focus_border_color').draft_error -and -not $s.options.pending}
    Edit $Status 'focus_border_opacity' '101';$Status=Await {param($s) (Field $s 'focus_border_opacity').draft_error -and -not $s.options.pending}
    Request @('settings','set','theme','light')|Out-Null;$Status=Await {param($s) $s.document.terminal.theme -eq 'light' -and (Ack $s)}
    Require ((Field $Status 'focus_border_color').value -ceq '#한글 한' -and (Field $Status 'focus_border_opacity').value -ceq '101' -and $Status.document.terminal.focus_border_color -ceq '#fff4b3' -and $Status.document.terminal.focus_border_opacity -eq 30) 'Invalid focus drafts changed saved values or disappeared after theme update'
    $evidence.checks+=@{name='focus_fields_and_chooser_keep_invalid_Unicode_and_opacity_drafts_without_opening_desktop_dialog';passed=$true}
    Guard $Status 'focus_border_color' $true;Edit $Status 'focus_border_color' '#12ABEF';[OptionsFixture]::Click([long]$color.parent,$picker,$owned.Id)
    $Status=Await {param($s) $s.options.composing -and (Field $s 'focus_border_color').value -ceq '#12ABEF'}
    Require ($Status.document.terminal.focus_border_color -ceq '#fff4b3' -and ([OptionsFixture]::Describe([long]$Status.options.window,$owned.Id)).Enabled) 'Chooser interrupted composition or committed the guarded color'
    Guard $Status 'focus_border_color' $false;$Status=Await {param($s) $s.document.terminal.focus_border_color -ceq '#12abef' -and -not $s.options.pending -and (Ack $s)}
    foreach($value in @('0','37','100')){Edit $Status 'focus_border_opacity' $value;$Status=Await {param($s) $s.document.terminal.focus_border_opacity -eq [int]$value -and -not $s.options.pending -and (Ack $s)}}
    Require ((Identities (Tree)) -ceq $identities) 'Focus options restarted a terminal'
    $evidence.checks+=@{name='focus_color_guard_commits_normalized_HEX_then_opacity_boundaries_apply_without_restarting_terminals';passed=$true;scope='application message guard only, not OS Korean IME/TSF'}
    return $Status
}
function Verify-About($Status){
    $window=[OptionsFixture]::Describe([long]$Status.options.window,$owned.Id);$client=[ChromeFixture]::Size([long]$Status.options.window,$owned.Id)
    [ChromeFixture]::Resize([long]$Status.options.window,$owned.Id,[int](620*$scale-$window.Width+$client[0]),[int](400*$scale-$window.Height+$client[1]))
    foreach($page in 0..2){
        $name=@('general','theme','keybindings')[$page];Click $Status ([long]$Status.options.tabs[$page].handle);$Status=Await {param($s) $s.options.page -eq $name}
        $reset=if($page -eq 2){$Status.options.keybindings.reset}else{$Status.options.reset};$right=0;$size=[ChromeFixture]::Size([long]$Status.options.window,$owned.Id)
        foreach($control in @($reset,$Status.options.reload,$Status.options.about_button,$Status.options.close)){
            $bounds=[OptionsFixture]::RelativeBounds([long]$Status.options.window,[long]$control,$owned.Id)
            Require ($bounds.X -ge $right -and $bounds.Width -gt 0 -and $bounds.X+$bounds.Width -le $size[0] -and $bounds.Y -ge 0 -and $bounds.Y+$bounds.Height -le $size[1]) ('Overlapping or clipped Options footer on '+$name);$right=$bounds.X+$bounds.Width
        }
    }
    Click $Status ([long]$Status.options.tabs[0].handle);$Status=Await {param($s) $s.options.page -eq 'general'}
    $evidence.checks+=@{name='about_footer_fits_all_pages_at_minimum_window_size';passed=$true}
    $revision=$Status.document.revision;Click $Status ([long]$Status.options.about_button);$Status=Await {param($s) [bool]$s.options.about};$about=$Status.options.about
    $native=[OptionsFixture]::Describe([long]$about.window,$owned.Id)
    Require ($native.Title -ceq 'About' -and $native.Owner -eq $Status.options.window -and -not $native.OwnerEnabled -and $native.Enabled -and -not $about.native_visible -and $Status.document.revision -eq $revision) 'About ownership, hidden state or settings revision changed'
    $version=[regex]::Match((Get-Content -Raw (Join-Path $PSScriptRoot '..\Cargo.toml')),'(?m)^version = "([^"]+)"').Groups[1].Value
    $body=[OptionsFixture]::Text([long]$about.body_handle,$owned.Id)
    foreach($text in @('flowmux - Agent Workflow Multiplexer Terminal','flowmux was inspired by the cmux (macOS) project.','Maintained by JSUYA (Junsu Choi).','https://github.com/flowmux-ai/flowmux',('Version: v'+$version),'License: GPL-3.0-or-later')){Require ($body.Contains($text)) ('About text missing: '+$text)}
    $controls=@([ChromeFixture]::Read([long]$about.window,$owned.Id));$buttons=@($controls|Where-Object {$_.Shown -and $_.Class -eq 'Button'});$edit=@($controls|Where-Object {$_.Handle -eq $about.body_handle})
    Require ($edit.Count -eq 1 -and ($edit[0].Style -band 0x800) -ne 0 -and $buttons.Count -eq 1 -and $buttons[0].Text -ceq 'OK') 'About body must be read-only with one OK button'
    [OptionsFixture]::Click([long]$tree.window_handle,[long]$entry[0].handle,$owned.Id);$Status=Await {param($s) $s.options.about.id -eq $about.id};Require ($Status.options.about.window -eq $about.window) 'Settings entry duplicated About'
    foreach($color in @('light','dark')){
        Request @('settings','set','theme',$color)|Out-Null;$Status=Await {param($s) $s.document.terminal.theme -eq $color -and (Ack $s)}
        Require ($Status.options.about.id -eq $about.id -and -not ([OptionsFixture]::Describe([long]$about.window,$owned.Id)).OwnerEnabled) 'Theme update dismissed About or enabled its owner'
        $bitmap=Join-Path $directory ('about-'+$color+'.bmp');$capture=Request @('chrome-capture',$bitmap);Require ($capture.root_handle -eq $about.window) 'Capture did not target owned About popup'
        [ChromeFixture]::Png($bitmap,(Join-Path $directory ('about-'+$color+'.png')));Remove-Item -LiteralPath $bitmap
    }
    $evidence.checks+=@{name='about_linux_text_version_readonly_body_single_popup_and_live_themes';passed=$true}
    foreach($key in @('OK','Enter','Escape')){
        if($key -ne 'OK'){Click $Status ([long]$Status.options.about_button);$Status=Await {param($s) [bool]$s.options.about};$about=$Status.options.about}
        switch($key){'OK'{[OptionsFixture]::Click([long]$about.window,[long]$about.cancel,$owned.Id)}'Enter'{[OptionsFixture]::PostEnter([long]$about.cancel,$owned.Id)}'Escape'{[OptionsFixture]::PostEscape([long]$about.body_handle,$owned.Id)}}
        $Status=Await {param($s) -not $s.options.about};Require ($Status.options.open -and ([OptionsFixture]::Describe([long]$Status.options.window,$owned.Id)).Enabled -and [OptionsFixture]::WindowDestroyed([long]$about.window)) ('About '+$key+' did not restore Options')
    }
    $evidence.checks+=@{name='about_ok_enter_escape_destroy_popup_and_restore_options';passed=$true}
    $font=$Status.document.terminal.font_family;$composed='조합중 한 é 😀 &, monospace';Guard $Status 'font_family' $true;Edit $Status 'font_family' $composed;Click $Status ([long]$Status.options.about_button)
    $Status=Await {param($s) $s.options.composing -and (Field $s 'font_family').value -ceq $composed}
    Require (-not $Status.options.about -and $Status.document.terminal.font_family -ceq $font -and ([OptionsFixture]::Describe([long]$Status.options.window,$owned.Id)).Enabled) 'About interrupted guarded composition or committed its draft'
    Guard $Status 'font_family' $false;$Status=Await {param($s) -not $s.options.composing -and $s.document.terminal.font_family -ceq $composed -and -not $s.options.pending -and $s.options.queued -eq 0 -and (Ack $s)}
    $evidence.checks+=@{name='about_defers_during_owned_ime_guard_and_preserves_raw_unicode';passed=$true;scope='application message guard only, not OS Korean IME/TSF'}
    Edit $Status 'font_size' '2';$Status=Await {param($s) (Field $s 'font_size').draft_error -and -not $s.options.pending};$revision=$Status.document.revision
    Click $Status ([long]$Status.options.about_button);$Status=Await {param($s) [bool]$s.options.about};[OptionsFixture]::PostEscape([long]$Status.options.about.window,$owned.Id);$Status=Await {param($s) -not $s.options.about}
    Require ((Field $Status 'font_size').value -ceq '2' -and (Field $Status 'font_size').draft_error -and (Field $Status 'font_family').value -ceq $composed -and $Status.document.revision -eq $revision -and (Identities (Tree)) -ceq $identities) 'About changed invalid/Unicode drafts, settings revision or terminal identity'
    $evidence.checks+=@{name='about_preserves_invalid_draft_unicode_revision_and_terminal_identity';passed=$true}
    return $Status
}
try {
    $doctor=Probe @('doctor');Require ($doctor.background_testing -and $doctor.status -eq 'ok') 'Working hidden debug build required'
    $started=[DateTime]::UtcNow;$startup=[Diagnostics.Stopwatch]::StartNew();$owned=[CliProbe]::Start($gui,@('--temporary','--shell=cmd','--cwd',$directory),$directory,$directory);$hostOut=$owned.StandardOutput.ReadToEndAsync();$hostErr=$owned.StandardError.ReadToEndAsync();$evidence.hosts+=,$owned.Id
    $file=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($owned.Id).json"
    do {Require (-not $owned.HasExited -and $startup.ElapsedMilliseconds -lt 8000) 'Host discovery exceeded eight seconds or exited';if((Test-Path -LiteralPath $file) -and (Get-Item -LiteralPath $file).LastWriteTimeUtc -ge $started){$record=Get-Content -Raw -LiteralPath $file|ConvertFrom-Json;Require ($record.pid -eq $owned.Id -and [bool]$record.pipe) 'Wrong discovery owner';$pipeName=$record.pipe;break};Start-Sleep -Milliseconds 20}while($true)
    $left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup budget exhausted';Require ((Request @('identify') ([int][Math]::Min(5000,$left))).pid -eq $owned.Id) 'Pipe owner mismatch'
    do {$left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup readiness exceeded eight seconds';$tree=Tree ([int][Math]::Min(5000,$left));if(@($tree.surfaces).Count -eq 1 -and $tree.surfaces[0].ready -and $tree.surfaces[0].running){break};Start-Sleep -Milliseconds 20}while($true)
    $evidence.observations+=@{name='startup';elapsedMs=$startup.ElapsedMilliseconds;pipe=$pipeName};$shells=@($tree.surfaces.pid);$identities=Identities $tree;$active=(Request @('identify')).surface
    $initial=Await {param($s) Ack $s};$defaults=$initial.document.terminal|ConvertTo-Json -Compress
    $entry=@($tree.chrome.controls|Where-Object {$_.kind -eq 'settings'});Require ($entry.Count -eq 1) 'Settings entry missing';[OptionsFixture]::Click([long]$tree.window_handle,[long]$entry[0].handle,$owned.Id)
    $status=Await {param($s) $s.options -and $s.options.open};$window=[OptionsFixture]::Describe([long]$status.options.window,$owned.Id);$scale=[Math]::Max(96,$window.Dpi)/96.0
    Require ($window.Owner -eq $tree.window_handle -and $window.OwnerEnabled -and $window.Enabled -and -not $status.options.modal -and $window.Title -ceq 'Options') 'Options ownership/nonmodal state is wrong'
    Require (($window.Style -band 0x00CF0000) -eq 0x00CF0000 -and [Math]::Abs($window.Width-760*$scale) -le 3 -and [Math]::Abs($window.Height-720*$scale) -le 3) 'Options window style/initial size differs'
    Require (($status.options.tabs.name -join ',') -ceq 'General,Theme,Keybindings' -and $status.options.page -eq 'general') 'Unexpected Options pages'
    $controls=@([ChromeFixture]::Read([long]$status.options.window,$owned.Id));Require (@($controls|Where-Object {$_.Shown -and $_.Class -eq 'Button' -and (($_.Style -band 15) -notin @(3,11) -or $_.Font -eq 0)}).Count -eq 0) 'Options buttons are not themed native controls'


    if($Case -eq 'cursor'){$status=Verify-Cursor $status}elseif($Case -eq 'about'){$status=Verify-About $status}elseif($Case -eq 'focus'){$status=Verify-Focus $status}else{
    Require ($status.options.auto_apply -and [bool]$status.options.viewport) 'Options immediate-apply/viewport diagnostics missing'
    foreach($row in $status.options.controls){Require ($row.value -ceq $row.baseline) ('First open mistook an uninitialized control for an unsaved draft: '+$row.key)}
    $order=@('zoom_percent','font_family','font_size','focus_border_color','focus_border_opacity','persist_browser_session','restore_terminal_scrollback','scrollback','minimap_enabled','minimap_width','minimap_opacity','default_shell','agent_bar_mode','usage_bar_enabled','agent_notification_target','editor_minimap_enabled','cursor_blink','cursor_blink_interval_ms','cursor_style')
    Require ((@($status.options.controls|Where-Object page -eq 'general'|ForEach-Object key) -join ',') -ceq ($order -join ',')) 'General controls differ from the Linux order'
    $font=Field $status 'font_family';$fontBounds=[OptionsFixture]::RelativeBounds([long]$font.parent,[long]$font.input,$owned.Id);$picker=[OptionsFixture]::RelativeBounds([long]$font.parent,[long]$status.options.font_picker.entrybutton,$owned.Id)
    Require ($picker.Y -eq $fontBounds.Y -and $picker.X -ge $fontBounds.X+$fontBounds.Width) 'Font chooser no longer follows its font row'
    $sizeRow=Field $status 'font_size';$sizeBounds=[OptionsFixture]::RelativeBounds([long]$sizeRow.parent,[long]$sizeRow.input,$owned.Id);$focus=Field $status 'focus_border_color';$focusBounds=[OptionsFixture]::RelativeBounds([long]$focus.parent,[long]$focus.input,$owned.Id)
    $engine=$status.options.browser_engine;$engineBounds=[OptionsFixture]::RelativeBounds([long]$status.options.viewport,[long]$engine.value,$owned.Id)
    Require ($engine.name -ceq 'WebView2' -and [OptionsFixture]::Text([long]$engine.value,$owned.Id) -ceq 'WebView2' -and $engineBounds.X -eq $sizeBounds.X -and $engineBounds.Y -ge $sizeBounds.Y+$sizeBounds.Height -and $engineBounds.Y+$engineBounds.Height -le $focusBounds.Y) 'Actual browser engine is not a read-only row between font and focus settings'
    Capture-Options $status 'general-top'
    if($Capture){
        Request @('settings','set','theme','light')|Out-Null;$status=Await {param($s) $s.document.terminal.theme -eq 'light' -and (Ack $s)};Capture-Options $status 'general-top-light'
        Request @('settings','set','theme','dark')|Out-Null;$status=Await {param($s) $s.document.terminal.theme -eq 'dark' -and (Ack $s)}
    }
    $previousBottom=0;$viewSize=[ChromeFixture]::Size([long]$status.options.viewport,$owned.Id)
    foreach($row in @($status.options.controls|Where-Object {$_.page -eq 'general'})){
        $native=[OptionsFixture]::Describe([long]$row.input,$owned.Id);$bounds=[OptionsFixture]::RelativeBounds([long]$row.parent,[long]$row.input,$owned.Id)
        Require (($native.Style -band 0x10000000) -ne 0 -and $bounds.Y -ge $previousBottom -and $bounds.X -ge 0 -and $bounds.Width -gt 0 -and $bounds.X+$bounds.Width -le $viewSize[0]) ('Hidden, overlapping or clipped General field: '+$row.key);$previousBottom=$bounds.Y+30*$scale
    }
    Require (@($status.options.controls|Where-Object {$_.apply}).Count -eq 0) 'Live Options still exposes row Apply controls'
    $children=@([ChromeFixture]::Read([long]$status.options.viewport,$owned.Id));$evidence.observations+=@{name='actual-viewport-groups';captions=@($children|Where-Object {$_.Class -eq 'Static'}|ForEach-Object {$_.Text})};Require (@($children|Where-Object {$_.Text -ceq 'Apply'}).Count -eq 0) 'Apply button remains in native viewport'
    Require (@($children|Where-Object {$_.Shown -and $_.Text -cin @('Terminal','Focus','Minimap','Shell','Agents','Editor','Browser','Session')}).Count -eq 0) 'Windows-only General section headers remain'
    $evidence.checks+=@{name='General_matches_Linux_order_without_extra_sections_and_font_chooser_and_readonly_engine_follow_their_rows';passed=$true}
    $viewport=[OptionsFixture]::RelativeBounds([long]$status.options.window,[long]$status.options.viewport,$owned.Id);$size=[ChromeFixture]::Size([long]$status.options.window,$owned.Id)
    Require ($viewport.X -ge 0 -and $viewport.Y -ge 0 -and $viewport.Width -gt 0 -and $viewport.Height -gt 0 -and $viewport.X+$viewport.Width -le $size[0] -and $viewport.Y+$viewport.Height -le $size[1]) 'Options viewport escapes its client'
    foreach($control in @($status.options.reset,$status.options.reload,$status.options.close)){$bounds=[OptionsFixture]::RelativeBounds([long]$status.options.window,[long]$control,$owned.Id);Require ($bounds.Y -ge $viewport.Y+$viewport.Height -and $bounds.Y+$bounds.Height -le $size[1]) 'Options viewport overlaps or clips its footer'}
    [ChromeFixture]::Resize([long]$status.options.window,$owned.Id,650,600);[OptionsFixture]::Scroll([long]$status.options.viewport,$owned.Id,$true)
    $status=Await {param($s) $s.options.scroll_offset -gt 0};$shell=Field $status 'cursor_style';$rowBounds=[OptionsFixture]::RelativeBounds((Parent-Of $status $shell),[long]$shell.input,$owned.Id);$viewSize=[ChromeFixture]::Size([long]$status.options.viewport,$owned.Id)
    Require ($rowBounds.Y -lt $viewSize[1] -and $rowBounds.Y+$rowBounds.Height -gt 0) 'Last General field cannot be reached at the bottom';Record 'viewport-scrolled-to-cursor' $status;Capture-Options $status 'general-bottom-small'
    [OptionsFixture]::Scroll([long]$status.options.viewport,$owned.Id,$false);$status=Await {param($s) $s.options.scroll_offset -eq 0}
    $evidence.checks+=@{name='owned_nonmodal_live_options_no_apply_and_scrolling_keeps_groups_and_footer_reachable';passed=$true}
    [OptionsFixture]::Scroll([long]$status.options.viewport,$owned.Id,$true);$status=Await {param($s) $s.options.scroll_offset -gt 0};$viewSize=[ChromeFixture]::Size([long]$status.options.viewport,$owned.Id)
    foreach($key in $order){$status=Reveal-Field $status $key}
    [OptionsFixture]::Scroll([long]$status.options.viewport,$owned.Id,$false);$status=Await {param($s) $s.options.scroll_offset -eq 0}
    $evidence.checks+=@{name='all_general_fields_distinct_and_shell_agents_editor_browser_session_controls_scroll_into_view';passed=$true}
    foreach($key in @('persist_browser_session','restore_terminal_scrollback','minimap_enabled','agent_bar_mode','usage_bar_enabled','editor_minimap_enabled','cursor_blink')){
        $status=Reveal-Field $status $key;$row=Field $status $key;$native=[OptionsFixture]::Describe([long]$row.input,$owned.Id)
        Require (($native.Style -band 15) -eq 3 -and ($native.Style -band 0x10000) -ne 0 -and $native.Title -ceq $row.label) ('Boolean setting lacks native checkbox keyboard/accessibility semantics: '+$key)
        $initial=[bool]$status.document.terminal.$key
        foreach($expected in @((-not $initial),$initial)){
            # Real native checkbox Space handling, sent only to this owned hidden HWND.
            [OptionsFixture]::KeyMessage([long]$row.input,$owned.Id,32,57,$false,$false,$false,$false)
            [OptionsFixture]::KeyMessage([long]$row.input,$owned.Id,32,57,$true,$false,$false,$false)
            $status=Await {param($s) $s.document.terminal.$key -eq $expected -and -not $s.options.pending -and $s.options.queued -eq 0 -and (Ack $s)}
            Require ([OptionsFixture]::Checked([long]$row.parent,[long]$row.input,$owned.Id) -eq $expected -and (Field $status $key).value -ceq $expected.ToString().ToLowerInvariant()) ('Native checkbox and saved setting diverged: '+$key)
        }
    }
    Request @('settings','set','cursor-blink','false')|Out-Null;$status=Await {param($s) -not $s.document.terminal.cursor_blink -and (Field $s 'cursor_blink').value -ceq 'false' -and (Ack $s)}
    $row=Field $status 'cursor_blink';Require (-not [OptionsFixture]::Checked([long]$row.parent,[long]$row.input,$owned.Id)) 'External setting did not refresh native checkbox state'
    Request @('settings','set','cursor-blink','true')|Out-Null;$status=Await {param($s) $s.document.terminal.cursor_blink -and (Field $s 'cursor_blink').value -ceq 'true' -and (Ack $s)}
    $evidence.checks+=@{name='seven_native_boolean_checkboxes_toggle_with_owned_Space_auto_save_and_external_refresh';passed=$true}
    foreach($enabled in @($true,$false)){
        Select-Field $status 'agent_bar_mode' $(if($enabled){0}else{1})
        $status=Await {param($s) $s.document.terminal.agent_bar_mode -eq $enabled -and -not $s.options.pending -and $s.options.queued -eq 0 -and (Ack $s)}
        $tree=Tree;$toggle=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'agent_bar'})
        Require ($toggle.Count -eq 1 -and $toggle[0].selected -eq $enabled -and -not $tree.agent_bar -and (Identities $tree) -ceq $identities) 'Live Agents bar option failed to update footer or preserved an empty bar'
    }
    $evidence.checks+=@{name='native_agents_bar_option_auto_applies_updates_footer_and_preserves_terminal_without_agents';passed=$true}
    foreach($choice in @(@(2,'both'),@(1,'workspace'),@(0,'agent_bar'))){
        Select-Field $status 'agent_notification_target' $choice[0]
        $status=Await {param($s) $s.document.terminal.agent_notification_target -ceq $choice[1] -and -not $s.options.pending -and $s.options.queued -eq 0 -and (Ack $s)}
        Require ((Identities (Tree)) -ceq $identities) 'Notification target option restarted a terminal'
    }
    $evidence.checks+=@{name='native_agent_notification_target_choices_auto_apply_and_preserve_terminals';passed=$true}
    $font='Cascadia Mono, "Malgun Gothic", "한글 한 é 😀 &", monospace';Edit $status 'font_family' $font
    $status=Await {param($s) $s.document.terminal.font_family -ceq $font -and -not $s.options.pending -and $s.options.queued -eq 0 -and (Ack $s)}
    Require ([OptionsFixture]::Text([long](Field $status 'font_family').input,$owned.Id) -ceq $font -and (Identities (Tree)) -ceq $identities) 'Immediate Unicode font value or terminal identity changed'
    Record 'unicode-immediate-applied' $status;$evidence.checks+=@{name='raw_unicode_native_edit_applies_without_button_and_reaches_terminal_ack_with_stable_pids';passed=$true}
    $oldSize=$status.document.terminal.font_size
    foreach($invalid in @('','2')){Edit $status 'font_size' $invalid;$status=Await {param($s) (Field $s 'font_size').value -ceq $invalid -and -not $s.options.pending -and [bool](Field $s 'font_size').draft_error};Require (-not $status.config_error -and $status.document.terminal.font_size -eq $oldSize -and [OptionsFixture]::Text([long](Field $status 'font_size').input,$owned.Id) -ceq $invalid) 'Invalid intermediate value changed persisted value or discarded native draft';Record ('invalid-size-'+$invalid.Length) $status}
    Edit $status 'font_size' '16';$status=Await {param($s) $s.document.terminal.font_size -eq 16 -and -not $s.options.pending -and $s.options.queued -eq 0 -and (Ack $s)}
    $evidence.checks+=@{name='invalid_intermediate_drafts_preserved_then_valid_repair_auto_applies';passed=$true}
    $burst='마지막 한 é 😀 & font, monospace';Edit $status 'font_family' '초기, monospace';Edit $status 'font_family' '중간, monospace';Edit $status 'font_family' $burst;Edit $status 'font_size' '18';Edit $status 'minimap_width' '64'
    $status=Await {param($s) $s.document.terminal.font_family -ceq $burst -and $s.document.terminal.font_size -eq 18 -and $s.document.terminal.minimap_width -eq 64 -and -not $s.options.pending -and $s.options.queued -eq 0 -and (Ack $s)}
    Require ((Field $status 'font_family').value -ceq $burst -and (Field $status 'font_size').value -ceq '18' -and (Field $status 'minimap_width').value -ceq '64') 'Older write completion overwrote a newer draft';Record 'burst-multiple-fields' $status
    $evidence.checks+=@{name='rapid_same_row_and_multiple_row_changes_converge_without_lost_latest_drafts';passed=$true}
    $composed='조합중 한 é 😀 &, monospace';Guard $status 'font_family' $true;Edit $status 'font_family' $composed
    $status=Await {param($s) $s.options.composing -and (Field $s 'font_family').value -ceq $composed};Require ($status.document.terminal.font_family -ceq $burst) 'Composition guard committed an unfinished native draft'
    Request @('settings','set','scrollback','12345')|Out-Null;$status=Await {param($s) $s.document.terminal.scrollback -eq 12345 -and (Ack $s)}
    Require ($status.options.composing -and $status.document.terminal.font_family -ceq $burst -and (Field $status 'font_family').value -ceq $composed) 'Unrelated external setting overwrote or committed the composing draft';Record 'composition-held-across-external-write' $status
    Guard $status 'font_family' $false;$status=Await {param($s) -not $s.options.composing -and $s.document.terminal.font_family -ceq $composed -and $s.document.terminal.scrollback -eq 12345 -and -not $s.options.pending -and $s.options.queued -eq 0 -and (Ack $s)}
    $evidence.checks+=@{name='owned_ime_message_guard_defers_draft_preserves_unrelated_external_write_then_commits_after_end';passed=$true;scope='application message guard only, not OS Korean IME/TSF'}
    $hiddenCommit='닫힌 뒤 완료 한 é 😀 &, monospace';Guard $status 'font_family' $true;Edit $status 'font_family' $hiddenCommit
    $status=Await {param($s) $s.options.composing -and (Field $s 'font_family').value -ceq $hiddenCommit};Click $status ([long]$status.options.close)
    $status=Await {param($s) -not $s.options.open};Require ($status.document.terminal.font_family -ceq $composed -and (Field $status 'font_family').value -ceq $hiddenCommit) 'Closing Options discarded or committed a composing draft'
    Guard $status 'font_family' $false;$status=Await {param($s) -not $s.options.open -and -not $s.options.composing -and $s.document.terminal.font_family -ceq $hiddenCommit -and -not $s.options.pending -and $s.options.queued -eq 0 -and (Ack $s)}
    Record 'composition-committed-after-hidden-end' $status;$tree=Tree;$entry=@($tree.chrome.controls|Where-Object {$_.kind -eq 'settings'});[OptionsFixture]::Click([long]$tree.window_handle,[long]$entry[0].handle,$owned.Id)
    $status=Await {param($s) $s.options.open};Require ((Field $status 'font_family').value -ceq $hiddenCommit -and $status.document.terminal.font_family -ceq $hiddenCommit) 'Options reopen replaced the latest hidden composition commit';Record 'hidden-composition-reopened' $status
    $evidence.checks+=@{name='owned_ime_guard_close_hidden_end_and_reopen_preserve_latest_raw_unicode';passed=$true;scope='controlled application message sequence only, not OS Korean IME/TSF'}
    $conflict='보존할 초안 한, monospace';$external='External winner, monospace';Guard $status 'font_family' $true;Edit $status 'font_family' $conflict
    $status=Await {param($s) $s.options.composing -and (Field $s 'font_family').value -ceq $conflict};Request @('settings','set','font-family',$external)|Out-Null;$status=Await {param($s) $s.document.terminal.font_family -ceq $external -and (Ack $s)}
    Require ((Field $status 'font_family').value -ceq $conflict) 'External same-field update erased the local draft';Guard $status 'font_family' $false
    $status=Await {param($s) -not $s.options.composing -and -not $s.options.pending -and (Field $s 'font_family').draft_error -match 'changed elsewhere|conflict'}
    Require ($status.document.terminal.font_family -ceq $external -and (Field $status 'font_family').value -ceq $conflict) 'Stale local auto-save overwrote external same-field value or discarded draft';Record 'same-field-conflict-retained' $status
    Click $status ([long]$status.options.reload);$status=Await {param($s) (Field $s 'font_family').value -ceq $external};$evidence.checks+=@{name='same_field_external_conflict_preserves_winning_value_and_unsaved_draft_until_reload';passed=$true}
    Request @('settings','shell','cmd','--arg','/d')|Out-Null
    $status=Await {param($s) $s.document.default_shell.program -ceq 'cmd' -and (@($s.document.default_shell.args) -join ',') -ceq '/d' -and (Field $s 'default_shell').value -ceq 'cmd' -and (Field $s 'default_shell').baseline -ceq 'cmd' -and (Ack $s)}
    Guard $status 'default_shell' $true;Edit $status 'default_shell' 'powershell';$status=Await {param($s) $s.options.composing -and (Field $s 'default_shell').value -ceq 'powershell'}
    Request @('settings','shell','cmd','--arg','/q')|Out-Null;$status=Await {param($s) $s.document.default_shell.program -ceq 'cmd' -and (@($s.document.default_shell.args) -join ',') -ceq '/q' -and (Ack $s)}
    Require ((Field $status 'default_shell').value -ceq 'powershell') 'Args-only external shell update overwrote composing program draft';Guard $status 'default_shell' $false
    $status=Await {param($s) -not $s.options.composing -and -not $s.options.pending -and (Field $s 'default_shell').draft_error -match 'changed elsewhere|conflict'}
    Require ($status.document.default_shell.program -ceq 'cmd' -and (@($status.document.default_shell.args) -join ',') -ceq '/q' -and (Field $status 'default_shell').value -ceq 'powershell' -and (Identities (Tree)) -ceq $identities) 'Args-only shell conflict advanced stale baseline, overwrote external shell, or restarted an existing terminal'
    Record 'shell-args-only-conflict-retained' $status;$evidence.checks+=@{name='full_shell_baseline_rejects_args_only_external_conflict_while_preserving_local_program_draft';passed=$true}
    Click $status ([long]$status.options.reload);$status=Await {param($s) (Field $s 'default_shell').value -ceq 'cmd' -and -not (Field $s 'default_shell').draft_error}
    Click $status ([long]$status.options.tabs[1].handle);$status=Await {param($s) $s.options.page -eq 'theme'};Select-Field $status 'theme' 1
    $status=Await {param($s) $s.document.terminal.theme -eq 'light' -and -not $s.options.pending -and $s.options.queued -eq 0 -and (Ack $s)};Require (@($status.surfaces|Where-Object {$_.applied.background -ne '#ffffff'}).Count -eq 0) 'Immediate theme selection did not reach terminal palette';Record 'theme-immediate' $status
    Click $status ([long]$status.options.reset);$status=Await {param($s) -not $s.options.pending -and ($s.document.terminal|ConvertTo-Json -Compress) -ceq $defaults -and (Ack $s)}
    Click $status ([long]$status.options.close);$status=Await {param($s) -not $s.options.open};$tree=Tree;$entry=@($tree.chrome.controls|Where-Object {$_.kind -eq 'settings'});[OptionsFixture]::Click([long]$tree.window_handle,[long]$entry[0].handle,$owned.Id)
    $status=Await {param($s) $s.options.open};Require (($status.document.terminal|ConvertTo-Json -Compress) -ceq $defaults -and (Field $status 'font_size').value -ceq '14' -and (Identities (Tree)) -ceq $identities -and (Request @('identify')).surface -eq $active) 'Reset/close/reopen lost persisted values or terminal identity'
    Record 'reset-close-reopen' $status;$evidence.checks+=@{name='theme_reset_and_options_reopen_preserve_defaults_terminal_pids_and_active_surface';passed=$true}
    }
    Click $status ([long]$status.options.close);Await {param($s) -not $s.options.open}|Out-Null
    Request @('quit','--discard-state')|Out-Null;Require ($owned.WaitForExit((Budget 5000)) -and $owned.ExitCode -eq 0) 'Owned host did not quit cleanly';$evidence.status='passed_hidden_options_live_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;$evidence.failureSettings=$status;throw}
finally {
    $cleaning=$true
    if($owned){try{if(-not $owned.HasExited){$stop=[Diagnostics.Stopwatch]::StartNew();if($pipeName){Request @('quit','--discard-state') 2500|Out-Null};if(-not $owned.WaitForExit([int][Math]::Max(1,5000-$stop.ElapsedMilliseconds))){throw 'Owned host cleanup deadline exceeded'}}}catch{$cleanupErrors+=$_.Exception.Message;if(-not $owned.HasExited){$owned.Kill();[CliProbe]::WaitAfterKill($owned)}}
        $evidence.observations+=@{name='host-exit';pid=$owned.Id;exitCode=$owned.ExitCode;stdoutComplete=$hostOut.Wait(500);stderrComplete=$hostErr.Wait(500);stdout=[CliProbe]::Output($hostOut);stderr=[CliProbe]::Output($hostErr)};$owned.Dispose()}
    if($cleanupErrors.Count){$evidence.status='failed';$evidence.cleanupErrors=$cleanupErrors};$evidence.clientPids=$clients;$evidence.shells=$shells;$evidence.elapsedMs=$clock.ElapsedMilliseconds;$evidence.finished=[DateTime]::UtcNow.ToString('o');if($evidence.status -eq 'failed'){$evidence|ConvertTo-Json -Depth 50|Set-Content -Encoding UTF8 (Join-Path $directory 'native-options-live-background.json')};if($evidence.status -eq 'failed'){Write-Output ('Evidence: '+$directory)}
}
if($cleanupErrors.Count){throw ($cleanupErrors -join '; ')}
[ordered]@{status=$evidence.status;checks=$evidence.checks.Count;elapsedMs=$evidence.elapsedMs}|ConvertTo-Json -Compress
