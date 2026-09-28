# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden owned native Options.48s work + bounded cleanup within55s; outer Job60s.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference='Stop';$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path;$cli=Join-Path $BuildDirectory 'flowmuxctl.exe';$gui=Join-Path $BuildDirectory 'flowmux.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'OptionsFixture.cs')
Add-Type -ReferencedAssemblies System.Drawing -Path (Join-Path $PSScriptRoot 'ChromeFixture.cs')
$directory=Join-Path $(if ($env:FLOWMUX_TEST_ARTIFACT_ROOT) { $env:FLOWMUX_TEST_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\dist\evidence' }) ('options-'+[guid]::NewGuid());[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$clock=[Diagnostics.Stopwatch]::StartNew();$owned=$null;$pipeName=$null;$clients=@();$shells=@();$cleaning=$false;$cleanupErrors=@();$hostOut=$null;$hostErr=$null
$evidence=[ordered]@{started=[DateTime]::UtcNow.ToString('o');mode='hidden-native-options';hosts=@();checks=@();observations=@();desktopInput=$false;clipboardAccess=$false;imeSimulation=$false;deferred='Physical keyboard/focus/IME, per-monitor DPI, accessibility and composed visual acceptance are not established.'}
function Require([bool]$Condition,[string]$Message){if(-not $Condition){throw $Message}}
function Budget([int]$Maximum=5000){if($cleaning){return $Maximum};$left=48000-$clock.ElapsedMilliseconds;Require ($left -gt 0) 'Options work budget expired';return [int][Math]::Min($Maximum,$left)}
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
function Await([scriptblock]$Condition){$wait=[Diagnostics.Stopwatch]::StartNew();do{$left=5000-$wait.ElapsedMilliseconds;Require ($left -gt 0) 'Options condition exceeded five seconds';$s=Request @('settings','show') ([int]$left);if($s.options){[OptionsFixture]::Describe([long]$s.options.window,$owned.Id)|Out-Null};if(& $Condition $s){return $s};Start-Sleep -Milliseconds 20}while($true)}
function Field($Status,[string]$Key){$rows=@($Status.options.controls|Where-Object {$_.key -eq $Key});Require ($rows.Count -eq 1) ('Missing Options field '+$Key);return $rows[0]}
function Click($Status,[long]$Control){[OptionsFixture]::Click([long]$Status.options.window,$Control,$owned.Id)}
function Record([string]$Name,$Status){$evidence.observations+=@{name=$Name;settings=$Status;window=[OptionsFixture]::Describe([long]$Status.options.window,$owned.Id);controls=@([ChromeFixture]::Read([long]$Status.options.window,$owned.Id))}}
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
    Require (($status.options.tabs.name -join ',') -ceq 'General,Theme' -and $status.options.page -eq 'general') 'Unexpected Options pages'
    $controls=@([ChromeFixture]::Read([long]$status.options.window,$owned.Id));Require (@($controls|Where-Object {$_.Shown -and $_.Class -eq 'Button' -and (($_.Style -band 15) -ne 11 -or $_.Font -eq 0)}).Count -eq 0) 'Options buttons are not themed native controls'
    Record 'opened-general' $status;$evidence.checks+=@{name='settings_entry_opens_owned_hidden_nonmodal_options_with_general_theme_pages';passed=$true}
    $font='Cascadia Mono, "Malgun Gothic", "한글 한 é 😀", monospace';$row=Field $status 'font_family';[OptionsFixture]::SetText([long]$status.options.window,[long]$row.input,$owned.Id,$font);Click $status ([long]$row.apply)
    $status=Await {param($s) $s.document.terminal.font_family -ceq $font -and -not $s.options.pending -and (Ack $s)}
    Require ([OptionsFixture]::Text([long](Field $status 'font_family').input,$owned.Id) -ceq $font) 'Native Unicode font value changed';Require ((Identities (Tree)) -ceq $identities) 'Font apply restarted a terminal'
    Record 'unicode-font-applied' $status;$evidence.checks+=@{name='native_unicode_edit_apply_reaches_backend_and_terminal_ack_without_pid_change';passed=$true}
    Click $status ([long]$status.options.tabs[1].handle);$status=Await {param($s) $s.options.page -eq 'theme'};$row=Field $status 'theme';[OptionsFixture]::Select([long]$status.options.window,[long]$row.input,$owned.Id,1)
    $status=Await {param($s) $s.document.terminal.theme -eq 'light' -and -not $s.options.pending -and (Ack $s)};Require (@($status.surfaces|Where-Object {$_.applied.background -ne '#ffffff'}).Count -eq 0) 'Theme did not reach live terminal palette';Record 'theme-applied' $status
    $evidence.checks+=@{name='theme_page_native_combo_applies_light_palette';passed=$true}
    Click $status ([long]$status.options.tabs[0].handle);$status=Await {param($s) $s.options.page -eq 'general'};$row=Field $status 'font_size';$size=$status.document.terminal.font_size;[OptionsFixture]::SetText([long]$status.options.window,[long]$row.input,$owned.Id,'2');Click $status ([long]$row.apply)
    $status=Await {param($s) -not $s.options.pending -and [bool]$s.config_error};Require ($status.document.terminal.font_size -eq $size -and (Field $status 'font_size').value -ceq '2' -and [OptionsFixture]::Text([long](Field $status 'font_size').input,$owned.Id) -ceq '2' -and $status.options.error_or_status -match 'between 6 and 72') 'Invalid setting did not retain its draft and readable error'
    Record 'invalid-draft-retained' $status;$evidence.checks+=@{name='invalid_native_value_preserves_document_and_draft_with_inline_error';passed=$true}
    Click $status ([long]$status.options.reset);$status=Await {param($s) -not $s.options.pending -and -not $s.config_error -and ($s.document.terminal|ConvertTo-Json -Compress) -ceq $defaults -and (Ack $s)}
    Require ((Field $status 'font_size').value -ceq '14') 'Reset left invalid draft';Record 'defaults-reset' $status;Click $status ([long]$status.options.close);$status=Await {param($s) -not $s.options.open}
    Require ((Identities (Tree)) -ceq $identities -and (Request @('identify')).surface -eq $active) 'Options changed terminal identity or active surface';Record 'closed' $status
    $evidence.checks+=@{name='reset_updates_controls_and_terminal_then_close_preserves_active_surface';passed=$true}
    Request @('quit','--discard-state')|Out-Null;Require ($owned.WaitForExit((Budget 5000)) -and $owned.ExitCode -eq 0) 'Owned host did not quit cleanly';$evidence.status='passed_hidden_options_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
    $cleaning=$true
    if($owned){try{if(-not $owned.HasExited){$stop=[Diagnostics.Stopwatch]::StartNew();if($pipeName){Request @('quit','--discard-state') 2500|Out-Null};if(-not $owned.WaitForExit([int][Math]::Max(1,5000-$stop.ElapsedMilliseconds))){throw 'Owned host cleanup deadline exceeded'}}}catch{$cleanupErrors+=$_.Exception.Message;if(-not $owned.HasExited){$owned.Kill();[CliProbe]::WaitAfterKill($owned)}}
        $evidence.observations+=@{name='host-exit';pid=$owned.Id;exitCode=$owned.ExitCode;stdoutComplete=$hostOut.Wait(500);stderrComplete=$hostErr.Wait(500);stdout=[CliProbe]::Output($hostOut);stderr=[CliProbe]::Output($hostErr)};$owned.Dispose()}
    if($cleanupErrors.Count){$evidence.status='failed';$evidence.cleanupErrors=$cleanupErrors};$evidence.clientPids=$clients;$evidence.shells=$shells;$evidence.elapsedMs=$clock.ElapsedMilliseconds;$evidence.finished=[DateTime]::UtcNow.ToString('o');$evidence|ConvertTo-Json -Depth 50|Set-Content -Encoding UTF8 (Join-Path $directory 'native-options-background.json');Write-Output ('Evidence: '+$directory)
}
if($cleanupErrors.Count){throw ($cleanupErrors -join '; ')}
[ordered]@{status=$evidence.status;checks=$evidence.checks.Count;elapsedMs=$evidence.elapsedMs}|ConvertTo-Json -Compress
