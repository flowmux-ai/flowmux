# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden release entrypoints only; invoke through run-check.ps1 with a 60s deadline.
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
Add-Type -Path (Join-Path $PSScriptRoot '..\scripts\CliProbe.cs')
$results=@()
$directory=$PSScriptRoot
$build=(Resolve-Path (Join-Path $PSScriptRoot 'font-picker-release')).Path
$output=Join-Path $PSScriptRoot '..\evidence\2026-09-28\native-font-picker-release-entrypoints.json'
foreach($name in @('flowmux.exe','flowmuxctl.exe','flowmux.com')) {
    Write-Host ('[release-doctor] checking '+$name)
    $started=[DateTime]::UtcNow.ToString('o')
    $p=[CliProbe]::Start((Join-Path $build $name),@('doctor'),$directory,$directory)
    try {
        $out=$p.StandardOutput.ReadToEndAsync()
        $err=$p.StandardError.ReadToEndAsync()
        if(-not $p.WaitForExit(5000)) {
            $p.Kill()
            [CliProbe]::WaitAfterKill($p)
            throw ('Owned release doctor timed out: '+$name+' '+[CliProbe]::Output($err))
        }
        $stdoutComplete=$out.Wait(3000)
        $stderrComplete=$err.Wait(3000)
        if(-not $stdoutComplete -or -not $stderrComplete -or $p.ExitCode -ne 0) {
            throw ('Release parser failed: '+$name+' '+[CliProbe]::Output($err))
        }
        $value=[CliProbe]::Output($out)|ConvertFrom-Json
        if($value.background_testing -or $value.status -ne 'ok' -or $value.platform -ne 'windows') {
            throw ('Unexpected release doctor response: '+$name)
        }
        $results+=[ordered]@{name=$name;pid=$p.Id;startedUtc=$started;finishedUtc=[DateTime]::UtcNow.ToString('o');exitCode=$p.ExitCode;result=$value}
        Write-Host ('[release-doctor] passed '+$name)
    } finally {
        if(-not $p.HasExited) {
            $p.Kill()
            [CliProbe]::WaitAfterKill($p)
        }
        $p.Dispose()
    }
}
$results|ConvertTo-Json -Depth 5|Set-Content -Encoding UTF8 $output
