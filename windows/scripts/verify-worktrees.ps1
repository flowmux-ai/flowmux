# SPDX-License-Identifier: GPL-3.0-or-later
# Owned hidden host and real Git worktrees only. Run under run-check.ps1 (100s).
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
if (-not $env:FLOWMUX_TEST_ARTIFACT_ROOT) { throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.' }
$ErrorActionPreference='Stop';$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path;$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe';$git=Join-Path $env:ProgramFiles 'Git\cmd\git.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'OptionsFixture.cs'),(Join-Path $PSScriptRoot 'PaneToolsFixture.cs')
Add-Type -ReferencedAssemblies System.Drawing -Path (Join-Path $PSScriptRoot 'ChromeFixture.cs')
$directory=Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT ('worktrees-'+[guid]::NewGuid());[IO.Directory]::CreateDirectory($directory)|Out-Null
$repo=Join-Path $directory '저장소 한';$paths=[ordered]@{current=(Join-Path $directory '현재 한');clean=(Join-Path $directory '제거 한');dirty=(Join-Path $directory '수정 한');locked=(Join-Path $directory '잠금 한');used=(Join-Path $directory '사용중 한')}
$junction=Join-Path $directory '연결 한';$clock=[Diagnostics.Stopwatch]::StartNew();$owned=$null;$secondary=$null;$pipeName=$null;$hostOut=$null;$hostErr=$null;$checks=@();$failure=$null;$cleanupErrors=@();$cleaning=$false;$diagnostic=[ordered]@{physicalInput=$false;physicalIme=$false;checks=@();lastTree=$null}
function Require([bool]$Value,[string]$Message){if(-not $Value){throw $Message}}
function Same([string]$A,[string]$B){return [string]::Equals($A,$B,[StringComparison]::Ordinal)}
function Path-Same([string]$A,[string]$B){return [string]::Equals($A.Replace('/','\').TrimEnd('\'),$B.Replace('/','\').TrimEnd('\'),[StringComparison]::OrdinalIgnoreCase)}
function Budget([int]$Maximum=5000){if($cleaning){return [Math]::Min($Maximum,2000)};$left=90000-$clock.ElapsedMilliseconds;Require ($left -gt 0) 'Worktrees work exceeded 90 seconds';return [int][Math]::Min($Maximum,$left)}
function Run([string]$File,[string[]]$Arguments,[int]$Exit=0,[int]$Maximum=5000){
 $diagnostic.lastCommand=@{file=$File;arguments=$Arguments};$p=[CliProbe]::Start($File,$Arguments,$directory,$directory);$out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
 try{Require ($p.WaitForExit((Budget $Maximum))) 'Owned command exceeded bounded deadline';Require ($out.Wait(500)-and $err.Wait(500)) 'Owned command pipes remained open';$stdout=[CliProbe]::Output($out);$stderr=[CliProbe]::Output($err);Require ($p.ExitCode -eq $Exit) ('Owned command failed: '+$stderr+' '+$stdout);return $stdout}
 catch{$diagnostic.commandFailure=@{file=$File;arguments=$Arguments;stdout=[CliProbe]::Output($out);stderr=[CliProbe]::Output($err)};throw}
 finally{if(-not $p.HasExited){$p.Kill();[CliProbe]::WaitAfterKill($p)};$p.Dispose()}
}
function Git([string[]]$Arguments){return Run $git (@('-c','core.quotePath=false','-c',('core.hooksPath='+$emptyHooks),'-c','commit.gpgSign=false','-c','core.autocrlf=false','-C',$repo)+$Arguments)}
function Request([string[]]$Arguments,[int]$Maximum=5000){Require ([bool]$pipeName) 'Explicit owned pipe required';return ((Run $cli (@('--pipe',$pipeName,'--json')+$Arguments) 0 $Maximum)|ConvertFrom-Json)}
function Tree([int]$Maximum=5000){$t=Request @('tree') $Maximum;Require ($t.background_testing) 'Hidden debug host required';[OptionsFixture]::Describe([long]$t.window_handle,$owned.Id)|Out-Null;$diagnostic.lastTree=$t;return $t}
function Await([scriptblock]$Condition){$watch=[Diagnostics.Stopwatch]::StartNew();do{$left=5000-$watch.ElapsedMilliseconds;Require ($left -ge 100) 'Worktrees condition exceeded five seconds';$t=Tree ([int]$left);if(& $Condition $t){return $t};Start-Sleep -Milliseconds 20}while($true)}
function Identities($Tree){return (@($Tree.surfaces|Sort-Object id|ForEach-Object {$_.id+':'+$_.pid+':'+$_.session+':'+$_.view_handle+':'+$_.holder.window}) -join '|')}
function Stable([string]$Before){$t=Tree;Require ((Same (Identities $t) $Before)-and @($t.surfaces|Where-Object {-not $_.running}).Count -eq 0) 'Worktrees UI changed an existing terminal PID/session/view';return $t}
function Passed([string]$Name){$script:checks+=,$Name;$diagnostic.checks=$checks}
function Git-Paths{return @((Git @('worktree','list','--porcelain','-z')).Split([char]0)|Where-Object {$_.StartsWith('worktree ')}|ForEach-Object {$_.Substring(9)})}
function Exists-In-Git([string]$Path){return @(Git-Paths|Where-Object {Path-Same $_ $Path}).Count -eq 1}
function Row($Tree,[string]$Path){$rows=@($Tree.worktrees.panel.rows|Where-Object {Path-Same $_.path $Path});Require ($rows.Count -eq 1) ('Expected one native worktree row: '+$Path);return $rows[0]}
function Click([long]$Handle){[OptionsFixture]::ClickMenu([OptionsFixture]::Parent($Handle,$owned.Id),$Handle,$owned.Id)}
function Settled{return Await {param($t) $t.worktrees.open -and -not $t.worktrees.loading -and $t.worktrees.list -and $t.worktrees.panel}}
function Refresh{$t=Settled;Click ([long]$t.worktrees.panel.refresh);return Settled}
function Panel($Tree){$panel=$Tree.worktrees.panel;Require ($Tree.worktrees.open -and $panel.window) 'Worktrees panel is not open';$native=[OptionsFixture]::Describe([long]$panel.window,$owned.Id);Require ([OptionsFixture]::Parent([long]$panel.window,$owned.Id) -eq $Tree.window_handle -and $panel.owner -eq $Tree.window_handle -and [OptionsFixture]::Describe([long]$Tree.window_handle,$owned.Id).Enabled -and -not $panel.native_visible) 'Worktrees must remain a hidden child dock of its enabled main window';$actual=[OptionsFixture]::RelativeBounds([long]$Tree.window_handle,[long]$panel.window,$owned.Id);Require ($actual.Width -gt 0 -and $actual.Height -gt 0 -and $actual.X -eq $panel.bounds.x -and $actual.Y -eq $panel.bounds.y -and $actual.Width -eq $panel.bounds.width -and $actual.Height -eq $panel.bounds.height) 'Worktrees native dock geometry differs from layout';foreach($handle in @($panel.refresh,$panel.close)){Require ([OptionsFixture]::Parent([long]$handle,$owned.Id) -eq $panel.window) 'Worktrees header action has another parent'};return $panel}
function Dialog([string]$Decision){$t=Await {param($v) $v.worktrees.decision -ceq $Decision -and $v.worktrees.dialog};$dialog=$t.worktrees.dialog;$native=[OptionsFixture]::Describe([long]$dialog.window,$owned.Id);Require ($native.Owner -eq $t.window_handle -and -not $native.OwnerEnabled -and -not $dialog.native_visible) 'Worktree confirmation is not modal to its exact hidden main owner';return $dialog}
function Dismiss($Dialog){Click ([long]$Dialog.cancel);$t=Await {param($v) -not $v.worktrees.dialog};Require ([OptionsFixture]::Describe([long]$t.window_handle,$owned.Id).Enabled) 'Closing worktree details/confirmation did not restore its panel owner';return $t}
function Protected($Tree,[string]$Path){$row=Row $Tree $Path;Require (-not [OptionsFixture]::Describe([long]$row.remove,$owned.Id).Enabled) ('Protected worktree Remove remains enabled: '+$Path);[PaneToolsFixture]::Command($owned,[OptionsFixture]::Parent([long]$row.remove,$owned.Id),[long]$row.remove);$t=Tree;Require (-not $t.worktrees.dialog -and (Test-Path -LiteralPath $Path)-and (Exists-In-Git $Path)) 'Disabled/stale Remove acted on a protected worktree';return $t}
function Secondary-Request([string[]]$Arguments,[int]$Maximum=5000){Require ($secondary -and $secondary.pipe) 'Explicit second owned pipe required';return ((Run $cli (@('--pipe',$secondary.pipe,'--json')+$Arguments) 0 $Maximum)|ConvertFrom-Json)}
function Start-Secondary{
 $root=Join-Path $directory 'second-host';[IO.Directory]::CreateDirectory($root)|Out-Null;$started=[DateTime]::UtcNow;$watch=[Diagnostics.Stopwatch]::StartNew();$p=[CliProbe]::Start($gui,@('--temporary','--shell=cmd','--cwd',$paths.used),$directory,$root);$script:secondary=@{process=$p;pipe=$null;out=$p.StandardOutput.ReadToEndAsync();err=$p.StandardError.ReadToEndAsync()};$discovery=Join-Path $env:LOCALAPPDATA ('flowmux\windows\instances\'+$p.Id+'.json')
 do{Budget|Out-Null;Require (-not $p.HasExited -and $watch.ElapsedMilliseconds -lt 8000) 'Second owned host discovery exceeded eight seconds';if((Test-Path -LiteralPath $discovery)-and (Get-Item -LiteralPath $discovery).LastWriteTimeUtc -ge $started){$record=Get-Content -Raw -LiteralPath $discovery|ConvertFrom-Json;Require ($record.pid -eq $p.Id -and $record.pipe -ne $pipeName) 'Second host discovery is not independent';$secondary.pipe=$record.pipe;break};Start-Sleep -Milliseconds 20}while($true)
 $watch.Restart();do{$left=5000-$watch.ElapsedMilliseconds;Require ($left -ge 100) 'Second owned terminal readiness exceeded five seconds';$t=Secondary-Request @('tree') ([int]$left);$diagnostic.secondTree=$t;if(@($t.surfaces).Count -eq 1 -and $t.surfaces[0].ready -and $t.surfaces[0].running){Require ($t.background_testing -and (Path-Same $t.surfaces[0].cwd $paths.used)) 'Second host is not hidden at the protected worktree';[OptionsFixture]::Describe([long]$t.window_handle,$p.Id)|Out-Null;return $t};Start-Sleep -Milliseconds 20}while($true)
}
# IPC has a two-second reply-close grace; allow its teardown within four
# seconds, matching the existing independent-window verifier.
function Stop-Secondary{
 if(-not $secondary){return};$p=$secondary.process
 if(-not $p.HasExited){Secondary-Request @('quit','--discard-state')|Out-Null;Require ($p.WaitForExit(4000)) 'Second owned host did not quit'};Require ($p.ExitCode -eq 0) 'Second owned host exited unsuccessfully'
 $diagnostic.secondHost=@{pid=$p.Id;exitCode=$p.ExitCode;stdout=[CliProbe]::Output($secondary.out);stderr=[CliProbe]::Output($secondary.err)};$p.Dispose();$script:secondary=$null
}
function Start-Host{
 $started=[DateTime]::UtcNow;$watch=[Diagnostics.Stopwatch]::StartNew();$script:owned=[CliProbe]::Start($gui,@('--temporary','--shell=cmd','--cwd',$paths.current),$directory,$directory);$script:hostOut=$owned.StandardOutput.ReadToEndAsync();$script:hostErr=$owned.StandardError.ReadToEndAsync();$discovery=Join-Path $env:LOCALAPPDATA ('flowmux\windows\instances\'+$owned.Id+'.json')
 do{Budget|Out-Null;Require (-not $owned.HasExited -and $watch.ElapsedMilliseconds -lt 8000) 'Hidden host discovery exceeded eight seconds';if((Test-Path -LiteralPath $discovery)-and (Get-Item -LiteralPath $discovery).LastWriteTimeUtc -ge $started){$record=Get-Content -Raw -LiteralPath $discovery|ConvertFrom-Json;Require ($record.pid -eq $owned.Id) 'Discovery returned another host';$script:pipeName=$record.pipe;break};Start-Sleep -Milliseconds 20}while($true)
 return Await {param($t) @($t.surfaces).Count -eq 1 -and $t.surfaces[0].ready -and $t.surfaces[0].running}
}
$gitEnvironment=@{};$emptyHooks=Join-Path $directory 'no-hooks';[IO.Directory]::CreateDirectory($emptyHooks)|Out-Null;$emptyConfig=Join-Path $directory 'empty-git-config';[IO.File]::WriteAllText($emptyConfig,'')
try{
 Require (Test-Path -LiteralPath $git) 'Real Git for Windows is required'
 foreach($name in @('GIT_CONFIG_NOSYSTEM','GIT_CONFIG_GLOBAL','GIT_CONFIG_SYSTEM','GIT_DIR','GIT_WORK_TREE','GIT_INDEX_FILE','GIT_COMMON_DIR','GIT_OBJECT_DIRECTORY','GIT_ALTERNATE_OBJECT_DIRECTORIES','GIT_CEILING_DIRECTORIES','GIT_CONFIG','GIT_CONFIG_PARAMETERS','GIT_CONFIG_COUNT','PATH','LOCALAPPDATA')){$gitEnvironment[$name]=[Environment]::GetEnvironmentVariable($name,'Process');[Environment]::SetEnvironmentVariable($name,$null,'Process')}
 $isolatedLocal=Join-Path $directory 'isolated-localappdata';[IO.Directory]::CreateDirectory($isolatedLocal)|Out-Null;[Environment]::SetEnvironmentVariable('LOCALAPPDATA',$isolatedLocal,'Process')
 [Environment]::SetEnvironmentVariable('PATH',((Split-Path $git -Parent)+';'+(Join-Path $env:WINDIR 'System32')+';'+$env:WINDIR+';'+(Join-Path $env:WINDIR 'System32\WindowsPowerShell\v1.0')),'Process')
 [Environment]::SetEnvironmentVariable('GIT_CONFIG_NOSYSTEM','1','Process');[Environment]::SetEnvironmentVariable('GIT_CONFIG_GLOBAL',$emptyConfig,'Process');[Environment]::SetEnvironmentVariable('GIT_CONFIG_SYSTEM',$emptyConfig,'Process')
 [IO.Directory]::CreateDirectory($repo)|Out-Null;Git @('init','--initial-branch=fixture-main')|Out-Null;Git @('config','user.name','Owned worktrees fixture')|Out-Null;Git @('config','user.email','worktrees@example.invalid')|Out-Null;Git @('config','core.hooksPath',$emptyHooks)|Out-Null
 $subject='첫 커밋 한 😀';[IO.File]::WriteAllText((Join-Path $repo '한글 한.txt'),"initial 한글`n",(New-Object Text.UTF8Encoding($false)));Git @('add','--all')|Out-Null;Git @('commit','-m',$subject)|Out-Null
 foreach($key in $paths.Keys){Git @('worktree','add','-b',('fixture-'+$key),$paths[$key])|Out-Null};Git @('worktree','lock','--reason','owned fixture lock',$paths.locked)|Out-Null
 $dirtyFile=Join-Path $paths.dirty '한글 한.txt';$dirtyText="unsaved worktree 한글 한 😀`n";[IO.File]::WriteAllText($dirtyFile,$dirtyText,(New-Object Text.UTF8Encoding($false)))
 Require (@(Git-Paths).Count -eq 6) 'Actual Git fixture did not create exactly six worktrees';foreach($path in @($repo)+@($paths.Values)){Require ((Test-Path -LiteralPath $path)-and (Exists-In-Git $path)) 'Fixture worktree path is absent from actual Git membership'}
 $tree=Start-Host;$source=Request @('identify');Require (Path-Same $source.cwd $paths.current) 'Host did not start inside the linked worktree';$before=Identities $tree
 $reply=Request @('test-shortcut',$source.surface,'{"code":"KeyW","key":"w","ctrlKey":true,"altKey":true}');Require (-not $reply.forwarded) 'Actual Worktrees shortcut was not dispatched'
 $tree=Settled;$panel=Panel $tree;Require (@($tree.worktrees.list.items).Count -eq 6 -and @($panel.rows).Count -eq 6 -and (Path-Same $tree.worktrees.list.current_worktree $paths.current)) 'Native panel omitted actual Git worktrees or selected the wrong cwd'
 $head=(Git @('rev-parse','HEAD')).Trim()
 foreach($path in @($repo)+@($paths.Values)){$item=@($tree.worktrees.list.items|Where-Object {Path-Same $_.path $path});Require ($item.Count -eq 1 -and $item[0].head -ceq $head -and (Same $item[0].commit_subject $subject)) 'Git worktree HEAD/Unicode commit subject differs';$row=Row $tree $path;Require ([OptionsFixture]::Describe([long]$row.info,$owned.Id).Enabled) 'Native worktree Info is unavailable';Require (@($row.label_handles).Count -eq 5) 'Native worktree row omitted labels';foreach($handle in @($row.label_handles)+@($row.info,$row.remove)){Require ([OptionsFixture]::Parent([long]$handle,$owned.Id) -eq $panel.viewport) 'Worktree row control escaped its viewport'};Require ((Same ([OptionsFixture]::Text([long]$row.label_handles[0],$owned.Id)) $row.branch)-and (Same ([OptionsFixture]::Text([long]$row.label_handles[1],$owned.Id)) $subject)-and (Path-Same ([OptionsFixture]::Text([long]$row.label_handles[2],$owned.Id)) $path)-and (Same ([OptionsFixture]::Text([long]$row.label_handles[3],$owned.Id)) $row.badges)) 'Native worktree branch/subject/path/badges text differs from actual Git'}
 Require ((Row $tree $paths.current).badges.Contains('Activated')-and (Row $tree $paths.locked).badges.Contains('Locked')-and (Row $tree $paths.dirty).badges.Contains('Modified 1')-and (Row $tree $paths.clean).badges.Contains('Clean')) 'Native worktree state badges differ from fixture facts'
 foreach($key in $paths.Keys){$item=@($tree.worktrees.list.items|Where-Object {Path-Same $_.path $paths[$key]})[0];Require ($item.branch -ceq ('fixture-'+$key)) 'Native branch differs from real Git'}
 Click ([long](Row $tree $paths.clean).info);$info=Dialog 'info';Require (-not $info.confirm -and $info.body.Replace('/','\').Contains($paths.clean) -and $info.body.Contains('fixture-clean') -and $info.body.Contains($subject)) 'Info lacks actual path/branch/Unicode commit or offers a destructive action';$tree=Dismiss $info;Stable $before|Out-Null
 Passed 'actual-six-worktree-Git-branches-Unicode-subjects-and-owned-modeless-Info'
 # Row navigation stays within this dock; it never starts a Git operation.
 $rowPaths=@($panel.rows.path)
 function Navigate([int]$Key,[string]$Expected){
  $p=Panel (Tree);[OptionsFixture]::PostKey([long]$p.viewport,$owned.Id,$Key,$false,$false);[OptionsFixture]::PostKey([long]$p.viewport,$owned.Id,$Key,$true,$false)
  $t=Await {param($v) (Path-Same $v.worktrees.panel.selected_path $Expected)};$p=Panel $t;$row=Row $t $Expected;$size=[ChromeFixture]::Size([long]$p.viewport,$owned.Id)
  Require ($row.branch_bounds.y -ge 0 -and $row.remove_bounds.y+$row.remove_bounds.height -le $size[1]) 'Keyboard selection did not reveal the full native worktree row'
  Require (-not $t.worktrees.dialog -and -not $t.worktrees.loading) 'Navigation activated a worktree operation'
  return $t
 }
 $tree=Navigate 40 $rowPaths[0];$tree=Navigate 38 $rowPaths[-1];$tree=Navigate 40 $rowPaths[0];$tree=Navigate 35 $rowPaths[-1];$tree=Navigate 36 $rowPaths[0]
 $panel=Panel $tree;[OptionsFixture]::PostKey([long]$panel.viewport,$owned.Id,17,$false,$false);[OptionsFixture]::PostKey([long]$panel.viewport,$owned.Id,35,$false,$false);[OptionsFixture]::PostKey([long]$panel.viewport,$owned.Id,17,$true,$false)
 $tree=Tree;Require (Path-Same $tree.worktrees.panel.selected_path $rowPaths[0]) 'Ctrl navigation was consumed as plain row selection'
 # Select by a real native STATIC notification and retain that path over Refresh.
 $row=Row $tree $rowPaths[2];Click ([long]$row.label_handles[2]);$tree=Await {param($v) (Path-Same $v.worktrees.panel.selected_path $rowPaths[2])};$infoHandle=(Row $tree $rowPaths[2]).info
 $tree=Refresh;Require ((Path-Same $tree.worktrees.panel.selected_path $rowPaths[2])-and (Row $tree $rowPaths[2]).info -eq $infoHandle) 'Refresh changed the selected path or rebuilt its native actions'
 $tree=Navigate 35 $rowPaths[-1];$panel=Panel $tree;$row=Row $tree $rowPaths[-1];$paint=Join-Path $directory 'worktree-selection.bmp';$capture=Request @('chrome-capture',$paint);Require ($capture.root_handle -eq $panel.window) 'Capture did not select the owned worktree dock'
 $edgeX=[int]$panel.viewport_bounds.x;$edgeY=[int]($panel.viewport_bounds.y+$row.branch_bounds.y);Require ([ChromeFixture]::Pixel($paint,$edgeX,$edgeY) -cne [ChromeFixture]::Pixel($paint,($edgeX+2),$edgeY)) 'Production viewport painter omitted the selected row outline';Remove-Item -LiteralPath $paint -Force
 $tree=Navigate 36 $rowPaths[0];Stable $before|Out-Null;Passed 'native_arrow_Home_End_wrap_scroll_Unicode_path_selection_refresh_and_production_outline'
 $originalSize=[ChromeFixture]::Size([long]$tree.window_handle,$owned.Id);$scale=[OptionsFixture]::Describe([long]$tree.window_handle,$owned.Id).Dpi/96.0;$sidebar=[int]$tree.chrome.sidebar_actual_width
 try{
  foreach($dip in @(200,250,300)){$width=$sidebar+2*[int][Math]::Round(4*$scale)+[int][Math]::Round(160*$scale)+[int][Math]::Round($dip*$scale);[ChromeFixture]::Resize([long]$tree.window_handle,$owned.Id,$width,$originalSize[1]);$tree=Await {param($t) $t.worktrees.panel.open -and [Math]::Abs($t.worktrees.panel.bounds.width-$dip*$scale) -le 1};$panel=Panel $tree;$viewport=[ChromeFixture]::Size([long]$panel.viewport,$owned.Id)
   foreach($row in $panel.rows){$infoRect=[OptionsFixture]::RelativeBounds([long]$panel.viewport,[long]$row.info,$owned.Id);$removeRect=[OptionsFixture]::RelativeBounds([long]$panel.viewport,[long]$row.remove,$owned.Id);Require ($infoRect.Width -gt 0 -and $removeRect.Width -gt 0 -and $infoRect.X -ge 0 -and $infoRect.X+$infoRect.Width -le $removeRect.X -and $removeRect.X+$removeRect.Width -le $viewport[0]) 'Narrow worktree Info/Remove overlap or escape the viewport horizontally'}
   $first=[OptionsFixture]::RelativeBounds([long]$panel.viewport,[long]$panel.rows[0].remove,$owned.Id);Require ($first.Y -ge 0 -and $first.Y+$first.Height -le $viewport[1]) 'First visible narrow-row action leaves the scroll viewport'
  }
  $width=$sidebar+2*[int][Math]::Round(4*$scale)+[int][Math]::Round(160*$scale)+[int][Math]::Round(190*$scale);[ChromeFixture]::Resize([long]$tree.window_handle,$owned.Id,$width,$originalSize[1]);$tree=Await {param($t) $t.worktrees.open -and -not $t.worktrees.panel.open -and $null -eq $t.worktrees.panel.bounds};Require (-not $tree.worktrees.panel.native_visible) 'Too-narrow worktree dock remained visible'
 }finally{[ChromeFixture]::Resize([long](Tree).window_handle,$owned.Id,$originalSize[0],$originalSize[1])}
 $tree=Await {param($t) $t.worktrees.panel.open};Stable $before|Out-Null;Passed '200-250-300-DIP-dock-keeps-native-actions-apart-and-narrower-dock-hides-then-restores'
 foreach($path in @($repo,$paths.current,$paths.locked)){$tree=Protected $tree $path}
 Request @('new-tab','--cwd',$paths.used,'--shell=cmd')|Out-Null;$used=Request @('identify');$tree=Await {param($t) @($t.surfaces).Count -eq 2 -and @($t.surfaces|Where-Object {-not $_.ready -or -not $_.running}).Count -eq 0};Request @('focus-tab',$source.surface)|Out-Null;$tree=Refresh;$before=Identities $tree;$tree=Protected $tree $paths.used
 Request @('detach-tab',$used.surface)|Out-Null;$tree=Await {param($t) @($t.detached_windows|Where-Object {$_.surface -ceq $used.surface -and -not $_.native_visible}).Count -eq 1};Request @('focus-tab',$source.surface)|Out-Null;$tree=Refresh;$tree=Protected $tree $paths.used;Stable $before|Out-Null
 Passed 'main-current-locked-and-live-main-or-detached-worktrees-reject-native-stale-Remove'
 # Remove the direct-path user first: only the junction-backed session may
 # explain protection in this group; the original guard must not mask a bug.
 Request @('close-tab',$used.surface)|Out-Null;$tree=Await {param($t) @($t.surfaces).Count -eq 1 -and @($t.detached_windows).Count -eq 0}
 # Shared discovery is isolated to this fixture. Only the second owned
 # host uses this target, so local or junction guards cannot mask this case.
 $secondTree=Start-Secondary;$secondBefore=Identities $secondTree;$tree=Refresh;$before=Identities $tree;Require (@($tree.surfaces|Where-Object {Path-Same $_.cwd $paths.used}).Count -eq 0) 'A local user would mask the cross-window guard'
 Require (@(Get-ChildItem -LiteralPath (Join-Path $env:LOCALAPPDATA 'flowmux\windows\instances') -Filter '*.json').Count -eq 2) 'Fixture discovery contains a host other than the two owned windows'
 Click ([long](Row $tree $paths.used).remove);$confirm=Dialog 'remove';Click ([long]$confirm.confirm)
 $tree=Await {param($t) $t.worktrees.open -and -not $t.worktrees.loading -and -not $t.worktrees.dialog -and $t.worktrees.error -like '*Close tabs and workspaces*'}
 Require ((Test-Path -LiteralPath $paths.used)-and (Exists-In-Git $paths.used)-and (Same ([IO.File]::ReadAllText((Join-Path $paths.used '한글 한.txt'))) "initial 한글`n")) 'Cross-window rejection changed owned worktree bytes or Git membership';Stable $before|Out-Null;$secondTree=Secondary-Request @('tree');Require ((Same (Identities $secondTree) $secondBefore)-and $secondTree.surfaces[0].running) 'Cross-window check replaced the second host terminal';Stop-Secondary;$tree=Refresh
 Passed 'isolated-second-hidden-host-protects-live-worktree-without-touching-user-windows'
 Run $env:ComSpec @('/D','/C',('mklink /J "'+$junction+'" "'+$paths.used+'"'))|Out-Null;Require (([IO.File]::GetAttributes($junction) -band [IO.FileAttributes]::ReparsePoint) -ne 0) 'Owned junction was not created'
 Request @('new-tab','--cwd',$junction,'--shell=cmd')|Out-Null;$alias=Request @('identify');$tree=Await {param($t) @($t.surfaces).Count -eq 2 -and @($t.surfaces|Where-Object {-not $_.ready -or -not $_.running}).Count -eq 0};Require (Path-Same $alias.cwd $junction) 'Junction fixture did not preserve its alias cwd';Require (@($tree.surfaces|Where-Object {Path-Same $_.cwd $paths.used}).Count -eq 0) 'A direct worktree cwd would mask the junction guard'
 Request @('focus-tab',$source.surface)|Out-Null;$tree=Refresh;$before=Identities $tree
 # UI protects lexical paths without filesystem I/O; the worker resolves the
 # junction after explicit confirmation and must reject its real live target.
 Click ([long](Row $tree $paths.used).remove);$confirm=Dialog 'remove';Click ([long]$confirm.confirm)
 $tree=Await {param($t) $t.worktrees.open -and -not $t.worktrees.loading -and -not $t.worktrees.dialog -and $t.worktrees.error -like '*Close tabs and workspaces*'}
 Require ((Test-Path -LiteralPath $junction)-and (Test-Path -LiteralPath $paths.used)-and (Exists-In-Git $paths.used)-and (Same ([IO.File]::ReadAllText((Join-Path $junction '한글 한.txt'))) "initial 한글`n")) 'Worker rejection changed the junction-backed worktree bytes or Git membership';Stable $before|Out-Null
 Passed 'junction-only-live-cwd-protects-real-worktree-without-direct-path-user'
 Click ([long](Row $tree $paths.clean).remove);$confirm=Dialog 'remove';Require ($confirm.confirm -gt 0 -and $confirm.body.Replace('/','\').Contains($paths.clean)) 'Clean removal confirmation lost its exact path';$tree=Dismiss $confirm;Require ((Test-Path -LiteralPath $paths.clean)-and (Exists-In-Git $paths.clean)) 'Cancel deleted clean worktree'
 Click ([long](Row $tree $paths.clean).remove);$confirm=Dialog 'remove';Click ([long]$confirm.confirm);$tree=Await {param($t) $t.worktrees.open -and -not $t.worktrees.loading -and -not $t.worktrees.dialog -and @($t.worktrees.list.items|Where-Object {Path-Same $_.path $paths.clean}).Count -eq 0};Require (-not (Test-Path -LiteralPath $paths.clean)-and -not (Exists-In-Git $paths.clean)) 'Confirmed clean removal did not update actual filesystem and Git membership';Stable $before|Out-Null
 Passed 'clean-Remove-explicit-Cancel-preserves-then-confirm-deletes-owned-path-and-Git-membership'
 Click ([long](Row $tree $paths.dirty).remove);$confirm=Dialog 'remove';Click ([long]$confirm.confirm);$force=Dialog 'force';Require ($force.confirm -gt 0 -and (Test-Path -LiteralPath $dirtyFile)-and (Same ([IO.File]::ReadAllText($dirtyFile)) $dirtyText)-and (Exists-In-Git $paths.dirty)) 'Ordinary removal changed dirty bytes before separate Force choice';$tree=Dismiss $force;Require ((Same ([IO.File]::ReadAllText($dirtyFile)) $dirtyText)-and (Exists-In-Git $paths.dirty)) 'Cancelling Force discarded dirty worktree bytes'
 Click ([long](Row $tree $paths.dirty).remove);$confirm=Dialog 'remove';Click ([long]$confirm.confirm);$force=Dialog 'force';Click ([long]$force.confirm);$tree=Await {param($t) $t.worktrees.open -and -not $t.worktrees.loading -and -not $t.worktrees.dialog -and @($t.worktrees.list.items|Where-Object {Path-Same $_.path $paths.dirty}).Count -eq 0};Require (-not (Test-Path -LiteralPath $paths.dirty)-and -not (Exists-In-Git $paths.dirty)) 'Force did not remove exactly the owned dirty worktree';Stable $before|Out-Null
 Passed 'dirty-Git-failure-requires-separate-Force-choice-with-Cancel-byte-preservation'
 $added=Join-Path $directory '갱신 한';Git @('worktree','add','-b','fixture-refresh',$added)|Out-Null;$tree=Refresh;Require (@($tree.worktrees.list.items|Where-Object {Path-Same $_.path $added}).Count -eq 1) 'Refresh did not observe a real external Git worktree';$panel=Panel $tree;Click ([long]$panel.close);$tree=Await {param($t) -not $t.worktrees.open};Stable $before|Out-Null
 Request @('new-tab','--cwd',$repo,'--shell=cmd')|Out-Null;$main=Request @('identify');$tree=Await {param($t) @($t.surfaces).Count -eq 3 -and @($t.surfaces|Where-Object {-not $_.ready -or -not $_.running}).Count -eq 0};$before=Identities $tree
 $buttons=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'worktrees' -and $_.layout_visible});Require ($buttons.Count -eq 1) 'Worktrees sidebar entry is missing';Click ([long]$buttons[0].handle);$tree=Settled;Require ($tree.worktrees.source.surface -ceq $main.surface -and (Path-Same $tree.worktrees.source.cwd $repo)-and (Path-Same $tree.worktrees.list.current_worktree $repo)) 'Reopening panel retained the previous surface/cwd result';$tree=Protected $tree $repo;Stable $before|Out-Null
 Passed 'Refresh-observes-external-Git-and-close-reopen-uses-new-local-surface-cwd'
}
catch{$failure=$_.Exception.Message}
finally{
 $cleaning=$true
 if($secondary){try{Stop-Secondary}catch{$cleanupErrors+=,$_.Exception.Message}finally{if($secondary){$p=$secondary.process;try{if(-not $p.HasExited){$p.Kill();[CliProbe]::WaitAfterKill($p)};$diagnostic.secondHost=@{pid=$p.Id;exitCode=$p.ExitCode;stdout=[CliProbe]::Output($secondary.out);stderr=[CliProbe]::Output($secondary.err)}}catch{$cleanupErrors+=,$_.Exception.Message}finally{$p.Dispose();$secondary=$null}}}}
 if($owned){try{if(-not $owned.HasExited){Request @('quit','--discard-state')|Out-Null;Require ($owned.WaitForExit(4000)) 'Owned host did not quit'};Require ($owned.ExitCode -eq 0) 'Owned host exited unsuccessfully'}catch{$cleanupErrors+=,$_.Exception.Message}finally{if(-not $owned.HasExited){$owned.Kill();[CliProbe]::WaitAfterKill($owned)};$diagnostic.host=@{pid=$owned.Id;exitCode=$owned.ExitCode;stdout=[CliProbe]::Output($hostOut);stderr=[CliProbe]::Output($hostErr)};$owned.Dispose()}}
 # Nonrecursive Directory.Delete removes the junction itself, never its
 # target. Do this before any recursive fixture cleanup, including failures.
 try{if(Test-Path -LiteralPath $junction){Require (([IO.File]::GetAttributes($junction) -band [IO.FileAttributes]::ReparsePoint) -ne 0) 'Refusing non-junction cleanup at alias path';[IO.Directory]::Delete($junction);Require (-not (Test-Path -LiteralPath $junction)-and (Test-Path -LiteralPath $paths.used)) 'Junction cleanup removed its target or retained the alias'}}catch{$cleanupErrors+=,$_.Exception.Message}
 foreach($name in $gitEnvironment.Keys){[Environment]::SetEnvironmentVariable($name,$gitEnvironment[$name],'Process')}
}
if($failure -or $cleanupErrors.Count){$diagnostic.failure=$failure;$diagnostic.cleanupErrors=$cleanupErrors;$diagnostic.elapsedMs=$clock.ElapsedMilliseconds;$diagnostic|ConvertTo-Json -Depth 60|Set-Content -Encoding UTF8 -LiteralPath (Join-Path $directory 'failure.json');throw ($failure+' '+($cleanupErrors -join '; '))}
Remove-Item -LiteralPath $directory -Recurse -Force
Write-Output ('passed: '+$checks.Count+' hidden real-Git Worktrees groups; elapsed='+$clock.ElapsedMilliseconds+'ms')
