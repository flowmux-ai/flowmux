# SPDX-License-Identifier: GPL-3.0-or-later
# Actual owned agent process, child-only home and native history UI; no account access.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",[switch]$HooksOnly)
if(-not $env:FLOWMUX_TEST_ARTIFACT_ROOT){throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.'}
$ErrorActionPreference='Stop';$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path;$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'OptionsFixture.cs'),(Join-Path $PSScriptRoot 'SearchDialogFixture.cs')
Add-Type -ReferencedAssemblies System.Drawing -Path (Join-Path $PSScriptRoot 'ChromeFixture.cs')
$directory=Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT 'sessions';[IO.Directory]::CreateDirectory($directory)|Out-Null
$clock=[Diagnostics.Stopwatch]::StartNew();$owned=$null;$pipeName=$null;$hostOut=$null;$hostErr=$null;$failure=$null;$cleanupErrors=@();$cleaning=$false;$checks=@();$savedLocal=$env:LOCALAPPDATA;$originalCodexHome=$env:CODEX_HOME
$diagnostic=[ordered]@{physicalInput=$false;clipboardAccess=$false;realAccount=$false;checks=@();lastTree=$null};$utf8=New-Object Text.UTF8Encoding($false)
function Require([bool]$Value,[string]$Message){if(-not $Value){throw $Message}}
function Path-Same([string]$A,[string]$B){return [string]::Equals($A.Replace('/','\').TrimEnd('\'),$B.Replace('/','\').TrimEnd('\'),[StringComparison]::OrdinalIgnoreCase)}
function Budget([int]$Maximum=5000){if($cleaning){return [Math]::Min($Maximum,2000)};$left=50000-$clock.ElapsedMilliseconds;Require ($left -gt 0) 'Agent sessions verification exceeded50s';return [int][Math]::Min($Maximum,$left)}
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
function Agent-Order($Tree){return ($Tree.agent_bar.items.surface -join ',')}
function Agent-Bounds($Tree,[string]$Id,[bool]$Main){$item=@($Tree.agent_bar.items|Where-Object {$_.surface -ceq $Id})[0];$r=[OptionsFixture]::RelativeBounds([long]$Tree.agent_bar.viewport,[long]$item.handle,$owned.Id);$v=[OptionsFixture]::RelativeBounds([long]$Tree.agent_bar.window,[long]$Tree.agent_bar.viewport,$owned.Id);$r.X+=$v.X;$r.Y+=$v.Y;if($Main){$b=[OptionsFixture]::RelativeBounds([long]$Tree.window_handle,[long]$Tree.agent_bar.window,$owned.Id);$r.X+=$b.X;$r.Y+=$b.Y};return $r}
function Agent-Point($Tree,[string]$Id,[bool]$Before){$r=Agent-Bounds $Tree $Id $true;return @{x=[int]($r.X+$r.Width*$(if($Before){0.25}else{0.75}));y=[int]($r.Y+$r.Height/2)}}
function Agent-Begin($Tree,[string]$Id){$item=@($Tree.agent_bar.items|Where-Object {$_.surface -ceq $Id})[0];$r=[OptionsFixture]::RelativeBounds([long]$Tree.agent_bar.viewport,[long]$item.handle,$owned.Id);[OptionsFixture]::TabPointerDown([long]$Tree.agent_bar.viewport,[long]$item.handle,$owned.Id,[int]($r.Width/2),[int]($r.Height/2));return Await {param($t) $t.chrome.agent_dragging}}
function Agent-Release($Tree,$Point){[OptionsFixture]::HostPointer([long]$Tree.window_handle,$owned.Id,0x202,$Point.x,$Point.y);return Await {param($t) -not $t.chrome.agent_dragging}}
function Agent-Drag($Tree,[string]$Source,[string]$Target,[bool]$Before){$p=Agent-Point $Tree $Target $Before;$t=Agent-Begin $Tree $Source;[OptionsFixture]::HostPointer([long]$t.window_handle,$owned.Id,0x200,$p.x,$p.y);return Agent-Release $t $p}
function Attention($Tree,[bool]$Agent,[bool]$Workspace){$item=@($Tree.agent_bar.items|Where-Object {$_.surface -ceq $source.id});$row=@($Tree.chrome.controls|Where-Object {$_.kind -ceq 'workspace' -and $_.workspace -ceq $sourceIdentity.workspace});Require ($item.Count -eq 1 -and $row.Count -eq 1 -and $item[0].attention -eq $Agent -and $row[0].attention -eq $Workspace) 'Notification attention does not match its source and configured target';Require ($item[0].handle -eq $barHandle -and $item[0].message -ceq $statusText -and $item[0].status -ceq 'working') 'Notification attention changed agent status, raw Korean text or native control identity'}
function Stable($Before){$t=Tree;foreach($s in $Before){$n=@($t.surfaces|Where-Object {$_.id -ceq $s.id});Require ($n.Count -eq 1 -and $n[0].pid -eq $s.pid -and $n[0].session -ceq $s.session -and $n[0].view_handle -eq $s.view_handle -and $n[0].holder.window -eq $s.holder.window -and $n[0].running) 'Sessions changed a retained terminal PID/session/view/holder'};return $t}
function Passed([string]$Name){$script:checks+=,$Name;$diagnostic.checks=$checks;Require ($env:CODEX_HOME -ceq $originalCodexHome) 'Fixture modified parent CODEX_HOME'}
function Panel($Tree){$p=$Tree.agent_sessions.panel;Require ($Tree.agent_sessions.open -and $p.open -and -not $p.native_visible -and [OptionsFixture]::Parent([long]$p.window,$owned.Id) -eq $Tree.window_handle) 'Sessions must be an owned hidden child dock';Require ([OptionsFixture]::Describe([long]$Tree.window_handle,$owned.Id).Enabled) 'Sessions dock disabled main';foreach($h in @($p.query_handle,$p.list,$p.preview,$p.refresh,$p.close,$p.resume)){Require ([OptionsFixture]::Parent([long]$h,$owned.Id) -eq $p.window) 'Session control belongs to another owner'};return $p}
function Agent-Sidebar([string]$Name,[string]$Status,[string]$Text,[string]$Agent='codex'){
 $tree=Await {param($t) $row=@($t.chrome.controls|Where-Object {$_.kind -ceq 'workspace' -and $_.workspace -ceq $sourceIdentity.workspace})[0];$row.workspace_lines.Count -eq 3 -and $row.workspace_lines[0].agent.status -ceq $Status -and $row.workspace_lines[1].text -ceq $Text}
 $row=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'workspace' -and $_.workspace -ceq $sourceIdentity.workspace})[0];$scale=[Math]::Max(96,$tree.chrome.dpi)/96.0
 Require ($row.workspace_lines[0].text -ceq $Agent -and $null -eq $row.workspace_lines[0].parent -and -not $row.workspace_lines[0].continues -and $row.workspace_lines[1].parent -ceq $false -and $row.workspace_lines[1].continues -and $row.workspace_lines[2].parent -ceq $false -and -not $row.workspace_lines[2].continues -and (Path-Same $row.workspace_lines[2].text $projectA)) 'Agent metadata is not Linux header/status/path nesting'
 Require ([Math]::Abs($row.rect.height-96*$scale) -le 2) 'Agent sidebar row did not grow to three metadata lines'
 Require ([OptionsFixture]::Text([long]$row.handle,$owned.Id) -ceq ($row.label) -and $row.label.Contains($Text.Replace('&','&&')) -and $row.label.Contains(($Agent+' ('+$Status+')'))) 'Native accessible caption differs from raw report text'
 $bmp=Join-Path $directory ($Name+'.bmp');$png=Join-Path $directory ($Name+'.png');$capture=Request @('chrome-capture',$bmp);Require ($capture.root_handle -eq $tree.window_handle) 'Agent sidebar capture targeted an open popup';[ChromeFixture]::Png($bmp,$png)
 $background=if($Status -eq 'blocked'){'#442b31'}elseif($Status -eq 'done'){'#27334a'}else{'#24272e'}
 $ink=if($Status -eq 'blocked'){'#ef4444'}elseif($Status -eq 'done'){'#3b82f6'}elseif($Status -eq 'working'){'#f59e0b'}else{'#abb1bc'}
 $x=$row.rect.x;$y=$row.rect.y;$gutter=$x+[int][Math]::Round(21*$scale);$nested=$gutter+[int][Math]::Round(14*$scale)
 Require ([ChromeFixture]::Pixel($png,($x+$row.rect.width-4),($y+$row.rect.height-4)) -ceq $background) 'Agent workspace tint differs from Linux rgba16/14 percent'
 Require ([ChromeFixture]::ColorCount($png,($gutter+8),($y+[int](27*$scale)),([int](46*$scale)),([int](20*$scale)),$ink) -gt 2) 'Native agent header status icon/color missing'
 Require ([ChromeFixture]::Pixel($png,$gutter,($y+[int](39*$scale))) -ceq $background -and [ChromeFixture]::Pixel($png,$nested,($y+[int](48*$scale))) -ceq '#abb1bc' -and [ChromeFixture]::Pixel($png,$nested,($y+[int](80*$scale))) -ceq $background) 'Nested native tree connectors continue through the wrong branch'
 $diagnostic.agentCapture=@{path=$png;row=$row};return $row.handle
}
function Native-Notice([string]$Name,[string]$Level,[string]$Title,[string]$Body){
 $notes=Request @('notifications','list');$entry=@($notes.entries|Where-Object {$_.surface -ceq $source.id})
 Require ($entry.Count -eq 1 -and $entry[0].level -ceq $Level -and $entry[0].title -ceq $Title -and $entry[0].body -ceq $Body -and $entry[0].pane -ceq $sourceIdentity.pane -and $entry[0].workspace -ceq $sourceIdentity.workspace) 'Native hook notification lost its level, raw Korean text, identity, or deduplication'
 Require ($notes.desktop.background_blocked -and $notes.desktop.native_calls -eq 0 -and -not $notes.desktop.error) 'Hook verification attempted desktop notification delivery'
 $shown=Request @('notifications','show');$popup=$shown.panel_snapshot;$row=@($popup.rows|Where-Object {$_.id -ceq $entry[0].id})[0]
 Require ($popup.open -and $popup.kind -ceq 'bell-popover' -and -not $popup.visible -and [OptionsFixture]::Text([long]$row.open_handle,$owned.Id) -ceq ($Title+"`n"+$Body+"`n"+$row.time)) 'Native hook popup lost its captionless layout or original Unicode HWND text'
 $bmp=Join-Path $directory ($Name+'.bmp');$paint=Request @('chrome-capture',$bmp);Require ($paint.root_handle -eq $shown.panel_handle -and $paint.subtree -and @($paint.controls|Where-Object {$_.handle -eq $row.open_handle}).Count -eq 1) 'Notification capture did not target its exact owned row';[ChromeFixture]::Png($bmp,(Join-Path $directory ($Name+'.png')))
 [SearchDialogFixture]::Close([IntPtr][long]$shown.panel_handle,$owned.Id);Require (-not (Request @('notifications','list')).panel_snapshot.open) 'Hook notification popup did not close'
}
function Query($Tree,[string]$Text){$p=Panel $Tree;[OptionsFixture]::SetTextAndNotify([long]$p.window,[long]$p.query_handle,$owned.Id,$Text)}
function Select-Session($Tree,[string]$Id){$p=Panel $Tree;$row=@($p.rows|Where-Object {$_.id -ceq $Id});Require ($row.Count -eq 1) 'Expected exact visible session row';[OptionsFixture]::ListSelect([long]$p.window,[long]$p.list,$owned.Id,[int]$row[0].index);return Await {param($t) $t.agent_sessions.selected -ceq $Id -and $t.agent_sessions.panel.resume_enabled -and -not $t.agent_sessions.loading}}
function Shortcut([string]$Surface){Request @('test-shortcut',$Surface,'{"code":"KeyJ","key":"j","ctrlKey":true,"altKey":true}')|Out-Null}
function Agent-Tab([string]$HistoryHome,[string]$Cwd,[string]$Current=''){$launch=@('new-tab','--cwd',$Cwd,'--shell',$agentExe,'--shell-arg=--hold','--shell-arg',$HistoryHome);if($Current){$launch+=@('--shell-arg=resume','--shell-arg',$Current)};Request $launch|Out-Null;$id=Request @('identify');$tree=Await {param($t) @($t.surfaces|Where-Object {$_.id -ceq $id.surface -and $_.ready -and $_.running}).Count -eq 1};Screen $id.surface {param($s) $s.text.Contains('SESSION_SOURCE_READY')}|Out-Null;return @($tree.surfaces|Where-Object {$_.id -ceq $id.surface})[0]}
function Hook([string]$Event,[string]$Payload,[bool]$Expected=$true,[bool]$CloseInput=$true,[string]$Provider='codex',[string]$Surface='', [bool]$Shell=$false,[bool]$Nested=$false,[string]$Config='',[int]$Padding=0){
 $path=Join-Path $homeA ('hook-'+$source.pid+'.json');$result=$path+'.result';if(Test-Path -LiteralPath $result){Remove-Item -LiteralPath $result -Force}
 $payloadJson=@{cli=$cli;provider=$Provider;event=$Event;payload=$Payload;close=$CloseInput;surface=$Surface;shell=$Shell;nested=$Nested;config=$Config;padding=$Padding}|ConvertTo-Json -Compress
 [IO.File]::WriteAllText(($path+'.tmp'),$payloadJson,$utf8);[IO.File]::Move(($path+'.tmp'),$path);$timer=[Diagnostics.Stopwatch]::StartNew()
 while(-not (Test-Path -LiteralPath $result)){Budget|Out-Null;Require ($timer.ElapsedMilliseconds -lt 4500) 'Owned hook process exceeded deadline';Start-Sleep -Milliseconds 10}
 $r=Get-Content -Raw -Encoding UTF8 -LiteralPath $result|ConvertFrom-Json;$diagnostic.lastHook=$r;Require (($r.exit -eq 0) -eq $Expected) ('Hook exit mismatch: '+$r.stderr);if($Expected -and -not $Config){return ($r.stdout|ConvertFrom-Json)};return $r
}
function Outside-Hook($Id){
 $pipe=New-Object IO.Pipes.NamedPipeClientStream('.',($pipeName -replace '^\\\\\.\\pipe\\',''),[IO.Pipes.PipeDirection]::InOut,[IO.Pipes.PipeOptions]::Asynchronous)
 try{$pipe.Connect((Budget 1000));$bytes=$utf8.GetBytes(((@{method='hooks';agent='codex';event='session-start';surface=$source.id;session_id=$Id}|ConvertTo-Json -Compress)+"`n"));$pipe.Write($bytes,0,$bytes.Length);$pipe.Flush();$reader=New-Object IO.StreamReader($pipe,$utf8);$read=$reader.ReadLineAsync();Require ($read.Wait((Budget 3000))) 'Outside hook response timed out';$r=$read.Result|ConvertFrom-Json;Require ($r.error -match 'Owned process is no longer live') ('Unrelated pipe caller was accepted: '+$read.Result)}finally{$pipe.Dispose()}
}
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
 static void Hook(string path, bool forwarding=false) {
  var json=new JavaScriptSerializer();var request=json.Deserialize<System.Collections.Generic.Dictionary<string,object>>(File.ReadAllText(path));
  if((bool)request["nested"] && !forwarding) {
   var nested=new ProcessStartInfo(Process.GetCurrentProcess().MainModule.FileName,"--forward \""+path+"\"");nested.UseShellExecute=false;nested.CreateNoWindow=true;nested.WindowStyle=ProcessWindowStyle.Hidden;
   using(var child=Process.Start(nested)){if(!child.WaitForExit(4000)){child.Kill();child.WaitForExit(1000);throw new TimeoutException("Nested fixture exceeded4s");}if(child.ExitCode!=0)throw new Exception("Nested fixture failed");}return;
  }
  string cli=(string)request["cli"];string arguments="--json hooks "+request["provider"]+" "+request["event"];
  if(!String.IsNullOrEmpty((string)request["surface"]))arguments+=" --surface "+request["surface"];
  if(!String.IsNullOrEmpty((string)request["config"])) {
   var config=json.Deserialize<System.Collections.Generic.Dictionary<string,object>>(File.ReadAllText((string)request["config"]));var events=(System.Collections.Generic.Dictionary<string,object>)config["hooks"];
   var payload=json.Deserialize<System.Collections.Generic.Dictionary<string,object>>((string)request["payload"]);string eventName=payload.ContainsKey("hook_event_name")?(string)payload["hook_event_name"]:((string)request["event"]=="session-start"?"SessionStart":"SessionEnd");
   var groups=(System.Collections.IList)events[eventName];var group=(System.Collections.Generic.Dictionary<string,object>)groups[groups.Count-1];var handlers=(System.Collections.IList)group["hooks"];var handler=(System.Collections.Generic.Dictionary<string,object>)handlers[0];
   if(handler.ContainsKey("commandWindows")){string command=(string)handler["commandWindows"];if(!command.StartsWith("powershell.exe -NoProfile -NonInteractive -EncodedCommand "))throw new Exception("Unexpected installed command");cli="powershell.exe";arguments=command.Substring("powershell.exe ".Length);}
   else{cli=(string)handler["command"];arguments="";foreach(string arg in (System.Collections.IList)handler["args"]){if(arg.IndexOfAny(new char[]{' ','\"','\r','\n'})>=0)throw new Exception("Unexpected hook argument");arguments+=(arguments.Length==0?"":" ")+arg;}}
  }
  var info=(bool)request["shell"] ? new ProcessStartInfo(Environment.GetEnvironmentVariable("COMSPEC"),"/D /S /C \"\""+cli+"\" "+arguments+"\"") : new ProcessStartInfo(cli,arguments);
  info.UseShellExecute=false;info.CreateNoWindow=true;info.WindowStyle=ProcessWindowStyle.Hidden;info.RedirectStandardInput=true;info.RedirectStandardOutput=true;info.RedirectStandardError=true;info.StandardOutputEncoding=new UTF8Encoding(false);info.StandardErrorEncoding=new UTF8Encoding(false);
  using(var child=Process.Start(info)) {
   var stdout=child.StandardOutput.ReadToEndAsync();var stderr=child.StandardError.ReadToEndAsync();byte[] payload=Encoding.UTF8.GetBytes((string)request["payload"]);int padding=Convert.ToInt32(request["padding"]);
   if(padding>0){byte[] prefix=Encoding.UTF8.GetBytes("{\"tool_calls\":[{\"tool_response\":\"");child.StandardInput.BaseStream.Write(prefix,0,prefix.Length);byte[] block=Encoding.UTF8.GetBytes(new String('x',4096)+"한 한 é 😀 PRIVATE_LARGE_RESULT");for(int n=0;n<padding;n+=block.Length)child.StandardInput.BaseStream.Write(block,0,block.Length);byte[] tail=Encoding.UTF8.GetBytes("\"}],");child.StandardInput.BaseStream.Write(tail,0,tail.Length);child.StandardInput.BaseStream.Write(payload,1,payload.Length-1);}
   else child.StandardInput.BaseStream.Write(payload,0,payload.Length);
   child.StandardInput.BaseStream.Flush();if((bool)request["close"])child.StandardInput.Close();
   if(!child.WaitForExit(4000)){child.Kill();child.WaitForExit(1000);throw new TimeoutException("Hook CLI exceeded4s");}
   if(!stdout.Wait(500)||!stderr.Wait(500))throw new TimeoutException("Hook output did not close");
   File.WriteAllText(path+".result.tmp",json.Serialize(new {exit=child.ExitCode,stdout=stdout.Result,stderr=stderr.Result}),new UTF8Encoding(false));File.Delete(path);File.Move(path+".result.tmp",path+".result");
  }
 }
 public static int Main(string[] args) {
  Console.OutputEncoding=new UTF8Encoding(false);
  if(args.Length==2 && args[0]=="--forward"){Hook(args[1],true);return 0;}
  if((args.Length==2 || (args.Length==4 && args[2]=="resume")) && args[0]=="--hold") {
   Environment.SetEnvironmentVariable("CODEX_HOME",Path.GetFullPath(args[1]),EnvironmentVariableTarget.Process);
   Environment.SetEnvironmentVariable("CLAUDE_CONFIG_DIR",Path.GetFullPath(args[1]),EnvironmentVariableTarget.Process);
   Console.WriteLine("SESSION_SOURCE_READY");Console.Out.Flush();var watch=Stopwatch.StartNew();string hook=Path.Combine(args[1],"hook-"+Process.GetCurrentProcess().Id+".json");
   while(watch.ElapsedMilliseconds<60000){if(File.Exists(hook))Hook(hook);Thread.Sleep(10);}return 0;
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
 Request @('settings','shell','cmd','--arg','/d')|Out-Null;$source=Agent-Tab $homeA $projectA $idA;$sourceIdentity=Request @('identify')
 $r=Hook 'session-end' (@{session_id=$idA}|ConvertTo-Json -Compress);Require ($r.accepted -and -not $r.session_id) 'Native end could not clear a startup resume ID before a start hook'
 $r=Hook 'session-start' (@{session_id=$idA.ToUpperInvariant();cwd='한글 한 é 😀'}|ConvertTo-Json -Compress);Require ($r.accepted -and $r.surface -ceq $source.id -and $r.session_id -ceq $idA) 'Owned child native hook did not establish its canonical conversation'
 $r=Hook 'session-end' (@{session_id=$idA}|ConvertTo-Json -Compress);Require ($r.accepted -and -not $r.session_id) 'Owned child native end did not clear its conversation'
 $cap=Request @('capabilities');Require ($cap.commands -contains 'hooks' -and ($cap.agent_activity.session_hooks -join ',') -ceq 'session-start,session-end' -and -not $cap.agent_activity.native_hooks) 'Session hook capability incorrectly advertised full native activity hooks'
 Passed 'native-session-hooks-inherited-context-owned-process-discovery-canonical-ID-and-no-invented-activity'
 if($HooksOnly){
 $hookBin=Join-Path $directory ('hook 한 한 é & % '+[char]36+[char]96+[char]39);[IO.Directory]::CreateDirectory($hookBin)|Out-Null;$hookCli=Join-Path $hookBin 'flowmuxctl.exe';[IO.File]::Copy($cli,$hookCli)
 $info=New-Object Diagnostics.ProcessStartInfo($hookCli,'hooks codex session-start --flowmux-hook');$info.UseShellExecute=$false;$info.CreateNoWindow=$true;$info.RedirectStandardOutput=$true;$info.RedirectStandardError=$true;$info.EnvironmentVariables.Remove('FLOWMUX_PIPE_NAME');$probe=[Diagnostics.Process]::Start($info);$probeOut=$probe.StandardOutput.ReadToEndAsync();$probeErr=$probe.StandardError.ReadToEndAsync()
 try{Require ($probe.WaitForExit((Budget 1000)) -and $probeOut.Wait(500) -and $probeErr.Wait(500) -and $probe.ExitCode -eq 0 -and -not $probeOut.Result -and -not $probeErr.Result) 'Native hook outside flowmux blocked or emitted agent context'}finally{if(-not $probe.HasExited){$probe.Kill();[CliProbe]::WaitAfterKill($probe)};$probe.Dispose()}
 Passed 'native-integration-is-silent-and-does-not-wait-for-stdin-or-discover-windows-outside-flowmux'
 $setupSource=$source;$setupAgentExe=$agentExe;$setupSourceIdentity=$sourceIdentity
 try{
  foreach($provider in @('codex','claude')){
   if($provider -ceq 'claude'){
    $agentExe=Join-Path $hookBin 'claude.exe';[IO.File]::Copy($setupAgentExe,$agentExe)
    Request @('new-workspace','--cwd',$projectA,'--shell=cmd')|Out-Null;$identity=Request @('identify');$tree=Await {param($t) @($t.surfaces|Where-Object {$_.id -ceq $identity.surface -and $_.ready -and $_.running}).Count -eq 1};$claudeLocal=@($tree.surfaces|Where-Object {$_.id -ceq $identity.surface})[0]
    $source=Agent-Tab $homeA $projectA;$sourceIdentity=Request @('identify')
   }
   $config=Join-Path $directory ($provider+'-hooks.json');$original=@{env=@{KEEP='한 한 é 😀 &'};hooks=@{SessionStart=@(@{matcher='startup';hooks=@(@{type='command';command='echo KEEP'})});Stop=@(@{hooks=@(@{type='command';command='echo STOP_KEEP'})})}}
   [IO.File]::WriteAllText($config,($original|ConvertTo-Json -Depth 12),$utf8);$acl=Get-Acl -LiteralPath $config;$acl.SetAccessRuleProtection($true,$true);Set-Acl -LiteralPath $config -AclObject $acl;$beforeAcl=(Get-Acl -LiteralPath $config).GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]::Access)
   $setup=@('hooks','setup','--agent',$provider,'--config',$config,'--flowmux-bin',$hookCli);$remove=@('hooks','uninstall','--agent',$provider,'--config',$config)
   $r=Request $setup;Require ($r.changed -and $r.operation -ceq 'setup' -and $r.agent -ceq $provider) 'Hook setup did not report the changed provider'
   $written=[IO.File]::ReadAllText($config);$time=(Get-Item -LiteralPath $config).LastWriteTimeUtc;$configured=$written|ConvertFrom-Json
   Require ($configured.env.KEEP -ceq $original.env.KEEP -and $configured.hooks.SessionStart[0].hooks[0].command -ceq 'echo KEEP' -and $configured.hooks.Stop[0].hooks[0].command -ceq 'echo STOP_KEEP') 'Hook setup replaced unrelated native settings/hooks'
   Require ((Get-Acl -LiteralPath $config).GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]::Access) -ceq $beforeAcl) 'Atomic hook update discarded protected ACL'
   $r=Request $setup;Require (-not $r.changed -and [IO.File]::ReadAllText($config) -ceq $written -and (Get-Item -LiteralPath $config).LastWriteTimeUtc -eq $time) 'Idempotent setup rewrote configuration'
   $r=Hook 'session-start' (@{session_id=$idA;cwd='한글 한 é'}|ConvertTo-Json -Compress) -Config $config;Require (-not $r.stdout -and -not $r.stderr) 'Installed hook leaked output into agent context'
   $tree=Await {param($t) $s=@($t.surfaces|Where-Object {$_.id -ceq $source.id})[0];$s.agent.pid -eq $source.pid -and $s.agent.session_id -ceq $idA}
   if($provider -ceq 'codex'){
    function Activity-Hook([string]$Name,[string]$Turn,[hashtable]$Extra=@{},[int]$Padding=0){
     $payload=$Extra.Clone();$payload.session_id=$idA;$payload.hook_event_name=$Name;if($provider -ceq 'claude'){$payload.prompt_id=$Turn}else{$payload.turn_id=$Turn}
     $r=Hook 'running' ($payload|ConvertTo-Json -Depth 8 -Compress) -Provider $provider -Config $config -Padding $Padding
     Require (-not $r.stderr -and $(if($provider -ceq 'codex' -and $Name -ceq 'Stop'){$r.stdout.Trim() -ceq '{}'}else{-not $r.stdout})) 'Installed activity hook emitted provider decisions or non-neutral context'
    }
    Require (($cap.agent_activity.native_activity_hooks.codex -join ',') -ceq 'turn-start,running,notification,subagent-start,subagent-stop,stop,interrupt') 'Codex native activity capability is incomplete'
    Request @('focus-tab',$local.id)|Out-Null
    Activity-Hook 'UserPromptSubmit' 'turn-a' @{prompt='PRIVATE_PROMPT_NEVER_RETAIN'}
    $sidebarHandle=Agent-Sidebar 'native-working' 'working' 'Working'
    $waitText='한글 한 é 😀 & 입력 대기';$doneText='한글 한 é 😀 & 작업 완료'
    Activity-Hook 'PermissionRequest' 'turn-a' @{message=$waitText;tool_input=@{private='PRIVATE_TOOL_NEVER_RETAIN'}} -Padding (4*1024*1024)
    Activity-Hook 'PostToolUse' 'turn-a' @{tool_response='PRIVATE_RESULT_NEVER_RETAIN'}
    Require ((Agent-Sidebar 'native-blocked' 'blocked' $waitText) -eq $sidebarHandle) 'Native permission status recreated the workspace control'
    Activity-Hook 'PermissionRequest' 'turn-a' @{message=$waitText}
    Native-Notice 'codex-input-popup' 'needs_input' 'Codex needs your input' $waitText
    Request @('notifications','clear')|Out-Null
    Activity-Hook 'SubagentStart' 'child-turn' @{agent_id='child-a'}
    Activity-Hook 'Stop' 'turn-a' @{last_assistant_message=$doneText}
    $tree=Await {param($t) $s=@($t.surfaces|Where-Object {$_.id -ceq $source.id})[0];$s.agent.status -ceq 'working' -and $s.agent.message -ceq '1 active Codex subagent(s)'}
    Require (@((Request @('notifications','list')).entries).Count -eq 0) 'Parent Stop notified completion while a child was active'
    Activity-Hook 'PermissionRequest' 'child-turn' @{agent_id='child-a';message=$waitText}
    Activity-Hook 'PreToolUse' 'child-turn' @{agent_id='child-a'}
    Require ((Agent-Sidebar 'native-child-blocked' 'blocked' $waitText) -eq $sidebarHandle) 'Child progress cleared an unresolved permission or recreated its workspace'
    Request @('notifications','clear')|Out-Null
    Activity-Hook 'SubagentStop' 'child-turn' @{agent_id='child-a'}
    Require ((Agent-Sidebar 'native-done' 'done' $doneText) -eq $sidebarHandle) 'Native completion lost raw Korean text or its retained workspace'
    Native-Notice 'codex-completed-popup' 'turn_completed' 'Codex ready' $doneText
    Request @('notifications','clear')|Out-Null
    Activity-Hook 'Stop' 'turn-a' @{stop_hook_active=$true;last_assistant_message='CONTINUED_STOP'}
    $tree=Await {param($t) $a=@($t.surfaces|Where-Object {$_.id -ceq $source.id})[0].agent;$a.status -ceq 'done' -and $a.message -ceq 'CONTINUED_STOP'}
    Require (@((Request @('notifications','list')).entries).Count -eq 0) 'Continuing the same Stop hook generated a second completion notification'
    Activity-Hook 'SubagentStart' 'child-turn' @{agent_id='child-a'}
    Activity-Hook 'UserPromptSubmit' 'turn-b'
    Activity-Hook 'Stop' 'turn-a' @{last_assistant_message='STALE_COMPLETION'}
    $tree=Tree;$presence=@($tree.surfaces|Where-Object {$_.id -ceq $source.id})[0].agent
    Require ($presence.status -ceq 'working' -and $presence.message -ceq 'Working' -and $presence.source -ceq 'flowmux:hook') 'Late root/child events changed the active turn'
    Activity-Hook 'Interrupt' 'turn-b'
    $tree=Await {param($t) $s=@($t.surfaces|Where-Object {$_.id -ceq $source.id})[0];$s.agent.status -ceq 'idle' -and $s.agent.seen -and $s.agent.message -ceq 'Turn interrupted'}
    Require (@((Request @('notifications','list')).entries).Count -eq 0) 'Interrupted or stale Codex events notified completion'
    Require (($tree|ConvertTo-Json -Depth 30 -Compress) -notmatch 'PRIVATE_PROMPT|PRIVATE_TOOL|PRIVATE_RESULT|PRIVATE_LARGE_RESULT') 'Native activity retained private provider payloads'
    Stable @($source,$local)|Out-Null
    Passed 'installed-Codex-activity-Korean-native-sidebar-permission-waits-child-aggregation-stale-events-and-interrupt-without-focus-or-PTY-changes'
   }else{
    Require ($cap.agent_activity.native_activity_hooks.claude -contains 'tool-batch' -and $cap.agent_activity.native_activity_hooks.claude -contains 'stop-failure') 'Claude activity capabilities missing'
    Request @('focus-tab',$claudeLocal.id)|Out-Null
    Activity-Hook 'UserPromptSubmit' 'prompt-a' @{prompt='PRIVATE_PROMPT_NEVER_RETAIN'}
    $sidebarHandle=Agent-Sidebar 'claude-working' 'working' 'Working' 'claude'
    Activity-Hook 'PermissionRequest' 'prompt-a' @{message=$waitText}
    Activity-Hook 'PreToolUse' 'prompt-a' @{tool_name='AskUserQuestion';tool_use_id='question-a';message=$waitText}
    Activity-Hook 'PostToolUse' 'prompt-a' @{tool_name='Bash';tool_use_id='other-tool'}
    Activity-Hook 'PostToolBatch' 'prompt-a' @{tool_calls=@(@{tool_use_id='other-tool';tool_response='PRIVATE_RESPONSE'})}
    Require ((Agent-Sidebar 'claude-question' 'blocked' $waitText 'claude') -eq $sidebarHandle) 'Parallel/batch completion cleared an unresolved question or recreated its workspace'
    Native-Notice 'claude-input-popup' 'needs_input' 'Claude needs your input' $waitText
    Activity-Hook 'PostToolUseFailure' 'prompt-a' @{tool_name='AskUserQuestion';tool_use_id='question-a'}
    $tree=Await {param($t) @($t.surfaces|Where-Object {$_.id -ceq $source.id})[0].agent.status -ceq 'working'}
    Activity-Hook 'PermissionRequest' 'prompt-a' @{message=$waitText}
    Activity-Hook 'PermissionDenied' 'prompt-a' @{tool_name='Bash';tool_use_id='denied-tool'}
    Require ((Agent-Sidebar 'claude-permission' 'blocked' $waitText 'claude') -eq $sidebarHandle) 'One denial cleared another parallel permission request'
    Activity-Hook 'PostToolBatch' 'prompt-a' -Padding (4*1024*1024)
    $tree=Await {param($t) @($t.surfaces|Where-Object {$_.id -ceq $source.id})[0].agent.status -ceq 'working'}
    Activity-Hook 'SubagentStart' 'prompt-a' @{agent_id='child-a'}
    Activity-Hook 'PermissionRequest' 'prompt-a' @{agent_id='child-a';message=$waitText}
    Activity-Hook 'Stop' 'prompt-a' @{last_assistant_message=$doneText}
    Require ((Agent-Sidebar 'claude-child-wait' 'blocked' $waitText 'claude') -eq $sidebarHandle) 'Parent Stop discarded a child permission wait'
    Request @('notifications','clear')|Out-Null
    Activity-Hook 'SubagentStop' 'prompt-a' @{agent_id='child-a';last_assistant_message='CHILD_NOT_PARENT_COMPLETION'}
    Require ((Agent-Sidebar 'claude-done' 'done' $doneText 'claude') -eq $sidebarHandle) 'Child completion replaced parent completion or its native control'
    Native-Notice 'claude-completed-popup' 'turn_completed' 'Claude ready' $doneText
    Request @('notifications','clear')|Out-Null
    Activity-Hook 'SubagentStop' 'prompt-a' @{agent_id='unknown-internal-agent'}
    Activity-Hook 'Notification' 'prompt-a' @{notification_type='idle_prompt';message='DO_NOT_BLOCK_COMPLETED_ROOT'}
    $tree=Tree;Require (@($tree.surfaces|Where-Object {$_.id -ceq $source.id})[0].agent.status -ceq 'done') 'Unattributed child or informational notification changed completion'
    Activity-Hook 'Notification' 'prompt-a' @{notification_type='agent_completed';message=$doneText}
    Native-Notice 'claude-background-popup' 'info' 'Claude background agent finished' $doneText
    $tree=Tree;Require (@($tree.surfaces|Where-Object {$_.id -ceq $source.id})[0].agent.status -ceq 'done') 'Background notification changed the root turn'
    Request @('notifications','clear')|Out-Null
    Activity-Hook 'UserPromptSubmit' 'prompt-b'
    Activity-Hook 'Stop' 'prompt-a' @{last_assistant_message='STALE_COMPLETION'}
    $tree=Tree;Require (@($tree.surfaces|Where-Object {$_.id -ceq $source.id})[0].agent.status -ceq 'working') 'Old prompt Stop completed the current prompt'
    Activity-Hook 'Stop' 'prompt-b' @{background_tasks=@(@{command='PRIVATE_COMMAND'});session_crons=@(@{prompt='PRIVATE_SCHEDULED_PROMPT'});last_assistant_message=$doneText}
    Require ((Agent-Sidebar 'claude-background' 'working' 'Background work pending' 'claude') -eq $sidebarHandle) 'Background work was marked complete'
    Require (@((Request @('notifications','list')).entries).Count -eq 0) 'Background or stale Claude Stop notified completion'
    Activity-Hook 'StopFailure' 'prompt-b' @{error='rate_limit'}
    Activity-Hook 'PostToolBatch' 'prompt-b'
    Require ((Agent-Sidebar 'claude-quota' 'blocked' 'API error: rate_limit' 'claude') -eq $sidebarHandle) 'Batch completion cleared a quota wait'
    Native-Notice 'claude-error-popup' 'error' 'Claude stopped' 'API error: rate_limit'
    Request @('notifications','clear')|Out-Null
    Activity-Hook 'Notification' 'prompt-b' @{notification_type='quota_auto_resume_fired'}
    $tree=Await {param($t) @($t.surfaces|Where-Object {$_.id -ceq $source.id})[0].agent.status -ceq 'working'}
    Native-Notice 'claude-resumed-popup' 'info' 'Claude resumed' 'status changed'
    Request @('notifications','clear')|Out-Null
    Activity-Hook 'StopFailure' 'prompt-b' @{error='rate_limit'}
    Activity-Hook 'Notification' 'prompt-b' @{notification_type='quota_auto_resume_disabled'}
    $tree=Await {param($t) $a=@($t.surfaces|Where-Object {$_.id -ceq $source.id})[0].agent;$a.status -ceq 'idle' -and $a.seen -and $a.message -ceq 'Auto-resume disabled'}
    $hookClock=[Diagnostics.Stopwatch]::StartNew();$r=Hook 'permission-request' (@{session_id=$idA;prompt_id='prompt-b';message=$waitText}|ConvertTo-Json -Compress) -Expected $false -CloseInput $false -Provider 'claude'
    Require ($r.stderr -match 'stdin exceeded one second' -and $hookClock.ElapsedMilliseconds -lt 3000) 'Streaming native hook waited indefinitely for EOF'
    $tree=Tree;Require (@($tree.surfaces|Where-Object {$_.id -ceq $source.id})[0].agent.status -ceq 'idle') 'Unfinished JSON stream changed live agent activity'
    Require (($tree|ConvertTo-Json -Depth 30 -Compress) -notmatch 'PRIVATE_PROMPT|PRIVATE_RESPONSE|PRIVATE_COMMAND|PRIVATE_SCHEDULED_PROMPT|PRIVATE_LARGE_RESULT') 'Claude activity retained private prompt/tool/background details'
    Stable @($source,$claudeLocal,$setupSource,$local)|Out-Null
    Passed 'installed-Claude-scoped-questions-permissions-batch-child-background-Korean-sidebar-stale-prompt-and-quota-states'
    Passed 'native-streaming-4MiB-tool-results-Korean-metadata-after-payload-and-bounded-EOF-without-partial-state'
    Passed 'native-hooks-Linux-notification-titles-levels-Unicode-popovers-ownership-dedup-and-no-false-completion-or-desktop-calls'
   }
   $r=Hook 'session-end' (@{session_id=$idA}|ConvertTo-Json -Compress) -Config $config;Require (-not $r.stdout -and -not $r.stderr) 'Installed end hook leaked output into agent context'
   $tree=Await {param($t) $s=@($t.surfaces|Where-Object {$_.id -ceq $source.id})[0];$s.agent.pid -eq $source.pid -and -not $s.agent.session_id}
   if($provider -in @('codex','claude')){
    Activity-Hook 'UserPromptSubmit' 'turn-after-end'
    $tree=Tree;$presence=@($tree.surfaces|Where-Object {$_.id -ceq $source.id})[0].agent
    Require (-not $presence.session_id -and $presence.status -ceq 'unknown' -and $presence.seen -and $presence.message -ceq 'Session ended') 'Activity resurrected an ended session or retained a false completion'
   }
   $r=Request $remove;$remaining=Get-Content -Raw -Encoding UTF8 -LiteralPath $config|ConvertFrom-Json;Require ($r.changed -and @($remaining.hooks.SessionStart).Count -eq 1 -and -not $remaining.hooks.SessionEnd -and $remaining.env.KEEP -ceq $original.env.KEEP -and $remaining.hooks.Stop[0].hooks[0].command -ceq 'echo STOP_KEEP') 'Uninstall removed unrelated settings or retained owned hooks'
   $written=[IO.File]::ReadAllText($config);$r=Request $remove;Require (-not $r.changed -and [IO.File]::ReadAllText($config) -ceq $written) 'Repeated uninstall rewrote configuration'
   foreach($bad in @('{bad','{"hooks":{"SessionEnd":{}}}','{"disableAllHooks":true}',('x'*1048577))){[IO.File]::WriteAllText($config,$bad,$utf8);Request $setup 3000 $true|Out-Null;Require ([IO.File]::ReadAllText($config) -ceq $bad) 'Invalid/disabled/oversized configuration was rewritten'}
   [IO.File]::WriteAllText($config,'{}',$utf8);$locked=[IO.File]::Open($config,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
   try{Request $setup 3000 $true|Out-Null;Require ([IO.File]::ReadAllText($config) -ceq '{}') 'Locked destination lost its original data'}finally{$locked.Dispose()}
   $lockPath=[IO.Path]::ChangeExtension($config,'flowmux-lock');$locked=[IO.File]::Open($lockPath,[IO.FileMode]::OpenOrCreate,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None)
   try{$bytes=$utf8.GetBytes('LOCK_OWNER_KEEP');$locked.Write($bytes,0,$bytes.Length);$locked.Flush();Request $setup 3000 $true|Out-Null;Require ([IO.File]::ReadAllText($config) -ceq '{}') 'Concurrent hook writer was not rejected'}finally{$locked.Dispose()}
   Request $setup 3000 $true|Out-Null;Require ([IO.File]::ReadAllText($lockPath) -ceq 'LOCK_OWNER_KEEP' -and [IO.File]::ReadAllText($config) -ceq '{}') 'Installer replaced/deleted an existing unowned lock file';[IO.File]::Delete($lockPath)
   Require (@(Get-ChildItem -LiteralPath $directory -Filter ($provider+'-hooks.flowmux*')).Count -eq 0) 'Hook setup retained temporary/lock files after success or ordinary failure'
   [IO.File]::Delete($config);$r=Request $remove;Require (-not $r.changed -and -not (Test-Path -LiteralPath $config)) 'Missing configuration uninstall created a file'
   $r=Request $setup;Require ($r.changed -and (Test-Path -LiteralPath $config)) 'Setup could not create a missing provider JSON file';Request $remove|Out-Null;Require ((Get-Content -Raw -Encoding UTF8 -LiteralPath $config).Trim() -ceq '{}') 'Removing hooks from a newly created configuration retained owned entries'
   Passed ('installed-'+$provider+'-native-hooks-Unicode-special-path-quiet-execution-preserved-settings-ACL-idempotence-invalid-and-locked-file-rejection')
   if($provider -ceq 'claude'){Request @('close-tab',$source.id)|Out-Null}
  }
 }finally{$source=$setupSource;$agentExe=$setupAgentExe;$sourceIdentity=$setupSourceIdentity;Request @('focus-tab',$source.id)|Out-Null}
 }else{
 Request @('focus-tab',$local.id)|Out-Null
 $tree=Await {param($t) $s=@($t.surfaces|Where-Object {$_.id -ceq $source.id})[0];$s.agent.pid -eq $source.pid -and $s.agent.source -ceq 'flowmux:proc'}
 $presence=@($tree.surfaces|Where-Object {$_.id -ceq $source.id})[0].agent
 Require ($presence.status -ceq 'unknown' -and $presence.seen -and -not $presence.seq -and -not $presence.message -and (Request @('identify')).surface -ceq $local.id -and -not $tree.agent_sessions.open) 'Automatic process detection guessed activity, changed focus or opened a panel'
 Agent-Sidebar 'agent-process' 'unknown' 'unknown'|Out-Null;Stable @($source,$local)|Out-Null
 Request @('focus-tab',$source.id)|Out-Null;Passed 'automatic-inactive-agent-process-discovery-and-Unicode-native-sidebar-without-query-or-report'
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
 $tabLabel='작업 탭 한 é 😀 & 긴 이름 검증';Request @('rename-tab',$source.id,$tabLabel)|Out-Null
 $tree=Await {param($t) $t.activity_panel -and $t.activity_panel.items[0].surface_label -ceq $tabLabel};$activity=$tree.activity_panel;$activityWindow=[long]$activity.window;$activityItem=$activity.items[0]
 $raw="codex`n"+$statusText+' · '+$tabLabel
 Require (-not $tree.agent_bar -and -not $activity.native_visible -and $activity.layout_visible -and $activity.activity -and [OptionsFixture]::Text([long]$activityItem.handle,$owned.Id) -ceq $raw.Replace('&','&&') -and $activityItem.tooltip -ceq $raw) 'Default Activity panel lost raw Korean tab/status text or tooltip ampersands'
 $activityBounds=[OptionsFixture]::RelativeBounds([long]$tree.window_handle,$activityWindow,$owned.Id);$activityRow=[OptionsFixture]::RelativeBounds([long]$activity.viewport,[long]$activityItem.handle,$owned.Id);$activitySize=[ChromeFixture]::Size([long]$tree.window_handle,$owned.Id)
 Require ($activityBounds.Y -ge $tree.chrome.sidebar_list_bottom -and $activityBounds.Y+$activityBounds.Height -eq $tree.chrome.sidebar_footer_top -and $activityBounds.Height -le ($activitySize[1]-76)/3+1 -and $activityRow.Height -gt 47 -and $activityRow.Height -le 90) 'Activity layout does not fit wrapped content below workspaces and above the footer'
 $bmp=Join-Path $directory 'activity-korean.bmp';$png=Join-Path $directory 'activity-korean.png';Request @('chrome-capture',$bmp,'--agent-bar')|Out-Null;[ChromeFixture]::Png($bmp,$png)
 Require ([ChromeFixture]::ColorCount($png,40,70,150,15,'#f59e0b') -gt 5) 'Wrapped Korean activity text was not painted below its first line'
 Click ([long]$activityItem.handle);Require ((Request @('identify')).surface -ceq $source.id) 'Activity card did not activate its live terminal'
 Stable @($source,$local)|Out-Null;Passed 'default-Activity-panel-raw-Korean-and-tab-label-three-line-wrap-tooltip-geometry-paint-and-click'
 foreach($zoom in @(50,125,200)){
  Request @('settings','set','zoom-percent',([string]$zoom))|Out-Null;$tree=Tree;$activity=$tree.activity_panel;$item=$activity.items[0]
  $font=[ChromeFixture]::FontHeight([long]$item.handle,$owned.Id);$row=[OptionsFixture]::RelativeBounds([long]$activity.viewport,[long]$item.handle,$owned.Id)
  Require ($activity.window -eq $activityWindow -and $item.handle -eq $activityItem.handle -and $font -eq [Math]::Floor(11*$tree.chrome.dpi*$zoom/7200+0.5) -and $row.Height -ge (10+37*$zoom/100)*$tree.chrome.dpi/96-1 -and $item.tooltip -ceq $raw) 'Activity zoom clipped its text row, changed native identity or lost Korean tooltip'
  $bmp=Join-Path $directory ('activity-zoom-'+$zoom+'.bmp');$png=Join-Path $directory ('activity-zoom-'+$zoom+'.png');Request @('chrome-capture',$bmp,'--agent-bar')|Out-Null;[ChromeFixture]::Png($bmp,$png)
 }
 Request @('settings','set','zoom-percent','100')|Out-Null;Stable @($source,$local)|Out-Null
 Passed 'Activity-global-zoom-native-font-row-height-Korean-tooltip-and-stable-window'
 $tree=Tree;Require (-not $tree.agent_bar -and -not (Request @('settings','show')).document.terminal.agent_bar_mode) 'Agents bar default differs from Linux'
 $toggle=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'agent_bar' -and $_.layout_visible})[0];Click ([long]$toggle.handle)
 $tree=Await {param($t) $t.agent_bar.layout_visible -and @($t.agent_bar.items).Count -eq 1};$bar=$tree.agent_bar;$barHandle=[long]$bar.items[0].handle
 Require (-not $tree.activity_panel -and [OptionsFixture]::WindowDestroyed($activityWindow)) 'Agents bar mode did not replace the Activity panel'
 Require (-not $bar.native_visible -and [OptionsFixture]::Parent([long]$bar.window,$owned.Id) -eq $tree.window_handle -and [OptionsFixture]::Parent($barHandle,$owned.Id) -eq $bar.viewport -and [OptionsFixture]::Text($barHandle,$owned.Id).Contains($statusText)) 'Agents bar lost native ownership or raw Korean status'
 Require ($bar.items[0].tooltip -ceq ("codex`n"+$statusText)) 'Native agent tooltip lost its full Korean status'
 $barBounds=[OptionsFixture]::RelativeBounds([long]$tree.window_handle,[long]$bar.window,$owned.Id)
 foreach($pane in $tree.layout.panes){Require ($pane[1].y+$pane[1].height -le $barBounds.Y) 'Agents bar overlaps terminal content'}
 $bmp=Join-Path $directory 'agent-bar-working.bmp';$png=Join-Path $directory 'agent-bar-working.png';Request @('chrome-capture',$bmp,'--agent-bar')|Out-Null;[ChromeFixture]::Png($bmp,$png)
 Require ([ChromeFixture]::ColorCount($png,64,3,168,47,'#f59e0b') -gt 5) 'Working agent bar status was not painted'
 Stable @($source,$local)|Out-Null;Passed 'Linux-agent-bar-default-footer-toggle-native-ownership-geometry-working-paint-and-raw-Korean'
 Require ((Request @('settings','show')).document.terminal.agent_notification_target -ceq 'agent_bar') 'Notification target default differs from Linux'
 foreach($level in @('info','completed','error')){Request @('notifications','clear')|Out-Null;Request @('notify','--surface',$source.id,'--level',$level,'ordinary notification')|Out-Null;Attention (Tree) $false $false}
 Request @('notifications','clear')|Out-Null;$noticeTitle='입력 요청 한';$noticeBody="승인 필요 é 😀`n두 번째 줄"
 $notice=Request @('notify','--surface',$source.id,'--level','attention','--title',$noticeTitle,$noticeBody);Require ($notice.accepted) 'Owned attention notification was rejected'
 $tree=Await {param($t) $t.agent_bar.items[0].attention};Attention $tree $true $false
 $bmp=Join-Path $directory 'agent-bar-attention.bmp';$png=Join-Path $directory 'agent-bar-attention.png';Request @('chrome-capture',$bmp,'--agent-bar')|Out-Null;[ChromeFixture]::Png($bmp,$png)
 Require ([ChromeFixture]::Pixel($png,222,40) -ceq '#493c27') 'Agent attention did not paint the Linux amber18 percent tint'
 $duplicate=Request @('notify','--surface',$source.id,'--level','attention','duplicate');Require (-not $duplicate.accepted -and $duplicate.reason -ceq 'duplicate') 'Repeated attention bypassed notification deduplication'
 Request @('focus-tab',$source.id)|Out-Null;Click $barHandle;$notes=Request @('notifications','list');Attention (Tree) $true $false
 Require ($notes.unread_count -eq 1 -and $notes.entries[0].title -ceq $noticeTitle -and $notes.entries[0].body -ceq $noticeBody) 'Hidden selection acknowledged unread attention or changed Unicode notification fields'
 Request @('settings','set','agent-notification-target','workspace')|Out-Null;$tree=Tree;Attention $tree $false $true
 $row=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'workspace' -and $_.workspace -ceq $sourceIdentity.workspace})[0]
 $bmp=Join-Path $directory 'workspace-attention.bmp';$png=Join-Path $directory 'workspace-attention.png';Request @('chrome-capture',$bmp)|Out-Null;[ChromeFixture]::Png($bmp,$png)
 Require ([ChromeFixture]::Pixel($png,($row.rect.x+$row.rect.width-4),($row.rect.y+$row.rect.height-4)) -ceq '#493c27') 'Workspace attention did not paint the Linux amber18 percent tint'
 Request @('settings','set','agent-notification-target','both')|Out-Null;Attention (Tree) $true $true
 Request @('settings','set','agent-notification-target','invalid') 3000 $true|Out-Null;Require ((Request @('settings','show')).document.terminal.agent_notification_target -ceq 'both') 'Invalid notification target replaced the last valid setting'
 Request @('notifications','mark-read',$notice.id)|Out-Null;Attention (Tree) $false $false
 foreach($action in @('delete','clear','show','open')){
  Request @('notifications','clear')|Out-Null;$notice=Request @('notify','--surface',$source.id,'--level','attention',('attention '+$action));Attention (Tree) $true $true
  if($action -in @('delete','open')){Request @('notifications',$action,$notice.id)|Out-Null}else{$result=Request @('notifications',$action);if($action -ceq 'show'){[SearchDialogFixture]::Close([IntPtr][long]$result.panel_handle,$owned.Id);Require (-not (Request @('notifications','list')).panel_snapshot.open) 'Owned notification popup did not close before sidebar capture'}}
  Attention (Tree) $false $false
 }
 Request @('notifications','clear')|Out-Null;$a=Request @('notify','--surface',$source.id,'--level','attention','source');$b=Request @('notify','--surface',$local.id,'--level','attention','other surface')
 Request @('notifications','delete',$a.id)|Out-Null;Attention (Tree) $false $true
 Request @('notifications','delete',$b.id)|Out-Null;Attention (Tree) $false $false
 Request @('settings','set','agent-notification-target','agent_bar')|Out-Null;Stable @($source,$local)|Out-Null
 Passed 'Linux-attention-targets-native-amber-paint-Unicode-dedup-hidden-focus-read-delete-clear-show-open-and-source-isolation'
 foreach($seq in @('1','2')){$stale=Request ($report+@('--seq',$seq,'--status','blocked','--message','stale'));Require (-not $stale.accepted -and $stale.agent.seq -eq 2 -and $stale.agent.status -ceq 'working' -and $stale.agent.message -ceq $statusText) 'Old/duplicate sequence overwrote current status'}
 $r=Request ($report+@('--seq','4','--status','blocked','--message',$statusText));Require ($r.accepted -and $r.agent.status -ceq 'blocked' -and $r.agent.activity -ceq 'needs_input' -and -not $r.agent.seen) 'Hidden blocked report lost its unseen state'
 Require ((Agent-Sidebar 'agent-blocked' 'blocked' $statusText) -eq $sidebarHandle) 'Status report replaced workspace HWND'
 $stale=Request ($report+@('--seq','3','--status','working'));Require (-not $stale.accepted -and $stale.agent.status -ceq 'blocked') 'Delayed progress cleared a newer input wait'
 $r=Request ($report+@('--seq','5','--status','idle'));Require ($r.accepted -and $r.agent.status -ceq 'done' -and $r.agent.activity -ceq 'idle' -and -not $r.agent.seen) 'Unseen idle transition did not derive Done'
 Require ((Agent-Sidebar 'agent-done' 'done' 'done') -eq $sidebarHandle) 'Done report replaced workspace HWND'
 $tree=Tree;Require ($tree.agent_bar.items[0].handle -eq $barHandle -and $tree.agent_bar.items[0].status -ceq 'done' -and [OptionsFixture]::Text($barHandle,$owned.Id).Contains('done')) 'Agent bar status changes replaced its native button'
 Request @('focus-tab',$source.id)|Out-Null;$tree=Tree;$state=@($tree.surfaces|Where-Object {$_.id -ceq $source.id})[0].agent
 Require ($state.status -ceq 'done' -and $state.seq -eq 5) 'Hidden focus falsely acknowledged Done'
 $agents=@(Request @('agents'));Require ($agents[0].status -ceq 'done' -and -not $agents[0].messaging) 'Agent listing discarded reported activity or advertised fake messaging'
 Request @('settings','set','focus-border-color','#12abef')|Out-Null
 foreach($focus in @(@('0','#24272e'),@('37','#1d5775'),@('100','#12abef'))){
  Request @('settings','set','focus-border-opacity',$focus[0])|Out-Null;$tree=Tree;$rect=Agent-Bounds $tree $source.id $false
  $bmp=Join-Path $directory ('agent-focus-'+$focus[0]+'.bmp');$png=Join-Path $directory ('agent-focus-'+$focus[0]+'.png');Request @('chrome-capture',$bmp,'--agent-bar')|Out-Null;[ChromeFixture]::Png($bmp,$png)
  Require ([ChromeFixture]::Pixel($png,([int]($rect.X+$rect.Width/2)),$rect.Y) -ceq $focus[1] -and $tree.agent_bar.items[0].handle -eq $barHandle) 'Selected agent outline lost configured focus opacity or native control identity'
 }
 Request @('settings','set','focus-border-color','#fff4b3')|Out-Null;Request @('settings','set','focus-border-opacity','30')|Out-Null
 Passed 'selected-agent-focus-color-opacity-zero-intermediate-full-and-stable-HWND'
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

 Require (@($tree.agent_bar.items).Count -eq 5) 'Agents bar truncated the sidebar overflow agent'
 $barSize=[ChromeFixture]::Size([long]$tree.window_handle,$owned.Id);[ChromeFixture]::Resize([long]$tree.window_handle,$owned.Id,900,$barSize[1]);$tree=Tree
 $bar=$tree.agent_bar;[OptionsFixture]::PostKey([long]$bar.viewport,$owned.Id,35,$false,$false);$tree=Await {param($t) $t.agent_bar.offset -gt 0}
 $lastItem=$tree.agent_bar.items[-1];$lastBounds=[OptionsFixture]::RelativeBounds([long]$bar.viewport,[long]$lastItem.handle,$owned.Id);$viewportSize=[ChromeFixture]::Size([long]$bar.viewport,$owned.Id)
 Require ($lastBounds.X -ge 0 -and $lastBounds.X+$lastBounds.Width -le $viewportSize[0]) 'Horizontal scroll did not expose the last full agent card'
 $bmp=Join-Path $directory 'agent-bar-end.bmp';$png=Join-Path $directory 'agent-bar-end.png';Request @('chrome-capture',$bmp,'--agent-bar')|Out-Null;[ChromeFixture]::Png($bmp,$png)
 Request @('focus-tab',$local.id)|Out-Null;Click ([long]$lastItem.handle);$tree=Tree;Require ((Request @('identify')).surface -ceq $lastItem.surface) 'Agent card click did not activate its captured terminal'
 Request @('focus-tab',$source.id)|Out-Null;$tree=Tree;$toggle=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'agent_bar'})[0];Click ([long]$toggle.handle)
 $tree=Await {param($t) -not $t.agent_bar};Require ([OptionsFixture]::WindowDestroyed([long]$bar.window)) 'Disabling agents bar retained its native window'
 $activity=$tree.activity_panel;Require (@($activity.items).Count -eq 5 -and $activity.activity -and -not $activity.native_visible) 'Activity panel omitted an agent when switching from bar mode'
 $activityOrder=$activity.items.surface -join ',';$activityHandles=$activity.items.handle -join ',';$contentBefore=$tree.layout|ConvertTo-Json -Depth 20 -Compress
 [OptionsFixture]::Scroll([long]$activity.viewport,$owned.Id,$true);$tree=Await {param($t) $t.activity_panel.offset -gt 0};$last=$tree.activity_panel.items[-1]
 $lastBounds=[OptionsFixture]::RelativeBounds([long]$activity.viewport,[long]$last.handle,$owned.Id);$viewSize=[ChromeFixture]::Size([long]$activity.viewport,$owned.Id)
 Require ($lastBounds.Y -ge 0 -and $lastBounds.Y+$lastBounds.Height -le $viewSize[1]) 'Vertical Activity scroll did not expose the final full card'
 $bmp=Join-Path $directory 'activity-scroll.bmp';$png=Join-Path $directory 'activity-scroll.png';Request @('chrome-capture',$bmp,'--agent-bar')|Out-Null;[ChromeFixture]::Png($bmp,$png)
 Request @('focus-tab',$local.id)|Out-Null;[OptionsFixture]::PostEnter([long]$last.handle,$owned.Id);$tree=Tree;Require ((Request @('identify')).surface -ceq $last.surface) 'Activity keyboard activation targeted another surface'
 Request @('focus-tab',$source.id)|Out-Null;[OptionsFixture]::PostKey([long]$activity.viewport,$owned.Id,36,$false,$false);$tree=Await {param($t) $t.activity_panel.offset -eq 0}
 [OptionsFixture]::PostKey([long]$activity.viewport,$owned.Id,34,$false,$false);$tree=Await {param($t) $t.activity_panel.offset -gt 0}
 [OptionsFixture]::PostKey([long]$activity.viewport,$owned.Id,33,$false,$false);$tree=Await {param($t) $t.activity_panel.offset -eq 0}
 $before=[OptionsFixture]::RelativeBounds([long]$tree.window_handle,[long]$activity.window,$owned.Id)
 [OptionsFixture]::TabPointerDown([long]$tree.window_handle,[long]$activity.window,$owned.Id,3,2);$tree=Await {param($t) $t.chrome.activity_resizing}
 [OptionsFixture]::HostPointer([long]$tree.window_handle,$owned.Id,0x200,3,($before.Y+2-100));$tree=Tree
 [OptionsFixture]::HostPointer([long]$tree.window_handle,$owned.Id,0x202,3,($before.Y+2-100));$tree=Await {param($t) -not $t.chrome.activity_resizing}
 $after=[OptionsFixture]::RelativeBounds([long]$tree.window_handle,[long]$activity.window,$owned.Id)
 Require ($after.Height -ge $before.Height+98 -and ($tree.layout|ConvertTo-Json -Depth 20 -Compress) -ceq $contentBefore -and ($tree.activity_panel.items.handle -join ',') -ceq $activityHandles -and ($tree.activity_panel.items.surface -join ',') -ceq $activityOrder) 'Activity divider resized terminal content, rebuilt cards, or lost order'
 [OptionsFixture]::PostKey([long]$activity.window,$owned.Id,40,$false,$false);$tree=Tree;$smaller=[OptionsFixture]::RelativeBounds([long]$tree.window_handle,[long]$activity.window,$owned.Id);Require ($smaller.Height -eq $after.Height-16) 'Activity divider cannot be adjusted by keyboard'
 Passed 'Activity-native-vertical-scroll-Page-keys-Enter-and-divider-preserve-card-order-and-terminal-geometry'
 [ChromeFixture]::Resize([long]$tree.window_handle,$owned.Id,$barSize[0],$barSize[1]);Request @('settings','set','agent-bar-mode','true')|Out-Null;$tree=Await {param($t) @($t.agent_bar.items).Count -eq 5};Stable (@($source,$local)+$extras)|Out-Null
 Passed 'agent-bar-all-providers-horizontal-end-scroll-live-target-activation-toggle-and-session-retention'

 $size=[ChromeFixture]::Size([long]$tree.window_handle,$owned.Id);[ChromeFixture]::Resize([long]$tree.window_handle,$owned.Id,1400,$size[1]);$tree=Tree
 $originalAgents=@($tree.agent_bar.items.surface);$originalOrder=Agent-Order $tree;$handles=@($tree.agent_bar.items.handle);$active=(Request @('identify')).surface;$workspaceBefore=$tree.workspaces|ConvertTo-Json -Depth 30 -Compress
 $p=Agent-Point $tree $originalAgents[1] $true;$tree=Agent-Begin $tree $originalAgents[4];[OptionsFixture]::HostPointer([long]$tree.window_handle,$owned.Id,0x200,$p.x,$p.y);$tree=Tree
 $r=Agent-Bounds $tree $originalAgents[1] $false
 $bmp=Join-Path $directory 'agent-bar-drop.bmp';$png=Join-Path $directory 'agent-bar-drop.png';Request @('chrome-capture',$bmp,'--agent-bar')|Out-Null;[ChromeFixture]::Png($bmp,$png)
 Require ([ChromeFixture]::ColorCount($png,$r.X,($r.Y+3),3,($r.Height-6),'#78aeed') -gt 30) 'Native agent insertion marker was not painted'
 $tree=Agent-Release $tree $p;$reordered=@($originalAgents[0],$originalAgents[4],$originalAgents[1],$originalAgents[2],$originalAgents[3]) -join ','
 Require ((Agent-Order $tree) -ceq $reordered -and (Request @('identify')).surface -ceq $active -and ($tree.workspaces|ConvertTo-Json -Depth 30 -Compress) -ceq $workspaceBefore) 'Agent drag changed tabs/focus or inserted at the wrong side'
 Require ((@([ChromeFixture]::Read([long]$tree.agent_bar.viewport,$owned.Id)|Where-Object {$_.Class -ceq 'Button'}).Handle -join ',') -ceq ($tree.agent_bar.items.handle -join ',')) 'Agent native keyboard order differs from the visible cards'
 $tree=Agent-Drag $tree $originalAgents[4] $originalAgents[3] $false;Require ((Agent-Order $tree) -ceq $originalOrder) 'Agent after insertion did not account for source removal'
 foreach($before in @($true,$false)){$tree=Agent-Drag $tree $originalAgents[2] $originalAgents[2] $before;Require ((Agent-Order $tree) -ceq $originalOrder) 'Agent self drop changed order'}
 $tree=Agent-Drag $tree $originalAgents[4] $originalAgents[1] $true
 $message='순서 유지 한 é 상태';$extra=$extras[1];Request @('report-agent','opencode','--surface',$extra.id,'--pid',[string]$extra.pid,'--seq','2','--status','working','--message',$message)|Out-Null
 $tree=Await {param($t) @($t.agent_bar.items|Where-Object {$_.surface -ceq $extra.id -and $_.message -ceq $message}).Count -eq 1}
 Require ((Agent-Order $tree) -ceq $reordered -and (@($tree.agent_bar.items.handle|Sort-Object) -join ',') -ceq (@($handles|Sort-Object) -join ',')) 'Status update discarded agent order or native control identity'
 Passed 'agent-drag-before-after-self-drop-native-marker-keyboard-order-and-raw-Korean-status-retain-sessions'
 foreach($cancel in @('escape','cancelmode','capture','outside')){
  $p=Agent-Point $tree $originalAgents[1] $false;$tree=Agent-Begin $tree $originalAgents[0];[OptionsFixture]::HostPointer([long]$tree.window_handle,$owned.Id,0x200,$p.x,$p.y)
  if($cancel -ceq 'escape'){[OptionsFixture]::PostEscape([long]$tree.window_handle,$owned.Id)}elseif($cancel -ceq 'outside'){$p=@{x=-100;y=-100}}else{[OptionsFixture]::HostPointer([long]$tree.window_handle,$owned.Id,$(if($cancel -ceq 'cancelmode'){0x1f}else{0x215}),0,0)}
  $tree=Agent-Release $tree $p;Require ((Agent-Order $tree) -ceq $reordered -and (Request @('identify')).surface -ceq $active -and @($tree.detached_windows).Count -eq 0) ('Cancelled agent drag changed order/focus or detached a terminal: '+$cancel)
 }
 $p=Agent-Point $tree $originalAgents[1] $false;$tree=Agent-Begin $tree $originalAgents[0];Request @('settings','set','agent-bar-mode','false')|Out-Null;$tree=Await {param($t) -not $t.agent_bar -and -not $t.chrome.agent_dragging}
 Request @('settings','set','agent-bar-mode','true')|Out-Null;$tree=Await {param($t) @($t.agent_bar.items).Count -eq 5};$tree=Agent-Release $tree $p
 Require ((Agent-Order $tree) -ceq $reordered) 'Toggle lost agent order or stale release committed a cancelled gesture'
 [ChromeFixture]::Resize([long]$tree.window_handle,$owned.Id,$size[0],$size[1]);$tree=Stable (@($source,$local)+$extras)
 Passed 'agent-drag-Escape-capture-loss-outside-and-toggle-cancel-with-runtime-order-retention'
 Request @('focus-tab',$local.id)|Out-Null;$tree=Tree;$tree=Agent-Begin $tree $originalAgents[1];$r=Agent-Bounds $tree $originalAgents[1] $true
 $tree=Agent-Release $tree @{x=[int]($r.X+$r.Width/2);y=[int]($r.Y+$r.Height/2)}
 Require ((Request @('identify')).surface -ceq $originalAgents[1] -and (Agent-Order $tree) -ceq $reordered) 'A stationary agent click was swallowed as a drag or reordered a card'
 Request @('focus-tab',$active)|Out-Null;[ChromeFixture]::Resize([long]$tree.window_handle,$owned.Id,900,$size[1]);$tree=Tree
 [OptionsFixture]::PostKey([long]$tree.agent_bar.viewport,$owned.Id,35,$false,$false);$tree=Await {param($t) $t.agent_bar.offset -gt 0}
 $hidden=$tree.agent_bar.items[0];$hiddenBounds=[OptionsFixture]::RelativeBounds([long]$tree.agent_bar.viewport,[long]$hidden.handle,$owned.Id);Require ($hiddenBounds.X+$hiddenBounds.Width -lt 0) 'Expected fully clipped agent card for pointer guard'
 [OptionsFixture]::TabPointerDown([long]$tree.agent_bar.viewport,[long]$hidden.handle,$owned.Id,20,20);$tree=Tree
 Require (-not $tree.chrome.agent_dragging -and (Request @('identify')).surface -ceq $active) 'Clipped agent card accepted a pointer gesture outside the viewport'
 [ChromeFixture]::Resize([long]$tree.window_handle,$owned.Id,$size[0],$size[1]);$tree=Tree
 Passed 'agent-pointer-click-selects-without-reordering-and-clipped-card-cannot-start-drag'


 $originalSize=[ChromeFixture]::Size([long]$tree.window_handle,$owned.Id);$beforeTall=@($tree.surfaces);$rowHandle=[long]$row.handle
 [ChromeFixture]::Resize([long]$tree.window_handle,$owned.Id,900,300)
 $tree=Await {param($t) $t.chrome.sidebar_max_offset_px -gt 0 -and $t.chrome.sidebar_offset_px -eq 0};$row=@($tree.chrome.controls|Where-Object {$_.handle -eq $rowHandle})[0]
 Require ($row.layout_visible -and $row.clip.height -lt $row.rect.height -and $row.rect.height -gt $tree.chrome.sidebar_list_bottom-$tree.chrome.sidebar_list_top) 'A tall agent row was hidden or shrunk to discard its final metadata'
 $bmp=Join-Path $directory 'agent-scroll-top.bmp';$png=Join-Path $directory 'agent-scroll-top.png';Request @('chrome-capture',$bmp)|Out-Null;[ChromeFixture]::Png($bmp,$png)
 Require ([ChromeFixture]::Pixel($png,($row.rect.x+3),298) -ceq '#24272e') 'Tall row painted over the footer outside its native region'
 [OptionsFixture]::TabPointerDown([long]$tree.window_handle,$rowHandle,$owned.Id,100,40);$tree=Await {param($t) $t.chrome.workspace_dragging}
 [OptionsFixture]::HostPointer([long]$tree.window_handle,$owned.Id,0x200,106,($tree.chrome.sidebar_list_bottom-3))
 $bmp=Join-Path $directory 'agent-scroll-drop.bmp';$png=Join-Path $directory 'agent-scroll-drop.png';Request @('chrome-capture',$bmp)|Out-Null;[ChromeFixture]::Png($bmp,$png)
 Require ([ChromeFixture]::ColorCount($png,80,($tree.chrome.sidebar_list_bottom-2),100,1,'#78aeed') -eq 100) 'Drop marker was hidden below the clipped workspace viewport'
 [OptionsFixture]::HostPointer([long]$tree.window_handle,$owned.Id,0x1f,0,0);$tree=Await {param($t) -not $t.chrome.workspace_dragging}
 $next=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'sidebar_next'})[0];Click ([long]$next.handle)
 $tree=Await {param($t) $t.chrome.sidebar_offset_px -eq $t.chrome.sidebar_max_offset_px};$row=@($tree.chrome.controls|Where-Object {$_.handle -eq $rowHandle})[0]
 Require ($row.clip.y -gt 0 -and $row.rect.y+$row.clip.y -eq $tree.chrome.sidebar_list_top -and $row.rect.y+$row.rect.height -le $tree.chrome.sidebar_list_bottom) 'Next did not expose the bottom of the full-height row'
 Require (-not [OptionsFixture]::Describe([long]$next.handle,$owned.Id).Enabled) 'Next remained enabled at the pixel scroll boundary'
 $bmp=Join-Path $directory 'agent-scroll-bottom.bmp';$png=Join-Path $directory 'agent-scroll-bottom.png';Request @('chrome-capture',$bmp)|Out-Null;[ChromeFixture]::Png($bmp,$png)
 Require ([ChromeFixture]::Pixel($png,($row.rect.x+3),($tree.chrome.sidebar_list_top-1)) -ceq '#24272e') 'Scrolled row painted over the Workspaces header'
 $lastY=$row.rect.y+[int][Math]::Round(247*$scale);Require ([ChromeFixture]::ColorCount($png,($row.rect.x+40),$lastY,150,([int](20*$scale)),'#abb1bc') -gt 5) 'Final agent path is not actually painted after scrolling'
 [OptionsFixture]::PostKey($rowHandle,$owned.Id,36,$false,$false);$tree=Await {param($t) $t.chrome.sidebar_offset_px -eq 0}
 [OptionsFixture]::PostKey($rowHandle,$owned.Id,34,$false,$false);$tree=Await {param($t) $t.chrome.sidebar_offset_px -gt 0}
 [OptionsFixture]::PostKey($rowHandle,$owned.Id,33,$false,$false);$tree=Await {param($t) $t.chrome.sidebar_offset_px -eq 0}
 [OptionsFixture]::PostKey($rowHandle,$owned.Id,35,$false,$false);$tree=Await {param($t) $t.chrome.sidebar_offset_px -eq $t.chrome.sidebar_max_offset_px}
 $previous=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'sidebar_previous'})[0];Click ([long]$previous.handle);$tree=Await {param($t) $t.chrome.sidebar_offset_px -eq 0}
 [OptionsFixture]::Wheel([long]$tree.window_handle,$owned.Id,20,50,-120);$tree=Tree;$wholeWheel=$tree.chrome.sidebar_offset_px
 $wheelLines=[OptionsFixture]::WheelLines();$wheelStep=if($wheelLines -eq [uint32]::MaxValue){$tree.chrome.sidebar_list_bottom-$tree.chrome.sidebar_list_top-[int][Math]::Round(20*$scale)}else{[Math]::Min($wheelLines,100)*[int][Math]::Round(20*$scale)}
 Require ($wholeWheel -eq [Math]::Min($wheelStep,$tree.chrome.sidebar_max_offset_px)) 'Wheel did not scroll according to native Windows line/page settings'
 [OptionsFixture]::PostKey($rowHandle,$owned.Id,36,$false,$false);$tree=Await {param($t) $t.chrome.sidebar_offset_px -eq 0}
 [OptionsFixture]::Wheel([long]$tree.window_handle,$owned.Id,20,50,-60);[OptionsFixture]::Wheel([long]$tree.window_handle,$owned.Id,20,50,-60);$tree=Tree;Require ($tree.chrome.sidebar_offset_px -eq $wholeWheel) 'High-resolution wheel deltas were rounded away or multiplied'
 [OptionsFixture]::PostKey($rowHandle,$owned.Id,35,$false,$false);$tree=Await {param($t) $t.chrome.sidebar_offset_px -eq $t.chrome.sidebar_max_offset_px}
 [ChromeFixture]::Resize([long]$tree.window_handle,$owned.Id,$originalSize[0],$originalSize[1]);$tree=Await {param($t) $t.chrome.sidebar_offset_px -eq 0 -and $t.chrome.sidebar_max_offset_px -eq 0};$row=@($tree.chrome.controls|Where-Object {$_.handle -eq $rowHandle})[0]
 Require (-not $row.clip -and @($row.workspace_lines).Count -eq 12) 'Growing the viewport retained a stale clipping region or lost metadata'
 Stable $beforeTall|Out-Null;Passed 'tall-agent-row-pixel-paging-keyboard-high-resolution-wheel-native-clipping-and-resize-retain-all-terminals'
 Request @('split','vertical','--shell=cmd')|Out-Null;$spare=Request @('identify');Request @('move-tab',$extras[3].id,'--to-pane',$spare.pane)|Out-Null
 Request @('focus-tab',$extras[3].id)|Out-Null;$tree=Tree;$row=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'workspace' -and $_.workspace -ceq $sourceIdentity.workspace})[0];Require ($row.workspace_lines[0].agent.name -ceq 'antigravity') 'Focused blocked agent pane did not lead the metadata tree'
 Request @('focus-tab',$source.id)|Out-Null;$tree=Tree;$row=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'workspace' -and $_.workspace -ceq $sourceIdentity.workspace})[0];Require ($row.workspace_lines[0].agent.name -ceq 'codex' -and $row.workspace_lines[3].agent.name -ceq 'antigravity') 'Equal-status agent blocks ignored pane MRU'
 Request @('close-tab',$spare.surface)|Out-Null;Stable (@($source,$local)+$extras)|Out-Null
 $tree=Tree;$p=Agent-Point $tree $source.id $true;$tree=Agent-Begin $tree $extras[0].id;Request @('close-tab',$extras[0].id)|Out-Null
 $tree=Await {param($t) -not $t.chrome.agent_dragging -and @($t.agent_bar.items).Count -eq 4};$remaining=Agent-Order $tree;$tree=Agent-Release $tree $p
 Require ((Agent-Order $tree) -ceq $remaining -and $tree.agent_bar.items.surface -cnotcontains $extras[0].id) 'Closing the dragged agent retained its card or a stale release changed the surviving order'
 foreach($extra in $extras[1..3]){Request @('close-tab',$extra.id)|Out-Null}
 $tree=Await {param($t) $row=@($t.chrome.controls|Where-Object {$_.kind -ceq 'workspace' -and $_.workspace -ceq $sourceIdentity.workspace})[0];$row.workspace_lines.Count -eq 3 -and @($t.surfaces).Count -eq 2};Stable @($source,$local)|Out-Null
 Passed 'five-owned-providers-Linux-urgency-and-pane-MRU-four-agent-cap-overflow-native-logos-and-close-shrink'
 Require ([IO.File]::Exists((Join-Path $homeA 'session_index.jsonl')) -and [IO.Directory]::Exists((Join-Path $homeA 'sessions'))) 'Owned session history fixture disappeared before discovery'
 Shortcut $source.id;$tree=Await {param($t) $t.agent_sessions.open -and -not $t.agent_sessions.loading -and @($t.agent_sessions.rows).Count -eq 2};$panel=Panel $tree
 Require (-not $tree.agent_sessions.agent.session_id) 'Native end resurrected the startup resume ID';Require ($tree.agent_sessions.agent.name -ceq 'Codex' -and $tree.agent_sessions.agent.pid -eq $source.pid -and (Path-Same $tree.agent_sessions.agent.home $homeA) -and $tree.agent_sessions.source.surface -ceq $source.id -and $tree.agent_sessions.source.session -ceq $source.session) 'Discovery did not resolve actual focused agent Job/home/session'
 $image=[Diagnostics.Process]::GetProcessById([int]$tree.agent_sessions.agent.pid);try{Require (Path-Same $image.MainModule.FileName $agentExe) 'History source image is not actual owned codex.exe'}finally{$image.Dispose()};Stable @($local,$source)|Out-Null;Passed 'actual-owned-codex-process-child-only-CODEX_HOME-and-CtrlAltJ-history-discovery'

 Require (@($panel.rows|Where-Object {$_.title -ceq $titleA -and $_.id -ceq $idA}).Count -eq 1 -and @($panel.rows|Where-Object {$_.title -ceq $titleB -and $_.id -ceq $idB}).Count -eq 1) 'Indexed Unicode/NFD titles or cross-project rows differ';Require (@($panel.rows.color|Select-Object -Unique).Count -eq 2) 'Distinct project row colors missing'
 $native=[OptionsFixture]::Describe([long]$panel.window,$owned.Id);$scale=$native.Dpi/96.0;$area=[OptionsFixture]::RelativeBounds([long]$tree.window_handle,[long]$panel.window,$owned.Id);Require ([Math]::Abs($area.Width-360*$scale) -le 2 -and [Math]::Abs($panel.preview_bounds.height-150*$scale) -le 2) 'Sessions dock/preview geometry differs from360/150DIP';foreach($pane in $tree.layout.panes){Require ($pane[1].x+$pane[1].width -le $area.X) 'Session dock overlaps terminal content'}
 Query $tree $idB;$tree=Await {param($t) @($t.agent_sessions.panel.rows).Count -eq 1 -and $t.agent_sessions.panel.rows[0].id -ceq $idB};Query $tree '한 é';$tree=Await {param($t) @($t.agent_sessions.panel.rows).Count -eq 1 -and $t.agent_sessions.panel.rows[0].id -ceq $idA};Require ([OptionsFixture]::Text([long]$tree.agent_sessions.panel.query_handle,$owned.Id) -ceq '한 é') 'Search normalized raw NFD input';Query $tree '';Passed 'native-two-project-rows-index-titles-colors360DIP-dock150DIP-preview-and-ID-NFD-filter'

 $tree=Await {param($t) @($t.agent_sessions.panel.rows).Count -eq 2};$tree=Select-Session $tree $idA;$panel=Panel $tree;$preview=[OptionsFixture]::Text([long]$panel.preview,$owned.Id);Require (([OptionsFixture]::Describe([long]$panel.preview,$owned.Id).Style -band 0x800) -ne 0) 'Conversation preview is not native readonly EDIT';Require ($preview.Contains($titleA) -and $preview.Contains('질문 한 é 😀') -and $preview.Contains('답변 한글') -and $preview.Contains('두 번째 줄') -and $preview -notmatch 'PRIVATE_SYSTEM|PRIVATE_TOOL|PRIVATE_EVENT') 'Native readonly preview omitted conversation or exposed private/tool records';Require (($preview -replace "`r`n","`n") -ceq ($panel.preview_text -replace "`r`n","`n")) 'Native preview differs from controller text'
 foreach($query in @('한 É','한 é')){
  Query $tree $query;$tree=Await {param($t) [string]::Equals($t.agent_sessions.panel.filter,$query,[StringComparison]::Ordinal)};$filtered=$tree.agent_sessions.panel
  Require (@($filtered.rows).Count -eq 1 -and $filtered.rows[0].id -ceq $idA -and $filtered.selected -ceq $idA -and $filtered.resume_enabled) 'Canonical session search lost the matching selected conversation'
  Require ([string]::Equals([OptionsFixture]::Text([long]$filtered.query_handle,$owned.Id),$query,[StringComparison]::Ordinal) -and [string]::Equals($filtered.rows[0].title,$titleA,[StringComparison]::Ordinal) -and [string]::Equals([OptionsFixture]::Text([long]$filtered.preview,$owned.Id),$preview,[StringComparison]::Ordinal)) 'Canonical session filtering changed raw query, title or conversation preview'
 }
 Query $tree '';$tree=Await {param($t) @($t.agent_sessions.panel.rows).Count -eq 2};Passed 'canonical-Hangul-accent-session-search-preserves-raw-query-title-preview-and-selected-ID'
 Request @('settings','set','zoom-percent','200')|Out-Null;$tree=Tree;$zoomed=Panel $tree
 Require ($zoomed.preview -eq $panel.preview -and $zoomed.list -eq $panel.list -and $zoomed.selected -ceq $idA -and [OptionsFixture]::Text([long]$zoomed.preview,$owned.Id) -ceq $preview -and [ChromeFixture]::FontHeight([long]$zoomed.preview,$owned.Id) -eq [Math]::Floor(22*$tree.chrome.dpi/72+0.5) -and [ChromeFixture]::FontHeight([long]$zoomed.list,$owned.Id) -eq [Math]::Floor(22*$tree.chrome.dpi/72+0.5)) 'Session zoom replaced native controls, preview text or selection, or failed to resize actual fonts'
 Request @('settings','set','zoom-percent','100')|Out-Null;Passed 'Sessions-global-zoom-preserves-native-preview-Korean-text-and-selected-history'
 Query $tree $idB;$tree=Await {param($t) @($t.agent_sessions.panel.rows).Count -eq 1 -and $t.agent_sessions.panel.rows[0].id -ceq $idB -and $null -eq $t.agent_sessions.selected -and $null -eq $t.agent_sessions.panel.selected -and -not $t.agent_sessions.panel.preview_text -and -not $t.agent_sessions.resume_enabled -and -not $t.agent_sessions.panel.resume_enabled};$panel=Panel $tree;Require ([OptionsFixture]::Text([long]$panel.preview,$owned.Id) -ceq '' -and -not [OptionsFixture]::Describe([long]$panel.resume,$owned.Id).Enabled) 'Filtering out the selected session retained its native preview or Resume action'
 Query $tree '';$tree=Await {param($t) @($t.agent_sessions.panel.rows).Count -eq 2};$tree=Select-Session $tree $idA;$panel=Panel $tree;Require ($panel.selected -ceq $idA -and ([OptionsFixture]::Text([long]$panel.preview,$owned.Id) -replace "`r`n","`n") -ceq ($preview -replace "`r`n","`n") -and [OptionsFixture]::Describe([long]$panel.resume,$owned.Id).Enabled) 'Selecting the same session after clearing its filter did not restore the native preview and Resume action';Passed 'selected-session-native-conversation-preview-excludes-private-records-and-filter-clears-then-restores-selection'

 $compositionSurfaces=@($tree.surfaces)
 [OptionsFixture]::CompositionGuard([long]$panel.window,[long]$panel.query_handle,$owned.Id,$true);Query $tree '한 é';$tree=Await {param($t) $t.agent_sessions.panel.composing -and $t.agent_sessions.panel.query -ceq '한 é'}
 Require (@($tree.agent_sessions.panel.rows).Count -eq 2) 'Search filtered during controlled composition'
 Require (-not [OptionsFixture]::Describe([long]$panel.resume,$owned.Id).Enabled -and -not [OptionsFixture]::Describe([long]$panel.refresh,$owned.Id).Enabled) 'Session Resume/Refresh remain actionable during text composition'
 [OptionsFixture]::PostEnter([long]$panel.query_handle,$owned.Id);[OptionsFixture]::PostEscape([long]$panel.query_handle,$owned.Id);Request @('settings','set','theme-preset','nord')|Out-Null;$tree=Tree
 Require ($tree.agent_sessions.open -and $tree.agent_sessions.panel.composing -and [string]::Equals([OptionsFixture]::Text([long]$panel.query_handle,$owned.Id),'한 é',[StringComparison]::Ordinal) -and @($tree.surfaces).Count -eq $compositionSurfaces.Count -and $tree.agent_sessions.selected -ceq $idA -and -not [OptionsFixture]::Describe([long]$panel.resume,$owned.Id).Enabled -and -not [OptionsFixture]::Describe([long]$panel.refresh,$owned.Id).Enabled) 'Theme or keys changed the composing draft, selection or disabled action state'
 Stable $compositionSurfaces|Out-Null;Passed 'session-composition-disables-Resume-Refresh-and-preserves-selection-processes-and-raw-draft-through-theme'
 [OptionsFixture]::CompositionGuard([long]$panel.window,[long]$panel.query_handle,$owned.Id,$false);[OptionsFixture]::PostKey([long]$panel.query_handle,$owned.Id,65,$true,$false);$tree=Await {param($t) -not $t.agent_sessions.panel.composing -and @($t.agent_sessions.panel.rows).Count -eq 1 -and $t.agent_sessions.panel.resume_enabled -and [OptionsFixture]::Describe([long]$panel.refresh,$owned.Id).Enabled};Require ($tree.agent_sessions.panel.window -eq $panel.window -and $tree.agent_sessions.panel.list -eq $panel.list -and $tree.agent_sessions.panel.rows[0].id -ceq $idA) 'Composition commit/theme rebuilt controls or selected another row';Query $tree '';Passed 'owned-IME-guard-keeps-NFD-draft-through-theme-and-Escape-until-commit'

 $tree=Await {param($t) @($t.agent_sessions.panel.rows).Count -eq 2 -and -not $t.agent_sessions.loading};$tree=Select-Session $tree $idA;$currentPreview=$tree.agent_sessions.panel.preview_text
 $r=Request ($report+@('--seq','10','--status','blocked','--session-id',$idA.ToUpperInvariant()));Require ($r.accepted -and $r.agent.session_id -ceq $idA) 'Current conversation report was not canonicalized or accepted'
 $tree=Await {param($t) $t.agent_sessions.agent.session_id -ceq $idA -and -not $t.agent_sessions.resume_enabled -and -not $t.agent_sessions.panel.resume_enabled};Require ($tree.agent_sessions.selected -ceq $idA -and [string]::Equals($tree.agent_sessions.panel.preview_text,$currentPreview,[StringComparison]::Ordinal)) 'Current-session guard changed selected history or preview'
 $r=Request ($report+@('--seq','9','--status','idle','--session-id',$idB));Require (-not $r.accepted -and $r.agent.session_id -ceq $idA -and $r.agent.seq -eq 10) 'Stale report replaced the current conversation'
 foreach($invalid in @('../session','한글',('a'*129),($idA+"`n"))){Request ($report+@('--seq','11','--status','blocked','--session-id',$invalid)) 3000 $true|Out-Null}
 $r=Request ($report+@('--seq','11','--status','blocked','--session-id',$idB));Require ($r.accepted -and $r.agent.session_id -ceq $idB) 'Conversation switch in the same process was rejected'
 $tree=Await {param($t) $t.agent_sessions.agent.session_id -ceq $idB -and $t.agent_sessions.panel.resume_enabled -and -not $t.agent_sessions.loading};Require ($tree.agent_sessions.selected -ceq $idA -and [string]::Equals($tree.agent_sessions.panel.preview_text,$currentPreview,[StringComparison]::Ordinal)) 'Conversation switch did not re-enable the other selected history without changing its preview'
 $r=Request ($report+@('--seq','12','--status','blocked'));Require ($r.accepted -and $r.agent.session_id -ceq $idB) 'Metadata-free activity report discarded the known current conversation'
 $listed=@(Request @('agents')|Where-Object {$_.tab -ceq $source.id});Require ($listed.Count -eq 1 -and $listed[0].session_id -ceq $idB -and $listed[0].pid -eq $source.pid) 'Agent listing lost the process-owned current conversation'
 Request ($report+@('--seq','13','--status','blocked','--session-id',$idA))|Out-Null;$tree=Await {param($t) $t.agent_sessions.agent.session_id -ceq $idA -and -not $t.agent_sessions.panel.resume_enabled};Stable @($source,$local)|Out-Null
 Passed 'ordered-current-session-ID-switch-stale-invalid-omitted-report-and-duplicate-resume-guards-preserve-raw-preview'

 $r=Hook 'session-start' (@{'thread-id'=$idB}|ConvertTo-Json -Compress);Require ($r.accepted -and $r.session_id -ceq $idB) 'Native conversation switch failed'
 $tree=Await {param($t) $t.agent_sessions.agent.session_id -ceq $idB -and $t.agent_sessions.panel.resume_enabled};Require ([string]::Equals($tree.agent_sessions.panel.preview_text,$currentPreview,[StringComparison]::Ordinal)) 'Native hook replaced the raw Korean preview'
 $r=Hook 'session-end' (@{session_id=$idA}|ConvertTo-Json -Compress);Require (-not $r.accepted -and $r.session_id -ceq $idB) 'Late end from the previous conversation cleared the current one'
 Outside-Hook $idA
 $r=Hook 'session-start' (@{session_id=$idA}|ConvertTo-Json -Compress) $false $true 'codex' '' $false $true;Require ($r.stderr -match 'nested agent') 'Nested agent hook was attributed to its outer agent'
 Hook 'session-start' (@{session_id=$idA}|ConvertTo-Json -Compress) $false $true 'claude'|Out-Null
 Hook 'session-start' (@{session_id=$idA}|ConvertTo-Json -Compress) $false $true 'codex' $local.id|Out-Null
 foreach($payload in @('{}','{"session_id":"한글"}',('x'*65537))){Hook 'session-start' $payload $false|Out-Null}
 $r=Hook 'session-start' (@{session_id=$idA}|ConvertTo-Json -Compress) $false $false;Require ($r.stderr -match 'stdin exceeded one second') 'Unclosed hook stdin did not fail within its bounded read budget'
 $tree=Tree;$state=@($tree.surfaces|Where-Object {$_.id -ceq $source.id})[0].agent;Require ($tree.agent_sessions.agent.session_id -ceq $idB -and $state.seq -eq 13 -and $state.status -ceq 'blocked') 'Rejected hooks mutated current conversation or activity sequence'
 Passed 'native-hooks-reject-unrelated-pipe-client-nested-agent-wrong-provider-other-terminal-invalid-oversized-JSON-and-unclosed-stdin'
 $r=Hook 'session-end' (@{session_id=$idB}|ConvertTo-Json -Compress);Require ($r.accepted -and -not $r.session_id) 'Matching session end did not clear current identity'
 $tree=Await {param($t) -not $t.agent_sessions.agent.session_id -and $t.agent_sessions.panel.resume_enabled};$r=Hook 'session-end' (@{session_id=$idB}|ConvertTo-Json -Compress);Require (-not $r.accepted) 'Duplicate session end was accepted'
 $r=Hook 'session-start' (@{session_id=$idA}|ConvertTo-Json -Compress) $true $true 'codex' '' $true;Require ($r.accepted -and $r.session_id -ceq $idA) 'Owned CMD hook launch chain was rejected'
 $tree=Await {param($t) $t.agent_sessions.agent.session_id -ceq $idA -and -not $t.agent_sessions.panel.resume_enabled};$state=@($tree.surfaces|Where-Object {$_.id -ceq $source.id})[0].agent;Require ($state.seq -eq 13 -and $state.status -ceq 'blocked' -and [string]::Equals($tree.agent_sessions.panel.preview_text,$currentPreview,[StringComparison]::Ordinal)) 'Lifecycle hooks invented status or modified raw preview';Stable @($source,$local)|Out-Null
 Passed 'native-session-switch-late-and-duplicate-end-current-ID-clear-owned-shell-chain-and-live-resume-guard'

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
 Require (-not $tree.agent_bar) 'Last agent exit retained an empty Agents bar'
 Request ($report+@('--seq','10','--status','idle')) 3000 $true|Out-Null;Passed 'reported-state-follows-live-tab-moves-and-clears-on-owned-process-exit-without-false-completion'

 # Run/exit/relaunch a child agent in the same CMD Job, with no agents query.
 Request @('focus-tab',$new.id)|Out-Null;$identity=Request @('identify');$plain=@((Tree).surfaces|Where-Object {$_.id -ceq $new.id})[0]
 foreach($run in @(1,2)){
  Request @('send-keys',$identity.pane,('"'+$agentExe+'" --hold "'+$homeA+'"'))|Out-Null;Request @('send-key','Enter','--surface',$new.id)|Out-Null
  $tree=Await {param($t) $s=@($t.surfaces|Where-Object {$_.id -ceq $new.id})[0];$s.agent.source -ceq 'flowmux:proc' -and $s.agent.pid -ne $plain.pid -and @($t.agent_bar.items).Count -eq 1}
  $child=@($tree.surfaces|Where-Object {$_.id -ceq $new.id})[0].agent
  Require ($child.name -ceq 'codex' -and $child.status -ceq 'unknown' -and -not $child.seq -and -not $child.message -and -not $child.session_id) 'Child launch inherited a previous process activity or sequence'
  Require (@($tree.agent_bar.items).Count -eq 1 -and $tree.agent_bar.items[0].surface -ceq $new.id) 'New child agent did not restore its Agents bar'
  $r=Request @('report-agent','codex','--surface',$new.id,'--pid',[string]$child.pid,'--seq','1','--status','blocked','--message','새 실행 한');Require ($r.accepted -and $r.agent.status -ceq 'blocked') 'Newly detected process rejected its first report'
  $image=[Diagnostics.Process]::GetProcessById([int]$child.pid)
  try{Require (Path-Same $image.MainModule.FileName $agentExe) 'Refusing to stop an unrelated child';$image.Kill();Require ($image.WaitForExit(2000)) 'Owned child exit exceeded2s'}finally{$image.Dispose()}
  $tree=Await {param($t) $s=@($t.surfaces|Where-Object {$_.id -ceq $new.id})[0];$row=@($t.chrome.controls|Where-Object {$_.kind -ceq 'workspace' -and $_.workspace -ceq $identity.workspace})[0];-not $s.agent -and @($row.workspace_lines|Where-Object {$_.agent}).Count -eq 0 -and -not $t.agent_bar}
  Stable @($plain,$local)|Out-Null
 }
 Passed 'automatic-owned-child-launch-exit-and-relaunch-clear-activity-with-same-terminal-PID-view-and-session'
 }

}
catch{$failure=$_.Exception.Message}
finally{
 $cleaning=$true
 if($owned){try{if(-not $owned.HasExited){Request @('quit','--discard-state')|Out-Null;Require ($owned.WaitForExit(4000)) 'Owned host quit timed out'};Require ($owned.ExitCode -eq 0) 'Owned host failed'}catch{$cleanupErrors+=,$_.Exception.Message}finally{try{if(-not $owned.HasExited){$owned.Kill();[CliProbe]::WaitAfterKill($owned)};$diagnostic.host=@{pid=$owned.Id;exitCode=$owned.ExitCode;stdout=[CliProbe]::Output($hostOut);stderr=[CliProbe]::Output($hostErr)}}catch{$cleanupErrors+=,$_.Exception.Message}finally{$owned.Dispose()}}}
 $env:LOCALAPPDATA=$savedLocal
}
if($failure -or $cleanupErrors.Count){$diagnostic.failure=$failure;$diagnostic.cleanupErrors=$cleanupErrors;$diagnostic.elapsedMs=$clock.ElapsedMilliseconds;$diagnostic|ConvertTo-Json -Depth 60|Set-Content -Encoding UTF8 -LiteralPath (Join-Path $directory 'failure.json');throw ($failure+' '+($cleanupErrors -join '; '))}
Write-Output ('passed: '+$checks.Count+' hidden agent-session groups; elapsed='+$clock.ElapsedMilliseconds+'ms')
