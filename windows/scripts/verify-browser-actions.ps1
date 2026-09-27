# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden owned WebView2 host + loopback fixture only. No foreground, input, clipboard or external sites.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path
$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor=& $cli doctor|ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) {throw 'Working debug build required; no host launched'}
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs')
Add-Type -Path (Join-Path $PSScriptRoot 'BrowserFixture.cs')
$directory=Join-Path $PSScriptRoot ('..\dist\evidence\browser-actions-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$fixture=New-Object BrowserFixture((Join-Path $PSScriptRoot 'browser-actions.html'));$origin=$fixture.Origin
$pipeName=$null;$process=$null;$hosts=@();$shells=@()
foreach($file in @('flowmux.exe','flowmuxctl.exe','flowmux-command.exe')) {
    $probe=[CliProbe]::Start((Join-Path $BuildDirectory $file),@('doctor'),$directory,$directory)
    try {
        $out=$probe.StandardOutput.ReadToEndAsync();$err=$probe.StandardError.ReadToEndAsync()
        if(-not $probe.WaitForExit(15000)){$probe.Kill();$probe.WaitForExit();throw 'Owned entry-point doctor timed out'}
        if(-not $out.Wait(3000) -or -not $err.Wait(3000) -or $probe.ExitCode -ne 0){throw ('Entry-point parser failed: '+$file+' '+$err.Result)}
        if(-not ($out.Result|ConvertFrom-Json).background_testing){throw 'Wrong entry-point build'}
    } finally {$probe.Dispose()}
}

$evidence=[ordered]@{started=(Get-Date).ToString('o');mode='background';checks=@();clipboardAccess=$false;desktopInput=$false;externalSites=$false;unicodeComparison='ordinal'}
function Request([string[]]$Arguments,[int]$Exit=0) {
    if (-not $script:pipeName) {throw 'Owned pipe required'}
    $p=[CliProbe]::Start($cli,(@('--pipe',$script:pipeName,'--json')+$Arguments),$directory,$directory)
    try {
        $out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
        if (-not $p.WaitForExit(30000)) {$p.Kill();$p.WaitForExit();throw 'Owned CLI timed out; not retried'}
        if (-not $out.Wait(3000) -or -not $err.Wait(3000)) {throw 'Owned output did not close'}
        if ($p.ExitCode -ne $Exit) {throw "CLI $Arguments exit $($p.ExitCode): $($err.Result) $($out.Result)"}
        if ($Exit -eq 0) {return ($out.Result|ConvertFrom-Json)}
        return ($err.Result|ConvertFrom-Json)
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
        if ($process.HasExited -or (Get-Date) -gt $deadline) {throw ('Owned startup failed: '+$stderr.Result)}
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
        if (-not $p.WaitForExit(30000)) {$p.Kill();$p.WaitForExit();throw 'Owned plain CLI timed out'}
        if (-not $out.Wait(3000) -or -not $err.Wait(3000) -or $p.ExitCode -ne 0) {throw ('Plain CLI failed: '+$err.Result)}
        return $out.Result.TrimEnd("`r","`n")
    } finally {$p.Dispose()}
}

$actionClients=@()
function Act([string]$Op,[string]$Selector,[string[]]$Values=@(),[int]$Exit=0) {
    $ref=Find-Ref (Snapshot) $Selector
    return Request (@('browser',$Op,$script:domPane,$ref)+$Values) $Exit
}
function Reset-Events {Eval-Page $script:domPane 'window.events=[];null'|Out-Null}
function Assert-Events([string]$Selector,[string[]]$Names) {
    $id=$Selector.TrimStart('#')|ConvertTo-Json -Compress
    $actual=Eval-Page $script:domPane ('window.events.filter(event=>event.id==='+$id+').map(event=>event.type).join(",")')
    if (-not (Same-Text $actual ($Names -join ','))) {throw ('Unexpected events '+$Selector+': '+$actual)}
}
function Begin-Action([string[]]$Arguments) {
    $p=[CliProbe]::Start($cli,(@('--pipe',$script:pipeName,'--json')+$Arguments),$directory,$directory)
    $job=[pscustomobject]@{process=$p;pid=$p.Id;output=$p.StandardOutput.ReadToEndAsync();error=$p.StandardError.ReadToEndAsync();completed=$false}
    $script:actionClients+=$job;return $job
}
function End-Action($Job) {
    try {
        if (-not $Job.process.WaitForExit(20000)) {$Job.process.Kill();$Job.process.WaitForExit();throw 'Action CLI timed out; not retried'}
        if (-not $Job.output.Wait(3000) -or -not $Job.error.Wait(3000) -or $Job.process.ExitCode -ne 0) {throw ('Action failed: '+$Job.error.Result)}
        return ($Job.output.Result|ConvertFrom-Json)
    } finally {$Job.completed=$true;$Job.process.Dispose()}
}
try {
    $tree=Start-Owned @('--temporary','--shell=cmd','--cwd',$directory)
    $source=Request @('identify');$terminal=$tree.surfaces[0];$script:shells+=$terminal.pid
    $opened=(Request @('browser','open',($origin+'/dom'),'--pane',$source.pane)).browser_pane_opened
    $script:domPane=$opened.pane
    Wait-Page $domPane '/dom' 'DOM 한글 한 é 😀'|Out-Null
    $evidence.selectInitial=Eval-Page $domPane 'document.querySelector("#pick").outerHTML'
    $evidence.selectParser=Eval-Page $domPane '(new DOMParser()).parseFromString("<select><option>first</option><option>second</option><option>third</option></select>","text/html").body.innerHTML'
    $evidence.selectApi=Eval-Page $domPane '(()=>{const select=document.createElement("select");for(const value of ["first","second","third"])select.add(new Option(value,value));return select.outerHTML;})()'

    $unicode="한글 한 é 😀 `" \\ `n');window.injected=true;//"
    Reset-Events
    $r=Act 'fill' '#entry' @($unicode)
    if (-not $r.ok -or $r.surface -ne $opened.surface) {throw 'Action result metadata differs'}
    $value=Eval-Page $domPane 'document.querySelector("#entry").value'
    if (-not (Same-Text $value $unicode.Replace("`n",''))) {throw 'Single-line Unicode fill differs'}
    Assert-Events '#entry' @('beforeinput','input','change')
    $event=Eval-Page $domPane 'events[0]'
    if (-not (Same-Text $event.data $unicode) -or $event.inputType -ne 'insertReplacementText' -or $event.trusted) {throw 'beforeinput did not preserve Unicode data'}
    if ((Eval-Page $domPane 'setterCalls') -ne 0 -or (Eval-Page $domPane 'typeof window.injected') -ne 'undefined') {throw 'Fill used an instance setter or escaped into code'}
    Reset-Events;Act 'fill' '#area' @($unicode)|Out-Null
    if (-not (Same-Text (Eval-Page $domPane 'document.querySelector("#area").value') $unicode)) {throw 'Textarea Unicode or newline changed'}
    Assert-Events '#area' @('beforeinput','input','change')
    Reset-Events;Act 'fill' '#area' @($unicode)|Out-Null;Assert-Events '#area' @()
    Act 'fill' '#area' @('')|Out-Null
    if (-not (Same-Text (Eval-Page $domPane 'document.querySelector("#area").value') '')) {throw 'Empty fill failed'}
    $evidence.checks+=@{name='exact_unicode_fill_native_setter_events_empty_and_idempotent_values';passed=$true}

    foreach($selector in @('#readonly','#disabled','#file')) {
        Reset-Events;$err=Act 'fill' $selector @('새 값') 1
        if (-not $err.error) {throw 'Invalid input was accepted'}
        Assert-Events $selector @()
    }
    Reset-Events;$err=Act 'fill' '#cancel' @('새 값') 1
    if (-not $err.error.Contains('canceled') -or -not (Same-Text (Eval-Page $domPane 'document.querySelector("#cancel").value') '원문')) {throw 'Canceled fill changed its value'}
    Assert-Events '#cancel' @('beforeinput')
    Reset-Events;$err=Act 'fill' '#replace' @('새 값') 1
    if (-not $err.error.Contains('changed during beforeinput') -or -not (Same-Text (Eval-Page $domPane 'document.querySelector("#replace").value') '새 대상')) {throw 'Fill changed a replacement element'}
    Assert-Events '#replace' @('beforeinput')
    Reset-Events;$err=Act 'fill' '#lock' @('새 값') 1
    if (-not $err.error.Contains('disabled') -or -not (Same-Text (Eval-Page $domPane 'document.querySelector("#lock").value') '원문')) {throw 'Fill bypassed a beforeinput disable'}
    Assert-Events '#lock' @('beforeinput')
    $evidence.checks+=@{name='disabled_readonly_file_cancel_replacement_and_reentrant_disable_rejected';passed=$true}

    $evidence.selectHtml=Eval-Page $domPane 'document.querySelector("#pick").outerHTML'
    $evidence.selectBefore=Eval-Page $domPane 'Array.from(document.querySelector("#pick").options).map(option=>({value:option.value,text:option.textContent}))'
    Reset-Events;Act 'select' '#pick' @('우선')|Out-Null
    if ((Eval-Page $domPane 'document.querySelector("#pick").selectedIndex') -ne 2) {throw 'Select did not prefer an exact option value'}
    Assert-Events '#pick' @('input','change')
    Reset-Events;Act 'select' '#pick' @('한글 항목')|Out-Null
    if (-not (Same-Text (Eval-Page $domPane 'document.querySelector("#pick").value') '한 😀')) {throw 'Select label did not preserve Unicode option value'}
    Act 'select' '#pick' @('é 😀')|Out-Null
    Reset-Events;Act 'select' '#pick' @('accent')|Out-Null;Assert-Events '#pick' @()
    Act 'select' '#pick' @('blocked') 1|Out-Null;Act 'select' '#pick' @('missing') 1|Out-Null
    Act 'select' '#entry' @('x') 1|Out-Null
    Reset-Events;Act 'select' '#multiple' @('한 😀')|Out-Null
    $selected=Eval-Page $domPane 'Array.from(document.querySelector("#multiple").selectedOptions).map(option=>option.value)'
    if ($selected.Count -ne 2 -or -not (Same-Text $selected[1] '한 😀')) {throw 'Multiple selection discarded existing selection'}
    $evidence.checks+=@{name='select_value_label_precedence_unicode_multiple_noop_and_disabled_options';passed=$true}

    Reset-Events;Act 'check' '#checked'|Out-Null;Assert-Events '#checked' @('input','change')
    Reset-Events;Act 'check' '#checked'|Out-Null;Assert-Events '#checked' @()
    Act 'uncheck' '#checked'|Out-Null;Assert-Events '#checked' @('input','change')
    Reset-Events;Act 'uncheck' '#checked'|Out-Null;Assert-Events '#checked' @()
    Act 'check' '#radio2'|Out-Null
    if ((Eval-Page $domPane 'document.querySelector("#radio1").checked || !document.querySelector("#radio2").checked') -ne $false) {throw 'Radio group exclusivity failed'}
    $err=Act 'uncheck' '#radio2' @() 1
    if (-not $err.error.Contains('radios cannot')) {throw 'Individual radio uncheck was accepted'}
    Act 'check' '#entry' @() 1|Out-Null
    $evidence.checks+=@{name='checkbox_idempotence_and_radio_group_semantics_without_click_or_keyboard';passed=$true}

    Reset-Events;$ref=Find-Ref (Snapshot) '#click'
    if ((Plain @('browser','click',$domPane,$ref)) -cne 'ok') {throw 'Plain action output differs'}
    if ((Eval-Page $domPane 'clicks') -ne 1) {throw 'Click was not dispatched exactly once'}
    Assert-Events '#click' @('click')
    Request @('browser','click',$domPane,$ref)|Out-Null
    if ((Eval-Page $domPane 'clicks') -ne 2) {throw 'Explicit repeated click did not preserve the current ref contract'}
    Reset-Events;Act 'dblclick' '#double'|Out-Null;Assert-Events '#double' @('dblclick')
    if ((Eval-Page $domPane 'doubleClicks') -ne 1) {throw 'Double-click listener did not run'}
    Reset-Events;Act 'hover' '#hover'|Out-Null;Assert-Events '#hover' @('mouseenter','mouseover')
    if ((Eval-Page $domPane 'events.some(event=>event.trusted)') -ne $false) {throw 'DOM pointer fixture unexpectedly used trusted input'}
    Act 'scroll' '#scroll' @('0','-20')|Out-Null
    if ((Eval-Page $domPane 'window.scrollY') -lt 500) {throw 'Element scroll did not move the viewport'}
    $ref=Find-Ref (Snapshot) '#replace-click'
    Request @('browser','click',$domPane,$ref)|Out-Null
    Request @('browser','click',$domPane,$ref) 1|Out-Null
    if ((Eval-Page $domPane 'clicks') -ne 3) {throw 'Stale ref clicked a replacement element'}
    $evidence.checks+=@{name='explicit_click_repeats_synthetic_double_hover_scroll_and_stale_replacement_refs';passed=$true}

    $snap=Snapshot;$entry=Find-Ref $snap '#entry'
    Reset-Events
    foreach($op in @('focus','blur')) {
        $err=Request @('browser',$op,$domPane,$entry) 1
        if (-not $err.error.Contains('background')) {throw 'Background DOM focus was not refused'}
    }
    Request @('browser','value',$domPane,$entry)|Out-Null
    if ((Eval-Page $domPane 'events.some(event=>/focus|key|composition/.test(event.type))') -ne $false) {throw 'Background actions requested focus or keyboard/composition events'}
    $other=(Request @('browser','open',($origin+'/dom'),'--pane',$source.pane)).browser_pane_opened
    Wait-Page $domPane '/dom' 'DOM 한글 한 é 😀'|Out-Null
    Request @('browser','fill',$domPane,$entry,'wrong tab') 1|Out-Null
    if (-not (Same-Text (Eval-Page $domPane 'document.querySelector("#entry").value') '')) {throw 'Ref from another tab changed the new tab'}
    Request @('close-tab',$other.surface)|Out-Null
    Request @('browser','fill',$domPane,$entry,'old ref') 1|Out-Null
    $ref=Find-Ref (Snapshot) '#link'
    $nav=Begin-Action @('browser','click',$domPane,$ref)
    # Navigation can invalidate the response even when the click was applied.
    try {End-Action $nav|Out-Null} catch {if(-not $_.Exception.Message.Contains('may have executed')){throw}}
    $wait=Request @('browser','wait',$domPane,'--url','clicked=1')
    if ($wait.result -ne $true) {throw 'Owned link click did not navigate'}
    Request @('browser','click',$domPane,$ref) 1|Out-Null
    $current=(Tree).surfaces|Where-Object {$_.id -eq $terminal.id}
    if ($current.pid -ne $terminal.pid -or -not $current.running) {throw 'Browser actions changed the source terminal'}
    $evidence.checks+=@{name='background_focus_guard_cross_tab_refs_navigation_and_terminal_identity';passed=$true}

    Reset-Events;$ref=Find-Ref (Snapshot) '#busy'
    $job=Begin-Action @('browser','fill',$domPane,$ref,'동시 한 😀')
    $deadline=(Get-Date).AddSeconds(3)
    do {
        $status=Request @('browser','status',$domPane)
        if ($status.action_pending) {break}
        if ($job.process.HasExited -or (Get-Date) -gt $deadline) {throw 'Pending-action admission was not observed'}
        Start-Sleep -Milliseconds 10
    } while ($true)
    $err=Request @('browser','snapshot',$domPane) 1
    if (-not $err.error.Contains('pending browser action')) {throw 'Snapshot overtook an action'}
    $err=Request @('browser','fill',$domPane,$ref,'replay') 1
    if (-not $err.error.Contains('pending browser action')) {throw 'Concurrent action was admitted'}
    End-Action $job|Out-Null
    if ((Request @('browser','status',$domPane)).action_pending -or -not (Same-Text (Eval-Page $domPane 'document.querySelector("#busy").value') '동시 한 😀')) {throw 'Action completion did not release admission'}
    Assert-Events '#busy' @('beforeinput','input','change')
    Reset-Events
    Request @('browser','fill',$domPane,$ref,'동시 한 😀')|Out-Null
    Assert-Events '#busy' @()
    $evidence.checks+=@{name='one_action_in_flight_status_snapshot_barrier_and_idempotent_explicit_repeat';passed=$true}

    Reset-Events;$ref=Find-Ref (Snapshot) '#area'
    $large='한'*20000
    $r=Raw-Request @{method='browser';op=@{kind='fill';pane=$domPane;target=$ref;value=$large}}
    if (-not $r.ok -or -not (Same-Text (Eval-Page $domPane 'document.querySelector("#area").value') $large)) {throw 'Large Unicode fill was not exact'}
    $ref=Find-Ref (Snapshot) '#area'
    $r=Raw-Request @{method='browser';op=@{kind='fill';pane=$domPane;target=$ref;value=('a'*65537)}}
    if (-not $r.error.Contains('64 KiB')) {throw 'Oversized action input was accepted'}
    Request @('browser','value',$domPane,$ref)|Out-Null
    $r=Raw-Request @{method='browser';op=@{kind='fill';pane=$domPane;target=$ref;value=([string][char]0)*40000}}
    if (-not $r.error.Contains('128 KiB')) {throw 'Encoded script bound was not enforced'}
    Act 'fill' '#area' @('복구 한 😀')|Out-Null
    $evidence.checks+=@{name='large_unicode_value_raw_and_encoded_bounds_recover_without_mutation';passed=$true}
    Tree|Out-Null
    $evidence.status='passed_background_browser_actions_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
    foreach($job in $actionClients){if(-not $job.completed){if(-not $job.process.HasExited){$job.process.Kill();$job.process.WaitForExit()};$job.process.Dispose()}}
    if ($pipeName -and $process -and -not $process.HasExited) {try {Request @('quit','--discard-state')|Out-Null} catch {}}
    if ($process) {if (-not $process.HasExited -and -not $process.WaitForExit(10000)) {$process.Kill();$process.WaitForExit()};if ($stderr -and $stderr.Wait(3000)) {$evidence.hostStderr=$stderr.Result};$evidence.hostExitCode=$process.ExitCode;$process.Dispose()}
    $fixture.Dispose();$evidence.hosts=$hosts;$evidence.shells=$shells;$evidence.actionClientPids=@($actionClients|ForEach-Object {$_.pid});$evidence.finished=(Get-Date).ToString('o')
    $evidence|ConvertTo-Json -Depth 12|Set-Content -Encoding UTF8 (Join-Path $directory 'native-browser-actions-background.json')
    Write-Output ('Evidence: '+$directory)
}
$evidence|ConvertTo-Json -Depth 12
