# SPDX-License-Identifier: GPL-3.0-or-later
# Own hidden host only. Run under run-check.ps1 -TimeoutSeconds 60.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path;$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
Add-Type -ReferencedAssemblies System.Drawing -Path (Join-Path $PSScriptRoot 'ChromeFixture.cs')
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'EditorFixture.cs'),(Join-Path $PSScriptRoot 'PaneToolsFixture.cs')
$directory=Join-Path $PSScriptRoot ('..\dist\evidence\pane-tools-'+[guid]::NewGuid());[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$fixture=New-Object EditorFixture($directory);$path=$fixture.Write('pane close 한글.txt',[EditorFixture]::Original,$false,$false)
$clock=[Diagnostics.Stopwatch]::StartNew();$hostProcess=$null;$pipeName=$null;$clients=@();$shells=@();$cleanup=$false;$out=$null;$err=$null;$editor=$null
$evidence=[ordered]@{started=[DateTime]::UtcNow.ToString('o');mode='hidden-native-pane-tools';checks=@();observations=@();desktopInput=$false;physicalIme=$false;deferred=@('Owned WM_COMMAND validates direct production buttons. Desktop pointer/menu navigation and composed GPU pixels are not covered.','Close Pane CLI uses the same whole-pane editor barrier as the menu; the hidden host deliberately suppresses native popup selection.','The dirty document is acknowledged before close; this does not force an unsynchronized edit debounce race.')}
function Require([bool]$Condition,[string]$Message){if(-not $Condition){throw $Message}}
function Budget([int]$Max=5000){if($cleanup){return $Max};$left=55000-$clock.ElapsedMilliseconds;Require ($left -gt 0) 'Pane tools exceeded55s inner budget';return [int][Math]::Min($Max,$left)}
function Probe([string[]]$Arguments,[int]$Exit=0,[int]$Max=5000){
 $p=[CliProbe]::Start($cli,$Arguments,$directory,$directory);$script:clients+=,$p.Id;$o=$p.StandardOutput.ReadToEndAsync();$e=$p.StandardError.ReadToEndAsync()
 try {Require ($p.WaitForExit((Budget $Max))) 'Owned CLI exceeded bounded deadline; no retry';Require ($o.Wait(500)-and $e.Wait(500)) 'CLI pipes did not close';$text=[CliProbe]::Output($o);Require ($p.ExitCode -eq $Exit) ('CLI exit differs: '+[CliProbe]::Output($e)+' '+$text);if($Exit -ne 0){return ([CliProbe]::Output($e)|ConvertFrom-Json)};return ($text|ConvertFrom-Json)}
 finally {if(-not $p.HasExited){$p.Kill();[CliProbe]::WaitAfterKill($p)};$p.Dispose()}
}
function Request([string[]]$Arguments,[int]$Exit=0,[int]$Max=5000){Require ([bool]$pipeName) 'Explicit owned pipe required';return Probe (@('--pipe',$pipeName,'--json')+$Arguments) $Exit $Max}
function Tree([int]$Max=5000){$t=Request @('tree') 0 $Max;Require ($t.background_testing) 'Hidden debug host required';[ChromeFixture]::Size([long]$t.window_handle,$hostProcess.Id)|Out-Null;return $t}
function Ready([int]$Count,[int]$Max=5000){$watch=[Diagnostics.Stopwatch]::StartNew();do{$left=$Max-$watch.ElapsedMilliseconds;Require ($left -gt 0) 'Terminal readiness exceeded condition budget';$t=Tree ([int]$left);if(@($t.surfaces).Count -eq $Count -and @($t.surfaces|Where-Object {-not $_.ready -or -not $_.pid -or -not $_.running}).Count -eq 0){$script:shells+=@($t.surfaces.pid);return $t};Start-Sleep -Milliseconds 20}while($true)}
function Stable($Tree,$Before){foreach($s in $Before){$now=@($Tree.surfaces|Where-Object {$_.id -eq $s.id});Require ($now.Count -eq 1 -and $now[0].pid -eq $s.pid -and $now[0].running) 'Pane UI replaced an existing terminal process'}}
function Tool($Tree,[string]$Pane,[string]$Kind){$items=@($Tree.chrome.controls|Where-Object {$_.pane -eq $Pane -and $_.kind -eq $Kind});Require ($items.Count -eq 1 -and $items[0].layout_visible) ('Missing visible tool '+$Kind);return $items[0]}
function Click([string]$Pane,[string]$Kind){$t=Tree;$button=Tool $t $Pane $Kind;[PaneToolsFixture]::Command($hostProcess,[long]$t.window_handle,[long]$button.handle);$evidence.observations+=@{name='native-command';pane=$Pane;kind=$Kind;button=$button}}
function Check-Geometry($Tree){
 $native=@([ChromeFixture]::Read([long]$Tree.window_handle,$hostProcess.Id))
 foreach($entry in @($Tree.layout.panes)){$pane=$entry[0];$r=$entry[1];$controls=@($Tree.chrome.controls|Where-Object {$_.pane -eq $pane -and $_.layout_visible});foreach($c in $controls){$n=@($native|Where-Object {$_.Handle -eq $c.handle});Require ($n.Count -eq 1) 'Reported control missing from native HWND tree';$v=$n[0];Require ($v.X -ge $r.x -and $v.Y -ge $r.y -and $v.X+$v.Width -le $r.x+$r.width+1 -and $v.Y+$v.Height -le $r.y+$r.height+1) 'Pane header spills outside owning pane'};for($i=0;$i -lt $controls.Count;$i++){for($j=$i+1;$j -lt $controls.Count;$j++){$a=$controls[$i].rect;$b=$controls[$j].rect;Require (-not ($a.x -lt $b.x+$b.width -and $b.x -lt $a.x+$a.width -and $a.y -lt $b.y+$b.height -and $b.y -lt $a.y+$a.height)) 'Pane header controls overlap'}}}
 $evidence.observations+=@{name='native-header-geometry';tree=$Tree;native=$native}
}
function Editor-Command([string]$Action,[string[]]$Options=@()){$r=Request (@('editor','command',$editor.surface,$Action)+$Options);if($r.psobject.Properties.Name -contains 'result'){return $r.result};return $r}
try {
 $doctor=Probe @('doctor');Require ($doctor.background_testing -and $doctor.status -eq 'ok') 'Debug background build required'
 $utc=[DateTime]::UtcNow;$startup=[Diagnostics.Stopwatch]::StartNew();$hostProcess=[CliProbe]::Start($gui,@('--temporary','--shell=cmd','--cwd',$fixture.Root),$directory,$directory);$out=$hostProcess.StandardOutput.ReadToEndAsync();$err=$hostProcess.StandardError.ReadToEndAsync()
 $discovery=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($hostProcess.Id).json"
 do {Budget|Out-Null;Require (-not $hostProcess.HasExited -and $startup.ElapsedMilliseconds -lt 8000) 'Startup exceeded eight seconds';if((Test-Path $discovery)-and(Get-Item $discovery).LastWriteTimeUtc -ge $utc){$record=Get-Content -Raw $discovery|ConvertFrom-Json;Require ($record.pid -eq $hostProcess.Id) 'Wrong discovery PID';$pipeName=$record.pipe;break};Start-Sleep -Milliseconds 20}while($true)
 $left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup budget exhausted';$tree=Ready 1 ([int]$left);$a=Request @('identify');$original=@($tree.surfaces)
 [ChromeFixture]::Resize([long]$tree.window_handle,$hostProcess.Id,1500,850);$tree=Tree;Check-Geometry $tree
 $order=@($tree.chrome.controls|Where-Object {$_.pane -eq $a.pane -and $_.kind -like 'pane_*' -and $_.layout_visible}|Sort-Object {$_.rect.x}|ForEach-Object {$_.kind})
 Require (($order -join ',') -eq 'pane_zoom,pane_split_right,pane_split_down,pane_add,pane_browser,pane_menu') 'Direct tool order differs from Linux'
 Click $a.pane 'pane_split_right';$tree=Ready 2;$b=Request @('identify');Require ($b.pane -ne $a.pane) 'Split right did not create a pane';Stable $tree $original;Check-Geometry $tree
 Click $b.pane 'pane_split_down';$tree=Ready 3;$c=Request @('identify');Require ($c.pane -ne $b.pane) 'Split down did not create a pane';$beforeZoom=@($tree.surfaces);$layout=($tree.layout|ConvertTo-Json -Depth 15 -Compress)
 Click $c.pane 'pane_zoom';$tree=Tree;Require ($tree.zoomed_pane -eq $c.pane) 'Native maximize did not zoom target';Require ((Tool $tree $c.pane 'pane_zoom').label -eq 'Restore pane') 'Zoom button caption did not change';Stable $tree $beforeZoom
 Click $c.pane 'pane_zoom';$tree=Tree;Require (-not $tree.zoomed_pane -and ($tree.layout|ConvertTo-Json -Depth 15 -Compress) -ceq $layout) 'Restore changed split geometry';Stable $tree $beforeZoom
 Click $c.pane 'pane_add';$tree=Ready 4;$newTab=Request @('identify');Require ($newTab.pane -eq $c.pane) 'Add tab split or retargeted source';$stable=@($tree.surfaces)
 Click $c.pane 'pane_browser';$watch=[Diagnostics.Stopwatch]::StartNew();do{$tree=Tree;Require ($watch.ElapsedMilliseconds -lt 5000) 'Browser creation exceeded five seconds';if(@($tree.browsers).Count -eq 1){break};Start-Sleep -Milliseconds 20}while($true)
 $browser=Request @('identify');Require ($browser.pane -eq $c.pane) 'Browser tool opened in the wrong pane';Stable $tree $stable;Check-Geometry $tree
 Click $c.pane 'pane_menu';$tree=Tree;Stable $tree $stable;Require (@($tree.layout.panes).Count -eq 3) 'Hidden pane menu changed layout'
 [ChromeFixture]::Resize([long]$tree.window_handle,$hostProcess.Id,420,400);$tree=Tree;Check-Geometry $tree;Stable $tree $stable
 [ChromeFixture]::Resize([long]$tree.window_handle,$hostProcess.Id,1500,850);$tree=Tree;Check-Geometry $tree
 $evidence.checks+=@{name='native_direct_tools_preserve_existing_terminal_pids_zoom_layout_and_bounded_header_geometry';passed=$true}
 $editor=(Request @('editor','open',$path,'--pane',$c.pane,'--root',$fixture.Root)).editor_opened;Require ([bool]$editor.surface) 'Editor open omitted identity';$watch=[Diagnostics.Stopwatch]::StartNew();do{$status=Request @('editor','status',$editor.surface);Require ($watch.ElapsedMilliseconds -lt 5000) 'Editor readiness exceeded five seconds';if($status.ready){break};Start-Sleep -Milliseconds 20}while($true)
 Editor-Command 'replace-text' @('--text',[EditorFixture]::Edited)|Out-Null;Request @('focus-pane',$a.pane)|Out-Null;$before=Tree;$identity=Request @('identify')
 $rejected=Request @('close-pane',$c.pane) 1;$evidence.observations+=@{name='dirty-close-response';response=$rejected};Require ($rejected.error -like '*unsaved changes*') 'Dirty close did not reject through editor barrier';$after=Tree;$afterIdentity=Request @('identify');Stable $after $stable
 Require (($before.workspaces|ConvertTo-Json -Depth 30 -Compress) -ceq ($after.workspaces|ConvertTo-Json -Depth 30 -Compress) -and $identity.surface -eq $afterIdentity.surface) 'Rejected whole-pane close changed tabs or logical focus'
 $read=Editor-Command 'read';Require ($read.dirty -and $read.content -ceq [EditorFixture]::Edited -and -not $read.document_focused) 'Rejected close lost dirty text or focused hidden editor'
 $evidence.observations+=@{name='dirty-pane-rejected';response=$rejected;before=$before;after=$after;editorRead=$read}
 Editor-Command 'save'|Out-Null;Request @('close-pane',$c.pane)|Out-Null;$editor=$null;$after=Tree
 $retained=@($stable|Where-Object {$_.id -in @($a.surface,$b.surface)});Stable $after $retained
 Require (@($after.layout.panes).Count -eq 2 -and @($after.surfaces).Count -eq 2 -and @($after.browsers).Count -eq 0 -and @($after.editors).Count -eq 0) 'Successful close did not remove every tab in the target pane'
 Require ($fixture.BytesEqual($path,[EditorFixture]::Encode([EditorFixture]::Edited,$false,$false))) 'Saved editor bytes changed during whole-pane close'
 Request @('close-pane',$b.pane)|Out-Null;$tree=Tree;$final=Request @('close-pane',$a.pane) 1;Require ($final.error -like '*final pane*') 'Final pane close was not refused';Stable (Tree) $original
 $evidence.observations+=@{name='clean-whole-pane-close-and-final-refusal';tree=$tree;final=$final}
 $evidence.checks+=@{name='whole_pane_close_seals_all_editors_rejects_dirty_without_partial_removal_and_preserves_final_pane';passed=$true}
 Request @('quit','--discard-state')|Out-Null;Require ($hostProcess.WaitForExit((Budget 5000))) 'Owned host quit timed out';Require ($hostProcess.ExitCode -eq 0) 'Owned host exit failed';$evidence.status='passed_background_pane_tools_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
 $cleanup=$true
 if($hostProcess){try{if(-not $hostProcess.HasExited -and $pipeName){if($editor){Editor-Command 'discard-document'|Out-Null};Request @('quit','--discard-state')|Out-Null};if(-not $hostProcess.WaitForExit(5000)){$hostProcess.Kill();[CliProbe]::WaitAfterKill($hostProcess);$evidence.status='failed';$evidence.cleanupError='Owned host required forced cleanup'}}catch{$evidence.status='failed';$evidence.cleanupError=$_.Exception.Message;if(-not $hostProcess.HasExited){$hostProcess.Kill();[CliProbe]::WaitAfterKill($hostProcess)}}
  $outDone=$out.Wait(500);$errDone=$err.Wait(500);$evidence.hosts=@($hostProcess.Id);$evidence.observations+=@{kind='host-exit';exitCode=$hostProcess.ExitCode;stdoutComplete=$outDone;stderrComplete=$errDone;stdout=[CliProbe]::Output($out);stderr=[CliProbe]::Output($err)};$hostProcess.Dispose()}
 $fixture.Dispose();$evidence.shells=@($shells|Select-Object -Unique);$evidence.clientPids=$clients;$evidence.elapsedMs=$clock.ElapsedMilliseconds;$evidence.finished=[DateTime]::UtcNow.ToString('o');$evidence|ConvertTo-Json -Depth 35|Set-Content -Encoding UTF8 (Join-Path $directory 'native-pane-tools-background.json');Write-Output ('Evidence: '+$directory)
}
if($evidence.status -ne 'passed_background_pane_tools_subset'){throw 'Pane tools verification failed'}
[ordered]@{status=$evidence.status;checks=$evidence.checks.Count;elapsedMs=$evidence.elapsedMs}|ConvertTo-Json -Compress
