# SPDX-License-Identifier: GPL-3.0-or-later
# Exact subsets in owned hidden processes; run under run-check.ps1 (50s).
param([Parameter(Mandatory=$true)][string]$Executable,[string]$OutputPath,
 [hashtable[]]$Cases=@(
  @{filter='keybindings::';expected=22},
  @{filter='native::settings_store::tests';expected=1},
  @{filter='command::tests';expected=8},
  @{filter='protocol::tests';expected=2}
 ))
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs')
$exe=(Resolve-Path -LiteralPath $Executable).Path
$checks=@()
foreach($case in $cases) {
 $p=[CliProbe]::Start($exe,@($case.filter,'--test-threads=1'),$PSScriptRoot,$PSScriptRoot)
 try {
  $o=$p.StandardOutput.ReadToEndAsync();$e=$p.StandardError.ReadToEndAsync()
  if(-not $p.WaitForExit(10000)){throw ('Owned native unit test timed out: '+$case.filter)}
  if(-not $o.Wait(1000) -or -not $e.Wait(1000)){throw ('Owned native unit output incomplete: '+$case.filter)}
  $stdout=[CliProbe]::Output($o);$stderr=[CliProbe]::Output($e)
  $result=[regex]::Matches($stdout,'(?m)^test result: ok\. (\d+) passed; (\d+) failed;')
  if($p.ExitCode -ne 0 -or $result.Count -ne 1){throw ('Native unit result or exit differs: '+$case.filter+' '+$stderr)}
  $passed=[int]$result[0].Groups[1].Value;$failed=[int]$result[0].Groups[2].Value
  if($passed -le 0 -or $passed -ne $case.expected -or $failed -ne 0){throw ('Native unit count differs: '+$case.filter+' expected '+$case.expected+', got '+$passed+' passed / '+$failed+' failed')}
  $checks+=@{filter=$case.filter;expected=$case.expected;passed=$passed;pid=$p.Id;exitCode=$p.ExitCode;stdout=$stdout;stderr=$stderr}
 } catch {Write-Output ([CliProbe]::Output($o));Write-Output ([CliProbe]::Output($e));throw}
 finally {if(-not $p.HasExited){$p.Kill();[CliProbe]::WaitAfterKill($p)};$p.Dispose()}
}
$total=($checks|ForEach-Object {$_.passed}|Measure-Object -Sum).Sum
$expectedTotal=($cases|ForEach-Object {$_.expected}|Measure-Object -Sum).Sum
if($checks.Count -ne $cases.Count -or $total -le 0 -or $total -ne $expectedTotal){throw 'Native unit aggregate count differs'}
[ordered]@{status='passed';checks=$checks.Count;totalPassed=$total}|ConvertTo-Json -Compress
