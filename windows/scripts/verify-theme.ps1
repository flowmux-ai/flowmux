# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden owned Theme; 50s work + bounded cleanup, outer Job60s.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
if (-not $env:FLOWMUX_TEST_ARTIFACT_ROOT) { throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.' }
$ErrorActionPreference='Stop';$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path;$cli=Join-Path $BuildDirectory 'flowmuxctl.exe';$gui=Join-Path $BuildDirectory 'flowmux.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'OptionsFixture.cs')
Add-Type -ReferencedAssemblies System.Drawing -Path (Join-Path $PSScriptRoot 'ChromeFixture.cs')
$directory=Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT ('theme-'+[guid]::NewGuid());[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$clock=[Diagnostics.Stopwatch]::StartNew();$owned=$null;$pipeName=$null;$clients=@();$shells=@();$cleaning=$false;$cleanupErrors=@();$hostOut=$null;$hostErr=$null
$evidence=[ordered]@{started=[DateTime]::UtcNow.ToString('o');mode='hidden-native-theme';hosts=@();checks=@();observations=@();artifacts=@();desktopInput=$false;clipboardAccess=$false;imeGuardMessageSimulation=$true;physicalIme=$false;deferred='Owned HWND messages exercise application guards, not physical Korean OS IME/TSF. ChooseColorW is intentionally blocked in hidden mode; real picker selection/Cancel, desktop focus, per-monitor DPI, accessibility and composed GPU rendering remain unverified.'}
function Require([bool]$Condition,[string]$Message){if(-not $Condition){throw $Message}}
function Budget([int]$Maximum=5000){if($cleaning){return $Maximum};$left=50000-$clock.ElapsedMilliseconds;Require ($left -gt 0) 'Theme work budget expired';return [int][Math]::Min($Maximum,$left)}
function Probe([string[]]$Arguments,[int]$Maximum=5000,[bool]$Reject=$false){
    Budget|Out-Null;$p=[CliProbe]::Start($cli,$Arguments,$directory,$directory);$script:clients+=,$p.Id;$out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
    try {Require ($p.WaitForExit((Budget $Maximum))) 'Owned CLI deadline exceeded; no retry';Require ($out.Wait(500) -and $err.Wait(500)) 'CLI output did not close';if($Reject){Require ($p.ExitCode -ne 0) 'Expected CLI conflict was accepted';return ([CliProbe]::Output($err)|ConvertFrom-Json)};Require ($p.ExitCode -eq 0) ('CLI failed: '+[CliProbe]::Output($err));return ([CliProbe]::Output($out)|ConvertFrom-Json)}
    catch {$evidence.observations+=@{kind='cli-failure';arguments=$Arguments;stdout=[CliProbe]::Output($out);stderr=[CliProbe]::Output($err)};throw}
    finally {if(-not $p.HasExited){$p.Kill();[CliProbe]::WaitAfterKill($p)};$p.Dispose()}
}
function Request([string[]]$Arguments,[int]$Maximum=5000,[bool]$Reject=$false){Require ([bool]$pipeName) 'Explicit owned pipe required';return Probe (@('--pipe',$pipeName,'--json')+$Arguments) $Maximum $Reject}
function Tree([int]$Maximum=5000){$t=Request @('tree') $Maximum;Require ($t.background_testing) 'Host is not in background mode';[OptionsFixture]::Describe([long]$t.window_handle,$owned.Id)|Out-Null;return $t}
function Identities($Tree){return (@($Tree.surfaces|Sort-Object id|ForEach-Object {$_.id.ToString()+':'+$_.pid.ToString()}) -join ',')}
function Ack($Status){return @($Status.surfaces).Count -gt 0 -and @($Status.surfaces|Where-Object {-not $_.applied -or $_.applied.revision -ne $Status.document.revision -or -not (Colors-Equal $_.applied.colors $Status.colors)}).Count -eq 0}
function Await([scriptblock]$Condition){$wait=[Diagnostics.Stopwatch]::StartNew();$last=$null;do{$left=5000-$wait.ElapsedMilliseconds;if($left -le 0){$evidence.observations+=@{kind='condition-timeout';settings=$last};throw 'Theme condition exceeded five seconds'};$s=Request @('settings','show') ([int]$left);if($s.options){[OptionsFixture]::Describe([long]$s.options.window,$owned.Id)|Out-Null};$last=$s;if(& $Condition $s){return $s};Start-Sleep -Milliseconds 20}while($true)}
function Click($Status,[long]$Control){[OptionsFixture]::Click([long]$Status.options.window,$Control,$owned.Id)}
function Colors-Equal($Actual,$Expected){
    if(-not $Actual -or -not $Expected -or @($Actual.palette).Count -ne 16 -or @($Expected.palette).Count -ne 16){return $false}
    foreach($key in @('background','foreground','cursor','selection_background','selection_foreground','dark')){if($Actual.$key -cne $Expected.$key){return $false}}
    return (@($Actual.palette) -join ',') -ceq (@($Expected.palette) -join ',')
}
function Settled($Status){return -not $Status.options.pending -and $Status.options.queued -eq 0 -and (Ack $Status)}
function Parent([long]$Handle){return [OptionsFixture]::Parent($Handle,$owned.Id)}
function Native-Click([long]$Handle){[OptionsFixture]::Click((Parent $Handle),$Handle,$owned.Id)}
function Field($Status,[string]$Key){$rows=@($Status.options.controls|Where-Object {$_.key -ceq $Key});Require ($rows.Count -eq 1) ('Missing Theme setting row '+$Key);return $rows[0]}
function Color-Field($Status,[string]$Key){$rows=@($Status.options.theme_panel.fields|Where-Object {$_.key -ceq $Key});Require ($rows.Count -eq 1) ('Missing Theme color row '+$Key);return $rows[0]}
function Edit($Status,[string]$Key,[string]$Text){$row=Field $Status $Key;[OptionsFixture]::SetTextAndNotify([long]$row.parent,[long]$row.input,$owned.Id,$Text)}
function Guard($Status,[string]$Key,[bool]$Active){$row=Field $Status $Key;[OptionsFixture]::CompositionGuard([long]$row.parent,[long]$row.input,$owned.Id,$Active)}
function Reveal($Status,[long]$Handle){
    $watch=[Diagnostics.Stopwatch]::StartNew();do{Budget|Out-Null;Require ($watch.ElapsedMilliseconds -lt 5000) 'Theme control cannot be reached within five seconds';$bounds=[OptionsFixture]::RelativeBounds([long]$Status.options.viewport,$Handle,$owned.Id);$size=[ChromeFixture]::Size([long]$Status.options.viewport,$owned.Id);if($bounds.Y -ge 0 -and $bounds.Y+$bounds.Height -le $size[1]){return};[OptionsFixture]::ScrollPage([long]$Status.options.viewport,$owned.Id,($bounds.Y -gt 0));$Status=Request @('settings','show') ([int][Math]::Max(1,5000-$watch.ElapsedMilliseconds))}while($true)
}
function Select-Preset($Status,[string]$Id){$preset=@($Status.options.theme_panel.presets|Where-Object {$_.id -ceq $Id});Require ($preset.Count -eq 1) ('Missing preset '+$Id);Reveal $Status ([long]$preset[0].button);Native-Click ([long]$preset[0].button);return Await {param($s) $s.document.terminal.theme_preset -ceq $Id -and (Settled $s)}}
function Record([string]$Name,$Status){
    $evidence.observations+=@{name=$Name;revision=$Status.document.revision;terminal=$Status.document.terminal;colors=$Status.colors;options=@{window=$Status.options.window;page=$Status.options.page;pending=$Status.options.pending;queued=$Status.options.queued;composing=$Status.options.composing;error=$Status.options.error_or_status};fields=$Status.options.theme_panel.fields;picker_status=$Status.options.theme_panel.picker_status;acks=@($Status.surfaces|ForEach-Object {@{surface=$_.surface;revision=$_.applied.revision;colors=$_.applied.colors}})}
}
function Editor-Read([int]$Maximum=5000){$reply=Request @('editor','command',$editorSurface,'read') $Maximum;if($reply.psobject.Properties.Name -contains 'result'){$reply=$reply.result};Require ($reply.document_focused -eq $false -and -not $reply.content_truncated) 'Owned editor focused or truncated the small fixture';return $reply}
function Rgb([string]$Hex){return 'rgb('+[Convert]::ToInt32($Hex.Substring(1,2),16)+', '+[Convert]::ToInt32($Hex.Substring(3,2),16)+', '+[Convert]::ToInt32($Hex.Substring(5,2),16)+')'}
function Assert-Editor($Status){
    $watch=[Diagnostics.Stopwatch]::StartNew();$last=$null
    do{$left=5000-$watch.ElapsedMilliseconds;if($left -le 0){$evidence.observations+=@{name='editor-appearance-timeout';editor=$last};throw 'Monaco appearance exceeded five seconds'};$last=Editor-Read ([int]$left);$appearance=$last.appearance;$applied=$appearance.applied
        if($applied -and -not $appearance.pending -and $applied.background -ceq $Status.colors.background -and $applied.foreground -ceq $Status.colors.foreground -and $applied.cursor -ceq $Status.colors.cursor){break};Start-Sleep -Milliseconds 20
    }while($true)
    Require ($last.content -ceq $editorText -and $last.dirty -eq $false -and $last.active_version -eq $editorVersion -and $last.document_id -ceq $editorDocument) 'Theme update altered Monaco content/dirty/version'
    Require ($appearance.font_family -ceq $editorInitial.appearance.font_family -and $appearance.font_size -eq $editorInitial.appearance.font_size -and $appearance.background -ceq (Rgb $Status.colors.background) -and $appearance.css_background -ceq $Status.colors.background -and $appearance.css_foreground -ceq $Status.colors.foreground) 'Theme changed existing Monaco font options or computed/CSS colors differ'
    Require ($applied.minimapEnabled -eq $editorInitial.appearance.applied.minimapEnabled) 'Theme changed Monaco minimap setting'
    $selectionBackground=if($Status.colors.selection_background){$Status.colors.selection_background}else{$Status.colors.foreground+'47'};$selectionForeground=if($Status.colors.selection_foreground){$Status.colors.selection_foreground}else{$Status.colors.foreground}
    Require ($applied.dark -eq $Status.colors.dark -and $applied.selectionBackground -ceq $selectionBackground -and $applied.selectionForeground -ceq $selectionForeground) 'Monaco cursor/selection mapping differs from effective theme'
    $evidence.observations+=@{name='actual-Monaco-appearance';surface=$editorSurface;version=$last.active_version;dirty=$last.dirty;appearance=$appearance}
}
function Capture-Chrome($Status){
    $path=Join-Path $directory 'theme-chrome.bmp';$png=Join-Path $directory 'theme-chrome.png';$capture=Request @('chrome-capture',$path);Require ($capture.root_handle -eq [long]$Status.options.window -and $capture.background -ceq $Status.colors.background) 'Native client target/fill differs from effective terminal theme'
    $preset=@($Status.options.theme_panel.presets|Where-Object {$_.id -ceq 'nord'})[0];$pixels=@()
    foreach($swatch in $preset.swatches){$rendered=@($capture.controls|Where-Object {$_.handle -eq $swatch.handle});Require ($rendered.Count -eq 1) 'Visible Nord swatch missing from production capture';$rect=$rendered[0];$x=[int]($rect.x+[Math]::Floor($rect.width/2));$y=[int]($rect.y+[Math]::Floor($rect.height/2));Require ($x -ge $rect.clip.x -and $x -lt $rect.clip.x+$rect.clip.width -and $y -ge $rect.clip.y -and $y -lt $rect.clip.y+$rect.clip.height) 'Nord swatch center is clipped';$actual=[ChromeFixture]::Pixel($path,$x,$y);Require ($actual -ceq $swatch.color) 'Native Nord swatch raster differs from its declared color';$pixels+=@{handle=$swatch.handle;expected=$swatch.color;actual=$actual;x=$x;y=$y}}
    $evidence.observations+=@{name='Nord-production-swatch-pixels';pixels=$pixels};[ChromeFixture]::Png($path,$png)
    $evidence.artifacts+=@{name='native-theme';path=$png;bytes=(Get-Item -LiteralPath $png).Length;sha256=(Get-FileHash -Algorithm SHA256 -LiteralPath $png).Hash.ToLowerInvariant();capture=$capture;scope='Owned production native client paint; excludes titlebar, GPU/WebView composition and desktop. EDIT raster may be absent.'};Require ((Get-Item -LiteralPath $png).Length -lt 500KB) 'Theme PNG exceeds small artifact limit'
}
try {
    $doctor=Probe @('doctor');Require ($doctor.background_testing -and $doctor.status -eq 'ok') 'Working hidden debug build required'
    $started=[DateTime]::UtcNow;$startup=[Diagnostics.Stopwatch]::StartNew();$owned=[CliProbe]::Start($gui,@('--temporary','--shell=cmd','--cwd',$directory),$directory,$directory);$hostOut=$owned.StandardOutput.ReadToEndAsync();$hostErr=$owned.StandardError.ReadToEndAsync();$evidence.hosts+=,$owned.Id
    $file=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($owned.Id).json"
    do {Require (-not $owned.HasExited -and $startup.ElapsedMilliseconds -lt 8000) 'Host discovery exceeded eight seconds or exited';if((Test-Path -LiteralPath $file) -and (Get-Item -LiteralPath $file).LastWriteTimeUtc -ge $started){$record=Get-Content -Raw -LiteralPath $file|ConvertFrom-Json;Require ($record.pid -eq $owned.Id -and [bool]$record.pipe) 'Wrong discovery owner';$pipeName=$record.pipe;break};Start-Sleep -Milliseconds 20}while($true)
    $left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup budget exhausted';Require ((Request @('identify') ([int][Math]::Min(5000,$left))).pid -eq $owned.Id) 'Pipe owner mismatch'
    do {$left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup readiness exceeded eight seconds';$tree=Tree ([int][Math]::Min(5000,$left));if(@($tree.surfaces).Count -eq 1 -and $tree.surfaces[0].ready -and $tree.surfaces[0].running){break};Start-Sleep -Milliseconds 20}while($true)
    $evidence.observations+=@{name='startup';elapsedMs=$startup.ElapsedMilliseconds;pipe=$pipeName};$shells=@($tree.surfaces.pid);$originalPid=$tree.surfaces[0].pid;$identities=Identities $tree;$active=(Request @('identify')).surface


    $status=Await {param($s) Ack $s};$source=Request @('identify');$editorText="Theme 한글 한 é 😀 &`n";$file=Join-Path $directory 'theme-한글.txt';[IO.File]::WriteAllText($file,$editorText,(New-Object Text.UTF8Encoding($false)))
    $opened=(Request @('editor','open',$file,'--root',$directory,'--pane',$source.pane)).editor_opened;$editorSurface=$opened.surface;Require ([bool]$editorSurface) 'Monaco open omitted its owned surface'
    $watch=[Diagnostics.Stopwatch]::StartNew();do{$left=5000-$watch.ElapsedMilliseconds;Require ($left -gt 0) 'Monaco readiness exceeded five seconds';$editorStatus=Request @('editor','status',$editorSurface) ([int]$left);if($editorStatus.view_handle){[OptionsFixture]::Describe([long]$editorStatus.view_handle,$owned.Id)|Out-Null};if($editorStatus.ready){break};Start-Sleep -Milliseconds 20}while($true)
    $watch=[Diagnostics.Stopwatch]::StartNew();do{$left=5000-$watch.ElapsedMilliseconds;Require ($left -gt 0) 'Initial Monaco appearance exceeded five seconds';$editorInitial=Editor-Read ([int]$left);if($editorInitial.appearance.applied -and -not $editorInitial.appearance.pending -and [bool]$editorInitial.appearance.font_family -and $editorInitial.appearance.font_size -gt 0){break};Start-Sleep -Milliseconds 20}while($true);$evidence.observations+=@{name='initial-Monaco-options-to-preserve';appearance=$editorInitial.appearance};$editorVersion=$editorInitial.active_version;$editorDocument=$editorInitial.document_id;Require ($null -ne $editorVersion -and [bool]$editorDocument) 'Monaco read omitted real document/version identity';Require ($editorInitial.content -ceq $editorText -and $editorInitial.dirty -eq $false) 'Monaco fixture was not opened exactly'
    $tree=Tree;$entry=@($tree.chrome.controls|Where-Object {$_.kind -eq 'settings'});Require ($entry.Count -eq 1) 'Options entry missing';Native-Click ([long]$entry[0].handle)
    $status=Await {param($s) $s.options -and $s.options.open};Native-Click ([long]$status.options.tabs[1].handle);$status=Await {param($s) $s.options.page -eq 'theme' -and $s.options.theme_panel}
    $ids=@('default','one-dark','dracula','nord','gruvbox-dark','catppuccin-mocha','tokyo-night','solarized-dark','solarized-light','github-light','catppuccin-latte')
    Require ((@($status.options.theme_panel.presets.id) -join ',') -ceq ($ids -join ',') -and @($status.options.theme_panel.fields).Count -eq 5) 'Native Theme catalog or five overrides differ'
    $evidence.observations+=@{name='native-preset-and-swatch-catalog';presets=$status.options.theme_panel.presets;fields=$status.options.theme_panel.fields}
    foreach($preset in $status.options.theme_panel.presets){Require ([OptionsFixture]::Text([long]$preset.button,$owned.Id) -ceq $preset.name) 'Native preset button caption differs';Require (@($preset.swatches).Count -eq 8) 'Preset must expose background/foreground and six ANSI native swatches';foreach($swatch in $preset.swatches){[OptionsFixture]::Describe([long]$swatch.handle,$owned.Id)|Out-Null;Require ($swatch.color -cmatch '^#[0-9a-fA-F]{6}$') 'Invalid preset swatch color'}}
    foreach($id in $ids){$status=Select-Preset $status $id;Require (@($status.options.theme_panel.presets|Where-Object {$_.selected -and $_.id -ceq $id}).Count -eq 1) 'Selected preset indication differs';Record ('preset-'+$id) $status}
    Require ((Identities (Tree)) -ceq $identities) 'Preset selection recreated the terminal';$evidence.checks+=@{name='eleven_native_preset_buttons_swatches_and_exact_renderer_ANSI16_ACK';passed=$true}

    $status=Select-Preset $status 'nord';Require ($status.colors.background -ceq '#2e3440' -and $status.colors.foreground -ceq '#d8dee9' -and $status.colors.palette[0] -ceq '#3b4252' -and $status.colors.palette[15] -ceq '#eceff4') 'Nord colors differ from shared Linux source'
    Assert-Editor $status;Capture-Chrome $status;$evidence.checks+=@{name='shared_Nord_terminal_palette_and_actual_Monaco_options_CSS_native_client_colors';passed=$true}

    $overrides=[ordered]@{background='#112233';foreground='#d4e5f6';cursor='#aabbcc';selection_background='#445566';selection_foreground='#fffefd'}
    foreach($key in $overrides.Keys){$row=Field $status ('theme_'+$key);Reveal $status ([long]$row.input);Edit $status ('theme_'+$key) $overrides[$key]}
    $status=Await {param($s) $ok=$true;foreach($key in $overrides.Keys){if($s.document.terminal.theme_overrides.$key -cne $overrides[$key]){$ok=$false}};$ok -and (Settled $s)}
    foreach($key in $overrides.Keys){Require ($status.colors.$key -ceq $overrides[$key]) ('Override did not reach effective '+$key)};Assert-Editor $status;Record 'five-native-overrides-applied' $status
    $status=Select-Preset $status 'dracula';foreach($key in $overrides.Keys){Require ($status.document.terminal.theme_overrides.$key -ceq $overrides[$key] -and $status.colors.$key -ceq $overrides[$key]) 'Preset switch discarded an override'}
    $evidence.checks+=@{name='five_HEX_edits_apply_and_preset_switch_preserves_overrides';passed=$true}

    $row=Field $status 'theme_background';Reveal $status ([long]$row.input);Edit $status 'theme_background' '';$status=Await {param($s) -not $s.document.terminal.theme_overrides.background -and $s.colors.background -ceq '#282a36' -and (Settled $s)};Record 'blank-inherits-Dracula-background' $status
    $picker=Color-Field $status 'theme_background';Require (-not $status.options.theme_panel.picker_available) 'OS color picker must be blocked in background mode';$before=$status.document.revision;Reveal $status ([long]$picker.picker);Native-Click ([long]$picker.picker)
    $status=Await {param($s) $s.options.theme_panel.picker_status -match 'disabled during hidden verification'};Require ($status.document.revision -eq $before) 'Blocked hidden color picker saved a value';Tree|Out-Null;Record 'native-picker-explicitly-blocked-in-hidden-mode' $status
    $evidence.checks+=@{name='blank_override_inherits_and_background_OS_color_picker_is_explicitly_blocked';passed=$true;scope='Physical ChooseColorW OK/Cancel remains unverified'}

    $invalid='#12 한 é';Edit $status 'theme_cursor' $invalid;$status=Await {param($s) (Field $s 'theme_cursor').value -ceq $invalid -and [bool](Field $s 'theme_cursor').draft_error -and -not $s.options.pending}
    Require ($status.document.terminal.theme_overrides.cursor -ceq $overrides.cursor) 'Invalid Unicode color draft changed saved value';Request @('settings','set','font-size','16')|Out-Null;$status=Await {param($s) $s.document.terminal.font_size -eq 16 -and (Ack $s)}
    Require ((Field $status 'theme_cursor').value -ceq $invalid -and [bool](Field $status 'theme_cursor').draft_error) 'Unrelated update erased invalid color draft';Record 'invalid-raw-color-retained' $status;Edit $status 'theme_cursor' '#abcdef';$status=Await {param($s) $s.colors.cursor -ceq '#abcdef' -and (Settled $s)}
    Guard $status 'theme_background' $true;Edit $status 'theme_background' '#123abc';$status=Await {param($s) $s.options.composing -and (Field $s 'theme_background').value -ceq '#123abc'};Require ($status.colors.background -ceq '#282a36') 'Color saved during controlled composition';Guard $status 'theme_background' $false;$status=Await {param($s) $s.colors.background -ceq '#123abc' -and (Settled $s)}
    Guard $status 'theme_foreground' $true;Edit $status 'theme_foreground' '#234bcd';Request @('settings','set','theme-foreground','#c0ffee')|Out-Null;$status=Await {param($s) $s.colors.foreground -ceq '#c0ffee' -and (Ack $s)};Guard $status 'theme_foreground' $false
    $status=Await {param($s) (Field $s 'theme_foreground').draft_error -match 'changed elsewhere|conflict' -and -not $s.options.pending};Require ((Field $status 'theme_foreground').value -ceq '#234bcd' -and $status.colors.foreground -ceq '#c0ffee') 'Stale color draft overwrote external winner';Record 'controlled-IME-and-color-CAS' $status
    Native-Click ([long]$status.options.reload);$status=Await {param($s) (Field $s 'theme_foreground').value -ceq '#c0ffee' -and -not (Field $s 'theme_foreground').draft_error};$evidence.checks+=@{name='invalid_Unicode_intermediate_color_IME_guard_and_per_color_CAS_preserve_drafts';passed=$true;scope='Controlled owned HWND guard messages only'}

    $staleOverrides=$status.document.terminal.theme_overrides|ConvertTo-Json -Depth 8 -Compress;Request @('settings','set','theme-cursor','#123456')|Out-Null;$status=Await {param($s) $s.colors.cursor -ceq '#123456' -and (Settled $s)};$winnerRevision=$status.document.revision;$winningOverrides=$status.document.terminal.theme_overrides|ConvertTo-Json -Depth 8 -Compress
    $rejected=Request @('settings','set','theme-overrides','{}','--expected',$staleOverrides) 5000 $true;Require ($rejected.error -match 'changed elsewhere|conflict') 'Whole-colors stale reset did not report conflict';$status=Request @('settings','show')
    Require ($status.document.revision -eq $winnerRevision -and ($status.document.terminal.theme_overrides|ConvertTo-Json -Depth 8 -Compress) -ceq $winningOverrides) 'Stale whole-colors reset discarded a newer override';Record 'whole-override-reset-CAS-rejected' $status;$evidence.checks+=@{name='whole_override_JSON_CAS_rejects_stale_reset_without_losing_external_color';passed=$true}

    Request @('settings','set','minimap-enabled','false')|Out-Null;$status=Await {param($s) $s.document.terminal.minimap_enabled -eq $false -and (Ack $s)};Reveal $status ([long]$status.options.theme_panel.reset);Native-Click ([long]$status.options.theme_panel.reset)
    $status=Await {param($s) -not $s.document.terminal.theme_overrides -and (Settled $s)}
    Require ($status.document.terminal.theme_preset -ceq 'dracula' -and $status.document.terminal.font_size -eq 16 -and $status.document.terminal.minimap_enabled -eq $false) 'Reset colors changed preset, font or minimap';Assert-Editor $status;Record 'reset-colors-only' $status
    # A held invalid row can keep an old baseline after an external full reset.
    # Reset's already-empty fast path must clear that row baseline as well as raw/error.
    $row=Field $status 'theme_cursor';Reveal $status ([long]$row.input);Edit $status 'theme_cursor' '#2468ac';$status=Await {param($s) $s.document.terminal.theme_overrides.cursor -ceq '#2468ac' -and (Field $s 'theme_cursor').baseline -ceq '#2468ac' -and (Settled $s)}
    $invalidReset='#broken 한';Edit $status 'theme_cursor' $invalidReset;$status=Await {param($s) (Field $s 'theme_cursor').value -ceq $invalidReset -and [bool](Field $s 'theme_cursor').draft_error -and (Settled $s)}
    Request @('settings','set','theme-overrides','{}')|Out-Null;$status=Await {param($s) -not $s.document.terminal.theme_overrides -and (Field $s 'theme_overrides').baseline -ceq '{}' -and (Settled $s)}
    Require ((Field $status 'theme_cursor').value -ceq $invalidReset -and (Field $status 'theme_cursor').baseline -ceq '#2468ac' -and [bool](Field $status 'theme_cursor').draft_error) 'External empty override update did not preserve the invalid local color draft';$emptyRevision=$status.document.revision;Record 'invalid-color-old-baseline-after-external-reset' $status
    Reveal $status ([long]$status.options.theme_panel.reset);Native-Click ([long]$status.options.theme_panel.reset)
    $status=Await {param($s) $cleared=$true;foreach($key in @('theme_background','theme_foreground','theme_cursor','theme_selection_background','theme_selection_foreground')){$row=Field $s $key;if($row.value -cne '' -or $row.baseline -cne '' -or $row.draft_error){$cleared=$false}};$cleared -and (Settled $s)}
    Require ($status.document.revision -eq $emptyRevision) 'Already-empty Reset unexpectedly wrote settings';Record 'empty-reset-clears-raw-error-and-stale-baselines' $status
    $row=Field $status 'theme_cursor';Reveal $status ([long]$row.input);Edit $status 'theme_cursor' '#13579b';$status=Await {param($s) $s.document.terminal.theme_overrides.cursor -ceq '#13579b' -and (Field $s 'theme_cursor').baseline -ceq '#13579b' -and -not (Field $s 'theme_cursor').draft_error -and (Settled $s)}
    Record 'valid-color-saves-after-empty-reset' $status;$evidence.checks+=@{name='already_empty_Reset_clears_held_invalid_color_baseline_and_next_valid_HEX_saves_without_false_CAS';passed=$true}

    Reveal $status ([long]$status.options.theme_panel.legacy);Native-Click ([long]$status.options.theme_panel.legacy);$status=Await {param($s) -not $s.document.terminal.theme_preset -and (Settled $s)}
    Require ((Identities (Tree)) -ceq $identities) 'Theme operations recreated terminal';Record 'legacy-mode-restored' $status;$evidence.checks+=@{name='native_Reset_colors_preserves_other_settings_and_legacy_mode_remains_available';passed=$true};$evidence.finalSettings=$status;$evidence.status='passed'
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
    if($cleanupErrors.Count){$evidence.status='failed';$evidence.cleanupErrors=$cleanupErrors};$evidence.clientPids=$clients;$evidence.shells=$shells;$evidence.elapsedMs=$clock.ElapsedMilliseconds;$evidence.finished=[DateTime]::UtcNow.ToString('o');if($evidence.status -eq 'failed'){$evidence|ConvertTo-Json -Depth 50|Set-Content -Encoding UTF8 (Join-Path $directory 'native-theme-background.json')};if($evidence.status -eq 'failed'){Write-Output ('Evidence: '+$directory)}
}
if($cleanupErrors.Count){throw ($cleanupErrors -join '; ')}
[ordered]@{status=$evidence.status;checks=$evidence.checks.Count;elapsedMs=$evidence.elapsedMs}|ConvertTo-Json -Compress
