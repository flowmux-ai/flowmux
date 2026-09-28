# SPDX-License-Identifier: GPL-3.0-or-later
# Run one Windows check, stream progress, and bound the entire owned process tree.
param(
    [Parameter(Mandatory=$true)][string]$Path,
    [string[]]$ArgumentList=@(),
    [ValidateRange(1,1800)][int]$TimeoutSeconds=120,
    [ValidatePattern('^[a-zA-Z0-9_-]+$')][string]$Name='check',
    [switch]$KeepArtifacts
)
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'CheckJob.cs')
$target=(Resolve-Path -LiteralPath $Path).Path
$directory=Join-Path ([IO.Path]::GetTempPath()) ('flowmux-checks\'+$Name+'-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null
$directory=(Resolve-Path $directory).Path
$artifactRoot=Join-Path $directory 'artifacts';[IO.Directory]::CreateDirectory($artifactRoot)|Out-Null
$stdout=Join-Path $directory 'stdout.txt';$stderr=Join-Path $directory 'stderr.txt'
$executable=$target;$arguments=$ArgumentList
if ([IO.Path]::GetExtension($target) -eq '.ps1') {
    $executable=Join-Path $PSHOME 'powershell.exe'
    $arguments=@('-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',$target)+$ArgumentList
}
function Write-Available($Reader) {
    $buffer=New-Object char[] 32768
    $count=$Reader.Read($buffer,0,$buffer.Length)
    if($count -gt 0){[Console]::Write($buffer,0,$count)}
}
$clock=[Diagnostics.Stopwatch]::StartNew();$job=$null;$readers=@();$nextBeat=5
$result=[ordered]@{name=$Name;target=$target;deadlineSeconds=$TimeoutSeconds;status='runner_error';started=[DateTime]::UtcNow.ToString('o')}
try {
    $previousArtifactRoot=[Environment]::GetEnvironmentVariable('FLOWMUX_TEST_ARTIFACT_ROOT','Process')
    try {
        [Environment]::SetEnvironmentVariable('FLOWMUX_TEST_ARTIFACT_ROOT',$artifactRoot,'Process')
        $job=New-Object CheckJob($executable,$arguments,(Split-Path $PSScriptRoot -Parent),$stdout,$stderr)
    } finally {[Environment]::SetEnvironmentVariable('FLOWMUX_TEST_ARTIFACT_ROOT',$previousArtifactRoot,'Process')}
    $result.pid=$job.Id
    foreach($file in @($stdout,$stderr)) {
        $stream=[IO.File]::Open($file,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::ReadWrite)
        $readers+=New-Object IO.StreamReader($stream,(New-Object Text.UTF8Encoding($false)),$true)
    }
    Write-Host "[$Name] started pid=$($job.Id), deadline=${TimeoutSeconds}s; logs: $directory"
    do {
        $exited=$job.Wait(100)
        foreach($reader in $readers){Write-Available $reader}
        if ($exited) {$result.exitCode=$job.ExitCode;$result.status=if($job.ExitCode -eq 0){'passed'}else{'failed'};break}
        if ($clock.Elapsed.TotalSeconds -ge $TimeoutSeconds) {$result.status='timeout';$result.exitCode=124;break}
        if ($clock.Elapsed.TotalSeconds -ge $nextBeat) {
            Write-Host "[$Name] running $([int]$clock.Elapsed.TotalSeconds)/${TimeoutSeconds}s, owned processes=$($job.Active)"
            $nextBeat=$clock.Elapsed.TotalSeconds+5
        }
    } while($true)
} catch {$result.error=$_.Exception.Message;$result.exitCode=125}
finally {
    if ($job) {
        try {
            # Always remove any descendants still alive after the root exits.
            $result.activeBeforeCleanup=$job.Active
            $job.Stop(124)
            $cleanup=[Diagnostics.Stopwatch]::StartNew()
            while($job.Active -gt 0 -and $cleanup.ElapsedMilliseconds -lt 3000){Start-Sleep -Milliseconds 25}
            $result.activeAfterCleanup=$job.Active
            if($result.activeAfterCleanup -ne 0){$result.status='cleanup_failed';$result.exitCode=125}
            foreach($reader in $readers){Write-Available $reader}
        } catch {$result.cleanupError=$_.Exception.Message;$result.status='cleanup_failed';$result.exitCode=125}
        $job.Dispose()
    }
    foreach($reader in $readers){$reader.Dispose()}
    $result.elapsedSeconds=[Math]::Round($clock.Elapsed.TotalSeconds,3)
    $result.finished=[DateTime]::UtcNow.ToString('o')
    $retain=$KeepArtifacts -or $result.status -ne 'passed'
    if(-not $retain){
        try {Remove-Item -LiteralPath $directory -Recurse -Force}
        catch {$retain=$true;$result.status='cleanup_failed';$result.exitCode=125;$result.cleanupError=$_.Exception.Message}
    }
    if($retain){
        [IO.Directory]::CreateDirectory($directory)|Out-Null
        $result|ConvertTo-Json -Depth 5|Set-Content -Encoding UTF8 (Join-Path $directory 'result.json')
        Write-Host "[$Name] $($result.status) in $($result.elapsedSeconds)s; result: $directory\result.json"
    }else{Write-Host "[$Name] $($result.status) in $($result.elapsedSeconds)s; temporary logs and artifacts removed"}
}
exit $result.exitCode
