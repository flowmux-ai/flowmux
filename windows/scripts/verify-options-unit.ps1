# SPDX-License-Identifier: GPL-3.0-or-later
# Exact subsets with explicit test counts; run under run-check.ps1 (30s).
param([Parameter(Mandatory=$true)][string]$Executable,[string]$OutputPath)
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs')
$exe=(Resolve-Path -LiteralPath $Executable).Path
$checks=@()
foreach($case in @(@{filter='native::settings_store::tests';expected=1},@{filter='command::tests';expected=8})) {
 $p=[CliProbe]::Start($exe,@($case.filter,'--test-threads=1'),$PSScriptRoot,$PSScriptRoot)
 try {
  $o=$p.StandardOutput.ReadToEndAsync();$e=$p.StandardError.ReadToEndAsync()
  if(-not $p.WaitForExit(10000)){throw 'Owned native unit test timed out'}
  if(-not $o.Wait(1000) -or -not $e.Wait(1000)){throw 'Owned native unit output incomplete'}
  $stdout=[CliProbe]::Output($o);$stderr=[CliProbe]::Output($e)
  if($p.ExitCode -ne 0 -or $stdout -notmatch ('test result: ok\. '+$case.expected+' passed; 0 failed')){throw ('Native unit count or exit differs: '+$case.filter+' '+$stderr)}
  $checks+=@{filter=$case.filter;passed=$case.expected;pid=$p.Id;exitCode=$p.ExitCode;stdout=$stdout;stderr=$stderr}
 } catch {Write-Output ([CliProbe]::Output($o));Write-Output ([CliProbe]::Output($e));throw}
 finally {if(-not $p.HasExited){$p.Kill();[CliProbe]::WaitAfterKill($p)};$p.Dispose()}
}
[ordered]@{status='passed';checks=$checks.Count;totalPassed=9}|ConvertTo-Json -Compress
