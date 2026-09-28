# SPDX-License-Identifier: GPL-3.0-or-later
# Run under an owned 60-second run-check.ps1 Job. No desktop input or visible windows.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
if (-not $env:FLOWMUX_TEST_ARTIFACT_ROOT) { throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.' }
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path;$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
Add-Type -ReferencedAssemblies System.Drawing -Path (Join-Path $PSScriptRoot 'ChromeFixture.cs')
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'EditorFixture.cs'),(Join-Path $PSScriptRoot 'FilesFixture.cs'),(Join-Path $PSScriptRoot 'FilesActionsFixture.cs'),(Join-Path $PSScriptRoot 'FilesDockFixture.cs')
$directory=Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT ('files-dock-'+[guid]::NewGuid());[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$fixture=New-Object FilesFixture($directory);$rootA=$fixture.Directory('source A');$rootB=$fixture.Directory('source B')
$fileA=$fixture.Write('source A/원본.txt','source A original', $false,$false);$fileA2=$fixture.Write('source A/다른 파일.txt','source A second',$false,$false);$fileB=$fixture.Write('source B/대상.txt','source B',$false,$false)
$clock=[Diagnostics.Stopwatch]::StartNew();$hostProcess=$null;$pipeName=$null;$clients=@();$shells=@();$cleanup=$false;$out=$null;$err=$null
$evidence=[ordered]@{started=[DateTime]::UtcNow.ToString('o');mode='hidden-native-files-dock';checks=@();observations=@();desktopInput=$false;clipboardAccess=$false;physicalIme=$false;deferred=@('Owned native messages exercise the production form handlers without opening a desktop popup. Pointer/menu navigation, physical IME, accessibility, DPI and composed WebView pixels remain unaccepted.','This case does not force filesystem races or operation cancellation. Existing Files action suites cover broader operation outcomes.')}
function Require([bool]$Condition,[string]$Message){if(-not $Condition){throw $Message}}
function Budget([int]$Max=5000){if($cleanup){return $Max};$left=55000-$clock.ElapsedMilliseconds;Require ($left -gt 0) 'Files dock exceeded55s inner budget';return [int][Math]::Min($Max,$left)}
function Probe([string[]]$Arguments,[int]$Max=5000){
 $p=[CliProbe]::Start($cli,$Arguments,$directory,$directory);$script:clients+=,$p.Id;$o=$p.StandardOutput.ReadToEndAsync();$e=$p.StandardError.ReadToEndAsync()
 try {Require ($p.WaitForExit((Budget $Max))) 'Owned CLI exceeded bounded deadline; no retry';Require ($o.Wait(500)-and $e.Wait(500)) 'CLI pipes did not close';Require ($p.ExitCode -eq 0) ('CLI failed: '+[CliProbe]::Output($e)+' '+[CliProbe]::Output($o));return ([CliProbe]::Output($o)|ConvertFrom-Json)}
 finally {if(-not $p.HasExited){$p.Kill();[CliProbe]::WaitAfterKill($p)};$p.Dispose()}
}
function Request([string[]]$Arguments,[int]$Max=5000){Require ([bool]$pipeName) 'Explicit owned pipe required';return Probe (@('--pipe',$pipeName,'--json')+$Arguments) $Max}
function Tree([int]$Max=5000){$t=Request @('tree') $Max;Require ($t.background_testing) 'Hidden debug host required';[ChromeFixture]::Size([long]$t.window_handle,$hostProcess.Id)|Out-Null;return $t}
function Ready([int]$Count,[int]$Max=5000){$watch=[Diagnostics.Stopwatch]::StartNew();do{$left=$Max-$watch.ElapsedMilliseconds;Require ($left -gt 0) 'Terminal readiness exceeded condition budget';$t=Tree ([int]$left);if(@($t.surfaces).Count -eq $Count -and @($t.surfaces|Where-Object {-not $_.ready -or -not $_.pid -or -not $_.running}).Count -eq 0){$script:shells+=@($t.surfaces.pid);return $t};Start-Sleep -Milliseconds 20}while($true)}
function Files([string]$Pane){return Request @('files','status','--pane',$Pane)}
function Show-Files([string]$Pane,[string]$Root){Request @('files','show','--pane',$Pane,'--root',$Root)|Out-Null;$s=Files $Pane;Require ($s.visible -and -not $s.loading -and -not $s.stale -and -not $s.last_error) 'Files Show failed to settle';return $s}
function Select-File($State,[string]$Name){$r=@($State.rows|Where-Object {$_.name -ceq $Name});Require ($r.Count -eq 1) ('Expected one file row '+$Name);Request @('files','select','--pane',$State.pane,'--token',$State.token,'--index',$r[0].index.ToString(),'--mode','replace')|Out-Null;return Files $State.pane}
function Stable($Tree,$Before){foreach($s in $Before){$now=@($Tree.surfaces|Where-Object {$_.id -eq $s.id});Require ($now.Count -eq 1 -and $now[0].pid -eq $s.pid -and $now[0].running) 'Dock change replaced a terminal process'}}
function Check-Dock([string]$Name,$Tree,$Active,$Other){
 $size=[ChromeFixture]::Size([long]$Tree.window_handle,$hostProcess.Id);$scale=[Math]::Max(96,$Tree.chrome.dpi)/96.0;$r=$Active.dock_bounds
 Require ($Active.dock_scope -eq 'window' -and $Active.dock_visible -and $Active.dock_source_pane -eq $Active.pane -and $r) 'Expected selected global Files source'
 if($Other){Require (-not $Other.dock_visible -and -not $Other.visible -and -not $Other.dock_bounds) 'Two Files sources remained logically docked'}
 Require ([Math]::Abs($r.x+$r.width-($size[0]-4*$scale)) -le 2 -and [Math]::Abs($r.y-4*$scale) -le 2 -and [Math]::Abs($r.height-($size[1]-8*$scale)) -le 2) 'Files is not a full-workbench right dock'
 foreach($pane in @($Tree.layout.panes)){$area=$pane[1];Require ($area.x+$area.width -le $r.x+1) 'Files overlaps a pane instead of reserving workbench width'}
 Require ([Math]::Abs($Active.list_top-58*$scale) -le 2 -and -not $Active.operation_form) 'Default tree retained the old oversized action area'
 $list=[FilesListProbe]::Read($hostProcess,[long]$Tree.window_handle,[long]$Active.panel_handle,[long]$Active.list_handle);Require ($list.Count -eq $Active.native_count) 'Native tree count differs'
 $controls=@([ChromeFixture]::Read([long]$Active.panel_handle,$hostProcess.Id));$shown=@($controls|Where-Object {$_.Shown});Require (@($shown|Where-Object {$_.Text -eq 'Actions'}).Count -eq 1) 'Files actions entrypoint missing'
 Require (@($shown|Where-Object {$_.Text -in @('Copy to','Rename to','Move to','Apply','Cancel','Expand','Collapse','Open')}).Count -eq 0) 'Default dock still shows expanded action controls'
 $evidence.observations+=@{name=$Name;tree=$Tree;active=$Active;other=$Other;nativeList=$list;nativeControls=$controls}
}
function Native-Command($Tree,$State,[int]$Id){[FilesDockFixture]::Command($hostProcess,[long]$Tree.window_handle,[long]$State.panel_handle,$Id)}
function Await-Form([string]$Pane,[bool]$Expected){$w=[Diagnostics.Stopwatch]::StartNew();do{Require ($w.ElapsedMilliseconds -lt 5000) 'Form transition exceeded five seconds';$s=Files $Pane;if([bool]$s.operation_form -eq $Expected){return $s};Start-Sleep -Milliseconds 20}while($true)}
try {
 $doctor=Probe @('doctor');Require ($doctor.background_testing -and $doctor.status -eq 'ok') 'Debug background build required'
 $utc=[DateTime]::UtcNow;$startup=[Diagnostics.Stopwatch]::StartNew();$hostProcess=[CliProbe]::Start($gui,@('--temporary','--shell=cmd','--cwd',$rootA),$directory,$directory);$out=$hostProcess.StandardOutput.ReadToEndAsync();$err=$hostProcess.StandardError.ReadToEndAsync()
 $discovery=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($hostProcess.Id).json"
 do {Budget|Out-Null;Require (-not $hostProcess.HasExited -and $startup.ElapsedMilliseconds -lt 8000) 'Startup exceeded eight seconds';if((Test-Path $discovery)-and(Get-Item $discovery).LastWriteTimeUtc -ge $utc){$record=Get-Content -Raw $discovery|ConvertFrom-Json;Require ($record.pid -eq $hostProcess.Id) 'Wrong discovery PID';$pipeName=$record.pipe;break};Start-Sleep -Milliseconds 20}while($true)
 $left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup budget exhausted';$tree=Ready 1 ([int]$left);$a=Request @('identify')
 Request @('split','vertical','--shell=cmd')|Out-Null;$tree=Ready 2;$b=Request @('identify');$stable=@($tree.surfaces)
 $fa=Show-Files $a.pane $rootA;$fa=Select-File $fa '원본.txt';$tree=Tree;Check-Dock 'source-a' $tree $fa $null;$panelA=$fa.panel_handle
 Native-Command $tree $fa 13;$fa=Files $a.pane;Require (-not $fa.operation_form) 'Hidden Actions request opened desktop menu/form'
 Native-Command $tree $fa 7;$form=Await-Form $a.pane $true;$originalIndex=$form.operation_form.index
 $fa=Select-File $form '다른 파일.txt';Require ($fa.operation_form.index -eq $originalIndex) 'Form target followed a later row selection'
 $copy=$fixture.File('source A/복사.txt');[FilesDockFixture]::Destination($hostProcess,[long]$tree.window_handle,[long]$fa.panel_handle,'복사.txt');Native-Command $tree $fa 14
 $wait=[Diagnostics.Stopwatch]::StartNew();do {if($wait.ElapsedMilliseconds -ge 5000){$evidence.observations+=@{name='native-apply-deadline';state=$fa};throw 'Native Apply copy did not settle'};$fa=Files $a.pane;if($fa.last_error){$evidence.observations+=@{name='native-apply-error';state=$fa};throw ('Native Apply failed: '+$fa.last_error)};if([FilesActionsFixture]::Exists($fixture,$copy)-and -not $fa.loading -and -not $fa.operation){break};Start-Sleep -Milliseconds 20}while($true)
 $evidence.observations+=@{name='native-apply-settled';state=$fa;destination=$copy}
 Require ($fixture.BytesEqual($copy,[Text.Encoding]::UTF8.GetBytes('source A original'))) 'Apply used changed selection instead of captured form source'
 Native-Command $tree $fa 7;$fa=Await-Form $a.pane $true;Request @('files','refresh','--pane',$a.pane)|Out-Null;$fa=Await-Form $a.pane $false
 $stale=$fixture.File('source A/stale.txt');[FilesDockFixture]::Destination($hostProcess,[long]$tree.window_handle,[long]$fa.panel_handle,'stale.txt');Native-Command $tree $fa 14;$fa=Files $a.pane;Require (-not [FilesActionsFixture]::Exists($fixture,$stale)) 'Stale form survived refreshed token'
 $fa=Select-File $fa '다른 파일.txt';$fb=Show-Files $b.pane $rootB;$fb=Select-File $fb '대상.txt';$fa=Files $a.pane;$tree=Tree;Stable $tree $stable;Check-Dock 'retarget-b' $tree $fb $fa
 Require ($fa.captured_root -ceq $rootA -and @($fa.selected_paths) -contains '다른 파일.txt') 'Retarget lost cached source A root/selection'
 $evidence.checks+=@{name='global_single_dock_and_captured_native_form_target_survive_retarget';passed=$true}
 Request @('toggle-pane-zoom',$a.pane)|Out-Null;$tree=Tree;Stable $tree $stable;Check-Dock 'zoom-other-pane' $tree (Files $b.pane) (Files $a.pane)
 Request @('toggle-pane-zoom',$a.pane)|Out-Null;Request @('new-workspace','--cwd',$rootA,'--shell=cmd')|Out-Null;$tree=Ready 3;$other=Request @('identify');$hidden=Files $b.pane;Require (-not $hidden.dock_visible -and -not $hidden.dock_bounds) 'Files leaked into another workspace'
 Request @('workspace','focus',$a.workspace)|Out-Null;$tree=Tree;Stable $tree $stable;Check-Dock 'workspace-return' $tree (Files $b.pane) (Files $a.pane)
 Request @('files','hide','--pane',$b.pane)|Out-Null;$hidden=Files $b.pane;Require (-not $hidden.dock_visible -and -not $hidden.dock_source_pane) 'Hide left dock reservation active'
 $fb=Show-Files $b.pane $rootB;Request @('close-tab',$b.surface)|Out-Null;$tree=Tree;Stable $tree @($stable|Where-Object {$_.id -eq $a.surface});$fa=Files $a.pane;Require (-not $fa.dock_source_pane) 'Closing source pane retained orphaned global dock'
 $fa=Show-Files $a.pane $rootA;$tree=Tree;Require ($fa.panel_handle -eq $panelA -and @($fa.selected_paths) -contains '다른 파일.txt') 'Cached A panel/selection was not restored';Check-Dock 'cached-a-restored' $tree $fa $null
 $evidence.checks+=@{name='zoom_workspace_hide_and_source_close_preserve_cached_dock_state';passed=$true}
 Request @('quit','--discard-state')|Out-Null;Require ($hostProcess.WaitForExit((Budget 5000))) 'Owned host quit timed out';Require ($hostProcess.ExitCode -eq 0) 'Owned host exit failed';$evidence.status='passed_background_files_dock_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
 $cleanup=$true
 if($hostProcess){try{if(-not $hostProcess.HasExited -and $pipeName){Request @('quit','--discard-state')|Out-Null};if(-not $hostProcess.WaitForExit(5000)){$hostProcess.Kill();[CliProbe]::WaitAfterKill($hostProcess);$evidence.status='failed';$evidence.cleanupError='Owned host required forced cleanup'}}catch{$evidence.status='failed';$evidence.cleanupError=$_.Exception.Message;if(-not $hostProcess.HasExited){$hostProcess.Kill();[CliProbe]::WaitAfterKill($hostProcess)}}
  $outDone=$out.Wait(500);$errDone=$err.Wait(500);$evidence.hosts=@($hostProcess.Id);$evidence.observations+=@{kind='host-exit';exitCode=$hostProcess.ExitCode;stdoutComplete=$outDone;stderrComplete=$errDone;stdout=[CliProbe]::Output($out);stderr=[CliProbe]::Output($err)};$hostProcess.Dispose()}
 $fixture.Dispose();$evidence.shells=@($shells|Select-Object -Unique);$evidence.clientPids=$clients;$evidence.elapsedMs=$clock.ElapsedMilliseconds;$evidence.finished=[DateTime]::UtcNow.ToString('o');if ($evidence.status -eq 'failed') { $evidence|ConvertTo-Json -Depth 30|Set-Content -Encoding UTF8 (Join-Path $directory 'native-files-dock-background.json') };if ($evidence.status -eq 'failed') { Write-Output ('Evidence: '+$directory) }
}
if($evidence.status -ne 'passed_background_files_dock_subset'){throw 'Files dock verification failed'}
[ordered]@{status=$evidence.status;checks=$evidence.checks.Count;elapsedMs=$evidence.elapsedMs}|ConvertTo-Json -Compress
