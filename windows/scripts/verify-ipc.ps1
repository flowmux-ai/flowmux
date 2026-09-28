# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden hosts only. No desktop input, visible windows, or persistent user state.
param([string]$BuildDirectory = "$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",[ValidateSet('existing','long-path')][string]$Case='existing')
if (-not $env:FLOWMUX_TEST_ARTIFACT_ROOT) { throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.' }
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path
$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor=(& $cli doctor | ConvertFrom-Json)
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) { throw 'A working debug build is required; no host was launched.' }
if($Case -eq 'long-path') {
    $clock=[Diagnostics.Stopwatch]::StartNew();$process=$null;$pipeName=$null;$hostOut=$null;$hostErr=$null;$failure=$null;$cleanupErrors=@();$savedLocal=$env:LOCALAPPDATA;$fixture=$null;$lastTree=$null
    $directory=Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT ('ipc-long-path-'+[guid]::NewGuid());[IO.Directory]::CreateDirectory($directory)|Out-Null
    Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'EditorFixture.cs'),(Join-Path $PSScriptRoot 'FilesFixture.cs'),(Join-Path $PSScriptRoot 'FilesActionsFixture.cs')
    Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class IpcLongPathProbe {
 [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern SafeFileHandle CreateFileW(string path,uint access,uint share,IntPtr security,uint creation,uint flags,IntPtr template);
 [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window,out uint pid);
 [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr window);
 public static byte[] Read(string root,string path) {
  if(!path.StartsWith(root+"\\",StringComparison.Ordinal))throw new ArgumentException("Discovery is outside the owned fixture");
  using(var handle=CreateFileW("\\\\?\\"+path.Replace('/','\\'),0x80000000,7,IntPtr.Zero,3,0x80,IntPtr.Zero)) {
   if(handle.IsInvalid)throw new Win32Exception(Marshal.GetLastWin32Error(),"Owned long discovery read failed");
   using(var stream=new FileStream(handle,FileAccess.Read)) {
    if(stream.Length<1||stream.Length>4096)throw new InvalidDataException("Discovery record size is invalid");
    var bytes=new byte[(int)stream.Length];int used=0;
    while(used<bytes.Length){int count=stream.Read(bytes,used,bytes.Length-used);if(count==0)throw new EndOfStreamException();used+=count;}return bytes;
   }
  }
 }
 public static void Hidden(long window,int owner) {uint pid;var hwnd=new IntPtr(window);GetWindowThreadProcessId(hwnd,out pid);if(pid!=owner||IsWindowVisible(hwnd))throw new InvalidOperationException("Expected the exact owned hidden host");}
}
'@
    function Long-Budget([int]$Maximum=5000) {$left=30000-$clock.ElapsedMilliseconds;if($left -le 0){throw 'Long-path IPC exceeded30s'};return [int][Math]::Min($Maximum,$left)}
    function Long-Request([string[]]$Arguments,[int]$Maximum=5000) {
        if(-not $pipeName){throw 'Explicit owned pipe required'};$client=[CliProbe]::Start($cli,(@('--pipe',$pipeName,'--json')+$Arguments),$directory,$directory);$out=$client.StandardOutput.ReadToEndAsync();$err=$client.StandardError.ReadToEndAsync()
        try{if(-not $client.WaitForExit((Long-Budget $Maximum))){throw 'Owned long-path CLI exceeded deadline'};if(-not $out.Wait(500)-or -not $err.Wait(500)){throw 'Owned long-path CLI output did not close'};if($client.ExitCode -ne 0){throw ('Owned long-path CLI failed: '+[CliProbe]::Output($err))};return ([CliProbe]::Output($out)|ConvertFrom-Json)}
        finally{if(-not $client.HasExited){$client.Kill();[CliProbe]::WaitAfterKill($client)};$client.Dispose()}
    }
    try {
        $fixture=New-Object FilesFixture($directory);$env:LOCALAPPDATA=$fixture.LongRoot('localappdata');if($env:LOCALAPPDATA.Length -le 300){throw 'Long-path fixture did not exceed300 UTF-16 units'}
        $startup=[Diagnostics.Stopwatch]::StartNew();$process=[CliProbe]::Start((Join-Path $BuildDirectory 'flowmux.exe'),@('--temporary','--shell=cmd','--cwd',$directory),$directory,$directory);$hostOut=$process.StandardOutput.ReadToEndAsync();$hostErr=$process.StandardError.ReadToEndAsync()
        $discovery=$env:LOCALAPPDATA+'\flowmux\windows\instances\'+$process.Id+'.json'
        do {
            Long-Budget|Out-Null;if($process.HasExited){throw 'Owned long-path host exited before publication'};if($startup.ElapsedMilliseconds -ge 8000){throw 'Long-path discovery exceeded8s'}
            if([FilesActionsFixture]::Exists($fixture,$discovery)) {
                $bytes=[IpcLongPathProbe]::Read($fixture.Root,$discovery);$record=(New-Object Text.UTF8Encoding($false,$true)).GetString($bytes)|ConvertFrom-Json
                if($record.pid -ne $process.Id -or $record.pipe -notmatch ('^\\\\\.\\pipe\\flowmux-'+$process.Id+'-[0-9a-f-]{36}$')){throw 'Long-path discovery published the wrong owned identity'};$pipeName=$record.pipe;break
            };Start-Sleep -Milliseconds 20
        }while($true)
        $identity=Long-Request @('identify');if($identity.pid -ne $process.Id){throw 'Long-path explicit IPC reached another process'}
        $name='IPC-'+[char]0xD55C+[char]0xAE00+'-'+[char]0x1112+[char]0x1161+[char]0x11AB;Long-Request @('workspace','rename',$identity.workspace,$name)|Out-Null
        $lastTree=Long-Request @('tree');[IpcLongPathProbe]::Hidden([long]$lastTree.window_handle,$process.Id);if(-not $lastTree.background_testing -or $lastTree.workspaces[0].name -cne $name){throw 'Long-path hidden IPC changed original Unicode metadata'}
        $ready=[Diagnostics.Stopwatch]::StartNew();while(@($lastTree.surfaces).Count -ne 1 -or -not $lastTree.surfaces[0].ready -or -not $lastTree.surfaces[0].running){if($ready.ElapsedMilliseconds -ge 5000){throw 'Long-path terminal readiness exceeded5s'};Start-Sleep -Milliseconds 20;$lastTree=Long-Request @('tree')}
        if(-not [IO.Directory]::Exists((Join-Path $directory 'state\terminal-profile')) -or [FilesActionsFixture]::Exists($fixture,($env:LOCALAPPDATA+'\flowmux\windows\terminal-profile'))){throw 'Hidden terminal did not use its isolated test profile'}
        Long-Request @('quit','--discard-state')|Out-Null;if(-not $process.WaitForExit((Long-Budget 4000)) -or $process.ExitCode -ne 0){throw 'Owned long-path host failed to quit cleanly'}
        if([FilesActionsFixture]::Exists($fixture,$discovery)){throw 'Normal long-path shutdown left its published discovery record'}
    } catch {$failure=$_.Exception.Message}
    finally {
        if($process){
            try{if(-not $process.HasExited -and $pipeName){Long-Request @('quit','--discard-state') 2000|Out-Null;$process.WaitForExit((Long-Budget 2000))|Out-Null}}catch{$cleanupErrors+=,$_.Exception.Message}
            try{if(-not $process.HasExited){$process.Kill();[CliProbe]::WaitAfterKill($process)};if($hostOut){$hostOut.Wait(500)|Out-Null};if($hostErr){$hostErr.Wait(500)|Out-Null};$hostLog=@{pid=$process.Id;exitCode=$process.ExitCode;stdout=[CliProbe]::Output($hostOut);stderr=[CliProbe]::Output($hostErr)}}catch{$cleanupErrors+=,$_.Exception.Message}finally{$process.Dispose()}
        }
        $env:LOCALAPPDATA=$savedLocal;if($fixture){$fixture.Dispose()}
    }
    if($failure -or $cleanupErrors.Count){@{error=$failure;cleanupErrors=$cleanupErrors;host=$hostLog;discovery=$discovery;lastTree=$lastTree;elapsedMs=$clock.ElapsedMilliseconds}|ConvertTo-Json -Depth 20|Set-Content -Encoding UTF8 (Join-Path $directory 'ipc.json');throw ($failure+' '+($cleanupErrors -join '; '))}
    Write-Output ('passed: long-path LOCALAPPDATA publication, hidden explicit IPC and clean discovery removal; elapsed='+$clock.ElapsedMilliseconds+'ms');return
}
Add-Type -Path (Join-Path $PSScriptRoot 'NativeInput.cs')
Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.IO.Pipes;
using System.Text;
public static class IpcProbe {
    public static NamedPipeClientStream Connect(string fullName) {
        var pipe = new NamedPipeClientStream(".", fullName.Substring(9), PipeDirection.InOut, PipeOptions.Asynchronous);
        try { pipe.Connect(3000); return pipe; } catch { pipe.Dispose(); throw; }
    }
    public static void Abandon(string name, int count) {
        for (int i=0; i<count; i++) using (var pipe=Connect(name)) {
            if ((i%2)==1) { var bytes=Encoding.UTF8.GetBytes("{\"method\":"); pipe.Write(bytes,0,bytes.Length); }
        }
    }
    public static string Request(string name, string request) {
        using (var pipe=Connect(name)) {
            var bytes=new UTF8Encoding(false).GetBytes(request+"\n");
            pipe.Write(bytes,0,bytes.Length);
            using (var reader=new StreamReader(pipe,new UTF8Encoding(false))) {
                var reply=reader.ReadLineAsync();
                if (!reply.Wait(5000)) throw new TimeoutException("IPC reply timed out");
                return reply.Result;
            }
        }
    }
}
'@
$directory=Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT ('ipc-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory) | Out-Null
$directory=(Resolve-Path $directory).Path
$hosts=New-Object 'System.Collections.Generic.List[object]'
$evidence=[ordered]@{ started=(Get-Date).ToString('o'); mode='background'; hosts=@(); checks=@() }
function Invoke-Owned($Owned,[string[]]$Arguments) {
    if (-not $Owned.pipe) { throw 'No owned pipe; refusing discovery fallback' }
    $value=& $cli --pipe $Owned.pipe --json @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Owned IPC request failed: $Arguments" }
    return (($value -join "`n") | ConvertFrom-Json)
}
function Assert-Hidden($Owned) {
    $tree=Invoke-Owned $Owned @('tree')
    $window=[IntPtr]([long]$tree.window_handle)
    if (-not $tree.background_testing -or [NativeInput]::IsWindowVisible($window) -or [NativeInput]::GetForegroundWindow() -eq $window) { throw 'Test host became visible or foreground' }
    return $tree
}
function Start-Owned {
    $launched=(Get-Date).ToUniversalTime()
    $old=$env:FLOWMUX_TEST_BACKGROUND
    try {
        $env:FLOWMUX_TEST_BACKGROUND='1'
        $process=Start-Process -FilePath (Join-Path $BuildDirectory 'flowmux.exe') -ArgumentList '--temporary' -PassThru
    } finally { $env:FLOWMUX_TEST_BACKGROUND=$old }
    $owned=@{ process=$process; pipe=$null; discovery=(Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($process.Id).json") }
    $hosts.Add($owned)
    $deadline=(Get-Date).AddSeconds(25)
    do {
        if ($process.HasExited) { throw "Owned host $($process.Id) exited during startup" }
        if ((Get-Date) -gt $deadline) { throw 'Fresh discovery did not arrive' }
        if ((Test-Path $owned.discovery) -and (Get-Item $owned.discovery).LastWriteTimeUtc -ge $launched) {
            # No parse retry: publication must expose a complete record.
            $record=Get-Content -Raw $owned.discovery | ConvertFrom-Json
            if ($record.pid -ne $process.Id -or -not $record.pipe) { throw 'Wrong discovery identity' }
            $owned.pipe=$record.pipe
            break
        }
        Start-Sleep -Milliseconds 25
    } while ($true)
    if ((Invoke-Owned $owned @('identify')).pid -ne $process.Id) { throw 'Connected to another process' }
    do {
        $tree=Assert-Hidden $owned
        if (@($tree.surfaces | Where-Object { -not $_.ready }).Count -eq 0) { break }
        if ((Get-Date) -gt $deadline) { throw 'Owned terminal did not become ready' }
        Start-Sleep -Milliseconds 50
    } while ($true)
    $evidence.hosts+=@{ pid=$process.Id; pipe=$owned.pipe; launchedUtc=$launched.ToString('o'); discoveryWrittenUtc=(Get-Item $owned.discovery).LastWriteTimeUtc.ToString('o') }
    return $owned
}
function Stop-Owned($Owned) {
    Invoke-Owned $Owned @('quit','--discard-state') | Out-Null
    if (-not $Owned.process.WaitForExit(10000)) { throw 'Owned host did not exit after its reply' }
    if (Test-Path $Owned.discovery) { throw 'Normal exit left a discovery record' }
}
function Reject-Target([string]$Name,[switch]$Inherited) {
    $oldPreference=$ErrorActionPreference; $oldPipe=$env:FLOWMUX_PIPE_NAME; $oldSurface=$env:FLOWMUX_SURFACE_ID
    try {
        $ErrorActionPreference='Continue'
        $env:FLOWMUX_SURFACE_ID=$null
        if ($Inherited) {
            $env:FLOWMUX_PIPE_NAME=$Name
            $output=& $cli --json new-workspace 2>&1
        } else { $output=& $cli --pipe $Name --json new-workspace 2>&1 }
        $status=$LASTEXITCODE
    } finally { $ErrorActionPreference=$oldPreference; $env:FLOWMUX_PIPE_NAME=$oldPipe; $env:FLOWMUX_SURFACE_ID=$oldSurface }
    if ($status -eq 0 -or ($output -join "`n") -notmatch 'Could not connect to requested Windows flowmux pipe') { throw "Missing target was not rejected with its connection error: $output" }
    return ($output -join "`n")
}
try {
    $a=Start-Owned
    $b=Start-Owned
    $identityA=Invoke-Owned $a @('identify')
    $identityB=Invoke-Owned $b @('identify')
    $nameA='A-'+[char]0xD55C+[char]0xAE00+'-'+[char]0x1112+[char]0x1161+[char]0x11AB
    $nameB='B-'+[char]::ConvertFromUtf32(0x1F600)
    Invoke-Owned $a @('workspace','rename',$identityA.workspace,$nameA) | Out-Null
    Invoke-Owned $b @('workspace','rename',$identityB.workspace,$nameB) | Out-Null
    for ($i=0;$i -lt 20;$i++) {
        if ((Invoke-Owned $a @('identify')).pid -ne $a.process.Id -or (Invoke-Owned $b @('identify')).pid -ne $b.process.Id) { throw 'Multiwindow routing crossed processes' }
    }
    $treeA=Assert-Hidden $a; $treeB=Assert-Hidden $b
    if ($treeA.workspaces[0].name -cne $nameA -or $treeB.workspaces[0].name -cne $nameB) { throw 'Unicode metadata crossed windows or changed' }
    $evidence.checks+=@{ name='two_hidden_windows_keep_unicode_metadata_and_explicit_process_routing'; roundTrips=40; pids=@($a.process.Id,$b.process.Id) }

    [IpcProbe]::Abandon($a.pipe,400)
    $bad=[IpcProbe]::Request($a.pipe,'not-json') | ConvertFrom-Json
    if (-not $bad.error) { throw 'Malformed request was accepted' }
    if ((Invoke-Owned $a @('identify')).pid -ne $a.process.Id) { throw 'Listener stopped after abandoned or malformed requests' }
    $evidence.checks+=@{ name='four_hundred_empty_or_truncated_disconnects_and_malformed_request_keep_listener_alive'; connections=400 }

    $idle=New-Object 'System.Collections.Generic.List[System.IO.Pipes.NamedPipeClientStream]'
    try {
        for ($i=0;$i -lt 8;$i++) { $idle.Add([IpcProbe]::Connect($a.pipe)) }
        if ((Invoke-Owned $a @('identify')).pid -ne $a.process.Id) { throw 'Idle clients blocked acceptance' }
    } finally { foreach ($connection in $idle) { $connection.Dispose() } }
    $evidence.checks+=@{ name='eight_idle_clients_do_not_block_other_requests'; idleClients=8 }

    $missing='\\.\pipe\flowmux-'+$a.process.Id+'-'+[guid]::NewGuid()
    $explicitError=Reject-Target $missing
    $inheritedError=Reject-Target $missing -Inherited
    if ((Assert-Hidden $a).workspaces.Count -ne 1 -or (Assert-Hidden $b).workspaces.Count -ne 1) { throw 'Missing endpoint fell back and mutated another window' }
    $evidence.checks+=@{ name='stale_explicit_and_inherited_endpoints_fail_without_mutating_either_window'; explicitError=$explicitError; inheritedError=$inheritedError }

    Stop-Owned $a
    if ((Invoke-Owned $b @('identify')).pid -ne $b.process.Id) { throw 'Closing one window disabled its survivor' }
    $staleError=Reject-Target $a.pipe
    if ((Assert-Hidden $b).workspaces.Count -ne 1) { throw 'Closed endpoint mutated surviving window' }
    $evidence.checks+=@{ name='quit_reply_and_discovery_cleanup_preserve_other_window'; staleError=$staleError }

    for ($i=0;$i -lt 6;$i++) {
        $restarted=Start-Owned
        [IpcProbe]::Abandon($restarted.pipe,40)
        Assert-Hidden $restarted | Out-Null
        Stop-Owned $restarted
    }
    $evidence.checks+=@{ name='six_fresh_start_quit_cycles_publish_complete_records_and_survive_early_disconnects'; cycles=6; additionalDisconnects=240 }
    Stop-Owned $b
    $evidence.status='passed_ipc_subset'
} catch {
    $evidence.status='failed'; $evidence.error=$_.Exception.Message
    throw
} finally {
    foreach ($owned in $hosts) {
        if (-not $owned.process.HasExited) {
            if ($owned.pipe) { try { Invoke-Owned $owned @('quit','--discard-state') | Out-Null } catch {} }
            if (-not $owned.process.WaitForExit(10000)) { Stop-Process -Id $owned.process.Id -Force }
        }
    }
    $evidence.finished=(Get-Date).ToString('o')
    if ($evidence.status -eq 'failed') { $evidence | ConvertTo-Json -Depth 20 | Set-Content -Encoding UTF8 (Join-Path $directory 'ipc.json') }
}
[ordered]@{status=$evidence.status;checks=$evidence.checks.Count}|ConvertTo-Json -Compress
