# SPDX-License-Identifier: GPL-3.0-or-later
# Actual owned agent process, child-only home and native history UI; no account access.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
if(-not $env:FLOWMUX_TEST_ARTIFACT_ROOT){throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.'}
$ErrorActionPreference='Stop';$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path;$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'OptionsFixture.cs')
Add-Type -ReferencedAssemblies System.Drawing -Path (Join-Path $PSScriptRoot 'ChromeFixture.cs')
$directory=Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT 'sessions';[IO.Directory]::CreateDirectory($directory)|Out-Null
$clock=[Diagnostics.Stopwatch]::StartNew();$owned=$null;$pipeName=$null;$hostOut=$null;$hostErr=$null;$failure=$null;$cleanupErrors=@();$cleaning=$false;$checks=@();$savedLocal=$env:LOCALAPPDATA;$originalCodexHome=$env:CODEX_HOME
$diagnostic=[ordered]@{physicalInput=$false;clipboardAccess=$false;realAccount=$false;checks=@();lastTree=$null};$utf8=New-Object Text.UTF8Encoding($false)
function Require([bool]$Value,[string]$Message){if(-not $Value){throw $Message}}
function Path-Same([string]$A,[string]$B){return [string]::Equals($A.Replace('/','\').TrimEnd('\'),$B.Replace('/','\').TrimEnd('\'),[StringComparison]::OrdinalIgnoreCase)}
function Budget([int]$Maximum=5000){if($cleaning){return [Math]::Min($Maximum,2000)};$left=75000-$clock.ElapsedMilliseconds;Require ($left -gt 0) 'Agent sessions verification exceeded75s';return [int][Math]::Min($Maximum,$left)}
function Request([string[]]$Arguments,[int]$Maximum=5000,[bool]$ExpectFailure=$false,[string]$WorkingDirectory=$directory){
 Require ([bool]$pipeName) 'Explicit owned pipe required';$diagnostic.lastCommand=$Arguments;$p=[CliProbe]::Start($cli,(@('--pipe',$pipeName,'--json')+$Arguments),$WorkingDirectory,$directory);$out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
 try{Require ($p.WaitForExit((Budget $Maximum))) 'Owned CLI exceeded deadline';Require ($out.Wait(500)-and $err.Wait(500)) 'Owned CLI pipes did not close';if($ExpectFailure){Require ($p.ExitCode -ne 0) 'Invalid agent report was accepted';return [CliProbe]::Output($err)};Require ($p.ExitCode -eq 0) ('Owned CLI failed: '+[CliProbe]::Output($err));return ([CliProbe]::Output($out)|ConvertFrom-Json)}
 catch{$diagnostic.commandFailure=@{arguments=$Arguments;stdout=[CliProbe]::Output($out);stderr=[CliProbe]::Output($err)};throw}
 finally{if(-not $p.HasExited){$p.Kill();[CliProbe]::WaitAfterKill($p)};$p.Dispose()}
}
function Tree([int]$Maximum=5000){$t=Request @('tree') $Maximum;Require ($t.background_testing -and -not $owned.HasExited -and -not [CliProbe]::IsWindowVisible([IntPtr][long]$t.window_handle)) 'Owned hidden host unavailable';[OptionsFixture]::Describe([long]$t.window_handle,$owned.Id)|Out-Null;$diagnostic.lastTree=$t;return $t}
function Await([scriptblock]$Condition){$watch=[Diagnostics.Stopwatch]::StartNew();do{$left=5000-$watch.ElapsedMilliseconds;Require ($left -ge 100) 'Agent sessions condition exceeded5s';$t=Tree ([int]$left);if(& $Condition $t){return $t};Start-Sleep -Milliseconds 20}while($true)}
function Screen([string]$Surface,[scriptblock]$Condition){$watch=[Diagnostics.Stopwatch]::StartNew();do{$left=5000-$watch.ElapsedMilliseconds;Require ($left -ge 100) 'Agent screen condition exceeded5s';$s=Request @('read-screen','--surface',$Surface,'--recent') ([int]$left);$diagnostic.lastScreen=$s;if(& $Condition $s){return $s};Start-Sleep -Milliseconds 20}while($true)}
function Click([long]$Handle){[OptionsFixture]::Click([OptionsFixture]::Parent($Handle,$owned.Id),$Handle,$owned.Id)}
function Stable($Before){$t=Tree;foreach($s in $Before){$n=@($t.surfaces|Where-Object {$_.id -ceq $s.id});Require ($n.Count -eq 1 -and $n[0].pid -eq $s.pid -and $n[0].session -ceq $s.session -and $n[0].view_handle -eq $s.view_handle -and $n[0].holder.window -eq $s.holder.window -and $n[0].running) 'Sessions changed a retained terminal PID/session/view/holder'};return $t}
function Passed([string]$Name){$script:checks+=,$Name;$diagnostic.checks=$checks;Require ($env:CODEX_HOME -ceq $originalCodexHome) 'Fixture modified parent CODEX_HOME'}
function Panel($Tree){$p=$Tree.agent_sessions.panel;Require ($Tree.agent_sessions.open -and $p.open -and -not $p.native_visible -and [OptionsFixture]::Parent([long]$p.window,$owned.Id) -eq $Tree.window_handle) 'Sessions must be an owned hidden child dock';Require ([OptionsFixture]::Describe([long]$Tree.window_handle,$owned.Id).Enabled) 'Sessions dock disabled main';foreach($h in @($p.query_handle,$p.list,$p.preview,$p.refresh,$p.close,$p.resume)){Require ([OptionsFixture]::Parent([long]$h,$owned.Id) -eq $p.window) 'Session control belongs to another owner'};return $p}
function Agent-Sidebar([string]$Name,[string]$Status,[string]$Text){
 $tree=Await {param($t) $row=@($t.chrome.controls|Where-Object {$_.kind -ceq 'workspace' -and $_.workspace -ceq $sourceIdentity.workspace})[0];$row.workspace_lines.Count -eq 3 -and $row.workspace_lines[0].agent.status -ceq $Status -and $row.workspace_lines[1].text -ceq $Text}
 $row=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'workspace' -and $_.workspace -ceq $sourceIdentity.workspace})[0];$scale=[Math]::Max(96,$tree.chrome.dpi)/96.0
 Require ($row.workspace_lines[0].text -ceq 'codex' -and $null -eq $row.workspace_lines[0].parent -and -not $row.workspace_lines[0].continues -and $row.workspace_lines[1].parent -ceq $false -and $row.workspace_lines[1].continues -and $row.workspace_lines[2].parent -ceq $false -and -not $row.workspace_lines[2].continues -and (Path-Same $row.workspace_lines[2].text $projectA)) 'Agent metadata is not Linux header/status/path nesting'
 Require ([Math]::Abs($row.rect.height-96*$scale) -le 2) 'Agent sidebar row did not grow to three metadata lines'
 Require ([OptionsFixture]::Text([long]$row.handle,$owned.Id) -ceq ($row.label) -and $row.label.Contains($Text.Replace('&','&&')) -and $row.label.Contains(('codex ('+$Status+')'))) 'Native accessible caption differs from raw report text'
 $bmp=Join-Path $directory ($Name+'.bmp');$png=Join-Path $directory ($Name+'.png');Request @('chrome-capture',$bmp)|Out-Null;[ChromeFixture]::Png($bmp,$png)
 $background=if($Status -eq 'blocked'){'#442b31'}elseif($Status -eq 'done'){'#27334a'}else{'#24272e'}
 $ink=if($Status -eq 'blocked'){'#ef4444'}elseif($Status -eq 'done'){'#3b82f6'}elseif($Status -eq 'working'){'#f59e0b'}else{'#abb1bc'}
 $x=$row.rect.x;$y=$row.rect.y;$gutter=$x+[int][Math]::Round(21*$scale);$nested=$gutter+[int][Math]::Round(14*$scale)
 Require ([ChromeFixture]::Pixel($png,($x+$row.rect.width-4),($y+$row.rect.height-4)) -ceq $background) 'Agent workspace tint differs from Linux rgba16/14 percent'
 Require ([ChromeFixture]::ColorCount($png,($gutter+8),($y+[int](27*$scale)),([int](46*$scale)),([int](20*$scale)),$ink) -gt 2) 'Native agent header status icon/color missing'
 Require ([ChromeFixture]::Pixel($png,$gutter,($y+[int](39*$scale))) -ceq $background -and [ChromeFixture]::Pixel($png,$nested,($y+[int](48*$scale))) -ceq '#abb1bc' -and [ChromeFixture]::Pixel($png,$nested,($y+[int](80*$scale))) -ceq $background) 'Nested native tree connectors continue through the wrong branch'
 $diagnostic.agentCapture=@{path=$png;row=$row};return $row.handle
}
function Query($Tree,[string]$Text){$p=Panel $Tree;[OptionsFixture]::SetTextAndNotify([long]$p.window,[long]$p.query_handle,$owned.Id,$Text)}
function Select-Session($Tree,[string]$Id){$p=Panel $Tree;$row=@($p.rows|Where-Object {$_.id -ceq $Id});Require ($row.Count -eq 1) 'Expected exact visible session row';[OptionsFixture]::ListSelect([long]$p.window,[long]$p.list,$owned.Id,[int]$row[0].index);return Await {param($t) $t.agent_sessions.selected -ceq $Id -and $t.agent_sessions.panel.resume_enabled -and -not $t.agent_sessions.loading}}
function Shortcut([string]$Surface){Request @('test-shortcut',$Surface,'{"code":"KeyJ","key":"j","ctrlKey":true,"altKey":true}')|Out-Null}
function Agent-Tab([string]$HistoryHome,[string]$Cwd){Request @('new-tab','--cwd',$Cwd,'--shell',$agentExe,'--shell-arg=--hold','--shell-arg',$HistoryHome)|Out-Null;$id=Request @('identify');$tree=Await {param($t) @($t.surfaces|Where-Object {$_.id -ceq $id.surface -and $_.ready -and $_.running}).Count -eq 1};Screen $id.surface {param($s) $s.text.Contains('SESSION_SOURCE_READY')}|Out-Null;return @($tree.surfaces|Where-Object {$_.id -ceq $id.surface})[0]}
function Write-History([string]$HistoryHome,[string]$Id,[string]$Cwd,[string]$Title){
 $path=Join-Path $HistoryHome ('sessions\2026\09\29\rollout-'+$Id+'.jsonl');[IO.Directory]::CreateDirectory((Split-Path $path))|Out-Null
 $records=@(@{type='session_meta';payload=@{id=$Id;cwd=$Cwd;source='cli'}},@{type='response_item';payload=@{type='message';role='developer';content=@(@{type='input_text';text='PRIVATE_SYSTEM_NEVER_PREVIEW'})}},@{type='response_item';payload=@{type='function_call';arguments='PRIVATE_TOOL_NEVER_PREVIEW'}},@{type='response_item';payload=@{type='message';role='user';content=@(@{type='input_text';text=('질문 한 é 😀 '+$Id)})}},@{type='response_item';payload=@{type='message';role='assistant';content=@(@{type='output_text';text="답변 한글`n두 번째 줄"})}},@{type='event_msg';payload=@{type='agent_message';message='PRIVATE_EVENT_NEVER_PREVIEW'}})
 [IO.File]::WriteAllLines($path,[string[]]@($records|ForEach-Object {$_|ConvertTo-Json -Depth 10 -Compress}),$utf8);[IO.File]::AppendAllText((Join-Path $HistoryHome 'session_index.jsonl'),((@{id=$Id;thread_name=$Title}|ConvertTo-Json -Compress)+"`n"),$utf8)
}
try{
 $env:LOCALAPPDATA=Join-Path $directory 'localappdata';[IO.Directory]::CreateDirectory($env:LOCALAPPDATA)|Out-Null
 $homeA=Join-Path $directory 'agent-home 한 A';$homeB=Join-Path $directory 'agent-home 한글 B';$projectA=Join-Path $directory '프로젝트 한 A';$projectB=Join-Path $directory '프로젝트 한글 B';foreach($p in @($homeA,$homeB,$projectA,$projectB)){[IO.Directory]::CreateDirectory($p)|Out-Null}
 $idA=[guid]::NewGuid().ToString();$idB=[guid]::NewGuid().ToString();$idOther=[guid]::NewGuid().ToString();$titleA='원본 한 é 😀 A';$titleB='다른 프로젝트 한글 B';Write-History $homeA $idA $projectA $titleA;Write-History $homeA $idB $projectB $titleB;Write-History $homeB $idOther $projectB '별도 agent home 전용'
 $longPreview=(('긴 미리보기 한 é 😀 '+('x'*96)+"`n")*3000)+'LONG_PREVIEW_END';Require ($longPreview.Length -gt 262144 -and $utf8.GetByteCount($longPreview) -lt 2*1024*1024) 'Large preview fixture is outside the supported regression range';$longRecord=@{type='response_item';payload=@{type='message';role='assistant';content=@(@{type='output_text';text=$longPreview})}};[IO.File]::AppendAllText((Join-Path $homeB ('sessions\2026\09\29\rollout-'+$idOther+'.jsonl')),(($longRecord|ConvertTo-Json -Depth 10 -Compress)+"`n"),$utf8)
 $agentExe=Join-Path $directory 'codex.exe'
 $code=@'
using System;
using System.IO;
using System.Text;
using System.Threading;
using System.Diagnostics;
using System.Web.Script.Serialization;
public static class OwnedCodexSessionFixture {
 public static int Main(string[] args) {
  Console.OutputEncoding=new UTF8Encoding(false);
  if(args.Length==2 && args[0]=="--hold") {
   Environment.SetEnvironmentVariable("CODEX_HOME",Path.GetFullPath(args[1]),EnvironmentVariableTarget.Process);
   Console.WriteLine("SESSION_SOURCE_READY");Console.Out.Flush();Thread.Sleep(60000);return 0;
  }
  if(args.Length==2 && args[0]=="resume") {
   Guid id;if(!Guid.TryParse(args[1],out id))return 21;
   string home=Environment.GetEnvironmentVariable("CODEX_HOME");if(String.IsNullOrEmpty(home))return 22;
   var proof=new {argv=args,cwd=Environment.CurrentDirectory,home=home,pid=Process.GetCurrentProcess().Id,image=Process.GetCurrentProcess().MainModule.FileName};
   File.WriteAllText(Path.Combine(home,"resume-"+id.ToString()+".json"),new JavaScriptSerializer().Serialize(proof),new UTF8Encoding(false));
   Console.WriteLine("RESUMED_"+id.ToString());return 0;
  }
  return 23;
 }
}
'@
 Add-Type -TypeDefinition $code -Language CSharp -ReferencedAssemblies System.dll,System.Web.Extensions.dll -OutputAssembly $agentExe -OutputType ConsoleApplication
 $started=[DateTime]::UtcNow;$watch=[Diagnostics.Stopwatch]::StartNew();$owned=[CliProbe]::Start($gui,@('--temporary','--shell=cmd','--cwd',$projectA),$projectA,$directory);$hostOut=$owned.StandardOutput.ReadToEndAsync();$hostErr=$owned.StandardError.ReadToEndAsync();$discovery=Join-Path $env:LOCALAPPDATA ('flowmux\windows\instances\'+$owned.Id+'.json')
 do{Budget|Out-Null;Require (-not $owned.HasExited -and $watch.ElapsedMilliseconds -lt 8000) 'Owned startup exceeded8s';if((Test-Path -LiteralPath $discovery)-and (Get-Item -LiteralPath $discovery).LastWriteTimeUtc -ge $started){$record=Get-Content -Raw -Encoding UTF8 -LiteralPath $discovery|ConvertFrom-Json;Require ($record.pid -eq $owned.Id) 'Wrong discovery host';$pipeName=$record.pipe;break};Start-Sleep -Milliseconds 20}while($true)
 $tree=Await {param($t) @($t.surfaces).Count -eq 1 -and $t.surfaces[0].ready -and $t.surfaces[0].running};$local=$tree.surfaces[0]
 Request @('rename-tab',$local.id,'Codex')|Out-Null
 Require (@(Request @('agents')).Count -eq 0) 'Plain shell or a Codex-looking title was classified as an agent'
 Request @('settings','shell','cmd','--arg','/d')|Out-Null;$source=Agent-Tab $homeA $projectA;$sourceIdentity=Request @('identify')
 $agentClock=[Diagnostics.Stopwatch]::StartNew();$agents=@(Request @('agents'));$diagnostic.agentListMs=$agentClock.ElapsedMilliseconds
 Require ($agents.Count -eq 1 -and $agents[0].agent -ceq 'codex' -and $agents[0].pid -eq $source.pid -and $agents[0].tab -ceq $source.id -and $agents[0].pane -ceq $sourceIdentity.pane -and $agents[0].workspace_id -ceq $sourceIdentity.workspace -and (Path-Same $agents[0].cwd $projectA) -and $agents[0].status -ceq 'unknown' -and -not $agents[0].messaging -and -not $agents[0].session_name -and $agentClock.ElapsedMilliseconds -lt 3000) 'Window agent list lost owned identity/Unicode cwd, guessed activity/messaging, or exceeded its response budget'
 Require (($agents|ConvertTo-Json -Depth 5) -notmatch 'CODEX_HOME|agent-home|PRIVATE_') 'Agent list exposed process environment or session history'
 Request @('focus-tab',$local.id)|Out-Null;$agents=@(Request @('agents'));$unchanged=Request @('identify')
 Require ($agents.Count -eq 1 -and $agents[0].tab -ceq $source.id -and $unchanged.surface -ceq $local.id -and -not (Tree).agent_sessions.open) 'Agent listing omitted an inactive tab, changed focus or opened a dock'
 Request @('focus-tab',$source.id)|Out-Null;Passed 'window-agent-list-empty-spoof-rejection-owned-identity-unknown-activity-Unicode-and-inactive-tabs-without-dock'

 $report=@('report-agent','codex','--surface',$source.id,'--pid',[string]$source.pid)
 $initial=Request ($report+@('--seq','1','--status','idle'));Require ($initial.accepted -and $initial.agent.status -ceq 'idle' -and $initial.agent.seen) 'Initial idle report invented a completion alert'
 $statusText='한글 한 é 😀 상태';$r=Request ($report+@('--seq','2','--status','working','--message',$statusText))
 Require ($r.accepted -and $r.agent.status -ceq 'working' -and $r.agent.activity -ceq 'running' -and $r.agent.message -ceq $statusText) 'Working status did not preserve raw Unicode'
 $sidebarHandle=Agent-Sidebar 'agent-working' 'working' $statusText
 foreach($seq in @('1','2')){$stale=Request ($report+@('--seq',$seq,'--status','blocked','--message','stale'));Require (-not $stale.accepted -and $stale.agent.seq -eq 2 -and $stale.agent.status -ceq 'working' -and $stale.agent.message -ceq $statusText) 'Old/duplicate sequence overwrote current status'}
 $r=Request ($report+@('--seq','4','--status','blocked','--message',$statusText));Require ($r.accepted -and $r.agent.status -ceq 'blocked' -and $r.agent.activity -ceq 'needs_input' -and -not $r.agent.seen) 'Hidden blocked report lost its unseen state'
 Require ((Agent-Sidebar 'agent-blocked' 'blocked' $statusText) -eq $sidebarHandle) 'Status report replaced workspace HWND'
 $stale=Request ($report+@('--seq','3','--status','working'));Require (-not $stale.accepted -and $stale.agent.status -ceq 'blocked') 'Delayed progress cleared a newer input wait'
 $r=Request ($report+@('--seq','5','--status','idle'));Require ($r.accepted -and $r.agent.status -ceq 'done' -and $r.agent.activity -ceq 'idle' -and -not $r.agent.seen) 'Unseen idle transition did not derive Done'
 Require ((Agent-Sidebar 'agent-done' 'done' 'done') -eq $sidebarHandle) 'Done report replaced workspace HWND'
 Request @('focus-tab',$source.id)|Out-Null;$tree=Tree;$state=@($tree.surfaces|Where-Object {$_.id -ceq $source.id})[0].agent
 Require ($state.status -ceq 'done' -and $state.seq -eq 5) 'Hidden focus falsely acknowledged Done'
 $agents=@(Request @('agents'));Require ($agents[0].status -ceq 'done' -and -not $agents[0].messaging) 'Agent listing discarded reported activity or advertised fake messaging'
 $r=Request ($report+@('--seq','6','--status','working'));Require ($r.accepted -and $r.agent.status -ceq 'working' -and $r.agent.seen) 'A new turn retained an old completion alert'
 Passed 'Linux-agent-header-status-path-native-tree-tints-Unicode-and-stable-workspace-HWND'
 Stable @($source,$local)|Out-Null;Passed 'ordered-agent-reports-stale-duplicates-Unicode-blocked-unseen-completion-and-hidden-focus'
 foreach($bad in @(
  @('report-agent','codex','--surface',$source.id,'--pid',[string]$owned.Id,'--seq','7','--status','idle'),
  @('report-agent','claude','--surface',$source.id,'--pid',[string]$source.pid,'--seq','7','--status','idle'),
  @('report-agent','codex','--surface',$local.id,'--pid',[string]$source.pid,'--seq','7','--status','idle'),
  @('report-agent','codex','--pid',[string]$source.pid,'--seq','7','--status','idle'),
  ($report+@('--seq','0','--status','idle')),
  ($report+@('--seq','7','--status','done')),
  ($report+@('--seq','7','--status','working','--message',"invalid`nline")),
  ($report+@('--seq','7','--status','working','--message',('한'*342)))
 )){Request $bad 3000 $true|Out-Null}
 $state=@((Tree).surfaces|Where-Object {$_.id -ceq $source.id})[0].agent;Require ($state.seq -eq 6 -and $state.status -ceq 'working') 'Invalid report mutated an existing state'
 $savedPipe=$env:FLOWMUX_PIPE_NAME;$savedSurface=$env:FLOWMUX_SURFACE_ID
 try{$env:FLOWMUX_PIPE_NAME=$pipeName;$env:FLOWMUX_SURFACE_ID=$source.id;$r=Request @('report-agent','CODEX','--pid',[string]$source.pid,'--seq','7','--status','blocked','--message',$statusText) 3000 $false $projectA;Require ($r.accepted -and $r.surface -ceq $source.id -and $r.agent.name -ceq 'codex') 'Inherited surface or canonical agent identity failed'}finally{$env:FLOWMUX_PIPE_NAME=$savedPipe;$env:FLOWMUX_SURFACE_ID=$savedSurface}
 Passed 'report-rejects-wrong-process-provider-plain-shell-missing-context-invalid-status-sequence-and-message'
 $pending=@();try{
  foreach($seq in @('9','8')){$arguments=$report+@('--seq',$seq,'--status',$(if($seq -eq '9'){'blocked'}else{'idle'}));$process=[CliProbe]::Start($cli,(@('--pipe',$pipeName,'--json')+$arguments),$directory,$directory);$pending+=,@{process=$process;stdout=$process.StandardOutput.ReadToEndAsync();stderr=$process.StandardError.ReadToEndAsync()}}
  foreach($request in $pending){Require ($request.process.WaitForExit((Budget 3000)) -and $request.stdout.Wait(500) -and $request.stderr.Wait(500) -and $request.process.ExitCode -eq 0) 'Concurrent cached report failed or exceeded3s'}
 }finally{foreach($request in $pending){if(-not $request.process.HasExited){$request.process.Kill();[CliProbe]::WaitAfterKill($request.process)};$request.process.Dispose()}}
 $state=@((Tree).surfaces|Where-Object {$_.id -ceq $source.id})[0].agent;Require ($state.seq -eq 9 -and $state.status -ceq 'blocked') 'Concurrent reports lost the newest sequence'
 Passed 'concurrent-ordered-reports-retain-newest-state-without-discovery-workers-or-waits'


 $extras=@();$originalAgentExe=$agentExe
 try{
  foreach($provider in @('claude','opencode','cline','antigravity')){
   $agentExe=Join-Path $directory ($provider+'.exe');[IO.File]::Copy($originalAgentExe,$agentExe);$extra=Agent-Tab $homeA $projectA;$extras+=,$extra
   $status=switch($provider){'claude'{'working'} 'opencode'{'idle'} 'cline'{'unknown'} 'antigravity'{'blocked'}}
   Request @('report-agent',$provider,'--surface',$extra.id,'--pid',[string]$extra.pid,'--seq','1','--status',$status)|Out-Null
  }
 }finally{$agentExe=$originalAgentExe}
 $tree=Await {param($t) $row=@($t.chrome.controls|Where-Object {$_.kind -ceq 'workspace' -and $_.workspace -ceq $sourceIdentity.workspace})[0];@($row.workspace_lines|Where-Object {$_.agent}).Count -eq 4 -and @($row.workspace_lines).Count -eq 12}
 $row=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'workspace' -and $_.workspace -ceq $sourceIdentity.workspace})[0];$headers=@($row.workspace_lines|Where-Object {$_.agent});$scale=[Math]::Max(96,$tree.chrome.dpi)/96.0
 Require (($headers.agent.name -join ',') -ceq 'antigravity,codex,claude,opencode' -and $headers[3].text -ceq 'opencode +1 agent' -and [Math]::Abs($row.rect.height-276*$scale) -le 2) 'Sidebar agent urgency/name ordering, four-agent cap, overflow suffix or row height differs from Linux'
 Require ($row.rect.y+$row.rect.height -le $tree.chrome.sidebar_list_bottom -and $row.workspace_lines[1].parent -ceq $true -and $row.workspace_lines[10].parent -ceq $false) 'Multiple agent metadata overlaps the footer or loses ancestor continuations'
 $bmp=Join-Path $directory 'agent-providers.bmp';$png=Join-Path $directory 'agent-providers.png';Request @('chrome-capture',$bmp)|Out-Null;[ChromeFixture]::Png($bmp,$png);$diagnostic.agentProvidersCapture=$png
 for($i=0;$i -lt 4;$i++){$x=$row.rect.x+[int][Math]::Round(45*$scale);$y=$row.rect.y+[int][Math]::Round((30+60*$i)*$scale);$side=[int][Math]::Round(14*$scale);Require ([ChromeFixture]::ColorCount($png,$x,$y,$side,$side,'#442b31') -lt $side*$side-5) 'A native provider logo was omitted'}

 Request @('split','vertical','--shell=cmd')|Out-Null;$spare=Request @('identify');Request @('move-tab',$extras[3].id,'--to-pane',$spare.pane)|Out-Null
 Request @('focus-tab',$extras[3].id)|Out-Null;$tree=Tree;$row=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'workspace' -and $_.workspace -ceq $sourceIdentity.workspace})[0];Require ($row.workspace_lines[0].agent.name -ceq 'antigravity') 'Focused blocked agent pane did not lead the metadata tree'
 Request @('focus-tab',$source.id)|Out-Null;$tree=Tree;$row=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'workspace' -and $_.workspace -ceq $sourceIdentity.workspace})[0];Require ($row.workspace_lines[0].agent.name -ceq 'codex' -and $row.workspace_lines[3].agent.name -ceq 'antigravity') 'Equal-status agent blocks ignored pane MRU'
 Request @('close-tab',$spare.surface)|Out-Null;Stable (@($source,$local)+$extras)|Out-Null
 foreach($extra in $extras){Request @('close-tab',$extra.id)|Out-Null}
 $tree=Await {param($t) $row=@($t.chrome.controls|Where-Object {$_.kind -ceq 'workspace' -and $_.workspace -ceq $sourceIdentity.workspace})[0];$row.workspace_lines.Count -eq 3 -and @($t.surfaces).Count -eq 2};Stable @($source,$local)|Out-Null
 Passed 'five-owned-providers-Linux-urgency-and-pane-MRU-four-agent-cap-overflow-native-logos-and-close-shrink'
 Shortcut $source.id;$tree=Await {param($t) $t.agent_sessions.open -and -not $t.agent_sessions.loading -and @($t.agent_sessions.rows).Count -eq 2};$panel=Panel $tree
 Require ($tree.agent_sessions.agent.name -ceq 'Codex' -and $tree.agent_sessions.agent.pid -eq $source.pid -and (Path-Same $tree.agent_sessions.agent.home $homeA) -and $tree.agent_sessions.source.surface -ceq $source.id -and $tree.agent_sessions.source.session -ceq $source.session) 'Discovery did not resolve actual focused agent Job/home/session'
 $image=[Diagnostics.Process]::GetProcessById([int]$tree.agent_sessions.agent.pid);try{Require (Path-Same $image.MainModule.FileName $agentExe) 'History source image is not actual owned codex.exe'}finally{$image.Dispose()};Stable @($local,$source)|Out-Null;Passed 'actual-owned-codex-process-child-only-CODEX_HOME-and-CtrlAltJ-history-discovery'

 Require (@($panel.rows|Where-Object {$_.title -ceq $titleA -and $_.id -ceq $idA}).Count -eq 1 -and @($panel.rows|Where-Object {$_.title -ceq $titleB -and $_.id -ceq $idB}).Count -eq 1) 'Indexed Unicode/NFD titles or cross-project rows differ';Require (@($panel.rows.color|Select-Object -Unique).Count -eq 2) 'Distinct project row colors missing'
 $native=[OptionsFixture]::Describe([long]$panel.window,$owned.Id);$scale=$native.Dpi/96.0;$area=[OptionsFixture]::RelativeBounds([long]$tree.window_handle,[long]$panel.window,$owned.Id);Require ([Math]::Abs($area.Width-360*$scale) -le 2 -and [Math]::Abs($panel.preview_bounds.height-150*$scale) -le 2) 'Sessions dock/preview geometry differs from360/150DIP';foreach($pane in $tree.layout.panes){Require ($pane[1].x+$pane[1].width -le $area.X) 'Session dock overlaps terminal content'}
 Query $tree $idB;$tree=Await {param($t) @($t.agent_sessions.panel.rows).Count -eq 1 -and $t.agent_sessions.panel.rows[0].id -ceq $idB};Query $tree '한';$tree=Await {param($t) @($t.agent_sessions.panel.rows).Count -eq 1 -and $t.agent_sessions.panel.rows[0].id -ceq $idA};Require ([OptionsFixture]::Text([long]$tree.agent_sessions.panel.query_handle,$owned.Id) -ceq '한') 'Search normalized raw NFD input';Query $tree '';Passed 'native-two-project-rows-index-titles-colors360DIP-dock150DIP-preview-and-ID-NFD-filter'

 $tree=Await {param($t) @($t.agent_sessions.panel.rows).Count -eq 2};$tree=Select-Session $tree $idA;$panel=Panel $tree;$preview=[OptionsFixture]::Text([long]$panel.preview,$owned.Id);Require (([OptionsFixture]::Describe([long]$panel.preview,$owned.Id).Style -band 0x800) -ne 0) 'Conversation preview is not native readonly EDIT';Require ($preview.Contains($titleA) -and $preview.Contains('질문 한 é 😀') -and $preview.Contains('답변 한글') -and $preview.Contains('두 번째 줄') -and $preview -notmatch 'PRIVATE_SYSTEM|PRIVATE_TOOL|PRIVATE_EVENT') 'Native readonly preview omitted conversation or exposed private/tool records';Require (($preview -replace "`r`n","`n") -ceq ($panel.preview_text -replace "`r`n","`n")) 'Native preview differs from controller text'
 Query $tree $idB;$tree=Await {param($t) @($t.agent_sessions.panel.rows).Count -eq 1 -and $t.agent_sessions.panel.rows[0].id -ceq $idB -and $null -eq $t.agent_sessions.selected -and $null -eq $t.agent_sessions.panel.selected -and -not $t.agent_sessions.panel.preview_text -and -not $t.agent_sessions.resume_enabled -and -not $t.agent_sessions.panel.resume_enabled};$panel=Panel $tree;Require ([OptionsFixture]::Text([long]$panel.preview,$owned.Id) -ceq '' -and -not [OptionsFixture]::Describe([long]$panel.resume,$owned.Id).Enabled) 'Filtering out the selected session retained its native preview or Resume action'
 Query $tree '';$tree=Await {param($t) @($t.agent_sessions.panel.rows).Count -eq 2};$tree=Select-Session $tree $idA;$panel=Panel $tree;Require ($panel.selected -ceq $idA -and ([OptionsFixture]::Text([long]$panel.preview,$owned.Id) -replace "`r`n","`n") -ceq ($preview -replace "`r`n","`n") -and [OptionsFixture]::Describe([long]$panel.resume,$owned.Id).Enabled) 'Selecting the same session after clearing its filter did not restore the native preview and Resume action';Passed 'selected-session-native-conversation-preview-excludes-private-records-and-filter-clears-then-restores-selection'

 [OptionsFixture]::CompositionGuard([long]$panel.window,[long]$panel.query_handle,$owned.Id,$true);Query $tree '한';$tree=Await {param($t) $t.agent_sessions.panel.composing -and $t.agent_sessions.panel.query -ceq '한'};Require (@($tree.agent_sessions.panel.rows).Count -eq 2) 'Search filtered during controlled composition';[OptionsFixture]::PostEscape([long]$panel.query_handle,$owned.Id);Request @('settings','set','theme-preset','nord')|Out-Null;$tree=Tree;Require ($tree.agent_sessions.open -and $tree.agent_sessions.panel.composing -and [OptionsFixture]::Text([long]$panel.query_handle,$owned.Id) -ceq '한') 'Theme/Escape erased composing raw search or closed panel'
 [OptionsFixture]::CompositionGuard([long]$panel.window,[long]$panel.query_handle,$owned.Id,$false);[OptionsFixture]::PostKey([long]$panel.query_handle,$owned.Id,65,$true,$false);$tree=Await {param($t) -not $t.agent_sessions.panel.composing -and @($t.agent_sessions.panel.rows).Count -eq 1};Require ($tree.agent_sessions.panel.window -eq $panel.window -and $tree.agent_sessions.panel.list -eq $panel.list -and $tree.agent_sessions.panel.rows[0].id -ceq $idA) 'Composition commit/theme rebuilt controls or selected another row';Query $tree '';Passed 'owned-IME-guard-keeps-NFD-draft-through-theme-and-Escape-until-commit'

 $tree=Await {param($t) @($t.agent_sessions.panel.rows).Count -eq 2};$tree=Select-Session $tree $idB;$before=@($tree.surfaces);Click ([long]$tree.agent_sessions.panel.resume);$tree=Await {param($t) @($t.surfaces).Count -eq $before.Count+1 -and @($t.surfaces|Where-Object {$_.id -cnotin @($before.id) -and $_.ready -and $_.running}).Count -eq 1};$new=@($tree.surfaces|Where-Object {$_.id -cnotin @($before.id)})[0];$identity=Request @('identify');Require ($identity.surface -ceq $new.id -and $identity.pane -ceq $sourceIdentity.pane -and $identity.workspace -ceq $sourceIdentity.workspace -and (Path-Same $identity.cwd $projectB)) 'Resume did not create one same-pane tab in selected project'
 Screen $new.id {param($s) $s.text.Contains('RESUMED_'+$idB)}|Out-Null;$proofPath=Join-Path $homeA ('resume-'+$idB+'.json');Require (Test-Path -LiteralPath $proofPath) 'Actual resumed agent produced no execution proof';$proof=Get-Content -Raw -Encoding UTF8 -LiteralPath $proofPath|ConvertFrom-Json;$diagnostic.resume=$proof;Require ((@($proof.argv)-join '|') -ceq ('resume|'+$idB) -and (Path-Same $proof.cwd $projectB) -and (Path-Same $proof.home $homeA) -and (Path-Same $proof.image $agentExe) -and $proof.pid -ne $source.pid) 'Resume argv/cwd/child-only home or actual executable differs';Stable $before|Out-Null;Passed 'Resume-actual-agent-argv-project-home-new-tab-and-original-PID-view-preservation'

 Screen $new.id {param($s) $s.text.Replace("`r",'').Replace("`n",'').Contains($projectB+'>')}|Out-Null;$marker='SESSION_CMD_'+$new.id;Request @('send-keys',$identity.pane,'echo SESSION_CMD_%FLOWMUX_SURFACE_ID%')|Out-Null;Request @('send-key','Enter','--surface',$new.id)|Out-Null;Screen $new.id {param($s) $s.text.Replace("`r",'').Replace("`n",'').Contains($marker)}|Out-Null;Stable @($new,$source,$local)|Out-Null;$tree=Await {param($t) -not $t.agent_sessions.loading -and -not $t.agent_sessions.agent -and @($t.agent_sessions.rows).Count -eq 0};Require (-not $tree.agent_sessions.panel.resume_enabled -and $tree.agent_sessions.status -match 'Focus a local') 'Plain resumed cmd shell incorrectly fell back to GUI history home';Passed 'agent-exit-returns-to-configured-cmd-with-expanded-environment-proof-and-no-agent-fallback'

 $normal=@($tree.surfaces|Where-Object {$_.id -ceq $new.id})[0];Require ([IO.Path]::GetFileNameWithoutExtension($normal.shell.program) -ieq 'cmd' -and (@($normal.shell.args)-join '|') -ceq '/d') 'Resumed tab retained its one-shot startup wrapper as the normal shell'
 $proofBefore=[IO.File]::ReadAllText($proofPath);$proofCount=@(Get-ChildItem -LiteralPath $homeA,$homeB -Filter 'resume-*.json' -File).Count;$splitBefore=@($tree.surfaces.id);Request @('split','vertical')|Out-Null
 $tree=Await {param($t) @($t.surfaces).Count -eq $splitBefore.Count+1 -and @($t.surfaces|Where-Object {$_.id -cnotin $splitBefore -and $_.ready -and $_.running}).Count -eq 1};$split=@($tree.surfaces|Where-Object {$_.id -cnotin $splitBefore})[0];$splitIdentity=Request @('identify')
 Require ($splitIdentity.surface -ceq $split.id -and $splitIdentity.pane -cne $identity.pane -and (Path-Same $splitIdentity.cwd $projectB) -and [IO.Path]::GetFileNameWithoutExtension($split.shell.program) -ieq 'cmd' -and (@($split.shell.args)-join '|') -ceq '/d') 'Split right cloned the resume wrapper instead of the configured normal CMD shell'
 Screen $split.id {param($s) $s.text.Replace("`r",'').Replace("`n",'').Contains($projectB+'>')}|Out-Null;Require (@(Get-ChildItem -LiteralPath $homeA,$homeB -Filter 'resume-*.json' -File).Count -eq $proofCount -and [IO.File]::ReadAllText($proofPath) -ceq $proofBefore) 'Split right reran the agent resume command or overwrote its execution proof';Stable @($source,$local,$new)|Out-Null
 Request @('close-tab',$split.id)|Out-Null;Request @('focus-tab',$new.id)|Out-Null;$tree=Await {param($t) @($t.surfaces).Count -eq $splitBefore.Count};Passed 'resume-startup-wrapper-is-one-shot-and-split-right-launches-normal-cmd-without-another-agent-resume'

 Request @('focus-tab',$source.id)|Out-Null;$tree=Await {param($t) $t.agent_sessions.agent.pid -eq $source.pid -and @($t.agent_sessions.rows).Count -eq 2 -and -not $t.agent_sessions.loading};$other=Agent-Tab $homeB $projectB;$tree=Await {param($t) $t.agent_sessions.agent.pid -eq $other.pid -and (Path-Same $t.agent_sessions.agent.home $homeB) -and @($t.agent_sessions.rows).Count -eq 1 -and $t.agent_sessions.rows[0].id -ceq $idOther -and -not $t.agent_sessions.loading};Require (-not $tree.agent_sessions.panel.preview_text -and -not $tree.agent_sessions.panel.resume_enabled) 'Switching agent scope retained a prior preview/resume target';Stable @($source,$local,$new)|Out-Null;Passed 'focused-agent-switch-replaces-home-and-rows-without-stale-selection-or-GUI-fallback'

 $tree=Select-Session $tree $idOther;$panel=Panel $tree;[OptionsFixture]::Scroll([long]$panel.preview,$owned.Id,$true);$firstLine=[OptionsFixture]::EditFirstVisibleLine([long]$panel.preview,$owned.Id);Require ($firstLine -gt 0) 'Large native preview did not scroll below its first line';Query $tree ([string]$panel.query);$tree=Tree;$afterLine=[OptionsFixture]::EditFirstVisibleLine([long]$panel.preview,$owned.Id);$diagnostic.largePreviewScroll=@{before=$firstLine;after=$afterLine};Require ($afterLine -eq $firstLine -and $tree.agent_sessions.panel.preview -eq $panel.preview -and $tree.agent_sessions.selected -ceq $idOther -and $tree.agent_sessions.panel.preview_text.Length -gt 262144 -and $tree.agent_sessions.panel.preview_text.Contains('LONG_PREVIEW_END')) 'Unchanged long preview rerender reset its native scroll or truncated its text'

 Click ([long]$tree.agent_sessions.panel.refresh);Request @('focus-tab',$local.id)|Out-Null;$tree=Await {param($t) -not $t.agent_sessions.loading -and -not $t.agent_sessions.agent -and @($t.agent_sessions.rows).Count -eq 0};Require (-not $tree.agent_sessions.panel.preview_text -and -not $tree.agent_sessions.panel.resume_enabled) 'Refresh completion crossed the focused-source identity';$panel=Panel $tree;[OptionsFixture]::PostEscape([long]$panel.query_handle,$owned.Id);$tree=Await {param($t) -not $t.agent_sessions.open};Require ([OptionsFixture]::WindowDestroyed([long]$panel.window)) 'Closing Sessions retained dock HWND';Request @('focus-tab',$source.id)|Out-Null;$tree=Tree;$entry=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'sessions' -and $_.layout_visible});Require ($entry.Count -eq 1) 'Sessions footer entry missing';Click ([long]$entry[0].handle);$tree=Await {param($t) $t.agent_sessions.open -and $t.agent_sessions.agent.pid -eq $source.pid -and @($t.agent_sessions.rows).Count -eq 2 -and -not $t.agent_sessions.loading};Panel $tree|Out-Null;Stable @($source,$local,$new,$other)|Out-Null;Passed 'long-preview-preserves-scroll-and-refresh-source-switch-Escape-footer-reopen-keeps-live-terminals'
 $agents=@(Request @('agents'));Require ($agents.Count -eq 2 -and @($agents|Where-Object {$_.tab -ceq $source.id -and $_.pid -eq $source.pid}).Count -eq 1 -and @($agents|Where-Object {$_.tab -ceq $other.id -and $_.pid -eq $other.pid -and (Path-Same $_.cwd $projectB)}).Count -eq 1) 'Window agent list missed a Job or inferred an agent from the resumed plain shell'
 Request @('new-workspace','--cwd',$projectB,'--shell=cmd')|Out-Null;$destination=Request @('identify')
 $movedReport=@('report-agent','codex','--surface',$other.id,'--pid',[string]$other.pid);Request ($movedReport+@('--seq','1','--status','blocked'))|Out-Null
 Request @('move-tab',$other.id,'--to-pane',$destination.pane)|Out-Null
 $agents=@(Request @('agents'));$moved=@($agents|Where-Object {$_.tab -ceq $other.id})
 Require ($moved.Count -eq 1 -and $moved[0].pid -eq $other.pid -and $moved[0].pane -ceq $destination.pane -and $moved[0].workspace_id -ceq $destination.workspace -and $moved[0].status -ceq 'blocked') 'Agent list retained stale pane/workspace ownership after moving a live process'
 Request @('close-tab',$other.id)|Out-Null;$agents=@(Request @('agents'))
 Require ($agents.Count -eq 1 -and $agents[0].tab -ceq $source.id -and $agents[0].pid -eq $source.pid) 'Agent list retained a closed/exited process or lost an inactive workspace'
 Request @('workspace','close',$destination.workspace)|Out-Null;Stable @($local,$source,$new)|Out-Null
 Passed 'window-agent-list-all-workspaces-move-and-close-retain-live-process-identities'
 # Exit only this fixture's known agent image; process death clears presence, never Done.
 $tree=Tree;$sourceNow=@($tree.surfaces|Where-Object {$_.id -ceq $source.id})[0];Require ($sourceNow.pid -eq $source.pid -and $sourceNow.agent.status -ceq 'blocked') 'Owned source changed before exit check'
 $image=[Diagnostics.Process]::GetProcessById([int]$source.pid);try{Require (Path-Same $image.MainModule.FileName $agentExe) 'Refusing to stop an unrelated process';$image.Kill();Require ($image.WaitForExit(2000)) 'Owned agent exit exceeded2s'}finally{$image.Dispose()}
 $tree=Await {param($t) $s=@($t.surfaces|Where-Object {$_.id -ceq $source.id})[0];$row=@($t.chrome.controls|Where-Object {$_.kind -ceq 'workspace' -and $_.workspace -ceq $sourceIdentity.workspace})[0];$null -ne $s.exit_code -and -not $s.agent -and @($row.workspace_lines|Where-Object {$_.agent}).Count -eq 0 -and $row.workspace_lines.Count -eq 1};Require (@(Request @('agents')).Count -eq 0) 'Exited agent retained a live activity row'
 Request ($report+@('--seq','10','--status','idle')) 3000 $true|Out-Null;Passed 'reported-state-follows-live-tab-moves-and-clears-on-owned-process-exit-without-false-completion'

}
catch{$failure=$_.Exception.Message}
finally{
 $cleaning=$true
 if($owned){try{if(-not $owned.HasExited){Request @('quit','--discard-state')|Out-Null;Require ($owned.WaitForExit(4000)) 'Owned host quit timed out'};Require ($owned.ExitCode -eq 0) 'Owned host failed'}catch{$cleanupErrors+=,$_.Exception.Message}finally{try{if(-not $owned.HasExited){$owned.Kill();[CliProbe]::WaitAfterKill($owned)};$diagnostic.host=@{pid=$owned.Id;exitCode=$owned.ExitCode;stdout=[CliProbe]::Output($hostOut);stderr=[CliProbe]::Output($hostErr)}}catch{$cleanupErrors+=,$_.Exception.Message}finally{$owned.Dispose()}}}
 $env:LOCALAPPDATA=$savedLocal
}
if($failure -or $cleanupErrors.Count){$diagnostic.failure=$failure;$diagnostic.cleanupErrors=$cleanupErrors;$diagnostic.elapsedMs=$clock.ElapsedMilliseconds;$diagnostic|ConvertTo-Json -Depth 60|Set-Content -Encoding UTF8 -LiteralPath (Join-Path $directory 'failure.json');throw ($failure+' '+($cleanupErrors -join '; '))}
Write-Output ('passed: '+$checks.Count+' hidden agent-session groups; elapsed='+$clock.ElapsedMilliseconds+'ms')
