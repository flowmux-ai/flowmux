# SPDX-License-Identifier: GPL-3.0-or-later
# Runs only hidden, temporary test hosts and hidden CLI processes.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",[switch]$ObserveLegacy)
if (-not $env:FLOWMUX_TEST_ARTIFACT_ROOT) { throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.' }
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path
$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor=(& $cli doctor | ConvertFrom-Json)
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) { throw 'A working debug build is required; no host was launched.' }
Add-Type -Path (Join-Path $PSScriptRoot 'NativeInput.cs')
Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.IO.Pipes;
using System.Text;
using System.Threading.Tasks;
public static class LimitsProbe {
    public static NamedPipeClientStream Connect(string fullName,int timeout) {
        var pipe=new NamedPipeClientStream(".",fullName.Substring(9),PipeDirection.InOut,PipeOptions.Asynchronous);
        try { pipe.Connect(timeout); return pipe; } catch { pipe.Dispose(); throw; }
    }
    public static bool StillWritable(NamedPipeClientStream pipe) {
        try { pipe.WriteByte((byte)'x'); return true; } catch (IOException) { return false; }
    }
    public static void Write(NamedPipeClientStream pipe,string text) {
        var bytes=new UTF8Encoding(false).GetBytes(text+"\n"); pipe.Write(bytes,0,bytes.Length);
    }
    public sealed class SilentServer : IDisposable {
        public readonly string Name;
        private readonly NamedPipeServerStream pipe;
        private readonly Task connection;
        public SilentServer() {
            Name="\\\\.\\pipe\\flowmux-"+System.Diagnostics.Process.GetCurrentProcess().Id+"-"+Guid.NewGuid();
            pipe=new NamedPipeServerStream(Name.Substring(9),PipeDirection.InOut,1,PipeTransmissionMode.Byte,PipeOptions.Asynchronous);
            connection=pipe.WaitForConnectionAsync();
        }
        public string Receive() {
            if (!connection.Wait(5000)) throw new TimeoutException("CLI did not connect to the owned silent server");
            var reader=new StreamReader(pipe,new UTF8Encoding(false),true,1024,true);
            var line=reader.ReadLineAsync();
            if (!line.Wait(5000)) throw new TimeoutException("CLI did not send a complete request");
            return line.Result;
        }
        public void Dispose() { pipe.Dispose(); }
    }
}
'@
$directory=Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT ('ipc-limits-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory) | Out-Null
$directory=(Resolve-Path $directory).Path
$evidence=[ordered]@{ started=(Get-Date).ToString('o'); mode='background'; legacyObservation=[bool]$ObserveLegacy; checks=@() }
$launched=(Get-Date).ToUniversalTime(); $old=$env:FLOWMUX_TEST_BACKGROUND
try { $env:FLOWMUX_TEST_BACKGROUND='1'; $owned=Start-Process -FilePath (Join-Path $BuildDirectory 'flowmux.exe') -ArgumentList '--temporary' -PassThru }
finally { $env:FLOWMUX_TEST_BACKGROUND=$old }
$discovery=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($owned.Id).json"
$pipeName=$null; $cliProcess=$null; $silent=$null
$connections=New-Object 'System.Collections.Generic.List[System.IO.Pipes.NamedPipeClientStream]'
function Invoke-Owned([string[]]$Arguments) {
    if (-not $script:pipeName) { throw 'No verified owned endpoint; refusing discovery fallback' }
    $value=& $cli --pipe $script:pipeName --json @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Owned IPC request failed: $Arguments" }
    return (($value -join "`n") | ConvertFrom-Json)
}
try {
    $deadline=(Get-Date).AddSeconds(25)
    do {
        if ($owned.HasExited -or (Get-Date) -gt $deadline) { throw 'Owned host failed to publish discovery' }
        if ((Test-Path $discovery) -and (Get-Item $discovery).LastWriteTimeUtc -ge $launched) {
            $record=Get-Content -Raw $discovery | ConvertFrom-Json
            if ($record.pid -ne $owned.Id -or -not $record.pipe) { throw 'Wrong discovery identity' }
            $script:pipeName=$record.pipe; break
        }
        Start-Sleep -Milliseconds 25
    } while ($true)
    if ((Invoke-Owned @('identify')).pid -ne $owned.Id) { throw 'Connected to another host' }
    do {
        $tree=Invoke-Owned @('tree')
        if (@($tree.surfaces | Where-Object { -not $_.ready }).Count -eq 0) { break }
        if ((Get-Date) -gt $deadline) { throw 'Test terminal was not ready' }
        Start-Sleep -Milliseconds 50
    } while ($true)
    $window=[IntPtr]([long]$tree.window_handle)
    if (-not $tree.background_testing -or [NativeInput]::IsWindowVisible($window) -or [NativeInput]::GetForegroundWindow() -eq $window) { throw 'Owned host became visible or foreground' }
    $evidence.pid=$owned.Id; $evidence.pipe=$pipeName; $evidence.shellPids=$tree.surfaces.pid
    $owned.Refresh(); $beforeThreads=$owned.Threads.Count
    $rejected=0
    for ($i=0;$i -lt 24;$i++) {
        try { $connections.Add([LimitsProbe]::Connect($pipeName,80)) } catch { $rejected++ }
    }
    $owned.Refresh(); $occupiedThreads=$owned.Threads.Count
    if (-not $ObserveLegacy -and ($connections.Count -ne 16 -or $rejected -ne 8)) { throw "Pool was not bounded: accepted=$($connections.Count), rejected=$rejected" }
    Start-Sleep -Milliseconds 6200
    $stillWritable=0
    foreach ($connection in $connections) { if ([LimitsProbe]::StillWritable($connection)) { $stillWritable++ } }
    if (-not $ObserveLegacy -and $stillWritable -ne 0) { throw 'Idle connections survived their request deadline' }
    $evidence.checks+=@{ name='idle_connection_capacity_and_expiration'; attempted=24; accepted=$connections.Count; rejected=$rejected; writableAfterSixSeconds=$stillWritable; threadsBefore=$beforeThreads; threadsWhileOccupied=$occupiedThreads }
    foreach ($connection in $connections) { $connection.Dispose() }; $connections.Clear()
    if ((Invoke-Owned @('identify')).pid -ne $owned.Id) { throw 'Host did not recover after idle clients' }
    if ($ObserveLegacy) {
        Invoke-Owned @('quit','--discard-state') | Out-Null
        if (-not $owned.WaitForExit(10000)) { throw 'Legacy observation host did not close' }
        $evidence.status='observed_legacy_unbounded_waits'
    } else {
        # The fake server belongs to this script process and never sends a reply.
        # It has no GUI and does not publish discovery or dispatch the received command.
        $silent=New-Object LimitsProbe+SilentServer
        $stdout=Join-Path $directory 'silent-cli-stdout.txt'; $stderr=Join-Path $directory 'silent-cli-stderr.txt'
        $elapsed=[Diagnostics.Stopwatch]::StartNew()
        $cliProcess=Start-Process -FilePath $cli -ArgumentList @('--pipe',$silent.Name,'--json','new-workspace') -WindowStyle Hidden -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru
        $null=$cliProcess.Handle
        $received=$silent.Receive()
        if (-not $cliProcess.WaitForExit(35000)) { throw 'CLI exceeded its reply deadline' }
        $cliProcess.WaitForExit(); $elapsed.Stop()
        $errorText=Get-Content -Raw $stderr -Encoding UTF8
        if ($cliProcess.ExitCode -eq 0 -or $errorText -notmatch 'deadline exceeded' -or $errorText -notmatch 'not retried') { throw "CLI did not report its transport timeout: $errorText" }
        $evidence.checks+=@{ name='cli_exits_when_owned_server_never_replies_without_retrying_mutation'; elapsedSeconds=$elapsed.Elapsed.TotalSeconds; request=$received; error=$errorText.Trim() }
        $silent.Dispose(); $silent=$null

        # Keep all other slots waiting for requests while the last peer sends
        # an authorized quit and deliberately never reads its successful reply.
        for ($i=0;$i -lt 15;$i++) { $connections.Add([LimitsProbe]::Connect($pipeName,1000)) }
        $unread=[LimitsProbe]::Connect($pipeName,1000); $connections.Add($unread)
        $elapsed=[Diagnostics.Stopwatch]::StartNew()
        [LimitsProbe]::Write($unread,'{"method":"quit","discard_state":true}')
        if (-not $owned.WaitForExit(8000)) { throw 'Unread quit reply or idle clients prevented host shutdown' }
        $elapsed.Stop()
        if (Test-Path $discovery) { throw 'Discovery survived coordinated shutdown' }
        foreach ($childId in $evidence.shellPids) { if (Get-Process -Id $childId -ErrorAction SilentlyContinue) { throw "Owned shell $childId survived shutdown" } }
        $evidence.checks+=@{ name='unread_quit_reply_finishes_and_cancels_fifteen_idle_clients_and_shells'; elapsedSeconds=$elapsed.Elapsed.TotalSeconds }
        $evidence.status='passed_ipc_limits_subset'
    }
} catch { $evidence.status='failed'; $evidence.error=$_.Exception.Message; throw }
finally {
    foreach ($connection in $connections) { $connection.Dispose() }
    if ($silent) { $silent.Dispose() }
    if ($cliProcess -and -not $cliProcess.HasExited) { $cliProcess.Kill(); $cliProcess.WaitForExit() }
    if (-not $owned.HasExited) {
        if ($script:pipeName) { try { Invoke-Owned @('quit','--discard-state') | Out-Null } catch {} }
        if (-not $owned.WaitForExit(10000)) { $owned.Kill(); $owned.WaitForExit() }
    }
    $evidence.finished=(Get-Date).ToString('o')
    if ($evidence.status -eq 'failed') { $evidence | ConvertTo-Json -Depth 20 | Set-Content -Encoding UTF8 (Join-Path $directory 'ipc-limits.json') }
}
[ordered]@{status=$evidence.status;checks=$evidence.checks.Count}|ConvertTo-Json -Compress