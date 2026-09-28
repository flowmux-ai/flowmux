# SPDX-License-Identifier: GPL-3.0-or-later
# Positive, failure, and timeout+descendant-cleanup checks. All children hidden.
$ErrorActionPreference='Stop'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs')
$pending=New-Object 'Threading.Tasks.TaskCompletionSource[string]'
if([CliProbe]::Output($pending.Task) -ne '[output not complete]'){throw 'Pending diagnostic output must not block'}
$pending.SetResult('finished')
if([CliProbe]::Output($pending.Task) -ne 'finished'){throw 'Completed diagnostic output differs'}
$directory=Join-Path $PSScriptRoot ('..\dist\runner-fixture-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null
$directory=(Resolve-Path $directory).Path
$runner=Join-Path $PSScriptRoot 'run-check.ps1'
$passed=Join-Path $directory 'pass.ps1';$failed=Join-Path $directory 'fail.ps1';$hung=Join-Path $directory 'hang.ps1'
"Write-Output 'fixture ready';exit 0"|Set-Content -Encoding UTF8 $passed
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
$checks=@()
foreach($case in @(@{file=$passed;name='runner-pass';code=0;limit=10},@{file=$failed;name='runner-fail';code=7;limit=10},@{file=$hung;name='runner-timeout';code=124;limit=3})) {
    # The runner is a separate noninteractive console process; its children use CREATE_NO_WINDOW.
    & (Join-Path $PSHOME 'powershell.exe') -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $runner -Path $case.file -Name $case.name -TimeoutSeconds $case.limit
    if($LASTEXITCODE -ne $case.code){throw ('Runner classification differs: '+$case.name+' '+$LASTEXITCODE)}
    $checks+=@{name=$case.name;exitCode=$LASTEXITCODE}
}
$checks|ConvertTo-Json|Set-Content -Encoding UTF8 (Join-Path $directory 'result.json')
Write-Output ('Runner evidence: '+$directory)
