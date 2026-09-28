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
$directory=Join-Path $PSScriptRoot ('..\dist\evidence\browser-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$fixture=New-Object BrowserFixture;$origin=$fixture.Origin
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
    $deadline=(Get-Date).AddSeconds(20)
    do {
        $failed=Request @('browser','status',$first.pane)
        if (-not $failed.loading -and $failed.navigation_error) {break}
        if ((Get-Date) -gt $deadline) {throw 'Failed navigation was not reported'}
        Start-Sleep -Milliseconds 50
    } while ($true)
    Request @('browser','navigate',$first.pane,($origin+'/one'))|Out-Null;Wait-Page $first.pane '/one' $oneTitle|Out-Null
    $evidence.checks+=@{name='native_history_reload_stop_redirect_failure_recovery_and_bounded_zoom';passed=$true;failure=$failed.navigation_error}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)
    foreach ($url in @('file:///C:/private','javascript:1','data:text/html,no','http://flowmux-terminal.localhost/')) {Request @('browser','navigate',$first.pane,$url) 1|Out-Null}
    Eval-Page $first.pane 'location.href="http://flowmux-terminal.localhost/";null'|Out-Null
    Start-Sleep -Milliseconds 200
    if ((Request @('browser','url',$first.pane)).url -ne ($origin+'/one')) {throw 'Page navigation reached reserved terminal origin'}
    Request @('browser','eval',$first.pane,'Promise.resolve(1)') 1|Out-Null
    Request @('browser','eval',$first.pane,'throw new Error("한글 오류")') 1|Out-Null
    Request @('browser','eval',$first.pane,'"한".repeat(100000)') 1|Out-Null
    Eval-Page $first.pane 'window.open("/two");null'|Out-Null
    foreach ($args in @(@('read-screen','--surface',$first.surface),@('selection','--surface',$first.surface,'read'),@('send-key','Enter','--surface',$first.surface),@('browser','url',$source.pane))) {Request $args 1|Out-Null}
    if ((Request @('identify')).shell) {throw 'Browser advertised a terminal shell'}
    Tree|Out-Null
    $evidence.checks+=@{name='forbidden_urls_popup_denial_script_errors_and_terminal_target_rejection';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)
    $second=(Request @('browser','open',($origin+'/two'),'--pane',$source.pane)).browser_pane_opened
    if ($second.placement_strategy -ne 'reuse_right_sibling' -or $second.pane -ne $first.pane) {throw 'Right browser pane not reused'}
    Wait-Page $second.pane '/two' $twoTitle|Out-Null
    $down=(Request @('browser','open',($origin+'/one'),'--pane',$source.pane,'--down')).browser_pane_opened
    if ($down.placement_strategy -ne 'split_down' -or $down.pane -eq $first.pane) {throw 'Down split differs'}
    Wait-Page $down.pane '/one' $oneTitle|Out-Null
    Request @('focus-tab',$first.surface)|Out-Null
    Eval-Page $first.pane 'window.retained="한글 한 é 😀";localStorage.setItem("browser-persist",window.retained);null'|Out-Null
    $view=(Request @('browser','status',$first.pane)).view_handle
    Request @('move-tab',$first.surface,'--to-pane',$source.pane)|Out-Null
    if (-not (Same-Text (Eval-Page $source.pane 'window.retained') '한글 한 é 😀') -or (Request @('browser','status',$source.pane)).view_handle -ne $view) {throw 'Browser move recreated document or view'}
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
    $deadline=(Get-Date).AddSeconds(20)
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
    $new=(Request @('identify'));$deadline=(Get-Date).AddSeconds(20)
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
    if (-not $process.WaitForExit(15000)) {throw 'Browser-only host did not close'}
    $process.Dispose();$process=$null
    $restored=Start-Owned @('--restore-window',$saved.window)
    if (@($restored.surfaces).Count -ne 0 -or @($restored.browsers).Count -ne 1 -or $restored.browsers[0].id -ne $first.surface) {throw 'Browser-only restore lost identity'}
    $restoredPane=(Request @('identify')).pane
    Wait-Page $restoredPane '/one' $oneTitle|Out-Null
    if (-not (Same-Text (Eval-Page $restoredPane 'localStorage.getItem("browser-persist")') '한글 한 é 😀')) {throw 'Isolated browser profile did not persist'}
    $evidence.checks+=@{name='browser_only_checkpoint_restart_and_separate_profile_persistence';passed=$true;window=$saved.window;surface=$first.surface}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)
    Tree|Out-Null
    $evidence.status='passed_background_browser_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
    if ($pipeName -and $process -and -not $process.HasExited) {try {Request @('quit','--discard-state')|Out-Null} catch {}}
    if ($process) {if (-not $process.HasExited -and -not $process.WaitForExit(10000)) {$process.Kill();[CliProbe]::WaitAfterKill($process)};if ($stderr -and $stderr.Wait(3000)) {$evidence.hostStderr=([CliProbe]::Output($stderr))};$evidence.hostExitCode=$process.ExitCode;$process.Dispose()}
    $fixture.Dispose();$evidence.hosts=$hosts;$evidence.shells=$shells;$evidence.finished=(Get-Date).ToString('o')
    $evidence|ConvertTo-Json -Depth 12|Set-Content -Encoding UTF8 (Join-Path $directory 'native-browser-background.json')
    Write-Output ('Evidence: '+$directory)
}
$evidence|ConvertTo-Json -Depth 12
