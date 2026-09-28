# SPDX-License-Identifier: GPL-3.0-or-later
# Run via run-check.ps1 with a 100s cap. Only exact owned hidden processes/HWNDs.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference='Stop';$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path;$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'OptionsFixture.cs'),(Join-Path $PSScriptRoot 'EditorFixture.cs')
$base=if($env:FLOWMUX_TEST_ARTIFACT_ROOT){$env:FLOWMUX_TEST_ARTIFACT_ROOT}else{[IO.Path]::GetTempPath()};$directory=Join-Path $base ('new-window-'+[guid]::NewGuid());$cwd=Join-Path $directory '새 창 한글 한 😀';[IO.Directory]::CreateDirectory($cwd)|Out-Null
$clock=[Diagnostics.Stopwatch]::StartNew();$hosts=New-Object 'System.Collections.Generic.List[object]';$checks=@();$last=@{};$failure=$null;$cleaning=$false;$lastCommand=$null
function Require([bool]$Value,[string]$Message){if(-not $Value){throw $Message}}
function Same([string]$A,[string]$B){return [string]::Equals($A,$B,[StringComparison]::Ordinal)}
function Budget([int]$Maximum=5000){if($cleaning){return [Math]::Min($Maximum,2000)};$left=90000-$clock.ElapsedMilliseconds;Require ($left -gt 0) 'New window work exceeded 90 seconds';return [int][Math]::Min($Maximum,$left)}
function Probe([string[]]$Arguments,[int]$Maximum=5000){
 $script:lastCommand=$Arguments;$p=[CliProbe]::Start($cli,$Arguments,$cwd,$directory);$o=$p.StandardOutput.ReadToEndAsync();$e=$p.StandardError.ReadToEndAsync()
 try{Require ($p.WaitForExit((Budget $Maximum))) 'Owned CLI exceeded bounded deadline';Require ($o.Wait(500)-and $e.Wait(500)) 'CLI output pipes remained open';Require ($p.ExitCode -eq 0) ('Owned CLI failed: '+[CliProbe]::Output($e));return ([CliProbe]::Output($o)|ConvertFrom-Json)}
 finally{if(-not $p.HasExited){$p.Kill();[CliProbe]::WaitAfterKill($p)};$p.Dispose()}
}
function Request($Owned,[string[]]$Arguments,[int]$Maximum=5000){Require ([bool]$Owned.pipe) 'Explicit owned pipe is required';return Probe (@('--pipe',$Owned.pipe,'--json')+$Arguments) $Maximum}
function Tree($Owned,[int]$Maximum=5000){$t=Request $Owned @('tree') $Maximum;Require ($t.background_testing -and -not $Owned.process.HasExited) 'Owned hidden host is unavailable';$native=[OptionsFixture]::Describe([long]$t.window_handle,$Owned.process.Id);Require ($native.Owner -eq 0 -and @([EditorWindowProbe]::VisibleTopLevelWindows($Owned.process)).Count -eq 0) 'New host is owned by another window or exposed desktop UI';$script:last[[string]$Owned.process.Id]=$t;return $t}
function Await($Owned,[scriptblock]$Condition){$watch=[Diagnostics.Stopwatch]::StartNew();do{$left=5000-$watch.ElapsedMilliseconds;Require ($left -ge 100) 'New window condition exceeded five seconds';$t=Tree $Owned ([int]$left);if(& $Condition $t){return $t};Start-Sleep -Milliseconds 20}while($true)}
function Ready($Owned,[datetime]$Started){
 $watch=[Diagnostics.Stopwatch]::StartNew();$path=Join-Path $env:LOCALAPPDATA ('flowmux\windows\instances\'+$Owned.process.Id+'.json')
 do{Budget|Out-Null;Require (-not $Owned.process.HasExited -and $watch.ElapsedMilliseconds -lt 8000) 'Owned GUI discovery exceeded eight seconds';if((Test-Path -LiteralPath $path)-and (Get-Item -LiteralPath $path).LastWriteTimeUtc -ge $Started){$record=Get-Content -Raw -LiteralPath $path|ConvertFrom-Json;Require ($record.pid -eq $Owned.process.Id -and $record.pipe) 'Wrong child discovery identity';$Owned.pipe=$record.pipe;break};Start-Sleep -Milliseconds 20}while($true)
 Require ((Request $Owned @('identify')).pid -eq $Owned.process.Id) 'Child pipe routes to another host'
 $t=Await $Owned {param($v) @($v.surfaces).Count -eq 1 -and @($v.surfaces|Where-Object {$_.ready -and $_.running -and $_.pid}).Count -eq 1};Require (-not $t.state.path -and -not $t.state.window) 'New window did not inherit temporary launch mode';return $t
}
function Adopt([int]$Id,[datetime]$Started){
 Require ($Id -gt 0 -and @($hosts|Where-Object {$_.process.Id -eq $Id}).Count -eq 0) 'CreateProcess receipt did not identify one new GUI'
 $p=[Diagnostics.Process]::GetProcessById($Id);$null=$p.Handle
 Require ([string]::Equals($p.Path,$gui,[StringComparison]::OrdinalIgnoreCase) -and $p.StartTime.ToUniversalTime() -ge $Started -and -not $p.HasExited) 'New-window receipt PID has the wrong executable or creation time'
 $owned=@{process=$p;pipe=$null;closed=$false;out=$null;err=$null};$hosts.Add($owned);Ready $owned $Started|Out-Null;return $owned
}
function Stop-Owned($Owned){
 if($Owned.closed){return};Request $Owned @('quit','--discard-state')|Out-Null;Require ($Owned.process.WaitForExit((Budget 4000))) 'Owned host did not close within four seconds';Require ($Owned.process.ExitCode -eq 0) 'Owned host exited unsuccessfully';if($Owned.out){Require ($Owned.out.Wait(500)-and $Owned.err.Wait(500)) 'Sibling GUI inherited the closed parent stdout/stderr pipes'};$Owned.closed=$true
}
function Identities($Tree){return (@($Tree.surfaces|Sort-Object id|ForEach-Object {$_.id+':'+$_.pid+':'+$_.session+':'+$_.view_handle+':'+$_.holder.window}) -join '|')}
function Stable($Owned,[string]$Before){$t=Tree $Owned;Require ((Same (Identities $t) $Before) -and @($t.surfaces|Where-Object {-not $_.running}).Count -eq 0) 'New window changed its source terminal identity, PID, session or view';return $t}
function Shortcut($Owned,[string]$Surface,[hashtable]$Event,[bool]$Forwarded){$r=Request $Owned @('test-shortcut',$Surface,($Event|ConvertTo-Json -Compress));Require ($r.surface -ceq $Surface -and $r.forwarded -eq $Forwarded) 'New-window renderer shortcut dispatch differs'}
function Context-Proof($Owned,$Source){
 $id=Request $Owned @('identify');$t=Tree $Owned;Require ($id.pid -eq $Owned.process.Id -and $id.pipe -ne $Source.pipe -and $id.surface -cne $Source.surface -and $id.workspace -cne $Source.workspace -and $id.pane -cne $Source.pane -and (Same $id.cwd $cwd)) 'Child inherited parent routing IDs or lost original Korean/NFD cwd'
 Require ([IO.Path]::GetFileNameWithoutExtension($t.surfaces[0].shell.program) -ieq 'cmd') 'Context proof requires the configured isolated CMD shell'
 $marker='NEW_'+[guid]::NewGuid().ToString('N').Substring(0,8);$expected=$marker+'|'+$Owned.pipe+'|'+$id.surface+'|'+$id.workspace+'|'+$id.pane+'|'+$cwd
 Request $Owned @('send-keys',$id.pane,('echo '+$marker+'^|%FLOWMUX_PIPE_NAME%^|%FLOWMUX_SURFACE_ID%^|%FLOWMUX_WORKSPACE_ID%^|%FLOWMUX_PANE_ID%^|%CD%'))|Out-Null;Request $Owned @('send-key','Enter','--surface',$id.surface)|Out-Null
 $watch=[Diagnostics.Stopwatch]::StartNew();do{$left=5000-$watch.ElapsedMilliseconds;Require ($left -ge 100) 'Fresh child terminal context output exceeded five seconds';$screen=Request $Owned @('read-screen','--surface',$id.surface,'--recent') ([int]$left);if($screen.text.Replace("`r",'').Replace("`n",'').Contains($expected)){return $id};Start-Sleep -Milliseconds 20}while($true)
}
try{
 $doctor=Probe @('doctor');Require ($doctor.background_testing -and $doctor.status -ceq 'ok') 'A working hidden debug build is required'
 $started=[DateTime]::UtcNow;$p=[CliProbe]::Start($gui,@('--temporary','--shell=cmd','--cwd',$cwd),$cwd,$directory);$parent=@{process=$p;pipe=$null;closed=$false;out=$p.StandardOutput.ReadToEndAsync();err=$p.StandardError.ReadToEndAsync()};$hosts.Add($parent);$tree=Ready $parent $started;$source=Request $parent @('identify');$source|Add-Member -NotePropertyName pipe -NotePropertyValue $parent.pipe -Force;$before=Identities $tree
 Request $parent @('settings','shell','cmd','--arg','/d')|Out-Null;$tree=Await $parent {param($v) @($v.surfaces[0].settings.bindings|Where-Object {$_.action -ceq 'new-window' -and $_.chord.code -ceq 'KeyN' -and $_.chord.ctrl -and $_.chord.shift -and -not $_.chord.alt}).Count -eq 1};Require (-not $tree.last_new_window_pid) 'Fresh parent unexpectedly has a new-window receipt'
 $default=@{code='KeyN';key='n';ctrlKey=$true;shiftKey=$true}
 foreach($guard in @(@{repeat=$true;forwarded=$false},@{isComposing=$true;forwarded=$true},@{keyCode=229;forwarded=$true})){$event=$default.Clone();$expected=$guard.forwarded;foreach($name in $guard.Keys){if($name -ne 'forwarded'){$event[$name]=$guard[$name]}};Shortcut $parent $source.surface $event $expected;$tree=Stable $parent $before;Require (-not $tree.last_new_window_pid) 'Guarded new-window shortcut launched a GUI'}
 $started=[DateTime]::UtcNow;Shortcut $parent $source.surface $default $false;$tree=Await $parent {param($v) $v.last_new_window_pid -gt 0};$child=Adopt ([int]$tree.last_new_window_pid) $started;$childTree=Tree $child;Require ($child.process.Id -ne $parent.process.Id -and $childTree.window_handle -ne $tree.window_handle) 'New window reused the source host/window';Stable $parent $before|Out-Null;$childIdentity=Context-Proof $child $source;$checks+='renderer-default-and-repeat-IME-229-guards-launch-exact-independent-hidden-host'
 $childBefore=Identities (Tree $child);Stop-Owned $parent;$childTree=Stable $child $childBefore;$childIdentity=Context-Proof $child $source;$checks+='child-retains-fresh-routing-Korean-cwd-and-live-session-after-parent-clean-exit'
 Request $child @('settings','keybindings','set','new-window','Ctrl+Alt+Y')|Out-Null;$tree=Await $child {param($v) @($v.surfaces[0].settings.bindings|Where-Object {$_.action -ceq 'new-window' -and $_.chord.code -ceq 'KeyY' -and $_.chord.ctrl -and $_.chord.alt -and -not $_.chord.shift}).Count -eq 1}
 Shortcut $child $childIdentity.surface $default $true;$tree=Stable $child $childBefore;Require (-not $tree.last_new_window_pid) 'Old default still launched after rebind';$started=[DateTime]::UtcNow;Shortcut $child $childIdentity.surface @{code='KeyY';key='y';ctrlKey=$true;altKey=$true} $false;$tree=Await $child {param($v) $v.last_new_window_pid -gt 0};$third=Adopt ([int]$tree.last_new_window_pid) $started;Stable $child $childBefore|Out-Null;$thirdIdentity=Context-Proof $third $childIdentity;$checks+='custom-new-window-binding-replaces-default-and-launches-an-independent-host'
 Request $child @('detach-tab',$childIdentity.surface)|Out-Null;$tree=Await $child {param($v) $v.main_empty -and @($v.detached_windows).Count -eq 1};$detached=@($tree.detached_windows|Where-Object {$_.surface -ceq $childIdentity.surface})[0];Require ($detached.window_handle -gt 0 -and -not $detached.native_visible) 'Last-tab fixture did not leave an empty hidden main window';$detachedBefore=Identities $tree
 $header=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'workspace_header' -and $_.layout_visible});Require ($header.Count -eq 1) 'Empty main has no workspace creation menu';[OptionsFixture]::ClickMenu([long]$tree.window_handle,[long]$header[0].handle,$child.process.Id)
 $tree=Await $child {param($v) $v.tab_menu.kind -ceq 'creation'};$menu=$tree.tab_menu.menu;$row=@($menu.rows|Where-Object {$_.label -ceq 'New window' -and $_.enabled});Require ($row.Count -eq 1) 'Empty main creation menu lacks New window';$previous=[int]$tree.last_new_window_pid;$started=[DateTime]::UtcNow;[OptionsFixture]::ClickMenu([long]$menu.window,[long]$row[0].window,$child.process.Id)
 $tree=Await $child {param($v) $v.last_new_window_pid -gt 0 -and $v.last_new_window_pid -ne $previous};$fourth=Adopt ([int]$tree.last_new_window_pid) $started;$tree=Stable $child $detachedBefore;Require ($tree.main_empty -and @($tree.detached_windows|Where-Object {$_.surface -ceq $childIdentity.surface -and $_.window_handle -eq $detached.window_handle}).Count -eq 1) 'Empty-main New window moved or closed its detached terminal';Context-Proof $fourth $childIdentity|Out-Null;$checks+='empty-main-creation-menu-launches-New-window-without-touching-detached-session'
 Request $child @('focus-tab',$childIdentity.surface)|Out-Null;Require ((Request $child @('identify')).surface -ceq $childIdentity.surface) 'Detached shortcut source was not selected'
 $previous=[int]$tree.last_new_window_pid;$started=[DateTime]::UtcNow;Shortcut $child $childIdentity.surface @{code='KeyY';key='y';ctrlKey=$true;altKey=$true} $false
 $tree=Await $child {param($v) $v.last_new_window_pid -gt 0 -and $v.last_new_window_pid -ne $previous};$fifth=Adopt ([int]$tree.last_new_window_pid) $started;$tree=Stable $child $detachedBefore;Require ($tree.main_empty -and @($tree.detached_windows|Where-Object {$_.surface -ceq $childIdentity.surface -and $_.window_handle -eq $detached.window_handle}).Count -eq 1) 'Detached shortcut changed its source window or session';Context-Proof $fifth $childIdentity|Out-Null;$checks+='detached-terminal-custom-shortcut-creates-independent-host-and-retains-source'
 $buttons=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'command_palette' -and $_.layout_visible});Require ($buttons.Count -eq 1) 'Empty main lacks its Command Palette button';$button=[long]$buttons[0].handle;[OptionsFixture]::Click([OptionsFixture]::Parent($button,$child.process.Id),$button,$child.process.Id)
 $tree=Await $child {param($v) $v.command_palette.open};$palette=$tree.command_palette;$description=[OptionsFixture]::Describe([long]$palette.window,$child.process.Id);Require ($description.Owner -eq $tree.window_handle) 'Empty-main palette belongs to a detached window'
 [OptionsFixture]::SetTextAndNotify([long]$palette.window,[long]$palette.query_handle,$child.process.Id,'New window');$tree=Await $child {param($v) $v.command_palette.open -and $v.command_palette.query -ceq 'New window' -and @($v.command_palette.filtered) -ccontains 'action:new-window'};$palette=$tree.command_palette
 $index=[Array]::IndexOf(@($palette.filtered),'action:new-window');$current=[int]$palette.selected_index;$key=if($current -gt $index){38}else{40};for($i=0;$i -lt [Math]::Abs($index-$current);$i++){Budget|Out-Null;[OptionsFixture]::PostKey([long]$palette.query_handle,$child.process.Id,$key,$false,$false);[OptionsFixture]::PostKey([long]$palette.query_handle,$child.process.Id,$key,$true,$false)}
 $tree=Await $child {param($v) $v.command_palette.open -and $v.command_palette.selected -ceq 'action:new-window'};$previous=[int]$tree.last_new_window_pid;$started=[DateTime]::UtcNow;[OptionsFixture]::PostEnter([long]$tree.command_palette.query_handle,$child.process.Id)
 $tree=Await $child {param($v) (-not $v.command_palette -or -not $v.command_palette.open) -and $v.last_new_window_pid -gt 0 -and $v.last_new_window_pid -ne $previous};$sixth=Adopt ([int]$tree.last_new_window_pid) $started;$tree=Stable $child $detachedBefore;Require ($tree.main_empty -and [OptionsFixture]::Describe([long]$tree.window_handle,$child.process.Id).Enabled -and @($tree.detached_windows|Where-Object {$_.surface -ceq $childIdentity.surface -and $_.window_handle -eq $detached.window_handle}).Count -eq 1) 'Empty-main palette launch changed its detached source or left the owner disabled';Context-Proof $sixth $childIdentity|Out-Null;$checks+='empty-main-Command-Palette-New-window-activation-restores-owner-and-creates-independent-host'
}
catch{$failure=$_.Exception.Message}
finally{
 $cleaning=$true
 $last.ownedProcesses=@()
 $snapshot={param($Owned,[string]$Phase)
  $record=[ordered]@{pid=$Owned.process.Id;phase=$Phase;hasExited=$null;exitCode=$null;stdoutComplete=$false;stderrComplete=$false;stdout=$null;stderr=$null}
  try{$record.hasExited=$Owned.process.HasExited;if($record.hasExited){$record.exitCode=$Owned.process.ExitCode}}catch{$record.stateError=$_.Exception.Message}
  if($Owned.out -and $Owned.out.Status -eq [Threading.Tasks.TaskStatus]::RanToCompletion){$record.stdoutComplete=$true;$record.stdout=$Owned.out.Result}
  if($Owned.err -and $Owned.err.Status -eq [Threading.Tasks.TaskStatus]::RanToCompletion){$record.stderrComplete=$true;$record.stderr=$Owned.err.Result}
  return $record
 }
 foreach($owned in $hosts){$last.ownedProcesses+=(& $snapshot $owned 'before-cleanup')}
 for($i=$hosts.Count-1;$i -ge 0;$i--){$owned=$hosts[$i];try{if(-not $owned.closed -and -not $owned.process.HasExited){Stop-Owned $owned}}catch{if(-not $failure){$failure='Cleanup: '+$_.Exception.Message}}finally{try{if(-not $owned.process.HasExited){$owned.process.Kill();[CliProbe]::WaitAfterKill($owned);if(-not $failure){$failure='Owned GUI required forced cleanup'}}}catch{if(-not $failure){$failure='Cleanup: '+$_.Exception.Message}};$last.ownedProcesses+=(& $snapshot $owned 'before-dispose');$owned.process.Dispose()}}
}
if($failure){[ordered]@{failure=$failure;checks=$checks;lastCommand=$lastCommand;hosts=$last;elapsedMs=$clock.ElapsedMilliseconds;physicalInput=$false}|ConvertTo-Json -Depth 60|Set-Content -Encoding UTF8 -LiteralPath (Join-Path $directory 'failure.json');throw $failure}
Remove-Item -LiteralPath $directory -Recurse -Force
Write-Output ('passed: '+$checks.Count+' hidden New window groups; elapsed='+$clock.ElapsedMilliseconds+'ms')
