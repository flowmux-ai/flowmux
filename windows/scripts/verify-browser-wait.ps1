# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden owned WebView2 host + loopback fixture only. No foreground, input, clipboard or external sites.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",[switch]$Extended)
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path
$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor=& $cli doctor|ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) {throw 'Working debug build required; no host launched'}
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs')
Add-Type -Path (Join-Path $PSScriptRoot 'BrowserFixture.cs')
$directory=Join-Path $(if ($env:FLOWMUX_TEST_ARTIFACT_ROOT) { $env:FLOWMUX_TEST_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\dist\evidence' }) ('browser-wait-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$fixture=New-Object BrowserFixture((Join-Path $PSScriptRoot 'browser-dom.html'));$origin=$fixture.Origin
$pipeName=$null;$process=$null;$hosts=@();$shells=@()
$evidence=[ordered]@{started=(Get-Date).ToString('o');mode='background';checks=@();clipboardAccess=$false;desktopInput=$false;externalSites=$false;unicodeComparison='ordinal'}
function Request([string[]]$Arguments,[int]$Exit=0) {
    if (-not $script:pipeName) {throw 'Owned pipe required'}
    $p=[CliProbe]::Start($cli,(@('--pipe',$script:pipeName,'--json')+$Arguments),$directory,$directory)
    try {
        $out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
        if (-not $p.WaitForExit(30000)) {$p.Kill();[CliProbe]::WaitAfterKill($p);throw 'Owned CLI timed out; not retried'}
        if (-not $out.Wait(3000) -or -not $err.Wait(3000)) {throw 'Owned output did not close'}
        if ($p.ExitCode -ne $Exit) {throw "CLI $Arguments exit $($p.ExitCode): $(([CliProbe]::Output($err))) $($out.Result)"}
        if ($Exit -eq 0) {return ($out.Result|ConvertFrom-Json)}
        return (([CliProbe]::Output($err))|ConvertFrom-Json)
    } finally {$p.Dispose()}
}
function Raw-Request($Body) {
    if (-not $script:pipeName) {throw 'Owned pipe required'}
    $stream=[IO.Pipes.NamedPipeClientStream]::new('.',$script:pipeName.Substring(9),[IO.Pipes.PipeDirection]::InOut,[IO.Pipes.PipeOptions]::Asynchronous,[Security.Principal.TokenImpersonationLevel]::Identification)
    try {
        $stream.Connect(3000)
        $writer=[IO.StreamWriter]::new($stream,(New-Object Text.UTF8Encoding($false)),4096,$true)
        $reader=[IO.StreamReader]::new($stream,(New-Object Text.UTF8Encoding($false)),$false,4096,$true)
        try {
            $writer.WriteLine(($Body|ConvertTo-Json -Depth 10 -Compress));$writer.Flush()
            $read=$reader.ReadLineAsync();if (-not $read.Wait(20000)) {throw 'Owned browser fixture request timed out; not retried'}
            return ($read.Result|ConvertFrom-Json)
        } finally {$writer.Dispose();$reader.Dispose()}
    } finally {$stream.Dispose()}
}
function Tree {
    $tree=Request @('tree');$window=[IntPtr]([long]$tree.window_handle)
    if (-not $tree.background_testing -or [CliProbe]::IsWindowVisible($window) -or [CliProbe]::GetForegroundWindow() -eq $window) {throw 'Owned host became visible or foreground'}
    foreach ($browser in $tree.browsers) {
        if ([CliProbe]::IsWindowVisible([IntPtr]([long]$browser.view_handle)) -or [CliProbe]::IsWindowVisible([IntPtr]([long]$browser.chrome_handle))) {throw 'Owned browser became visible'}
    }
    return $tree
}
function Start-Owned([string[]]$Launch) {
    $script:pipeName=$null;$utc=[DateTime]::UtcNow
    $script:process=[CliProbe]::Start($gui,$Launch,$directory,$directory)
    $script:hosts+= $process.Id
    $script:stdout=$process.StandardOutput.ReadToEndAsync();$script:stderr=$process.StandardError.ReadToEndAsync()
    $file=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($process.Id).json";$deadline=(Get-Date).AddSeconds(40)
    do {
        if ($process.HasExited -or (Get-Date) -gt $deadline) {throw ('Owned startup failed: '+([CliProbe]::Output($stderr)))}
        if ((Test-Path $file) -and (Get-Item $file).LastWriteTimeUtc -ge $utc) {
            $record=Get-Content -Raw $file|ConvertFrom-Json
            if ($record.pid -ne $process.Id) {throw 'Wrong discovery owner'}
            $script:pipeName=$record.pipe;break
        }
        Start-Sleep -Milliseconds 50
    } while ($true)
    if ((Request @('identify')).pid -ne $process.Id) {throw 'Wrong pipe owner'}
    do {
        $tree=Tree
        if (@($tree.surfaces|Where-Object {-not $_.ready}).Count -eq 0) {return $tree}
        if ((Get-Date) -gt $deadline) {throw 'Terminal readiness timed out'}
        Start-Sleep -Milliseconds 50
    } while ($true)
}
function Same-Text([string]$Left,[string]$Right) {return [string]::Equals($Left,$Right,[StringComparison]::Ordinal)}
function Eval-Page([string]$Pane,[string]$Source) {return (Request @('browser','eval',('pane:'+$Pane),$Source)).result}
function Wait-Page([string]$Pane,[string]$Suffix,[string]$Title) {
    $deadline=(Get-Date).AddSeconds(20)
    do {
        $status=Request @('browser','status',('pane:'+$Pane))
        if (-not $status.loading -and $status.url.Contains($Suffix) -and (Same-Text $status.title $Title)) {
            if ((Eval-Page $Pane 'document.readyState') -eq 'complete') {return $status}
        }
        if ((Get-Date) -gt $deadline) {throw ('Page did not load: '+($status|ConvertTo-Json -Compress))}
        Start-Sleep -Milliseconds 50
    } while ($true)
}
function Find-Ref($Snapshot,[string]$Selector) {
    $found=@($Snapshot.refs.psobject.Properties|Where-Object {$_.Value.selector -ceq $Selector})
    if ($found.Count -ne 1) {throw ('Expected one ref for '+$Selector)}
    return $found[0].Name
}
function Query([string]$Op,[string]$Ref) {return (Request @('browser',$Op,$script:domPane,$Ref)).result}
function Snapshot {return Request @('browser','snapshot',$script:domPane)}
function Fresh-Page {
    Request @('browser','navigate',$script:domPane,($origin+'/dom'))|Out-Null
    Wait-Page $script:domPane '/dom' 'DOM 한글 한 é 😀'|Out-Null
}
function Plain([string[]]$Arguments) {
    $p=[CliProbe]::Start($cli,(@('--pipe',$script:pipeName)+$Arguments),$directory,$directory)
    try {
        $out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
        if (-not $p.WaitForExit(30000)) {$p.Kill();[CliProbe]::WaitAfterKill($p);throw 'Owned plain CLI timed out'}
        if (-not $out.Wait(3000) -or -not $err.Wait(3000) -or $p.ExitCode -ne 0) {throw ('Plain CLI failed: '+([CliProbe]::Output($err)))}
        return $out.Result.TrimEnd("`r","`n")
    } finally {$p.Dispose()}
}

$waitClients=@()
function Begin-Wait([string]$Pane,[string[]]$Condition) {
    $p=[CliProbe]::Start($cli,(@('--pipe',$script:pipeName,'--json','browser','wait',$Pane)+$Condition),$directory,$directory)
    $job=[pscustomobject]@{process=$p;pid=$p.Id;output=$p.StandardOutput.ReadToEndAsync();error=$p.StandardError.ReadToEndAsync();completed=$false}
    $script:waitClients+= $job
    return $job
}
function End-Wait($Job,[int]$Exit=0) {
    $p=$Job.process
    try {
        if (-not $p.WaitForExit(45000)) {$p.Kill();[CliProbe]::WaitAfterKill($p);throw 'Owned wait CLI timed out; not retried'}
        if (-not $Job.output.Wait(3000) -or -not $Job.error.Wait(3000)) {throw 'Owned wait output remained open'}
        if ($p.ExitCode -ne $Exit) {throw ('Wait CLI exit '+$p.ExitCode+': '+([CliProbe]::Output($Job.error)))}
        if ($Exit -eq 0) {return ($Job.output.Result|ConvertFrom-Json)}
        return (([CliProbe]::Output($Job.error))|ConvertFrom-Json)
    } finally {$Job.completed=$true;$p.Dispose()}
}
function Wait-True([string[]]$Condition) {
    $result=Request (@('browser','wait',$script:domPane)+$Condition)
    if ($result.result -ne $true) {throw ('Wait unexpectedly expired: '+($Condition -join ' '))}
    return $result
}
try {
    $tree=Start-Owned @('--temporary','--shell=cmd','--cwd',$directory)
    $source=Request @('identify');$terminal=$tree.surfaces[0];$script:shells+=$terminal.pid
    $opened=(Request @('browser','open',($origin+'/dom'),'--pane',$source.pane)).browser_pane_opened
    $script:domPane=$opened.pane
    Wait-True @('--url','/dom')|Out-Null
    Wait-True @('--ready-state','complete')|Out-Null
    $before=Eval-Page $domPane '({html:document.documentElement.outerHTML,mutations:domMutationCount,events:inputEventCount})'
    foreach ($condition in @(@('--selector','#heading'),@('--selector','#hidden'),@('--text','한글 한 é 😀'),@('--js','document.querySelector("#heading")'),@('--js','() => true'),@('--js','return true;'))) {
        $r=Wait-True $condition
        if ($r.surface -ne $opened.surface) {throw 'Wait did not report its original surface'}
    }
    if ((Plain @('browser','wait',$domPane,'--js','true')) -cne 'true') {throw 'Plain true output differs'}
    $after=Eval-Page $domPane '({html:document.documentElement.outerHTML,mutations:domMutationCount,events:inputEventCount})'
    if (-not (Same-Text $before.html $after.html) -or $before.mutations -ne $after.mutations -or $before.events -ne $after.events) {throw 'Read-only conditions mutated DOM or dispatched input'}
    Eval-Page $domPane 'document.querySelector("#heading").textContent="가";null'|Out-Null
    Wait-True @('--text','가')|Out-Null
    if ((Request @('browser','wait',$domPane,'--text','가','--timeout-ms','120')).result -ne $false) {throw 'Text wait normalized Hangul'}
    $evidence.checks+=@{name='five_conditions_plain_json_exact_unicode_and_no_input_or_dom_mutation';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)

    Eval-Page $domPane 'setTimeout(()=>{const p=document.createElement("p");p.id="delayed";p.textContent="지연 한 😀";document.body.append(p);},250);null'|Out-Null
    Wait-True @('--selector','#delayed','--poll-ms','25')|Out-Null
    Wait-True @('--text','지연 한 😀')|Out-Null
    Request @('browser','navigate',$domPane,($origin+'/stream'))|Out-Null
    Wait-True @('--ready-state','loading','--poll-ms','10')|Out-Null
    Wait-True @('--ready-state','interactive','--poll-ms','10')|Out-Null
    Wait-True @('--ready-state','complete','--poll-ms','25')|Out-Null
    Wait-True @('--js','window.deferredDone === true')|Out-Null
    $evidence.checks+=@{name='delayed_dom_and_streaming_loading_interactive_complete_states';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)

    $time=[Diagnostics.Stopwatch]::StartNew()
    if ((Plain @('browser','wait',$domPane,'--selector','#never','--timeout-ms','200','--poll-ms','20')) -cne 'false') {throw 'Timeout did not return plain false'}
    $time.Stop();if ($time.ElapsedMilliseconds -lt 180 -or $time.ElapsedMilliseconds -gt 4000) {throw 'Wait timeout did not bound completion'}
    $err=Request @('browser','wait',$domPane,'--selector','[') 1
    if (-not $err.error) {throw 'Invalid CSS lacked an error'}
    $err=Request @('browser','wait',$domPane,'--js','(()=>{window.waitCalls=(window.waitCalls||0)+1;throw new Error("owned predicate failure")})()') 1
    if (-not $err.error.Contains('owned predicate failure') -or (Eval-Page $domPane 'window.waitCalls') -ne 1) {throw 'Runtime failure replayed the predicate'}
    $late=Request @('browser','wait',$domPane,'--js','()=>{const end=Date.now()+350;while(Date.now()<end){};window.lateCompleted=(window.lateCompleted||0)+1;return true;}','--timeout-ms','100','--poll-ms','10')
    if ($late.result -ne $false) {throw 'Late true callback bypassed the wait deadline'}
    Wait-True @('--js','window.lateCompleted === 1','--timeout-ms','2000')|Out-Null
    $err=Request @('browser','wait',$domPane,'--js','Promise.resolve(true)') 1
    if (-not $err.error.Contains('asynchronous')) {throw 'Promise was treated as truthy'}
    foreach ($options in @(@{text='a';url='b'},@{js='true';timeout_ms=120001},@{js='true';poll_ms=0},@{ready_state='done'},@{text=''})) {
        $op=@{kind='wait';pane=$domPane};foreach($key in $options.Keys){$op[$key]=$options[$key]}
        $r=Raw-Request @{method='browser';op=$op}
        if (-not $r.error) {throw 'Invalid raw wait bypassed validation'}
    }
    Wait-True @('--js','true')|Out-Null
    $evidence.checks+=@{name='timeout_false_invalid_inputs_and_single_runtime_exception_without_replay';passed=$true;timeoutElapsedMs=$time.ElapsedMilliseconds}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)

    $job=Begin-Wait $domPane @('--text','둘째 한글','--timeout-ms','8000','--poll-ms','25')
    Request @('browser','navigate',$domPane,($origin+'/redirect'))|Out-Null
    $r=End-Wait $job
    if ($r.result -ne $true -or $r.surface -ne $opened.surface) {throw 'Wait did not survive redirect to a new document'}
    Eval-Page $domPane 'setTimeout(()=>window.originalReady=true,4000);null'|Out-Null
    $job=Begin-Wait $domPane @('--js','window.pinWaitStarted=true; return window.originalReady === true;','--timeout-ms','10000')
    Wait-True @('--js','window.pinWaitStarted === true')|Out-Null
    $other=(Request @('browser','open',($origin+'/dom'),'--pane',$source.pane)).browser_pane_opened
    if ($job.process.HasExited) {throw 'Pin fixture finished before the original tab was hidden'}
    $r=End-Wait $job
    if ($r.result -ne $true -or $r.surface -ne $opened.surface) {throw 'Wait followed the pane active tab instead of the initial surface'}
    Wait-True @('--ready-state','complete')|Out-Null
    if ((Eval-Page $domPane 'window.originalReady === true') -ne $false) {throw 'Fixture did not distinguish the new active tab'}
    $job=Begin-Wait $domPane @('--js','window.closeWaitStarted=true; return false;','--timeout-ms','8000')
    Wait-True @('--js','window.closeWaitStarted === true')|Out-Null
    Request @('close-tab',$other.surface)|Out-Null
    $r=End-Wait $job 1
    if (-not $r.error.Contains('closed')) {throw 'Closing the owned browser did not cancel its wait'}
    $evidence.checks+=@{name='navigation_redirect_hidden_surface_pin_and_close_cancellation';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)

    $jobs=@()
    for($i=0;$i -lt 8;$i++) {$jobs+=Begin-Wait $domPane @('--js',('window.capacity'+$i+'=true; return Boolean(window.releaseWaits);'),'--timeout-ms','10000','--poll-ms','50')}
    $deadline=(Get-Date).AddSeconds(5)
    do {
        $admitted=Eval-Page $domPane 'Array.from({length:8},(_,i)=>window["capacity"+i]).filter(Boolean).length'
        if ($admitted -eq 8) {break}
        if ((Get-Date) -gt $deadline) {throw 'Eight waits were not admitted'}
        Start-Sleep -Milliseconds 50
    } while ($true)
    $err=Request @('browser','wait',$domPane,'--js','true') 1
    if (-not $err.error.Contains('eight')) {throw 'Wait capacity limit missing'}
    Tree|Out-Null
    Eval-Page $domPane 'window.releaseWaits=true;null'|Out-Null
    foreach($job in $jobs) {if ((End-Wait $job).result -ne $true) {throw 'Concurrent wait failed'}}
    Wait-True @('--js','true')|Out-Null
    $current=(Tree).surfaces|Where-Object {$_.id -eq $terminal.id}
    if ($current.pid -ne $terminal.pid -or -not $current.running) {throw 'Waits changed the terminal process'}
    $evidence.checks+=@{name='bounded_concurrent_waits_keep_other_commands_responsive_and_release_capacity';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)

    if ($Extended) {
        Eval-Page $domPane 'window.longWaitTarget=Date.now()+27000;null'|Out-Null
        $time=[Diagnostics.Stopwatch]::StartNew()
        $job=Begin-Wait $domPane @('--js','Date.now() >= window.longWaitTarget','--timeout-ms','32000','--poll-ms','100')
        $r=End-Wait $job;$time.Stop()
        if ($r.result -ne $true -or $time.ElapsedMilliseconds -lt 25000 -or $time.ElapsedMilliseconds -gt 40000) {throw 'Long wait exceeded or bypassed its transport budget'}
        $evidence.checks+=@{name='wait_exceeds_default_server_and_client_deadlines_without_retransmission';passed=$true;elapsedMs=$time.ElapsedMilliseconds}
        Write-Host ("[check] passed "+$evidence.checks[-1].name)
    } else {
        $evidence.skippedChecks=@('wait_exceeds_default_server_and_client_deadlines_without_retransmission')
        Write-Host '[check] deferred 27-second transport check; run with -Extended before release or after transport changes'
    }
    Tree|Out-Null
    $evidence.status='passed_background_browser_wait_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
    foreach($job in $waitClients) {if(-not $job.completed){if(-not $job.process.HasExited){$job.process.Kill();[CliProbe]::WaitAfterKill($job.process)};$job.process.Dispose()}}
    if ($pipeName -and $process -and -not $process.HasExited) {try {Request @('quit','--discard-state')|Out-Null} catch {}}
    if ($process) {if (-not $process.HasExited -and -not $process.WaitForExit(10000)) {$process.Kill();[CliProbe]::WaitAfterKill($process)};if ($stderr -and $stderr.Wait(3000)) {$evidence.hostStderr=([CliProbe]::Output($stderr))};$evidence.hostExitCode=$process.ExitCode;$process.Dispose()}
    $evidence.waitClientPids=@($waitClients|ForEach-Object {$_.pid})
    $fixture.Dispose();$evidence.hosts=$hosts;$evidence.shells=$shells;$evidence.finished=(Get-Date).ToString('o')
    $evidence|ConvertTo-Json -Depth 12|Set-Content -Encoding UTF8 (Join-Path $directory 'native-browser-wait-background.json')
    Write-Output ('Evidence: '+$directory)
}
$evidence|ConvertTo-Json -Depth 12
