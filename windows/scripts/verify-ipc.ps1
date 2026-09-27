# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden hosts only. No desktop input, visible windows, or persistent user state.
param([string]$BuildDirectory = "$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
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
$directory=Join-Path $PSScriptRoot ('..\dist\evidence\ipc-'+[guid]::NewGuid())
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
    $evidence | ConvertTo-Json -Depth 20 | Set-Content -Encoding UTF8 (Join-Path $directory 'ipc.json')
}
$evidence | ConvertTo-Json -Depth 20
