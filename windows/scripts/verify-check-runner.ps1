# SPDX-License-Identifier: GPL-3.0-or-later
# Positive, failure, and timeout+descendant-cleanup checks. All children hidden.
param([switch]$KeepArtifacts)
$ErrorActionPreference='Stop'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs')
$pending=New-Object 'Threading.Tasks.TaskCompletionSource[string]'
if([CliProbe]::Output($pending.Task) -ne '[output not complete]'){throw 'Pending diagnostic output must not block'}
$pending.SetResult('finished')
if([CliProbe]::Output($pending.Task) -ne 'finished'){throw 'Completed diagnostic output differs'}
$checksRoot=Join-Path ([IO.Path]::GetTempPath()) 'flowmux-checks'
$artifactBase=if($env:FLOWMUX_TEST_ARTIFACT_ROOT){$env:FLOWMUX_TEST_ARTIFACT_ROOT}else{$checksRoot}
$directory=Join-Path $artifactBase ('runner-fixture-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null
$directory=(Resolve-Path $directory).Path
$runner=Join-Path $PSScriptRoot 'run-check.ps1'
$passed=Join-Path $directory 'pass.ps1';$failed=Join-Path $directory 'fail.ps1';$hung=Join-Path $directory 'hang.ps1'
@'
if(-not $env:FLOWMUX_TEST_ARTIFACT_ROOT){throw 'Runner omitted owned artifact root'}
[IO.File]::WriteAllText((Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT 'probe.txt'),$env:FLOWMUX_TEST_ARTIFACT_ROOT)
Write-Output 'fixture ready';exit 0
'@|Set-Content -Encoding UTF8 $passed
"Write-Error 'expected failure';exit 7"|Set-Content -Encoding UTF8 $failed
@'
$info=New-Object Diagnostics.ProcessStartInfo
$info.FileName=Join-Path $PSHOME 'powershell.exe'
$info.Arguments='-NoProfile -NonInteractive -Command "Start-Sleep -Seconds 300"'
$info.UseShellExecute=$false;$info.CreateNoWindow=$true
$p=[Diagnostics.Process]::Start($info)
Write-Output ('owned grandchild='+$p.Id)
Start-Sleep -Seconds 300
'@|Set-Content -Encoding UTF8 $hung
$checks=@();$ownedResults=@();$complete=$false;$incomingArtifactRoot=$env:FLOWMUX_TEST_ARTIFACT_ROOT
try {
    foreach($case in @(@{file=$passed;name='runner-pass';code=0;limit=10;keep=$false},@{file=$passed;name='runner-keep';code=0;limit=10;keep=$true},@{file=$failed;name='runner-fail';code=7;limit=10;keep=$false},@{file=$hung;name='runner-timeout';code=124;limit=3;keep=$false})) {
        # Each case gets an exact private name, including when other checks run.
        $name=$case.name+'-'+[guid]::NewGuid();$keep=@();if($case.keep){$keep=@('-KeepArtifacts')}
        & (Join-Path $PSHOME 'powershell.exe') -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $runner -Path $case.file -Name $name -TimeoutSeconds $case.limit @keep
        $code=$LASTEXITCODE;if($code -ne $case.code){throw ('Runner classification differs: '+$name+' '+$code)}
        if($env:FLOWMUX_TEST_ARTIFACT_ROOT -cne $incomingArtifactRoot){throw 'Runner changed its parent artifact environment'}
        $paths=@(Get-ChildItem -LiteralPath $checksRoot -Directory -Filter ($name+'-*'))
        if($case.keep -or $case.code -ne 0){
            if($paths.Count -ne 1){throw 'Required runner diagnostics were not retained'}
            $result=Get-Content -Raw -LiteralPath (Join-Path $paths[0].FullName 'result.json')|ConvertFrom-Json
            $expectedStatus=if($case.code -eq 0){'passed'}elseif($case.code -eq 124){'timeout'}else{'failed'}
            if($result.status -ne $expectedStatus -or $result.exitCode -ne $case.code -or $result.activeAfterCleanup -ne 0){throw 'Runner classification, exit or owned Job cleanup differs'}
            if($case.keep){$root=Join-Path $paths[0].FullName 'artifacts';if([IO.File]::ReadAllText((Join-Path $root 'probe.txt')) -cne $root){throw 'Child artifact escaped the runner root'}}
            $ownedResults+=$paths[0].FullName
        }elseif($paths.Count -ne 0){throw 'Successful default check retained its temporary results'}
        $checks+=@{name=$case.name;exitCode=$code;keepArtifacts=$case.keep}
    }
    if($KeepArtifacts){$checks|ConvertTo-Json|Set-Content -Encoding UTF8 (Join-Path $directory 'result.json');Write-Output ('Runner results: '+$directory)}
    $complete=$true;Write-Output ('Runner retention/failure/timeout checks passed: '+$checks.Count)
}finally{
    # Preserve all diagnostics on assertion failure; delete only our exact roots.
    if($complete -and -not $KeepArtifacts){foreach($path in $ownedResults){Remove-Item -LiteralPath $path -Recurse -Force};Remove-Item -LiteralPath $directory -Recurse -Force}
}
