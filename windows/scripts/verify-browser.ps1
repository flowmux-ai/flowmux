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
Add-Type -Path (Join-Path $PSScriptRoot 'OptionsFixture.cs'),(Join-Path $PSScriptRoot 'FindFixture.cs')
Add-Type -AssemblyName System.Drawing
Add-Type -ReferencedAssemblies System.Drawing -Path (Join-Path $PSScriptRoot 'ChromeFixture.cs')
$directory=Join-Path $(if ($env:FLOWMUX_TEST_ARTIFACT_ROOT) { $env:FLOWMUX_TEST_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\dist\evidence' }) ('browser-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$fixture=New-Object BrowserFixture;$origin=$fixture.Origin
$pipeName=$null;$process=$null;$hosts=@();$shells=@()
$evidence=[ordered]@{started=(Get-Date).ToString('o');mode='background';checks=@();clipboardAccess=$false;desktopInput=$false;externalSites=$false;unicodeComparison='ordinal'}
function Request([string[]]$Arguments,[int]$Exit=0) {
    if (-not $script:pipeName) {throw 'Owned pipe required'}
    $p=[CliProbe]::Start($cli,(@('--pipe',$script:pipeName,'--json')+$Arguments),$directory,$directory)
    try {
        $out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
        if (-not $p.WaitForExit(5000)) {$p.Kill();[CliProbe]::WaitAfterKill($p);throw 'Owned CLI timed out; not retried'}
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
            $read=$reader.ReadLineAsync();if (-not $read.Wait(5000)) {throw 'Owned browser fixture request timed out; not retried'}
            return ($read.Result|ConvertFrom-Json)
        } finally {$writer.Dispose();$reader.Dispose()}
    } finally {$stream.Dispose()}
}
function Tree {
    $tree=Request @('tree');$window=[IntPtr]([long]$tree.window_handle)
    if (-not $tree.background_testing -or [CliProbe]::IsWindowVisible($window) -or [CliProbe]::GetForegroundWindow() -eq $window) {throw 'Owned host became visible or foreground'}
    foreach ($frame in $tree.detached_windows) {
        $native=[OptionsFixture]::Describe([long]$frame.window_handle,$process.Id)
        if ($native.Owner -ne 0 -or $frame.native_visible -ne $false) {throw 'Detached browser frame is owned or visible'}
    }
    foreach ($browser in $tree.browsers) {
        if ([CliProbe]::IsWindowVisible([IntPtr]([long]$browser.view_handle)) -or [CliProbe]::IsWindowVisible([IntPtr]([long]$browser.chrome_handle))) {throw 'Owned browser became visible'}
        $holder=$browser.holder
        $frames=@($tree.detached_windows|Where-Object {$_.surface -eq $browser.id});if ($frames.Count -gt 1) {throw 'Browser has multiple detached frames'}
        $expectedRoot=$tree.window_handle;if ($frames.Count -eq 1) {$expectedRoot=$frames[0].window_handle}
        if (-not $holder.window -or $holder.window -eq $browser.view_handle -or $holder.window -eq $browser.chrome_handle -or $holder.parent -ne $expectedRoot -or $holder.root -ne $expectedRoot -or $holder.native_visible -ne $false -or $browser.chrome.parent -ne $holder.window -or [OptionsFixture]::Parent([long]$holder.window,$process.Id) -ne $expectedRoot -or [CliProbe]::IsWindowVisible([IntPtr]([long]$holder.window))) {throw 'Browser holder hierarchy or hidden state differs'}
    }
    return $tree
}
function Start-Owned([string[]]$Launch) {
    $script:pipeName=$null;$utc=[DateTime]::UtcNow
    $script:process=[CliProbe]::Start($gui,$Launch,$directory,$directory)
    $script:hosts+= $process.Id
    $script:stdout=$process.StandardOutput.ReadToEndAsync();$script:stderr=$process.StandardError.ReadToEndAsync()
    $file=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($process.Id).json";$deadline=(Get-Date).AddSeconds(5)
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
function Leaves($Node) {if ($Node.content) {$Node} else {Leaves $Node.first;Leaves $Node.second}}
function Location($Tree,[string]$Surface) {
    $panes=@($Tree.workspaces|ForEach-Object {Leaves $_.root}|Where-Object {@($_.content.surfaces.id) -contains $Surface})
    if ($panes.Count -ne 1) {throw 'Browser surface location is missing or ambiguous'}
    return $panes[0].id
}
function Check-Find([string]$Pane,[long]$Owner,[string]$Query) {
    $find=(Request @('browser','status',$Pane)).find
    if (-not $find.panel_handle -or -not $find.panel_query_handle -or $find.panel_owner -ne $Owner -or ([OptionsFixture]::Describe([long]$find.panel_handle,$process.Id)).Owner -ne $Owner -or -not (Same-Text ([FindFixture]::ReadText([long]$find.panel_query_handle)) $Query)) {throw 'Browser find owner or raw native query changed'}
    return $find
}
function Check-Stable($Before,$After) {
    foreach ($field in @('id','view_handle','chrome_handle','address_handle','url','zoom','generation')) {if ($Before.$field -cne $After.$field) {throw ('Browser reparent changed '+$field)}}
    if ($Before.holder.window -ne $After.holder.window) {throw 'Browser reparent replaced its holder'}
    Check-Toolbar $After|Out-Null
}
function Eval-Page([string]$Pane,[string]$Source) {return (Request @('browser','eval',('pane:'+$Pane),$Source)).result}
function Wait-Page([string]$Pane,[string]$Suffix,[string]$Title) {
    $deadline=(Get-Date).AddSeconds(5)
    do {
        $status=Request @('browser','status',('pane:'+$Pane))
        if (-not $status.loading -and $status.url.Contains($Suffix) -and (Same-Text $status.title $Title)) {
            if ((Eval-Page $Pane 'document.readyState') -eq 'complete') {return $status}
        }
        if ((Get-Date) -gt $deadline) {throw ('Page did not load: '+($status|ConvertTo-Json -Compress))}
        Start-Sleep -Milliseconds 50
    } while ($true)
}
function Check-Toolbar($Status) {
    $chrome=@([ChromeFixture]::Read([long]$Status.chrome_handle,$process.Id));$shown=@($chrome|Where-Object Shown)
    $dpi=[ChromeFixture]::GetDpiForWindow([IntPtr]([long]$Status.chrome_handle));$area=[ChromeFixture]::Size([long]$Status.chrome_handle,$process.Id)
    if($Status.chrome.rows -ne 1 -or [Math]::Abs($area[1]-40*$dpi/96) -gt 1){throw 'Browser toolbar is not one40-DIP row'}
    $holder=$Status.holder.bounds;$viewport=$Status.bounds
    if (-not $holder -or -not $viewport -or $Status.chrome.parent -ne $Status.holder.window -or $holder.width -ne $area[0] -or $viewport.x -ne $holder.x -or $viewport.y -ne ($holder.y+$area[1]) -or $viewport.width -ne $holder.width -or $viewport.height -ne [Math]::Max(1,$holder.height-$area[1])) {throw 'Browser holder, toolbar and root-coordinate viewport geometry differ'}
    foreach($c in $shown){if($c.Y -lt 0 -or $c.Y+$c.Height -gt $area[1] -or $c.X -lt 0 -or $c.X+$c.Width -gt $area[0]){throw 'Browser toolbar control escapes its row'}}
    $ordered=@($shown|Sort-Object X);for($i=1;$i -lt $ordered.Count;$i++){if($ordered[$i].X -lt $ordered[$i-1].X+$ordered[$i-1].Width){throw 'Browser toolbar controls overlap'}}
    if(@($shown|Where-Object Class -eq 'Edit').Count -ne 1 -or @($shown|Where-Object Handle -eq $Status.chrome.tools_handle).Count -ne 1){throw 'Address or tools entry missing'}
    $reload=@($chrome|Where-Object Text -ceq 'Reload')[0];$stop=@($chrome|Where-Object Text -ceq 'Stop')[0]
    if($reload.Width -ne [Math]::Round(30*$dpi/96) -or $reload.X -ne $stop.X -or $reload.Y -ne $stop.Y -or $reload.Width -ne $stop.Width -or $reload.Height -ne $stop.Height){throw 'Reload/Stop geometry changed during metadata refresh'}
    return $chrome
}
try {
    $initial=Start-Owned @('--new-window','--shell=cmd','--cwd',$directory);$source=(Request @('identify'));$terminal=$initial.surfaces[0]
    $script:shells+=$terminal.pid
    $first=(Request @('browser','open',($origin+'/one'),'--pane',$source.pane)).browser_pane_opened
    if ($first.placement_strategy -ne 'split_right' -or $first.pane -eq $source.pane) {throw 'Wrong browser placement'}
    $oneTitle='첫째 한글 한 é 😀';$twoTitle='둘째 한글 한 é 😀'
    $loaded=Wait-Page $first.pane '/one' $oneTitle
    $page=Eval-Page $first.pane '({text:document.querySelector("#label").textContent,ipc:typeof window.ipc,host:typeof window.flowmuxHost,identity:typeof window.__flowmuxIdentity,settings:typeof window.__flowmuxSettings})'
    if (-not (Same-Text $page.text $oneTitle) -or $page.ipc -ne 'undefined' -or $page.host -ne 'undefined' -or $page.identity -ne 'undefined' -or $page.settings -ne 'undefined') {throw 'Unicode page or bridge isolation differs'}
    if ([BrowserFixture]::ReadText($loaded.address_handle) -ne ($origin+'/one')) {throw 'Native address differs'}
    $evidence.checks+=@{name='native_webview_unicode_dom_address_and_no_terminal_bridge';passed=$true;page=$page}
    $evidence.checks+=@{name='single_row_native_browser_geometry_and_tools_entry';passed=$true;controls=(Check-Toolbar $loaded);diagnostics=$loaded.chrome}
    $root=Tree
    [ChromeFixture]::Resize([long]$root.window_handle,$process.Id,400,500)
    Request @('resize-pane',$first.pane,'--ratio','0.7')|Out-Null
    $narrow=Request @('browser','status',$first.pane);$narrowControls=Check-Toolbar $narrow
    Request @('resize-pane',$first.pane,'--ratio','0.4')|Out-Null
    $compact=Request @('browser','status',$first.pane);$compactControls=Check-Toolbar $compact
    [ChromeFixture]::Resize([long]$root.window_handle,$process.Id,1184,761)
    Request @('resize-pane',$first.pane,'--ratio','0.5')|Out-Null
    Check-Toolbar (Request @('browser','status',$first.pane))|Out-Null
    $evidence.checks+=@{name='narrow_compact_and_restored_browser_toolbar_has_no_overlap';passed=$true;narrow=$narrowControls;compact=$compactControls}

    Write-Host ("[check] passed "+$evidence.checks[-1].name)
    Request @('browser','navigate',$first.pane,($origin+'/한글?q=한#😀'))|Out-Null
    $unicode=Wait-Page $first.pane '/%ED%95%9C%EA%B8%80' $oneTitle
    if ([BrowserFixture]::ReadText($unicode.address_handle) -ne $unicode.url -or -not (Same-Text (Eval-Page $first.pane 'decodeURI(location.href)') ($origin+'/한글?q=한#😀'))) {throw 'Unicode address changed codepoints'}
    Request @('browser','navigate',$first.pane,($origin+'/one'))|Out-Null;Wait-Page $first.pane '/one' $oneTitle|Out-Null
    Request @('browser','navigate',$first.pane,($origin+'/two'))|Out-Null
    $two=Wait-Page $first.pane '/two' $twoTitle
    if (-not $two.can_go_back) {throw 'History back not available'}
    Request @('browser','back',$first.pane)|Out-Null
    $back=Wait-Page $first.pane '/one' $oneTitle
    if (-not $back.can_go_forward) {throw 'History forward not available'}
    Request @('browser','forward',$first.pane)|Out-Null;Wait-Page $first.pane '/two' $twoTitle|Out-Null
    $load=Eval-Page $first.pane 'window.fixtureLoad'
    Request @('browser','reload',$first.pane)|Out-Null;Wait-Page $first.pane '/two' $twoTitle|Out-Null
    if ((Eval-Page $first.pane 'window.fixtureLoad') -eq $load) {throw 'Reload did not create a new document'}
    Request @('browser','zoom',$first.pane,'1.25')|Out-Null
    if ((Request @('browser','status',$first.pane)).zoom -ne 1.25) {throw 'Zoom did not change'}
    Request @('browser','zoom',$first.pane,'4') 1|Out-Null
    Request @('browser','navigate',$first.pane,($origin+'/slow'))|Out-Null
    Request @('browser','eval',$first.pane,'document.title') 1|Out-Null
    Request @('browser','stop',$first.pane)|Out-Null
    Request @('browser','navigate',$first.pane,($origin+'/one'))|Out-Null;Wait-Page $first.pane '/one' $oneTitle|Out-Null
    Request @('browser','navigate',$first.pane,($origin+'/redirect'))|Out-Null;Wait-Page $first.pane '/two' $twoTitle|Out-Null
    Request @('browser','navigate',$first.pane,($origin+'/fail'))|Out-Null
    $deadline=(Get-Date).AddSeconds(5)
    do {
        $failed=Request @('browser','status',$first.pane)
        if (-not $failed.loading -and $failed.navigation_error) {break}
        if ((Get-Date) -gt $deadline) {throw 'Failed navigation was not reported'}
        Start-Sleep -Milliseconds 50
    } while ($true)
    Request @('browser','navigate',$first.pane,($origin+'/one'))|Out-Null;Wait-Page $first.pane '/one' $oneTitle|Out-Null
    Check-Toolbar (Request @('browser','status',$first.pane))|Out-Null
    $evidence.checks+=@{name='native_history_reload_stop_redirect_failure_recovery_and_bounded_zoom';passed=$true;failure=$failed.navigation_error}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)
    foreach ($url in @('file:///C:/private','javascript:1','data:text/html,no','http://flowmux-terminal.localhost/')) {Request @('browser','navigate',$first.pane,$url) 1|Out-Null}
    Eval-Page $first.pane 'location.href="http://flowmux-terminal.localhost/";null'|Out-Null
    Start-Sleep -Milliseconds 200
    if ((Request @('browser','url',$first.pane)).url -ne ($origin+'/one')) {throw 'Page navigation reached reserved terminal origin'}
    Request @('browser','eval',$first.pane,'Promise.resolve(1)') 1|Out-Null
    Request @('browser','eval',$first.pane,'throw new Error("한글 오류")') 1|Out-Null
    Request @('browser','eval',$first.pane,'"한".repeat(100000)') 1|Out-Null
    $popupResult=Eval-Page $first.pane 'window.fixturePopup=window.open("/two");({returned:window.fixturePopup!==null})'
    if (-not $popupResult.returned) {throw 'Native popup did not return a WindowProxy'}
    $deadline=(Get-Date).AddSeconds(5)
    do {
        $popupTree=Tree;$children=@($popupTree.browsers|Where-Object {$_.popup_opener -eq $first.surface})
        if ($children.Count -gt 1 -or @($popupTree.browsers).Count -gt 2) {throw 'One popup request created multiple browser tabs'}
        if ($children.Count -eq 1 -and $popupTree.popup.pending -eq 0) {break}
        if ((Get-Date) -gt $deadline) {throw 'Native popup tab did not attach'}
        Start-Sleep -Milliseconds 50
    } while ($true)
    $child=$children[0];$popupStatus=Request @('browser','status',$first.pane)
    if ($popupStatus.id -ne $child.id -or $popupStatus.popup_opener -ne $first.surface -or @($popupTree.browsers).Count -ne 2) {throw 'Popup did not become the single child tab in its source pane'}
    Wait-Page $first.pane '/two' $twoTitle|Out-Null
    if (-not (Eval-Page $first.pane 'window.opener!==null && window.opener.fixturePopup===window')) {throw 'Native popup lost its opener or returned WindowProxy'}
    Request @('close-tab',$child.id)|Out-Null
    $afterPopup=Tree;$original=Request @('browser','status',$first.pane)
    $stableTerminal=@($afterPopup.surfaces|Where-Object {$_.id -eq $terminal.id})
    if (@($afterPopup.browsers).Count -ne 1 -or $original.id -ne $first.surface -or $original.view_handle -ne $loaded.view_handle -or $original.url -ne ($origin+'/one') -or -not (Eval-Page $first.pane 'window.fixturePopup.closed')) {throw 'Popup close did not restore the original browser tab'}
    if ($stableTerminal.Count -ne 1 -or $stableTerminal[0].pid -ne $terminal.pid -or -not $stableTerminal[0].running) {throw 'Popup lifecycle changed the original terminal identity or process'}
    foreach ($args in @(@('read-screen','--surface',$first.surface),@('selection','--surface',$first.surface,'read'),@('send-key','Enter','--surface',$first.surface),@('browser','url',$source.pane))) {Request $args 1|Out-Null}
    if ((Request @('identify')).shell) {throw 'Browser advertised a terminal shell'}
    Tree|Out-Null
    $evidence.checks+=@{name='forbidden_urls_popup_tab_lifecycle_script_errors_and_terminal_target_rejection';passed=$true;popup=@{surface=$child.id;opener=$first.surface;pane=$first.pane;returnedWindowProxy=$popupResult.returned;closed=$true}}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)
    $second=(Request @('browser','open',($origin+'/two'),'--pane',$source.pane)).browser_pane_opened
    if ($second.placement_strategy -ne 'reuse_right_sibling' -or $second.pane -ne $first.pane) {throw 'Right browser pane not reused'}
    Wait-Page $second.pane '/two' $twoTitle|Out-Null
    $down=(Request @('browser','open',($origin+'/one'),'--pane',$source.pane,'--down')).browser_pane_opened
    if ($down.placement_strategy -ne 'split_down' -or $down.pane -eq $first.pane) {throw 'Down split differs'}
    Wait-Page $down.pane '/one' $oneTitle|Out-Null
    Request @('focus-tab',$first.surface)|Out-Null
    Eval-Page $first.pane 'window.retained="한글 한 é 😀";localStorage.setItem("browser-persist",window.retained);null'|Out-Null
    $beforeMove=Request @('browser','status',$first.pane);$view=$beforeMove.view_handle
    Request @('move-tab',$first.surface,'--to-pane',$source.pane)|Out-Null
    if (-not (Same-Text (Eval-Page $source.pane 'window.retained') '한글 한 é 😀') -or (Request @('browser','status',$source.pane)).view_handle -ne $view) {throw 'Browser move recreated document or view'}
    $afterMove=Request @('browser','status',$source.pane)
    if ($beforeMove.holder.window -ne $afterMove.holder.window -or $beforeMove.chrome_handle -ne $afterMove.chrome_handle) {throw 'Browser move replaced its holder or native toolbar'}
    Check-Toolbar $afterMove|Out-Null
    Request @('browser','find-show',$source.pane)|Out-Null
    if (-not (Request @('browser','find',$source.pane,$oneTitle)).found) {throw 'Known Korean browser text was not found before detachment'}
    $mainFind=Check-Find $source.pane ([long]$root.window_handle) $oneTitle
    $draftQuery='미실행 한 é 😀'
    [OptionsFixture]::SetText([long]$mainFind.panel_handle,[long]$mainFind.panel_query_handle,$process.Id,$draftQuery)
    $detachedReply=Request @('detach-tab',$first.surface);$detachedTree=Tree
    $frames=@($detachedTree.detached_windows|Where-Object {$_.surface -eq $first.surface})
    if ($frames.Count -ne 1 -or $frames[0].window_handle -ne $detachedReply.window_handle) {throw 'Browser detachment did not create exactly one frame'}
    $browserFrame=$frames[0];$detachedPane=Location $detachedTree $first.surface
    $detachedStatus=Request @('browser','status',$detachedPane);Check-Stable $afterMove $detachedStatus
    if (-not (Same-Text (Eval-Page $detachedPane 'window.retained') '한글 한 é 😀')) {throw 'Detached browser lost its Korean DOM state'}
    $detachedFind=Check-Find $detachedPane ([long]$browserFrame.window_handle) $draftQuery
    if ($detachedFind.panel_handle -eq $mainFind.panel_handle -or -not (Same-Text $detachedFind.query $oneTitle)) {throw 'Detach did not recreate find owner while preserving executed query and native draft separately'}
    Request @('downloads','show')|Out-Null;$downloads=Request @('downloads','list')
    if (-not $downloads.panel_handle -or $downloads.panel_owner -ne $browserFrame.window_handle -or ([OptionsFixture]::Describe([long]$downloads.panel_handle,$process.Id)).Owner -ne $browserFrame.window_handle) {throw 'Downloads panel did not use detached browser owner'}
    $popupResult=Eval-Page $detachedPane 'window.fixtureDetachedPopup=window.open("/two");window.fixtureDetachedPopup!==null'
    if (-not $popupResult) {throw 'Detached browser popup did not return a WindowProxy'}
    $deadline=(Get-Date).AddSeconds(5)
    do {
        $popupTree=Tree;$children=@($popupTree.browsers|Where-Object {$_.popup_opener -eq $first.surface})
        if ($children.Count -gt 1 -or @($popupTree.browsers).Count -gt (@($detachedTree.browsers).Count+1)) {throw 'Detached popup created duplicate browsers'}
        if ($children.Count -eq 1 -and $popupTree.popup.pending -eq 0) {break}
        if ((Get-Date) -gt $deadline) {throw 'Detached popup did not attach within five seconds'}
        Start-Sleep -Milliseconds 30
    } while ($true)
    $popupChild=$children[0];$childPane=Location $popupTree $popupChild.id
    $childFrames=@($popupTree.detached_windows|Where-Object {$_.surface -eq $popupChild.id})
    $openerTabs=@($popupTree.workspaces|Where-Object {$_.id -eq $browserFrame.workspace}|ForEach-Object {Leaves $_.root}|ForEach-Object {$_.content.surfaces})
    if ($childFrames.Count -ne 1 -or $childFrames[0].window_handle -eq $browserFrame.window_handle -or $openerTabs.Count -ne 1 -or $openerTabs[0].id -ne $first.surface) {throw 'Detached popup was added to its opener instead of an independent single-surface window'}
    $childTabs=@($popupTree.workspaces|Where-Object {$_.id -eq $childFrames[0].workspace}|ForEach-Object {Leaves $_.root}|ForEach-Object {$_.content.surfaces})
    if ($childTabs.Count -ne 1 -or $childTabs[0].id -ne $popupChild.id) {throw 'Independent popup contains extra tabs'}
    Wait-Page $childPane '/two' $twoTitle|Out-Null
    $relation=Eval-Page $childPane '({related:window.opener!==null&&window.opener.fixtureDetachedPopup===window,raw:document.querySelector("#label").textContent=window.opener.retained})'
    if (-not $relation.related -or -not (Same-Text $relation.raw '한글 한 é 😀')) {throw 'Detached popup lost WindowProxy/opener or Korean DOM relationship'}
    Eval-Page $detachedPane 'window.fixtureDetachedPopup.close();null'|Out-Null
    $deadline=(Get-Date).AddSeconds(5)
    do {
        $closedPopup=Tree
        if (@($closedPopup.browsers|Where-Object {$_.id -eq $popupChild.id}).Count -eq 0 -and @($closedPopup.detached_windows|Where-Object {$_.surface -eq $popupChild.id}).Count -eq 0) {break}
        if ((Get-Date) -gt $deadline) {throw 'WindowProxy.close did not remove the detached popup'}
        Start-Sleep -Milliseconds 30
    } while ($true)
    if (@($closedPopup.browsers).Count -ne @($detachedTree.browsers).Count -or -not (Eval-Page $detachedPane 'window.fixtureDetachedPopup.closed')) {throw 'Detached popup close left a blank replacement or stale WindowProxy'}
    Request @('move-tab',$first.surface,'--to-pane',$source.pane)|Out-Null
    $reattached=Tree;$returned=Request @('browser','status',$source.pane);Check-Stable $afterMove $returned
    if (@($reattached.detached_windows).Count -ne 0 -or -not (Same-Text (Eval-Page $source.pane 'window.retained') '한글 한 é 😀')) {throw 'Browser reattach lost its DOM or left detached windows'}
    $returnedFind=Check-Find $source.pane ([long]$root.window_handle) $draftQuery
    if ($returnedFind.panel_handle -eq $detachedFind.panel_handle) {throw 'Reattach kept the find panel owned by the destroyed frame'}
    Request @('downloads','show')|Out-Null;$downloads=Request @('downloads','list')
    if ($downloads.panel_owner -ne $root.window_handle -or ([OptionsFixture]::Describe([long]$downloads.panel_handle,$process.Id)).Owner -ne $root.window_handle) {throw 'Downloads owner was not restored to main window'}
    Request @('browser','find-close',$source.pane)|Out-Null
    $evidence.checks+=@{name='browser_detach_popup_windowproxy_korean_dom_find_download_owners_and_reattach';passed=$true;surface=$first.surface;popup=$popupChild.id;retainedView=$returned.view_handle}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)
    $mixedSave=Request @('save-state')
    $mixed=Get-Content -Raw -Encoding UTF8 $mixedSave.path|ConvertFrom-Json
    if (@($mixed.screens.psobject.Properties).Count -ne 1 -or $mixed.screens.($first.surface) -or $mixed.shells.($first.surface)) {throw 'Mixed checkpoint confused browser and terminal'}
    $stable=(Tree).surfaces|Where-Object {$_.id -eq $terminal.id}
    if ($stable.pid -ne $terminal.pid -or -not $stable.running) {throw 'Browser work restarted source shell'}
    Request @('read-screen','--surface',$terminal.id)|Out-Null
    $evidence.checks+=@{name='right_reuse_down_split_mixed_pane_move_keeps_webview_and_source_process';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)
    # Calling inactive terminals must retain their own cwd when another tab is active.
    $otherCwd=Join-Path $directory 'other 한글';[IO.Directory]::CreateDirectory($otherCwd)|Out-Null
    Request @('new-tab','--cwd',$otherCwd,'--shell=cmd')|Out-Null;$otherTerminal=Request @('identify')
    $raw=Raw-Request @{method='new_tab';caller_surface=$terminal.id;shell='cmd'}
    if ($raw.error) {throw ('Inactive caller failed: '+$raw.error)}
    $fromInactive=Request @('identify')
    if ($fromInactive.cwd -ne $directory -or $fromInactive.surface -eq $otherTerminal.surface) {throw 'Inactive terminal inherited another active tab cwd'}
    $deadline=(Get-Date).AddSeconds(5)
    do {
        $children=@((Tree).surfaces|Where-Object {$_.id -eq $fromInactive.surface -or $_.id -eq $otherTerminal.surface})
        if (@($children|Where-Object {-not $_.ready -or -not $_.pid}).Count -eq 0) {break}
        if ((Get-Date) -gt $deadline) {throw 'Owned cwd terminals did not start'}
        Start-Sleep -Milliseconds 50
    } while ($true)
    $script:shells+=@($children|ForEach-Object {$_.pid})
    Request @('close-tab',$fromInactive.surface)|Out-Null;Request @('close-tab',$otherTerminal.surface)|Out-Null
    Request @('focus-tab',$first.surface)|Out-Null
    $evidence.checks+=@{name='inactive_calling_terminal_keeps_own_cwd_in_mixed_pane';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)
    # A terminal split from a browser must use the configured shell rather than indexing browser shell metadata.
    Request @('split','vertical')|Out-Null
    $new=(Request @('identify'));$deadline=(Get-Date).AddSeconds(5)
    do {
        $newSurface=(Tree).surfaces|Where-Object {$_.id -eq $new.surface}
        if ($newSurface.ready -and $newSurface.pid) {break}
        if ((Get-Date) -gt $deadline) {throw 'Default terminal split from browser did not start'}
        Start-Sleep -Milliseconds 50
    } while ($true)
    $script:shells+=$newSurface.pid
    if ($newSurface.shell.program -ne 'powershell') {throw 'Browser split did not use configured default shell'}
    Request @('close-tab',$new.surface)|Out-Null
    Request @('close-tab',$terminal.id)|Out-Null
    Request @('close-tab',$second.surface)|Out-Null
    Request @('close-tab',$down.surface)|Out-Null
    $only=Tree
    if (@($only.surfaces).Count -ne 0 -or @($only.browsers).Count -ne 1) {throw 'Browser-only layout differs'}
    $saved=Request @('save-state');$checkpoint=Get-Content -Raw -Encoding UTF8 $saved.path|ConvertFrom-Json
    if (@($checkpoint.screens.psobject.Properties).Count -ne 0 -or @($checkpoint.shells.psobject.Properties).Count -ne 0) {throw 'Browser was saved as terminal'}
    Request @('quit')|Out-Null
    if (-not $process.WaitForExit(5000)) {throw 'Browser-only host did not close'}
    $process.Dispose();$process=$null
    $restored=Start-Owned @('--restore-window',$saved.window)
    if (@($restored.surfaces).Count -ne 0 -or @($restored.browsers).Count -ne 1 -or $restored.browsers[0].id -ne $first.surface) {throw 'Browser-only restore lost identity'}
    $restoredPane=(Request @('identify')).pane
    Wait-Page $restoredPane '/one' $oneTitle|Out-Null
    if (-not (Same-Text (Eval-Page $restoredPane 'localStorage.getItem("browser-persist")') '한글 한 é 😀')) {throw 'Isolated browser profile did not persist'}
    $evidence.checks+=@{name='browser_only_checkpoint_restart_and_separate_profile_persistence';passed=$true;window=$saved.window;surface=$first.surface}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)
    Eval-Page $restoredPane 'window.survivor="분리 생존 한 é 😀";window.survivor'|Out-Null
    Request @('browser','find-show',$restoredPane)|Out-Null
    if (-not (Request @('browser','find',$restoredPane,$oneTitle)).found) {throw 'Restored browser find failed before sole-tab detachment'}
    $beforeSole=Request @('browser','status',$restoredPane)
    Request @('detach-tab',$first.surface)|Out-Null
    $sole=Tree;$solePane=Location $sole $first.surface;$soleFrames=@($sole.detached_windows)
    if ($soleFrames.Count -ne 1 -or @($sole.browsers).Count -ne 1 -or @($sole.surfaces).Count -ne 0) {throw 'Sole browser detachment created replacement surfaces'}
    $soleFrame=$soleFrames[0];$soleStatus=Request @('browser','status',$solePane);Check-Stable $beforeSole $soleStatus
    Check-Find $solePane ([long]$soleFrame.window_handle) $oneTitle|Out-Null
    [FindFixture]::PostClose([long]$sole.window_handle,$process.Id)
    $deadline=(Get-Date).AddSeconds(5)
    do {
        if ($process.HasExited) {throw 'Closing main window terminated the detached browser'}
        $survived=Tree
        if ($survived.main_closed -and -not $survived.state.saving) {break}
        if ((Get-Date) -gt $deadline) {throw 'Main window did not close while keeping its browser alive'}
        Start-Sleep -Milliseconds 30
    } while ($true)
    if (@($survived.detached_windows).Count -ne 1 -or @($survived.browsers).Count -ne 1 -or @($survived.surfaces).Count -ne 0) {throw 'Main close changed detached browser ownership'}
    $afterMainClose=Request @('browser','status',$solePane);Check-Stable $soleStatus $afterMainClose
    if (-not (Same-Text (Eval-Page $solePane 'window.survivor') '분리 생존 한 é 😀')) {throw 'Main close recreated or destroyed the detached DOM'}
    $toolbar=@([ChromeFixture]::Read([long]$afterMainClose.chrome_handle,$process.Id));$go=@($toolbar|Where-Object Text -ceq 'Go')
    if ($go.Count -ne 1) {throw 'Detached native Go control is missing'}
    [OptionsFixture]::SetText([long]$afterMainClose.chrome_handle,[long]$afterMainClose.address_handle,$process.Id,($origin+'/one#after-main-close'))
    [OptionsFixture]::Click([long]$afterMainClose.chrome_handle,[long]$go[0].Handle,$process.Id)
    $controlled=Wait-Page $solePane '#after-main-close' $oneTitle
    if ($controlled.view_handle -ne $soleStatus.view_handle -or -not (Same-Text (Eval-Page $solePane 'window.survivor') '분리 생존 한 é 😀')) {throw 'Native browser controls failed or recreated the surviving same-document state'}
    Request @('browser','find-show',$solePane)|Out-Null
    if (-not (Request @('browser','find',$solePane,$oneTitle)).found) {throw 'Find failed after main window close'}
    Check-Find $solePane ([long]$soleFrame.window_handle) $oneTitle|Out-Null
    Request @('downloads','show')|Out-Null;$downloads=Request @('downloads','list')
    if ($downloads.panel_owner -ne $soleFrame.window_handle -or ([OptionsFixture]::Describe([long]$downloads.panel_handle,$process.Id)).Owner -ne $soleFrame.window_handle) {throw 'Downloads no longer belongs to the surviving browser'}
    $evidence.lastLiveState=@{mainClosed=$survived.main_closed;surface=$first.surface;frame=$soleFrame.window_handle;view=$controlled.view_handle;findOwner=$soleFrame.window_handle}
    [FindFixture]::PostClose([long]$soleFrame.window_handle,$process.Id)
    if (-not $process.WaitForExit(5000)) {throw 'Closing the final detached browser did not terminate the owned host'}
    if ($process.ExitCode -ne 0) {throw 'Final detached browser close exited with failure'}
    $evidence.hostExitCode=$process.ExitCode
    if ($stderr.Wait(1000)) {$evidence.hostStderr=([CliProbe]::Output($stderr))}
    $process.Dispose();$process=$null;$pipeName=$null
    $evidence.checks+=@{name='sole_browser_detach_survives_main_close_native_controls_find_and_final_frame_exit';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)
    $evidence.status='passed_background_browser_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
    if ($pipeName -and $process -and -not $process.HasExited) {try {Request @('quit','--discard-state')|Out-Null} catch {}}
    if ($process) {if (-not $process.HasExited -and -not $process.WaitForExit(5000)) {$process.Kill();[CliProbe]::WaitAfterKill($process)};if ($stderr -and $stderr.Wait(3000)) {$evidence.hostStderr=([CliProbe]::Output($stderr))};$evidence.hostExitCode=$process.ExitCode;$process.Dispose()}
    $fixture.Dispose();$evidence.hosts=$hosts;$evidence.shells=$shells;$evidence.finished=(Get-Date).ToString('o')
    if ($evidence.status -eq 'failed') {
        $evidence|ConvertTo-Json -Depth 12|Set-Content -Encoding UTF8 (Join-Path $directory 'native-browser-background.json')
    }
}
Write-Host ("[check] browser: "+$evidence.checks.Count+" groups passed")
