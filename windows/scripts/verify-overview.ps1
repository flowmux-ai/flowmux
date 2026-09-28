# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden owned overview workflow; 48s work + bounded cleanup, outer Job60s.
param([ValidateSet('workflow','dirty','browser')][string]$Case='workflow',[string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
if (-not $env:FLOWMUX_TEST_ARTIFACT_ROOT) { throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.' }
$ErrorActionPreference='Stop';$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path;$cli=Join-Path $BuildDirectory 'flowmuxctl.exe';$gui=Join-Path $BuildDirectory 'flowmux.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'OptionsFixture.cs'),(Join-Path $PSScriptRoot 'OverviewFixture.cs')
Add-Type -ReferencedAssemblies System.Drawing -Path (Join-Path $PSScriptRoot 'ChromeFixture.cs')
if($Case -eq 'browser'){Add-Type -Path (Join-Path $PSScriptRoot 'BrowserFixture.cs')}
$directory=Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT ('overview-'+[guid]::NewGuid());[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$workBudget=if($Case -eq 'browser'){45000}else{48000};$fixture=$null;$clock=[Diagnostics.Stopwatch]::StartNew();$owned=$null;$pipeName=$null;$clients=@();$shells=@();$cleaning=$false;$cleanupErrors=@();$hostOut=$null;$hostErr=$null
$evidence=[ordered]@{started=[DateTime]::UtcNow.ToString('o');mode='hidden-native-overview';case=$Case;hosts=@();checks=@();observations=@();artifacts=@();desktopInput=$false;clipboardAccess=$false;imeSimulation=$false;previewPolicy='Only logically visible, ready active pane WebViews are captured. Inactive workspace thumbnails are unsupported in this slice; errors are retained, with no synthetic substitutes.';terminalRasterLimitation='Prior hidden terminal capture showed background and cursor without terminal text; PNG success does not establish terminal glyph rendering.';deferred='Inactive workspace thumbnails, hidden terminal glyph rendering, physical keyboard/focus/IME, per-monitor DPI, accessibility and composed GPU/desktop visual acceptance are not established.'}
function Require([bool]$Condition,[string]$Message){if(-not $Condition){throw $Message}}
function Budget([int]$Maximum=5000){if($cleaning){return $Maximum};$left=$workBudget-$clock.ElapsedMilliseconds;Require ($left -gt 0) 'Overview work budget expired';return [int][Math]::Min($Maximum,$left)}
function Probe([string[]]$Arguments,[int]$Maximum=5000){
    Budget|Out-Null;$p=[CliProbe]::Start($cli,$Arguments,$directory,$directory);$script:clients+=,$p.Id;$out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
    try {Require ($p.WaitForExit((Budget $Maximum))) 'Owned CLI deadline exceeded; no retry';Require ($out.Wait(500) -and $err.Wait(500)) 'CLI output did not close';Require ($p.ExitCode -eq 0) ('CLI failed: '+[CliProbe]::Output($err));return ([CliProbe]::Output($out)|ConvertFrom-Json)}
    catch {$evidence.observations+=@{kind='cli-failure';arguments=$Arguments;stdout=[CliProbe]::Output($out);stderr=[CliProbe]::Output($err)};throw}
    finally {if(-not $p.HasExited){$p.Kill();[CliProbe]::WaitAfterKill($p)};$p.Dispose()}
}
function Request([string[]]$Arguments,[int]$Maximum=5000){Require ([bool]$pipeName) 'Explicit owned pipe required';return Probe (@('--pipe',$pipeName,'--json')+$Arguments) $Maximum}
function Tree([int]$Maximum=5000){
    $t=Request @('tree') $Maximum;Require ($t.background_testing) 'Host is not in background mode';[OptionsFixture]::Describe([long]$t.window_handle,$owned.Id)|Out-Null
    Require ($t.overview.pending_captures -le 2) 'Overview exceeded two retained native capture slots'
    if($t.overview.open -and $t.overview.capture_budget_expired -and $t.overview.capture_elapsed_ms -ge 5500){
        Require ($t.overview.queued_captures -eq 0 -and @($t.overview.cards|ForEach-Object {$_.previews}|Where-Object {$_.status -eq 'Loading preview…'}).Count -eq 0) 'Overview generation deadline left queued or loading previews after the next refresh'
    }
    return $t
}
function Identities($Tree){return (@($Tree.surfaces|Sort-Object id|ForEach-Object {$_.id.ToString()+':'+$_.pid.ToString()}) -join ',')}

function Await([scriptblock]$Condition,[int]$Maximum=5000){
    $wait=[Diagnostics.Stopwatch]::StartNew();$lastTree=$null
    do {$left=$Maximum-$wait.ElapsedMilliseconds;if($left -le 0){$evidence.observations+=@{name='condition-timeout';maximumMs=$Maximum;elapsedMs=$wait.ElapsedMilliseconds;tree=$lastTree};throw 'Overview condition exceeded deadline'}
        $t=Tree ([int][Math]::Min(5000,$left));$lastTree=$t;if(& $Condition $t){return $t};Start-Sleep -Milliseconds 20
    }while($true)
}
function Open-Overview {
    $t=Tree;$entry=@($t.chrome.controls|Where-Object {$_.kind -eq 'overview'});Require ($entry.Count -eq 1) 'Overview entry missing';[OptionsFixture]::Click([long]$t.window_handle,[long]$entry[0].handle,$owned.Id)
    return Await {param($t) $t.overview.open}
}
function Card($Tree,[string]$Workspace){$cards=@($Tree.overview.cards|Where-Object {$_.workspace -eq $Workspace});Require ($cards.Count -eq 1) 'Expected exactly one workspace card';return $cards[0]}
function Click-Card($Tree,$Card,[bool]$Close=$false){[OptionsFixture]::Click([long]$Tree.overview.viewport,[long]$(if($Close){$Card.close}else{$Card.button}),$owned.Id)}
function Record([string]$Name,$Tree){$evidence.observations+=@{name=$Name;tree=$Tree;controls=$(if($Tree.overview.open){@([ChromeFixture]::Read([long]$Tree.overview.viewport,$owned.Id))}else{@()})}}
function Ready([int]$Count){$t=Await {param($t) @($t.surfaces).Count -eq $Count -and @($t.surfaces|Where-Object {-not $_.ready -or -not $_.running}).Count -eq 0};$script:shells=@($t.surfaces.pid);return $t}
function Capture([string]$Name,$Tree){
    $bitmap=Join-Path $directory ($Name+'.bmp');$png=Join-Path $directory ($Name+'.png');$result=Request @('chrome-capture',$bitmap);[ChromeFixture]::Png($bitmap,$png)
    Require ((Get-Item -LiteralPath $png).Length -gt 1000) 'Native overview render is unexpectedly empty'
    $evidence.artifacts+=@{name=$Name;path=$png;bytes=(Get-Item -LiteralPath $png).Length;sha256=(Get-FileHash -Algorithm SHA256 -LiteralPath $png).Hash.ToLowerInvariant();capture=$result;overview=$Tree.overview;scope='Actual production overlay/card renderer and owned WebView2 thumbnails on an offscreen DIB; not a composed GPU/desktop screenshot'}
}
try {
    $doctor=Probe @('doctor');Require ($doctor.background_testing -and $doctor.status -eq 'ok') 'Working hidden debug build required'
    $started=[DateTime]::UtcNow;$startup=[Diagnostics.Stopwatch]::StartNew();$owned=[CliProbe]::Start($gui,@('--temporary','--shell=cmd','--cwd',$directory),$directory,$directory);$hostOut=$owned.StandardOutput.ReadToEndAsync();$hostErr=$owned.StandardError.ReadToEndAsync();$evidence.hosts+=,$owned.Id
    $file=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($owned.Id).json"
    do {Require (-not $owned.HasExited -and $startup.ElapsedMilliseconds -lt 8000) 'Host discovery exceeded eight seconds or exited';if((Test-Path -LiteralPath $file) -and (Get-Item -LiteralPath $file).LastWriteTimeUtc -ge $started){$record=Get-Content -Raw -LiteralPath $file|ConvertFrom-Json;Require ($record.pid -eq $owned.Id -and [bool]$record.pipe) 'Wrong discovery owner';$pipeName=$record.pipe;break};Start-Sleep -Milliseconds 20}while($true)
    $left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup budget exhausted';Require ((Request @('identify') ([int][Math]::Min(5000,$left))).pid -eq $owned.Id) 'Pipe owner mismatch'
    do {$left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup readiness exceeded eight seconds';$tree=Tree ([int][Math]::Min(5000,$left));if(@($tree.surfaces).Count -eq 1 -and $tree.surfaces[0].ready -and $tree.surfaces[0].running){break};Start-Sleep -Milliseconds 20}while($true)
    $evidence.observations+=@{name='startup';elapsedMs=$startup.ElapsedMilliseconds;pipe=$pipeName};$shells=@($tree.surfaces.pid);$identities=Identities $tree;$active=(Request @('identify')).surface;$original=Request @('identify')

    $name='한글 한 é 😀 & overview';Request @('workspace','rename',$original.workspace,$name)|Out-Null
    Request @('new-workspace')|Out-Null;$second=Request @('identify');$tree=Ready 2
    if($Case -eq 'browser') {
        $fixture=New-Object BrowserFixture((Join-Path $PSScriptRoot 'browser-capture.html'));$url=$fixture.Origin+'/dom'
        $opened=(Request @('browser','open',$url,'--pane',$second.pane)).browser_pane_opened;Require ([bool]$opened.surface -and [bool]$opened.pane) 'Browser did not open'
        $tree=Await {param($t) @($t.browsers|Where-Object {$_.id -eq $opened.surface -and -not $_.loading -and $_.visible -and $_.url -ceq $url -and $_.title -ceq 'Capture 한글 한 é 😀'}).Count -eq 1} 6000
        $page=(Request @('browser','eval',$opened.pane,'({ready:document.readyState,color:getComputedStyle(document.getElementById("top")).backgroundColor,focusEvents:window.focusEvents,scrollY:window.scrollY})')).result
        Require ($page.ready -eq 'complete' -and $page.color -ceq 'rgb(12, 34, 56)' -and $page.focusEvents -eq 0 -and $page.scrollY -eq 0) 'Real loopback fixture did not load its known color without focus'
        $before=Identities $tree;$tree=Open-Overview
        $tree=Await {param($t) @($t.overview.cards|ForEach-Object {$_.previews}|Where-Object {$_.surface -eq $opened.surface -and $_.status -ne 'Loading preview…'}).Count -eq 1} 6000
        Record 'browser-preview-before-raster-assertion' $tree;$preview=@($tree.overview.cards|ForEach-Object {$_.previews}|Where-Object {$_.surface -eq $opened.surface})
        Require ($preview.Count -eq 1 -and $preview[0].status -eq 'captured' -and $preview[0].source_png_sha256.Length -eq 64) 'Active browser did not produce a real CapturePreview thumbnail'
        Capture 'overview-browser-known-color' $tree;$artifact=$evidence.artifacts[-1];$card=Card $tree $second.workspace;$bounds=@($artifact.capture.controls|Where-Object {$_.handle -eq $card.button})
        Require ($bounds.Count -eq 1) 'Production capture omitted the actual browser workspace card'
        $clip=$bounds[0].clip;$knownPixels=[ChromeFixture]::ColorCount($artifact.path,[int]$clip.x,[int]$clip.y,[int]$clip.width,[int]$clip.height,'#0c2238')
        $evidence.observations+=@{name='known-browser-thumbnail-pixels';source=$url;sourcePage=$page;browserSurface=$opened.surface;preview=$preview[0];cardClip=$clip;expectedColor='#0c2238';matchingPixels=$knownPixels;artifact=$artifact.path}
        Require ($knownPixels -ge 100) 'Known page color absent from actual production overview thumbnail pixels'
        Require ((Identities $tree) -ceq $before) 'Browser thumbnail restarted a terminal'
        $evidence.checks+=@{name='active_browser_capturepreview_is_drawn_as_known_color_pixels_in_actual_native_overview_card';passed=$true;matchingPixels=$knownPixels}
        [OptionsFixture]::Click([long]$tree.overview.window,[long]$tree.overview.dismiss,$owned.Id);$tree=Await {param($t) -not $t.overview.open}
        $after=(Request @('browser','eval',$opened.pane,'({focusEvents:window.focusEvents,scrollY:window.scrollY})')).result
        Require ($after.focusEvents -eq 0 -and $after.scrollY -eq 0 -and (Identities $tree) -ceq $before) 'Overview browser capture changed focus, scroll or terminal identities'
        Record 'browser-overview-dismissed' $tree;$evidence.checks+=@{name='browser_overview_roundtrip_preserves_page_focus_scroll_and_terminal_pids';passed=$true}
    } elseif($Case -eq 'dirty') {
        $path=Join-Path $directory '문서 한.txt';[IO.File]::WriteAllText($path,"original 한글`n",(New-Object Text.UTF8Encoding($false)))
        $opened=(Request @('editor','open',$path,'--pane',$second.pane,'--root',$directory)).editor_opened
        Require ([bool]$opened.surface) 'Editor Open did not create a surface';$editor=$opened.surface
        Request @('editor','command',$editor,'replace-text','--text',"changed 한글`n")|Out-Null;Request @('editor','flush',$editor)|Out-Null
        $tree=Await {param($t) @($t.editors|Where-Object {$_.surface -eq $editor -and $_.dirty}).Count -eq 1};$before=Identities $tree
        $tree=Open-Overview;Click-Card $tree (Card $tree $second.workspace) $true
        $tree=Await {param($t) $t.state.error -match 'unsaved changes'}
        Require ($tree.overview.open -and @($tree.workspaces|Where-Object {$_.id -eq $second.workspace}).Count -eq 1 -and (Identities $tree) -ceq $before) 'Overview bypassed dirty editor close barrier'
        $read=Request @('editor','command',$editor,'read');if($read.result){$read=$read.result};Require ($read.content -ceq "changed 한글`n" -and [IO.File]::ReadAllText($path) -ceq "original 한글`n") 'Dirty close changed document bytes'
        Record 'dirty-close-refused' $tree;$evidence.checks+=@{name='overview_close_uses_editor_barrier_and_preserves_dirty_buffer_disk_and_processes';passed=$true}
        Request @('editor','command',$editor,'discard-document')|Out-Null;Request @('editor','flush',$editor)|Out-Null
        Click-Card $tree (Card $tree $second.workspace) $true;$tree=Await {param($t) @($t.workspaces).Count -eq 1 -and @($t.overview.cards).Count -eq 1}
        Record 'clean-close-completed' $tree;$evidence.checks+=@{name='explicit_discard_allows_guarded_close_and_overlay_card_refresh';passed=$true}
        [OptionsFixture]::Click([long]$tree.overview.window,[long]$tree.overview.dismiss,$owned.Id);$tree=Await {param($t) -not $t.overview.open}
    } else {
        Request @('split','vertical')|Out-Null;Request @('new-workspace')|Out-Null;$third=Request @('identify');$tree=Ready 4;$identities=Identities $tree;$shells=@($tree.surfaces.pid)
        $tree=Open-Overview;$overlay=$tree.overview
        Require ([OverviewFixture]::Parent([long]$overlay.window,$owned.Id) -eq $tree.window_handle -and ([OptionsFixture]::Describe([long]$overlay.window,$owned.Id).Style -band 0x40000000) -ne 0) 'Overview is not a child overlay inside the main window'
        Require (@($overlay.cards).Count -eq 3 -and (Card $tree $original.workspace).name -ceq $name -and @($overlay.cards|Where-Object {$_.active}).Count -eq 1 -and (Card $tree $third.workspace).active) 'Real workspace cards or selected state differ'
        $tree=Await {param($t) @($t.overview.cards|ForEach-Object {$_.previews}|Where-Object {$_.status -eq 'Loading preview…'}).Count -eq 0} 6000
        Record 'preview-settled-before-raster-assertion' $tree
        $previews=@($tree.overview.cards|ForEach-Object {$_.previews})
        $inactive=@($tree.overview.cards|Where-Object {-not $_.active}|ForEach-Object {@{workspace=$_.workspace;name=$_.name;previews=@($_.previews)}})
        $evidence.observations+=@{name='preview-capture-scope';captured=@($previews|Where-Object {$_.status -eq 'captured'});unavailable=@($previews|Where-Object {$_.status -eq 'Preview unavailable'});inactiveWorkspaces=$inactive;limitation='Inactive thumbnails are not implemented; real active-view capture remains mandatory'}
        Require ($previews.Count -eq 4 -and @($previews|Where-Object {$_.status -eq 'captured' -and $_.source_png_sha256.Length -eq 64 -and $_.width -gt 0 -and $_.height -gt 0}).Count -ge 1) 'No real owned WebView preview captured; visual subset remains unverified'
        Require ((Identities $tree) -ceq $identities -and (Request @('identify')).surface -eq $third.surface) 'Opening overview changed live terminal identity or active surface'
        Capture 'overview-real-previews' $tree;Record 'owned-overlay-real-previews' $tree;$evidence.checks+=@{name='in_window_overlay_has_real_unicode_cards_active_pane_raster_explicit_inactive_gaps_and_stable_pids';passed=$true}
        $renamed=$name+' renamed';Request @('workspace','rename',$original.workspace,$renamed)|Out-Null;Request @('workspace','reorder',$original.workspace,'2')|Out-Null
        $tree=Await {param($t) (Card $t $original.workspace).name -ceq $renamed -and $t.overview.cards[2].workspace -eq $original.workspace};Record 'live-rename-reorder' $tree
        $evidence.checks+=@{name='open_overview_updates_names_and_workspace_order_without_reopening';passed=$true}
        [ChromeFixture]::Resize([long]$tree.window_handle,$owned.Id,500,300);$tree=Await {param($t) $t.overview.columns -eq 1};[OverviewFixture]::Scroll([long]$tree.overview.window,$owned.Id,$true)
        $tree=Await {param($t) $t.overview.offset -gt 0};$last=Card $tree $original.workspace;$controls=@([ChromeFixture]::Read([long]$tree.overview.viewport,$owned.Id));Require (@($controls|Where-Object {$_.Handle -eq $last.button -and $_.Shown}).Count -eq 1) 'Last card unreachable in short viewport';Capture 'overview-short-window' $tree;Record 'short-window-scrolled' $tree
        $evidence.checks+=@{name='short_window_clips_and_scrolls_to_last_real_workspace_card';passed=$true}
        [OverviewFixture]::Key([long]$tree.overview.window,[long]$tree.overview.dismiss,$owned.Id,13);$tree=Await {param($t) -not $t.overview.open}
        Require ((Request @('identify')).surface -eq $third.surface) 'Enter on Close overview selected a workspace';$tree=Open-Overview
        $chosen=Card $tree $original.workspace;[OverviewFixture]::Key([long]$tree.overview.window,[long]$chosen.button,$owned.Id,13);$tree=Await {param($t) -not $t.overview.open -and $t.active_workspace -eq $original.workspace}
        Require ((Request @('identify')).surface -eq $original.surface -and (Identities $tree) -ceq $identities) 'Card Enter failed to select exact existing surface'
        Record 'selected-card' $tree;$evidence.checks+=@{name='keyboard_enter_routes_to_actual_dismiss_or_card_hwnd_and_preserves_processes';passed=$true}
        $tree=Open-Overview;$close=Card $tree $second.workspace;[OverviewFixture]::Key([long]$tree.overview.window,[long]$close.close,$owned.Id,13)
        $tree=Await {param($t) @($t.workspaces).Count -eq 2 -and @($t.overview.cards).Count -eq 2};Require ($tree.overview.open -and $tree.active_workspace -eq $original.workspace) 'Close card dismissed overview or selected another workspace'
        Click-Card $tree (Card $tree $third.workspace) $true;$tree=Await {param($t) @($t.workspaces).Count -eq 1 -and @($t.overview.cards).Count -eq 1}
        Click-Card $tree (Card $tree $original.workspace) $true;$tree=Await {param($t) [OptionsFixture]::Text([long]$t.overview.status,$owned.Id) -match 'last|final'}
        Require (@($tree.workspaces).Count -eq 1 -and $tree.overview.open) 'Final workspace was closed';Record 'close-final-protection' $tree
        $evidence.checks+=@{name='close_buttons_keep_overview_current_and_preserve_final_workspace_guard';passed=$true}
        [OverviewFixture]::Key([long]$tree.overview.window,[long](Card $tree $original.workspace).button,$owned.Id,27);$tree=Await {param($t) -not $t.overview.open};Require ((Request @('identify')).surface -eq $original.surface) 'Escape changed selected surface'
    }
    Request @('quit','--discard-state')|Out-Null;Require ($owned.WaitForExit((Budget 5000)) -and $owned.ExitCode -eq 0) 'Owned overview host did not quit cleanly';$evidence.status='passed_hidden_overview_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
    $cleaning=$true
    if($fixture){$fixture.Dispose()}
    if($owned){try{if(-not $owned.HasExited){$stop=[Diagnostics.Stopwatch]::StartNew();if($pipeName){Request @('quit','--discard-state') 2500|Out-Null};if(-not $owned.WaitForExit([int][Math]::Max(1,5000-$stop.ElapsedMilliseconds))){throw 'Owned host cleanup deadline exceeded'}}}catch{$cleanupErrors+=$_.Exception.Message;if(-not $owned.HasExited){$owned.Kill();[CliProbe]::WaitAfterKill($owned)}}
        $evidence.observations+=@{name='host-exit';pid=$owned.Id;exitCode=$owned.ExitCode;stdoutComplete=$hostOut.Wait(500);stderrComplete=$hostErr.Wait(500);stdout=[CliProbe]::Output($hostOut);stderr=[CliProbe]::Output($hostErr)};$owned.Dispose()}
    if($cleanupErrors.Count){$evidence.status='failed';$evidence.cleanupErrors=$cleanupErrors};$evidence.clientPids=$clients;$evidence.shells=$shells;$evidence.elapsedMs=$clock.ElapsedMilliseconds;$evidence.finished=[DateTime]::UtcNow.ToString('o');if($evidence.status -eq 'failed'){$evidence|ConvertTo-Json -Depth 50|Set-Content -Encoding UTF8 (Join-Path $directory 'native-overview-background.json')};if($evidence.status -eq 'failed'){Write-Output ('Evidence: '+$directory)}
}
if($cleanupErrors.Count){throw ($cleanupErrors -join '; ')}
[ordered]@{status=$evidence.status;checks=$evidence.checks.Count;elapsedMs=$evidence.elapsedMs}|ConvertTo-Json -Compress
