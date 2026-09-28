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
$directory=Join-Path $(if ($env:FLOWMUX_TEST_ARTIFACT_ROOT) { $env:FLOWMUX_TEST_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\dist\evidence' }) ('browser-dom-'+[guid]::NewGuid())
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
try {
    $tree=Start-Owned @('--temporary','--shell=cmd','--cwd',$directory)
    $source=Request @('identify');$terminal=$tree.surfaces[0];$script:shells+=$terminal.pid
    $opened=(Request @('browser','open',($origin+'/dom'),'--pane',$source.pane)).browser_pane_opened
    $script:domPane=$opened.pane
    Wait-Page $domPane '/dom' 'DOM 한글 한 é 😀'|Out-Null
    $before=Eval-Page $domPane '({html:document.documentElement.outerHTML,mutations:domMutationCount,events:inputEventCount})'
    $snap=Snapshot;$heading=Find-Ref $snap '#heading';$entry=Find-Ref $snap '#entry';$checked=Find-Ref $snap '#checked';$disabled=Find-Ref $snap '#disabled'
    if ($snap.surface -ne $opened.surface -or $snap.frame_count -ne 1 -or $snap.omitted_refs -ne 0) {throw 'Snapshot metadata differs'}
    if (-not (Same-Text (Query 'text' $heading) '한글 한 é 😀 "quoted" \ backslash')) {throw 'Korean text query changed code units'}
    $expected=Eval-Page $domPane 'fixtureText'
    if (-not (Same-Text (Query 'value' $entry) $expected.Replace("`n",'')) -or -not (Same-Text (Query 'value' (Find-Ref $snap '#area')) $expected)) {throw 'Input/textarea Unicode value changed'}
    if ((Query 'is-checked' $checked) -ne $true -or (Query 'is-enabled' $disabled) -ne $false -or (Query 'is-visible' $heading) -ne $true) {throw 'DOM state queries differ'}
    $injection=(Request @('browser','attr',$domPane,(Find-Ref $snap '#visible'),'data-note')).result
    if (-not (Same-Text $injection "'); window.intrusion = true; //") -or (Eval-Page $domPane 'typeof window.intrusion') -ne 'undefined') {throw 'Attribute escaped into JavaScript'}
    if (-not (Same-Text (Query 'text' (Find-Ref $snap '#error-data')) 'error: not found')) {throw 'Page data was treated as an error envelope'}
    $after=Eval-Page $domPane '({html:document.documentElement.outerHTML,mutations:domMutationCount,events:inputEventCount,stamped:document.querySelectorAll("[data-flowmux-ref]").length})'
    if (-not (Same-Text $before.html $after.html) -or $before.mutations -ne $after.mutations -or $before.events -ne $after.events -or $after.stamped -ne 0) {throw 'Snapshot/query mutated DOM or dispatched input'}
    $evidence.checks+=@{name='snapshot_and_typed_queries_preserve_exact_unicode_without_dom_mutation_or_input';passed=$true;refs=@($snap.refs.psobject.Properties).Count;nodeCount=$snap.node_count}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)

    foreach ($label in @('중복 첫째','중복 둘째','깊은 항목 0','깊은 항목 1')) {
        $ref=@($snap.refs.psobject.Properties|Where-Object {Same-Text $_.Value.name $label})
        if ($ref.Count -ne 1 -or -not (Same-Text (Query 'text' $ref[0].Name) $label)) {throw 'Ambiguous duplicate/deep selector'}
    }
    $long=Find-Ref $snap '#long-name'
    if (-not (Same-Text $snap.refs.$long.name ('a'*119))) {throw 'Excerpt split a surrogate pair'}
    foreach ($selector in @('#long-jamo','#long-combining')) {
        $ref=Find-Ref $snap $selector
        if (-not (Same-Text $snap.refs.$ref.name ('a'*119))) {throw 'Excerpt split decomposed Hangul or a combining grapheme'}
    }
    if ((Request @('browser','count',$domPane,'#duplicate')).result -ne 2 -or (Request @('browser','count',$domPane,'#hidden')).result -ne 1) {throw 'CSS count differs'}
    if (($snap.refs.psobject.Properties.Value.name -join '').Contains('shadow only') -or $snap.refs.psobject.Properties.Value.selector -contains '#hidden') {throw 'Snapshot crossed hidden/shadow document boundary'}
    if ((Plain @('browser','is-checked',$domPane,$checked)) -ne 'true' -or (Plain @('browser','count',$domPane,'#duplicate')) -ne '2' -or -not (Same-Text (Plain @('browser','text',$domPane,$heading)) '한글 한 é 😀 "quoted" \ backslash')) {throw 'Plain scalar output differs'}
    $evidence.checks+=@{name='unique_duplicate_and_deep_selectors_surrogate_safe_excerpt_and_plain_output';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)

    $next=Snapshot
    if (@($next.refs.psobject.Properties|Where-Object {$snap.refs.psobject.Properties.Name -contains $_.Name}).Count -ne 0) {throw 'Ref numbers were reused'}
    Request @('browser','text',$domPane,$heading) 1|Out-Null
    $entry=Find-Ref $next '#entry';$visible=Find-Ref $next '#visible';$checked=Find-Ref $next '#checked'
    Eval-Page $domPane 'document.querySelector("#entry").value="변경 한 😀";document.querySelector("#checked").checked=false;document.styleSheets[0].insertRule("#visible{opacity:0}",0);null'|Out-Null
    if (-not (Same-Text (Query 'value' $entry) '변경 한 😀') -or (Query 'is-checked' $checked) -ne $false -or (Query 'is-visible' $visible) -ne $false) {throw 'Property/CSSOM query state is stale'}
    Eval-Page $domPane 'document.querySelector("#entry").setAttribute("title","changed");null'|Out-Null
    Request @('browser','value',$domPane,$entry) 1|Out-Null
    $next=Snapshot;$heading=Find-Ref $next '#heading'
    Eval-Page $domPane 'document.querySelector("#heading").outerHTML="<h1 id=heading>교체 한 😀</h1>";null'|Out-Null
    Request @('browser','text',$domPane,$heading) 1|Out-Null
    $next=Snapshot
    if (-not (Same-Text (Query 'text' (Find-Ref $next '#heading')) '교체 한 😀')) {throw 'Fresh snapshot did not recover replacement element'}
    $evidence.checks+=@{name='latest_snapshot_only_dom_mutation_rejects_old_refs_and_property_queries_stay_live';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)

    $old=Find-Ref $next '#heading'
    $second=(Request @('browser','open',($origin+'/dom'),'--pane',$source.pane)).browser_pane_opened
    Wait-Page $second.pane '/dom' 'DOM 한글 한 é 😀'|Out-Null
    Request @('browser','text',$domPane,$old) 1|Out-Null
    $other=Snapshot
    Request @('browser','text',$domPane,$old) 1|Out-Null
    Request @('focus-tab',$opened.surface)|Out-Null
    Request @('browser','text',$domPane,$old) 1|Out-Null
    $next=Snapshot;$stable=Find-Ref $next '#heading'
    Request @('move-tab',$opened.surface,'--to-pane',$source.pane)|Out-Null;$script:domPane=$source.pane
    if (-not (Same-Text (Query 'text' $stable) '교체 한 😀')) {throw 'Stable visible surface move lost ref identity'}
    $current=(Tree).surfaces|Where-Object {$_.id -eq $terminal.id}
    if ($current.pid -ne $terminal.pid -or -not $current.running) {throw 'DOM automation changed source terminal process'}
    $evidence.checks+=@{name='cross_tab_refs_rejected_hide_invalidates_and_visible_move_keeps_surface_identity';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)

    Eval-Page $domPane 'history.pushState({},"","/dom?new=한글");null'|Out-Null
    Request @('browser','text',$domPane,$stable) 1|Out-Null
    $next=Snapshot;$old=Find-Ref $next '#heading'
    Request @('browser','reload',$domPane)|Out-Null;Wait-Page $domPane '/dom' 'DOM 한글 한 é 😀'|Out-Null
    Request @('browser','text',$domPane,$old) 1|Out-Null
    $next=Snapshot;$old=Find-Ref $next '#heading'
    Request @('browser','navigate',$domPane,($origin+'/dom'))|Out-Null;Wait-Page $domPane '/dom' 'DOM 한글 한 é 😀'|Out-Null
    Request @('browser','text',$domPane,$old) 1|Out-Null
    $plain=Plain @('browser','snapshot',$domPane)
    if (-not $plain.Contains('[ref=e') -or -not $plain.Contains('한글')) {throw 'Plain snapshot missing readable refs'}
    $evidence.checks+=@{name='history_url_reload_navigation_invalidate_old_refs_and_plain_snapshot_is_readable';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)

    $next=Snapshot;$old=Find-Ref $next '#heading'
    Request @('browser','count',$domPane,'[') 1|Out-Null
    Request @('browser','text',$domPane,'#heading') 1|Out-Null
    Request @('browser','attr',$domPane,$old,('a'*257)) 1|Out-Null
    Eval-Page $domPane 'document.querySelector("#heading").textContent="a".repeat(3999)+"😀";null'|Out-Null
    $clipped=Snapshot
    if (-not (Same-Text $clipped.page.text ('a'*3999))) {throw 'Page excerpt split an emoji surrogate pair'}
    foreach ($suffix in @('한','é','👨‍👩‍👧‍👦')) {
        $literal=ConvertTo-Json -Compress -InputObject $suffix
        Eval-Page $domPane ('document.querySelector("#heading").textContent="a".repeat(3999)+'+$literal+';null')|Out-Null
        $clipped=Snapshot
        if (-not (Same-Text $clipped.page.text ('a'*3999))) {throw 'Page excerpt split a grapheme'}
    }
    Eval-Page $domPane 'document.querySelector("#heading").textContent="한".repeat(50000);null'|Out-Null
    $large=Snapshot
    Request @('browser','text',$domPane,(Find-Ref $large '#heading')) 1|Out-Null
    Fresh-Page
    Eval-Page $domPane 'const fragment=document.createDocumentFragment();for(let i=0;i<10001;i++)fragment.append(document.createElement("div"));document.body.append(fragment);null'|Out-Null
    Request @('browser','snapshot',$domPane) 1|Out-Null
    Request @('browser','text',$domPane,$old) 1|Out-Null
    Fresh-Page
    Eval-Page $domPane 'const fragment=document.createDocumentFragment();for(let i=0;i<2050;i++){const el=document.createElement("button");el.id="limit-"+i;el.textContent="limit";fragment.append(el);}document.body.append(fragment);null'|Out-Null
    Request @('browser','snapshot',$domPane) 1|Out-Null
    Fresh-Page
    Eval-Page $domPane 'const fragment=document.createDocumentFragment();for(let i=0;i<1900;i++){const el=document.createElement("button");el.id="a".repeat(800)+i;el.textContent="limit";fragment.append(el);}document.body.append(fragment);null'|Out-Null
    Request @('browser','snapshot',$domPane) 1|Out-Null
    Fresh-Page
    $final=Snapshot
    if (-not (Same-Text (Query 'text' (Find-Ref $final '#heading')) '한글 한 é 😀 "quoted" \ backslash')) {throw 'Oversize failure damaged later snapshots'}
    $evidence.checks+=@{name='invalid_selectors_attributes_and_oversized_dom_results_fail_without_reusing_refs';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)
    Tree|Out-Null
    $evidence.status='passed_background_browser_dom_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
    if ($pipeName -and $process -and -not $process.HasExited) {try {Request @('quit','--discard-state')|Out-Null} catch {}}
    if ($process) {if (-not $process.HasExited -and -not $process.WaitForExit(10000)) {$process.Kill();[CliProbe]::WaitAfterKill($process)};if ($stderr -and $stderr.Wait(3000)) {$evidence.hostStderr=([CliProbe]::Output($stderr))};$evidence.hostExitCode=$process.ExitCode;$process.Dispose()}
    $fixture.Dispose();$evidence.hosts=$hosts;$evidence.shells=$shells;$evidence.finished=(Get-Date).ToString('o')
    if ($evidence.status -eq 'failed') { $evidence|ConvertTo-Json -Depth 12|Set-Content -Encoding UTF8 (Join-Path $directory 'native-browser-dom-background.json') }
    if ($evidence.status -eq 'failed') { Write-Output ('Evidence: '+$directory) }
}
[ordered]@{status=$evidence.status;checks=$evidence.checks.Count}|ConvertTo-Json -Compress