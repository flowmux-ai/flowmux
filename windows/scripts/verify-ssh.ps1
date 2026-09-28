# SPDX-License-Identifier: GPL-3.0-or-later
# Own hidden windows and an isolated loopback Git sshd; run under run-check.ps1 with a 100s cap.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",[Alias('Host')][string]$SshHost='',[int]$Port=22,[string]$IdentityFile='',[string]$ConfigFile='',[string]$RemoteDirectory='')
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path;$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
Add-Type -ReferencedAssemblies System.Drawing -Path (Join-Path $PSScriptRoot 'ChromeFixture.cs')
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'EditorFixture.cs'),(Join-Path $PSScriptRoot 'OptionsFixture.cs'),(Join-Path $PSScriptRoot 'FindFixture.cs'),(Join-Path $PSScriptRoot 'PaneToolsFixture.cs')
$base=if($env:FLOWMUX_TEST_ARTIFACT_ROOT){$env:FLOWMUX_TEST_ARTIFACT_ROOT}else{[IO.Path]::GetTempPath()}
$directory=Join-Path $base ('ssh-한글-한-'+[guid]::NewGuid());[IO.Directory]::CreateDirectory($directory)|Out-Null
$fixture=New-Object EditorFixture($directory);$clock=[Diagnostics.Stopwatch]::StartNew();$hostProcess=$null;$server=$null;$out=$null;$err=$null;$serverOut=$null;$serverErr=$null;$pipeName=$null;$cleanup=$false;$failure=$null;$checks=0;$proofMarkers=@{}
$diagnostic=[ordered]@{mode='owned-hidden-real-SSH';physicalInput=$false;physicalIme=$false;checks=@();lastTree=$null}
function Require([bool]$Value,[string]$Message){if(-not $Value){throw $Message}}
function Same([string]$A,[string]$B){return [string]::Equals($A,$B,[StringComparison]::Ordinal)}
function Budget([int]$Max=5000){if($cleanup){return $Max};$left=90000-$clock.ElapsedMilliseconds;Require ($left -gt 0) 'SSH check exceeded 90 seconds';return [int][Math]::Min($Max,$left)}
function Passed([string]$Name){$script:checks++;$diagnostic.checks+=,$Name}
function Run([string]$File,[string[]]$Arguments,[int]$Exit=0,[int]$Max=5000){
 $diagnostic.lastCommand=@{file=$File;arguments=$Arguments};$p=[CliProbe]::Start($File,$Arguments,$directory,$directory);$o=$p.StandardOutput.ReadToEndAsync();$e=$p.StandardError.ReadToEndAsync()
 try{Require ($p.WaitForExit((Budget $Max))) ('Owned process deadline: '+$File+' '+($Arguments -join ' '));Require ($o.Wait(500)-and $e.Wait(500)) 'Owned output pipes did not close';$stdout=[CliProbe]::Output($o);$stderr=[CliProbe]::Output($e);Require ($p.ExitCode -eq $Exit) ('Unexpected exit '+$p.ExitCode+': '+$stderr+' '+$stdout);if($Exit){return $stderr};return $stdout}
 finally{if(-not $p.HasExited){$p.Kill();[CliProbe]::WaitAfterKill($p)};$p.Dispose()}
}
function Request([string[]]$Arguments,[int]$Exit=0,[int]$Max=5000){Require ([bool]$pipeName) 'Explicit owned pipe required';return ((Run $cli (@('--pipe',$pipeName,'--json')+$Arguments) $Exit $Max)|ConvertFrom-Json)}
function Tree([int]$Max=5000){$t=Request @('tree') 0 $Max;Require ($t.background_testing) 'Hidden debug host required';[ChromeFixture]::Size([long]$t.window_handle,$hostProcess.Id)|Out-Null;$diagnostic.lastTree=$t;return $t}
function Await([scriptblock]$Condition){$watch=[Diagnostics.Stopwatch]::StartNew();do{$left=5000-$watch.ElapsedMilliseconds;Require ($left -ge 100) 'SSH condition exceeded five seconds';$t=Tree ([int]$left);if(& $Condition $t){return $t};Start-Sleep -Milliseconds 20}while($true)}
function Stable($Tree,$Before){foreach($s in $Before){$n=@($Tree.surfaces|Where-Object {$_.id -ceq $s.id});Require ($n.Count -eq 1 -and $n[0].pid -eq $s.pid -and $n[0].view_handle -eq $s.view_handle -and $n[0].running) 'SSH UI replaced an existing terminal PID or WebView'}}
function Start-Host([string[]]$Arguments,[bool]$AllowDisconnected=$false){
 $utc=[DateTime]::UtcNow;$timer=[Diagnostics.Stopwatch]::StartNew();$script:pipeName=$null;$script:hostProcess=[CliProbe]::Start($gui,$Arguments,$directory,$directory);$script:out=$hostProcess.StandardOutput.ReadToEndAsync();$script:err=$hostProcess.StandardError.ReadToEndAsync();$recordPath=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($hostProcess.Id).json"
 do{Budget|Out-Null;Require (-not $hostProcess.HasExited -and $timer.ElapsedMilliseconds -lt 8000) 'Hidden SSH host startup exceeded eight seconds';if((Test-Path $recordPath)-and (Get-Item $recordPath).LastWriteTimeUtc -ge $utc){$r=Get-Content -Raw $recordPath|ConvertFrom-Json;Require ($r.pid -eq $hostProcess.Id) 'Wrong host discovery PID';$script:pipeName=$r.pipe;break};Start-Sleep -Milliseconds 20}while($true)
 return Await {param($t) @($t.surfaces).Count -gt 0 -and @($t.surfaces|Where-Object {-not $_.ready -or (-not $AllowDisconnected -and (-not $_.running -or -not $_.pid))}).Count -eq 0}
}
function Stop-Host{
 Request @('quit','--discard-state')|Out-Null;Require ($hostProcess.WaitForExit((Budget 5000))) 'Hidden host quit exceeded five seconds';Require ($hostProcess.ExitCode -eq 0) 'Hidden host exit failed';Require ($out.Wait(500)-and $err.Wait(500)) 'Host pipes did not close';$hostProcess.Dispose();$script:hostProcess=$null;$script:pipeName=$null
}
function Open-Dialog{
 $t=Tree;$buttons=@($t.chrome.controls|Where-Object {$_.kind -ceq 'workspace_header' -and $_.layout_visible});Require ($buttons.Count -eq 1) 'Missing workspace creation button';[OptionsFixture]::Click([long]$t.window_handle,[long]$buttons[0].handle,$hostProcess.Id)
 $t=Await {param($v) $v.tab_menu.kind -ceq 'creation'};$menu=$t.tab_menu.menu;$rows=@($menu.rows|Where-Object {$_.label -ceq 'New SSH Workspace'});Require ($rows.Count -eq 1 -and $rows[0].enabled) 'New SSH Workspace is unavailable';[OptionsFixture]::ClickMenu([long]$menu.window,[long]$rows[0].window,$hostProcess.Id)
 $t=Await {param($v) $v.ssh_dialog -and -not $v.tab_menu};return $t.ssh_dialog
}

function Field($Panel,[string]$Key){$row=@($Panel.fields|Where-Object {$_.key -ceq $Key});Require ($row.Count -eq 1) ('Missing SSH field '+$Key);return [long]$row[0].input}
function Fill($Panel,[hashtable]$Values){foreach($key in $Values.Keys){[OptionsFixture]::SetText([long]$Panel.window,(Field $Panel $key),$hostProcess.Id,[string]$Values[$key])}}
function Submit($Panel){[OptionsFixture]::ClickMenu([long]$Panel.window,[long]$Panel.connect,$hostProcess.Id)}
function Reject($Panel,$Before,[hashtable]$Values){Fill $Panel $Values;Submit $Panel;$t=Await {param($v) $v.ssh_dialog.id -ceq $Panel.id -and $v.ssh_dialog.error -and -not $v.ssh_dialog.pending};Require (@($t.workspaces).Count -eq 1 -and @($t.surfaces).Count -eq 1) 'Invalid SSH form created a workspace';foreach($key in $Values.Keys){Require (Same ([OptionsFixture]::Text((Field $Panel $key),$hostProcess.Id)) ([string]$Values[$key])) ('Validation changed raw '+$key)};Stable $t $Before;return $t}
function Posix([string]$Path){Require ($Path -match '^[A-Za-z]:\\') 'Fixture requires a local Windows drive path';return '/'+$Path.Substring(0,1).ToLowerInvariant()+$Path.Substring(2).Replace('\','/')}
function Write-Utf8([string]$Path,[string[]]$Lines){[IO.File]::WriteAllLines($Path,$Lines,(New-Object Text.UTF8Encoding($false)))}
function Configure-Server{
 $git=Join-Path $env:ProgramFiles 'Git\usr\bin';$sshd=Join-Path $git 'sshd.exe';$keygen=Join-Path $env:WINDIR 'System32\OpenSSH\ssh-keygen.exe';Require ((Test-Path $sshd)-and (Test-Path $keygen)) 'Actual SSH prerequisite missing: Git sshd and Windows OpenSSH keygen are required'
 $user=(Run (Join-Path $git 'id.exe') @('-un')).Trim();$hostKey=Join-Path $directory 'host-key';$script:IdentityFile=Join-Path $directory 'identity 한글 한';$script:ConfigFile=Join-Path $directory 'config 한글 한'
 Run $keygen @('-q','-t','ed25519','-f',$hostKey,'-N','')|Out-Null;Run $keygen @('-q','-t','ed25519','-f',$IdentityFile,'-N','')|Out-Null;[IO.File]::Copy($IdentityFile+'.pub',(Join-Path $directory 'authorized_keys'))
 $listener=New-Object Net.Sockets.TcpListener([Net.IPAddress]::Loopback,0);try{$listener.Start();$script:Port=$listener.LocalEndpoint.Port}finally{$listener.Stop()}
 $known=Join-Path $directory 'known_hosts';$global=Join-Path $directory 'global_known_hosts';$pub=([IO.File]::ReadAllText($hostKey+'.pub')).Trim().Split(' ');Write-Utf8 $known @('[127.0.0.1]:'+$Port+' '+$pub[0]+' '+$pub[1]);Write-Utf8 $global @()
 Write-Utf8 $ConfigFile @('Host *','  BatchMode yes','  IdentitiesOnly yes','  StrictHostKeyChecking yes','  ConnectTimeout 3',('  UserKnownHostsFile "'+$known.Replace('\','/')+'"'),('  GlobalKnownHostsFile "'+$global.Replace('\','/')+'"'))
 $serverConfig=Join-Path $directory 'sshd_config';Write-Utf8 $serverConfig @("Port $Port",'ListenAddress 127.0.0.1',('HostKey "'+(Posix $hostKey)+'"'),('PidFile "'+(Posix (Join-Path $directory 'sshd.pid'))+'"'),('AuthorizedKeysFile "'+(Posix (Join-Path $directory 'authorized_keys'))+'"'),'StrictModes no','PasswordAuthentication no','KbdInteractiveAuthentication no','PubkeyAuthentication yes','UseDNS no','LogLevel ERROR','PrintLastLog no','PrintMotd no','AllowTcpForwarding no','X11Forwarding no')
 $script:server=[CliProbe]::Start($sshd,@('-D','-e','-f',(Posix $serverConfig)),$directory,$directory);$script:serverOut=$server.StandardOutput.ReadToEndAsync();$script:serverErr=$server.StandardError.ReadToEndAsync();$timer=[Diagnostics.Stopwatch]::StartNew();$listening=$false
 do{Require (-not $server.HasExited -and $timer.ElapsedMilliseconds -lt 4000) 'Owned loopback sshd did not listen within four seconds';$tcp=New-Object Net.Sockets.TcpClient;try{$task=$tcp.ConnectAsync('127.0.0.1',$Port);if($task.Wait(100)-and $tcp.Connected){$listening=$true}}catch{}finally{$tcp.Dispose()};if(-not $listening){Start-Sleep -Milliseconds 20}}until($listening)
 $script:SshHost=$user+'@127.0.0.1';$script:RemoteDirectory=Posix $fixture.Root
}
function Remote-Proof([string]$Surface,[string]$Pane,[string]$ExpectedDirectory=$RemoteDirectory){
 $prefix='SSH_'+[guid]::NewGuid().ToString('N').Substring(0,8)+' ';$unicode='한글 한 😀';$marker=$prefix+$unicode;$bytes=[Text.Encoding]::UTF8.GetBytes($prefix);$octal=($bytes|ForEach-Object {'\'+[Convert]::ToString($_,8).PadLeft(3,'0')}) -join ''; $cwdMarker='CWD_'+[guid]::NewGuid().ToString('N')+':'
 # Neither expected full marker occurs in echoed command input.
 $cwdOctal=([Text.Encoding]::UTF8.GetBytes($cwdMarker)|ForEach-Object {'\'+[Convert]::ToString($_,8).PadLeft(3,'0')}) -join ''
 Request @('focus-tab',$Surface)|Out-Null;Request @('send-keys',$Pane,('printf '''+$octal+'%s\n'' '''+$unicode+'''; printf '''+$cwdOctal+'%s\n'' "$PWD"'))|Out-Null;Request @('send-key','Enter','--pane',$Pane)|Out-Null
 $timer=[Diagnostics.Stopwatch]::StartNew();do{$left=5000-$timer.ElapsedMilliseconds;Require ($left -ge 100) 'Actual SSH remote marker/CWD exceeded five seconds';$screen=Request @('read-screen','--surface',$Surface) 0 ([int]$left);$diagnostic.lastScreen=$screen.text;$flat=$screen.text.Replace("`r",'').Replace("`n",'');if($flat.Contains($marker)-and $flat.Contains($cwdMarker+$ExpectedDirectory)){break};Start-Sleep -Milliseconds 20}while($true)
 $t=Tree;$s=@($t.surfaces|Where-Object {$_.id -ceq $Surface});Require ($s.Count -eq 1 -and $s[0].running -and $s[0].pid -and [IO.Path]::GetFileNameWithoutExtension($s[0].shell.program) -ieq 'ssh' -and (Same $s[0].remote_cwd $ExpectedDirectory)) 'Remote proof lacks a running OpenSSH ConPTY or exact remote directory';$script:proofMarkers[$Surface]=$marker;return $t
}
function Toolbar($Tree,[string]$Workspace,[string]$State){
 $bar=$Tree.ssh_toolbar;Require ($bar -and $bar.workspace -ceq $Workspace -and $bar.state -ceq $State) ('SSH toolbar has wrong workspace/state; expected '+$State)
 foreach($key in @('status','connect','disconnect','authentication','ports')){Require ([long]$bar.controls.$key -ne 0 -and [OptionsFixture]::Parent([long]$bar.controls.$key,$hostProcess.Id) -eq $Tree.window_handle) ('SSH toolbar control has wrong native owner: '+$key)}
 Require (-not [OptionsFixture]::Describe([long]$bar.controls.ports,$hostProcess.Id).Enabled) 'Unsupported SSH Ports action is enabled'
 $connect=[OptionsFixture]::Describe([long]$bar.controls.connect,$hostProcess.Id);$disconnect=[OptionsFixture]::Describe([long]$bar.controls.disconnect,$hostProcess.Id)
 if($State -ceq 'connected'){Require (-not $connect.Enabled -and $disconnect.Enabled) 'Connected SSH toolbar has wrong native action availability'}
 if($State -ceq 'disconnected'){Require ($connect.Enabled -and -not $disconnect.Enabled) 'Disconnected SSH toolbar has wrong native action availability'}
 $caption=[OptionsFixture]::Text([long]$bar.controls.status,$hostProcess.Id);Require ($caption.Contains($SshHost) -and $caption.Contains(' · '+$State)) 'Actual SSH status HWND omits its destination or connection state'
 $native=@([ChromeFixture]::Read([long]$Tree.window_handle,$hostProcess.Id));$size=[ChromeFixture]::Size([long]$Tree.window_handle,$hostProcess.Id);$rects=@()
 $tabs=@($Tree.chrome.controls|Where-Object {$_.kind -ceq 'tab' -and $_.layout_visible});Require ($tabs.Count -gt 0) 'SSH toolbar geometry requires a visible pane tab row';$tabTop=($tabs|ForEach-Object {$_.rect.y}|Measure-Object -Minimum).Minimum
 foreach($key in @('status','connect','disconnect','authentication','ports')){$actual=@($native|Where-Object {$_.Handle -eq [long]$bar.controls.$key});Require ($actual.Count -eq 1 -and $actual[0].Shown -and $actual[0].Font -ne 0) ('SSH toolbar native control is absent, hidden or lacks its font: '+$key);$r=$actual[0];Require ($r.Width -gt 0 -and $r.Height -gt 0 -and $r.X -ge $Tree.chrome.sidebar_actual_width -and $r.Y -ge 0 -and $r.X+$r.Width -le $size[0] -and $r.Y+$r.Height -le $size[1] -and $r.Y+$r.Height -le $tabTop) ('SSH toolbar escapes the client/sidebar or overlaps the pane tab row: '+$key);$rects+=,$r}
 for($i=0;$i -lt $rects.Count;$i++){for($j=$i+1;$j -lt $rects.Count;$j++){$a=$rects[$i];$b=$rects[$j];Require (-not ($a.X -lt $b.X+$b.Width -and $b.X -lt $a.X+$a.Width -and $a.Y -lt $b.Y+$b.Height -and $b.Y -lt $a.Y+$a.Height)) 'SSH toolbar controls overlap'}}
 return $bar
}
function Toolbar-Paint($Tree,$Bar){
 # Offscreen production STATIC/BUTTON painters only; no desktop or WebView capture.
 $path=Join-Path $directory 'ssh-toolbar.bmp';$capture=Request @('chrome-capture',$path);Require ($capture.root_handle -eq $Tree.window_handle -and -not $capture.subtree) 'SSH toolbar capture selected a different native root'
 $native=@([ChromeFixture]::Read([long]$Tree.window_handle,$hostProcess.Id))
 foreach($key in @('status','connect','disconnect','authentication','ports')){$r=@($native|Where-Object {$_.Handle -eq [long]$Bar.controls.$key})[0];$drawn=@($capture.controls|Where-Object {$_.handle -eq [long]$Bar.controls.$key});Require ($drawn.Count -eq 1 -and $drawn[0].x -eq $r.X -and $drawn[0].y -eq $r.Y -and $drawn[0].width -eq $r.Width -and $drawn[0].height -eq $r.Height) ('Actual SSH control was not included at its live bounds: '+$key);$x=$r.X+4;$y=$r.Y+4;$width=$r.Width-8;$height=$r.Height-8;Require ($width -gt 0 -and $height -gt 0 -and [ChromeFixture]::Pixel($path,$r.X,$r.Y) -ceq $capture.background -and [ChromeFixture]::ColorCount($path,$x,$y,$width,$height,$capture.background) -lt $width*$height-5) ('Production SSH native painter omitted control text: '+$key)}
 Remove-Item -LiteralPath $path -Force
}
function Retained-History([string]$Surface,[string]$Marker){
 Require ($Marker -and $Marker.Contains('한글 한 😀')) 'History assertion requires a previously acknowledged unique Unicode marker'
 $screen=Request @('read-screen','--surface',$Surface,'--recent');$flat=$screen.text.Replace("`r",'').Replace("`n",'');Require ($flat.Contains($Marker)) 'SSH disconnect/reconnect erased acknowledged Unicode terminal history'
}
function Check-Config($Tree,[string]$Workspace,[string]$Name){$ws=@($Tree.workspaces|Where-Object {$_.id -ceq $Workspace});Require ($ws.Count -eq 1 -and $ws[0].ssh -and (Same $ws[0].name $Name) -and (Same $ws[0].ssh.cwd $RemoteDirectory) -and (Same $ws[0].ssh.target.identity_file $IdentityFile) -and (Same $ws[0].ssh.target.config_file $ConfigFile) -and $ws[0].ssh.target.port -eq $Port -and -not $ws[0].ssh.tmux) 'SSH config/name paths lost original Unicode or connection options';return $ws[0]}
try{
 $doctor=(Run $cli @('doctor'))|ConvertFrom-Json;Require ($doctor.status -ceq 'ok' -and $doctor.background_testing) 'A hidden debug SSH build is required'
 if(-not $SshHost){Configure-Server}else{Require ($IdentityFile -and $ConfigFile -and $RemoteDirectory.StartsWith('/')) 'External fixture requires identity, pinned noninteractive config and absolute remote directory'}
 $tree=Start-Host @('--shell=cmd','--cwd',$fixture.Root);$original=@($tree.surfaces);$local=Request @('identify');$panel=Open-Dialog;$native=[OptionsFixture]::Describe([long]$panel.window,$hostProcess.Id)
 Require ($panel.owner -eq $tree.window_handle -and $native.Owner -eq $tree.window_handle -and $native.Enabled -and -not $native.OwnerEnabled -and -not $panel.native_visible -and [Math]::Abs($native.Width-460*$native.Dpi/96.0) -le 1) 'SSH dialog is not the hidden owned 460-DIP modal'
 Require ((@($panel.fields.label)-join '|') -ceq 'Host|Remote directory|Workspace name|Port|Identity file|SSH config file' -and @($panel.fields).Count -eq 6) 'SSH dialog field structure differs from Linux'
 $bottom=0;$size=[ChromeFixture]::Size([long]$panel.window,$hostProcess.Id);foreach($row in $panel.fields){$b=[OptionsFixture]::RelativeBounds([long]$panel.window,[long]$row.input,$hostProcess.Id);Require ($b.Y -ge $bottom -and $b.X -ge 0 -and $b.X+$b.Width -le $size[0] -and $b.Y+$b.Height -le $size[1]) 'SSH input leaves its panel or overlaps another field';$bottom=$b.Y+$b.Height}
 $name='원격 한글 한 é 😀';Reject $panel $original @{host='-oProxyCommand=invalid';name=$name}|Out-Null;Reject $panel $original @{host=$SshHost;port='0';name=$name}|Out-Null;Reject $panel $original @{port=[string]$Port;cwd='C:\not-posix';name=$name}|Out-Null;Passed 'owned-six-field-layout-and-invalid-host-port-POSIX-CWD-preserve-raw-Unicode-and-existing-PID'
 Fill $panel @{host=$SshHost;cwd=$RemoteDirectory;name=$name;port=[string]$Port;identity=$IdentityFile;config=$ConfigFile};$input=Field $panel 'host'
 [OptionsFixture]::CompositionGuard([long]$panel.window,$input,$hostProcess.Id,$true);[OptionsFixture]::PostEnter($input,$hostProcess.Id);$tree=Tree;Require ($tree.ssh_dialog.id -ceq $panel.id -and $tree.ssh_dialog.composing -and @($tree.workspaces).Count -eq 1) 'Composing Enter submitted SSH';[OptionsFixture]::CompositionGuard([long]$panel.window,$input,$hostProcess.Id,$false)
 [OptionsFixture]::KeyMessage($input,$hostProcess.Id,229,0,$false,$false,$false,$false);[OptionsFixture]::PostEnter($input,$hostProcess.Id);$tree=Tree;Require ($tree.ssh_dialog.id -ceq $panel.id -and @($tree.workspaces).Count -eq 1) 'PROCESS/229 Enter submitted SSH'
 [OptionsFixture]::KeyMessage($input,$hostProcess.Id,229,0,$true,$false,$false,$false);[OptionsFixture]::PostKey($input,$hostProcess.Id,13,$false,$true);[OptionsFixture]::PostKey($input,$hostProcess.Id,13,$true,$false);$tree=Tree;Require ($tree.ssh_dialog.id -ceq $panel.id -and @($tree.workspaces).Count -eq 1) 'Repeated Enter submitted SSH';Stable $tree $original
 [FindFixture]::PostClose([long]$panel.window,$hostProcess.Id);$tree=Await {param($t) -not $t.ssh_dialog};Require ([OptionsFixture]::Describe([long]$tree.window_handle,$hostProcess.Id).Enabled) 'Closing SSH form left owner disabled';Stable $tree $original;Passed 'owned-composition-PROCESS-repeat-guards-and-window-close-cancel'
 $panel=Open-Dialog;Fill $panel @{host=$SshHost;cwd=$RemoteDirectory;name=$name;port=[string]$Port;identity=$IdentityFile;config=$ConfigFile};Submit $panel
 $tree=Await {param($t) -not $t.ssh_dialog -and @($t.workspaces).Count -eq 2 -and @($t.surfaces).Count -eq 2 -and @($t.surfaces|Where-Object {-not $_.ready -or -not $_.running}).Count -eq 0};$remote=Request @('identify');Check-Config $tree $remote.workspace $name|Out-Null;$tree=Remote-Proof $remote.surface $remote.pane;Stable $tree $original;Passed 'native-Connect-starts-real-SSH-ConPTY-with-remote-Unicode-and-CWD-output'
 $changedDirectory=$RemoteDirectory
 if($server){
  $changed=Join-Path $fixture.Root '이동 한';[IO.Directory]::CreateDirectory($changed)|Out-Null;$changedDirectory=Posix $changed
  $encoded=($changedDirectory.Split('/')|ForEach-Object {[Uri]::EscapeDataString($_)}) -join '/'
  Request @('send-keys',$remote.pane,('cd '''+$changedDirectory+'''; printf ''\033]7;%s\007'' ''file://flowmux-check'+$encoded+''''))|Out-Null;Request @('send-key','Enter','--pane',$remote.pane)|Out-Null
  Await {param($t) @($t.surfaces|Where-Object {$_.id -ceq $remote.surface -and (Same $_.remote_cwd $changedDirectory)}).Count -eq 1}|Out-Null
  Remote-Proof $remote.surface $remote.pane $changedDirectory|Out-Null
 }
 Request @('new-tab')|Out-Null;$tab=Request @('identify');Require ($tab.workspace -ceq $remote.workspace -and $tab.pane -ceq $remote.pane -and $tab.surface -cne $remote.surface) 'SSH new-tab lost workspace or pane';Remote-Proof $tab.surface $tab.pane $changedDirectory|Out-Null
 Request @('split','vertical')|Out-Null;$split=Request @('identify');Require ($split.workspace -ceq $remote.workspace -and $split.pane -cne $tab.pane) 'SSH split lost workspace or failed to split';$tree=Remote-Proof $split.surface $split.pane $changedDirectory;Require (@($tree.surfaces.pid|Select-Object -Unique).Count -eq 4) 'SSH tabs did not receive distinct processes';Stable $tree $original;Passed 'new-tab-and-split-reuse-SSH-config-and-execute-in-exact-remote-CWD'
 $before=@($tree.surfaces);$shape=$tree.workspaces|ConvertTo-Json -Depth 40 -Compress;$rejected=Request @('move-tab',$remote.surface,'--to-pane',$local.pane) 1;Require ([bool]$rejected.error) 'Moving SSH tab into local workspace was accepted';$detached=Request @('detach-tab',$remote.surface) 1;Require ([bool]$detached.error) 'Detaching an SSH tab was accepted';$tree=Tree;Require (@($tree.detached_windows).Count -eq 0) 'Rejected SSH tear-out created a detached window';Require (Same ($tree.workspaces|ConvertTo-Json -Depth 40 -Compress) $shape) 'Rejected SSH/local move or tear-out mutated topology';Stable $tree $before;Passed 'SSH-local-workspace-move-rejected-without-session-loss'
 # Disconnect stops every SSH session in this workspace without replacing its retained views.
 Request @('focus-tab',$split.surface)|Out-Null;$tree=Await {param($t) $t.ssh_toolbar.workspace -ceq $remote.workspace -and $t.ssh_toolbar.state -ceq 'connected'};$bar=Toolbar $tree $remote.workspace 'connected';Toolbar-Paint $tree $bar
 $sshBefore=@($tree.surfaces|Where-Object {$_.id -in @($remote.surface,$tab.surface,$split.surface)});Require ($sshBefore.Count -eq 3) 'Reconnect requires three real SSH sessions';$oldPids=@($sshBefore.pid);$history=@{};foreach($s in $sshBefore){$history[$s.id]=$proofMarkers[$s.id];Retained-History $s.id $history[$s.id]}
 $workspaceBefore=@($tree.workspaces|Where-Object {$_.id -ceq $remote.workspace})[0];$shape=$workspaceBefore.root|ConvertTo-Json -Depth 40 -Compress
 [OptionsFixture]::ClickMenu([long]$tree.window_handle,[long]$bar.controls.disconnect,$hostProcess.Id)
 $tree=Await {param($t) $t.ssh_toolbar.state -ceq 'disconnected' -and @($t.surfaces|Where-Object {$_.id -in @($remote.surface,$tab.surface,$split.surface) -and $_.running}).Count -eq 0 -and @(Get-Process -Id $oldPids -ErrorAction SilentlyContinue).Count -eq 0};$bar=Toolbar $tree $remote.workspace 'disconnected';Stable $tree $original
 $workspaceAfter=@($tree.workspaces|Where-Object {$_.id -ceq $remote.workspace})[0];Require (Same ($workspaceAfter.root|ConvertTo-Json -Depth 40 -Compress) $shape) 'Disconnect changed pane/tab identities or remote CWD'
 foreach($s in $sshBefore){$now=@($tree.surfaces|Where-Object {$_.id -ceq $s.id});Require ($now.Count -eq 1 -and $now[0].view_handle -eq $s.view_handle -and $now[0].holder.window -eq $s.holder.window -and (Same $now[0].remote_cwd $changedDirectory)) 'Disconnect replaced a retained SSH view, holder or CWD';Retained-History $s.id $history[$s.id]}
 Request @('new-tab')|Out-Null;$offline=Request @('identify');$tree=Tree;Require ($offline.workspace -ceq $remote.workspace -and $offline.pane -ceq $split.pane -and @($tree.surfaces).Count -eq 5 -and @($tree.surfaces|Where-Object {$_.id -cne $local.surface -and $_.running}).Count -eq 0) 'Creating a disconnected SSH tab spawned a connection or changed its source';Toolbar $tree $remote.workspace 'disconnected'|Out-Null;Stable $tree $original
 Passed 'native-toolbar-caption-geometry-paint-Disconnect-stops-SSH-PIDs-preserves-local-history-and-offline-new-tab'
 $bar=$tree.ssh_toolbar;[OptionsFixture]::ClickMenu([long]$tree.window_handle,[long]$bar.controls.connect,$hostProcess.Id)
 $tree=Await {param($t) @($t.surfaces).Count -eq 5 -and @($t.surfaces|Where-Object {-not $_.ready -or -not $_.running -or -not $_.pid}).Count -eq 0};Stable $tree $original
 foreach($s in $sshBefore){$now=@($tree.surfaces|Where-Object {$_.id -ceq $s.id})[0];Require ($now.pid -ne $s.pid -and $now.view_handle -eq $s.view_handle -and $now.holder.window -eq $s.holder.window) 'Reconnect did not replace only the SSH process';Retained-History $s.id $history[$s.id]}
 foreach($identity in @($remote,$tab,$split,$offline)){Remote-Proof $identity.surface $identity.pane $changedDirectory|Out-Null}
 $tree=Await {param($t) $t.ssh_toolbar.state -ceq 'connected'};$bar=Toolbar $tree $remote.workspace 'connected';$connected=@($tree.surfaces);Require (@($connected.pid|Select-Object -Unique).Count -eq 5) 'Reconnect duplicated an SSH process identity';Check-Config $tree $remote.workspace $name|Out-Null
 # Disabled native Connect still receives an owned stale WM_COMMAND; it must be harmless.
 [PaneToolsFixture]::Command($hostProcess,[long]$tree.window_handle,[long]$bar.controls.connect);[PaneToolsFixture]::Command($hostProcess,[long]$tree.window_handle,[long]$bar.controls.connect);$tree=Tree;Stable $tree $connected;Require (@($tree.surfaces).Count -eq 5) 'Repeated Connect added sessions';Passed 'native-Connect-reuses-surface-identities-Unicode-history-remote-CWD-and-ignores-repeat'
 $saved=Request @('save-state');$state=[IO.File]::ReadAllText($saved.path,[Text.Encoding]::UTF8)|ConvertFrom-Json;Check-Config $state $remote.workspace $name|Out-Null;Stop-Host
 $tree=Start-Host @('--restore-window',$saved.window) $true;Check-Config $tree $remote.workspace $name|Out-Null;Require (@($tree.surfaces).Count -eq 5 -and @($tree.surfaces|Where-Object {$_.id -in @($remote.surface,$tab.surface,$split.surface,$offline.surface)}).Count -eq 4 -and @($tree.surfaces|Where-Object {$_.id -cne $local.surface -and $_.running}).Count -eq 0 -and @($tree.surfaces|Where-Object {$_.id -ceq $local.surface -and $_.running}).Count -eq 1) 'SSH restore did not preserve identities in the disconnected state'
 Request @('focus-tab',$remote.surface)|Out-Null;$tree=Tree;$bar=Toolbar $tree $remote.workspace 'disconnected';[OptionsFixture]::ClickMenu([long]$tree.window_handle,[long]$bar.controls.connect,$hostProcess.Id);$tree=Await {param($t) @($t.surfaces|Where-Object {-not $_.running -or -not $_.ready}).Count -eq 0};Remote-Proof $remote.surface $remote.pane $changedDirectory|Out-Null;Passed 'save-restore-preserves-NFD-config-paths-stays-disconnected-until-native-Connect-and-executes-remotely'

 Stop-Host
}catch{$failure=$_.Exception.Message}
finally{
 $cleanup=$true
 if($hostProcess){try{if(-not $hostProcess.HasExited -and $pipeName){Request @('quit','--discard-state')|Out-Null;$null=$hostProcess.WaitForExit(3000)}}catch{if(-not $failure){$failure='Cleanup: '+$_.Exception.Message}}
  try{if(-not $hostProcess.HasExited){$hostProcess.Kill();[CliProbe]::WaitAfterKill($hostProcess);if(-not $failure){$failure='Host required forced cleanup'}};$null=$out.Wait(500);$null=$err.Wait(500);$diagnostic.hostStdout=[CliProbe]::Output($out);$diagnostic.hostStderr=[CliProbe]::Output($err)}catch{if(-not $failure){$failure=$_.Exception.Message}}finally{$hostProcess.Dispose()}}
 if($server){try{if(-not $server.HasExited){$server.Kill();[CliProbe]::WaitAfterKill($server)};Require ($serverOut.Wait(500)-and $serverErr.Wait(500)) 'sshd output pipes did not close';$diagnostic.serverStderr=[CliProbe]::Output($serverErr)}catch{if(-not $failure){$failure='sshd cleanup: '+$_.Exception.Message}}finally{$server.Dispose()}}
 try{$fixture.Dispose()}catch{if(-not $failure){$failure='Fixture cleanup: '+$_.Exception.Message}}
}
if($failure){$diagnostic.failure=$failure;$diagnostic.elapsedMs=$clock.ElapsedMilliseconds;$diagnostic|ConvertTo-Json -Depth 60|Set-Content -Encoding UTF8 -LiteralPath (Join-Path $directory 'failure.json');throw $failure}
Remove-Item -LiteralPath $directory -Recurse -Force
Write-Output ("passed: $checks hidden SSH groups; actual loopback/external SSH Unicode and remote-CWD markers verified; elapsed="+$clock.ElapsedMilliseconds+'ms')
