# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden owned WebView2 host + loopback fixture only. No foreground, input, clipboard or external sites.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",
    [ValidateSet('all','files-close')][string]$Case='all')
if (-not $env:FLOWMUX_TEST_ARTIFACT_ROOT) { throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.' }
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
$directory=Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT ('browser-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$fixture=New-Object BrowserFixture;$origin=$fixture.Origin
$pipeName=$null;$process=$null;$hosts=@();$shells=@()
$evidence=[ordered]@{started=(Get-Date).ToString('o');mode='background';checks=@();clipboardAccess=$false;desktopInput=$false;externalSites=$false;unicodeComparison='ordinal'}
function Request([string[]]$Arguments,[int[]]$Exit=@(0),[ValidateRange(1,5000)][int]$TimeoutMilliseconds=5000) {
    if (-not $script:pipeName) {throw 'Owned pipe required'}
    $p=[CliProbe]::Start($cli,(@('--pipe',$script:pipeName,'--json')+$Arguments),$directory,$directory)
    try {
        $requestClock=[Diagnostics.Stopwatch]::StartNew();$out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
        if (-not $p.WaitForExit($TimeoutMilliseconds)) {$p.Kill();[CliProbe]::WaitAfterKill($p);throw 'Owned CLI timed out; not retried'}
        if (-not $out.Wait([int][Math]::Max(1,$TimeoutMilliseconds-$requestClock.ElapsedMilliseconds)) -or -not $err.Wait([int][Math]::Max(1,$TimeoutMilliseconds-$requestClock.ElapsedMilliseconds))) {throw 'Owned output did not close'}
        if ($Exit -notcontains $p.ExitCode) {throw "CLI $Arguments exit $($p.ExitCode): $(([CliProbe]::Output($err))) $($out.Result)"}
        if ($p.ExitCode -eq 0) {return ($out.Result|ConvertFrom-Json)}
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
    $browser=Request @('browser','status',$Pane);$find=$browser.find
    if (-not $find.panel_handle -or -not $find.panel_query_handle -or $find.panel_owner -ne $Owner -or $browser.holder.root -ne $Owner -or -not (Same-Text ([FindFixture]::ReadText([long]$find.panel_query_handle)) $Query)) {throw 'Browser find root owner or raw native query changed'}
    # A child panel moves with its stable holder; GW_OWNER is only meaningful for popups.
    if ([OptionsFixture]::Parent([long]$find.panel_handle,$process.Id) -ne $browser.holder.window -or [OptionsFixture]::Parent([long]$find.panel_query_handle,$process.Id) -ne $find.panel_handle -or [OptionsFixture]::Parent([long]$browser.holder.window,$process.Id) -ne $Owner -or (([OptionsFixture]::Describe($Owner,$process.Id)).Style -band 0x40000000) -ne 0) {throw 'Browser find native parent chain differs from its holder/root'}
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
function Address-Unchanged($Before,[string]$Draft) {
    # Queued owned HWND messages precede this IPC roundtrip. This tests the
    # native guard/dispatch path, not physical Korean IME or desktop focus.
    $tree=Tree;$after=@($tree.browsers|Where-Object {$_.id -eq $Before.id})
    if($after.Count -ne 1 -or $after[0].url -cne $Before.url -or $after[0].generation -ne $Before.generation -or $after[0].view_handle -ne $Before.view_handle -or -not (Same-Text ([BrowserFixture]::ReadText($Before.address_handle)) $Draft)){$evidence.addressFailure=@{beforeUrl=$Before.url;afterUrl=$after[0].url;beforeGeneration=$Before.generation;afterGeneration=$after[0].generation;beforeView=$Before.view_handle;afterView=$after[0].view_handle;expectedDraft=$Draft;actualDraft=[BrowserFixture]::ReadText($Before.address_handle)};throw 'Guarded address input navigated, replaced its view or changed raw Unicode draft'}
    $running=@($tree.surfaces|Where-Object {$_.id -eq $terminal.id})
    if($running.Count -ne 1 -or $running[0].pid -ne $terminal.pid -or -not $running[0].running){throw 'Address input changed the sibling terminal process'}
}
function Check-Toolbar($Status) {
    $chrome=@([ChromeFixture]::Read([long]$Status.chrome_handle,$process.Id));$shown=@($chrome|Where-Object Shown)
    $dpi=[ChromeFixture]::GetDpiForWindow([IntPtr]([long]$Status.chrome_handle));$area=[ChromeFixture]::Size([long]$Status.chrome_handle,$process.Id)
    if($Status.chrome.rows -ne 1 -or [Math]::Abs($area[1]-40*$dpi/96) -gt 1){throw 'Browser toolbar is not one40-DIP row'}
    $holder=$Status.holder.bounds;$viewport=$Status.bounds;$findHeight=0
    if($Status.find.panel_handle){
        $findBounds=[OptionsFixture]::RelativeBounds([long]$Status.holder.window,[long]$Status.find.panel_handle,$process.Id)
        if([OptionsFixture]::Parent([long]$Status.find.panel_handle,$process.Id) -ne $Status.holder.window -or $findBounds.X -ne 0 -or $findBounds.Y -ne $area[1] -or $findBounds.Width -ne $area[0] -or $findBounds.Height -le 0 -or $findBounds.Height -ne $Status.find.panel_bounds.height){throw 'Inline find geometry differs from its retained holder'}
        $findHeight=$findBounds.Height
    }
    if (-not $holder -or -not $viewport -or $Status.chrome.parent -ne $Status.holder.window -or $holder.width -ne $area[0] -or $viewport.x -ne $holder.x -or $viewport.y -ne ($holder.y+$area[1]+$findHeight) -or $viewport.width -ne $holder.width -or $viewport.height -ne [Math]::Max(1,$holder.height-$area[1]-$findHeight)) {throw 'Browser holder, toolbar and root-coordinate viewport geometry differ'}
    foreach($c in $shown){if($c.Y -lt 0 -or $c.Y+$c.Height -gt $area[1] -or $c.X -lt 0 -or $c.X+$c.Width -gt $area[0]){throw 'Browser toolbar control escapes its row'}}
    $ordered=@($shown|Sort-Object X);for($i=1;$i -lt $ordered.Count;$i++){if($ordered[$i].X -lt $ordered[$i-1].X+$ordered[$i-1].Width){throw 'Browser toolbar controls overlap'}}
    if(@($shown|Where-Object Class -eq 'Edit').Count -ne 1 -or @($shown|Where-Object Handle -eq $Status.chrome.tools_handle).Count -ne 1){throw 'Address or tools entry missing'}
    $reload=@($chrome|Where-Object Text -ceq 'Reload')[0];$stop=@($chrome|Where-Object Text -ceq 'Stop')[0]
    if($reload.Width -ne [Math]::Round(30*$dpi/96) -or $reload.X -ne $stop.X -or $reload.Y -ne $stop.Y -or $reload.Width -ne $stop.Width -or $reload.Height -ne $stop.Height){throw 'Reload/Stop geometry changed during metadata refresh'}
    return $chrome
}
function Bookmarks-Wait([int]$Count,[bool]$Error=$false,[long]$After=0) {
    $clock=[Diagnostics.Stopwatch]::StartNew()
    do {$tree=Tree;$panel=$tree.bookmarks.panel
        if($panel -and -not $tree.bookmarks.busy -and @($panel.rows).Count -eq $Count -and $panel.error -eq $Error -and $panel.generation -gt $After){
            $native=[OptionsFixture]::Describe([long]$panel.window,$process.Id)
            if($panel.native_visible -or $native.Owner -ne $panel.owner -or [Math]::Abs($native.Width-336*$native.Dpi/96) -gt 2){throw 'Bookmarks lost hidden popup ownership or Linux width'}
            return $panel
        }
        if($clock.ElapsedMilliseconds -gt 5000){throw ('Bookmarks exceeded5s: '+($tree.bookmarks|ConvertTo-Json -Depth 6 -Compress))};Start-Sleep -Milliseconds 20
    }while($true)
}
function Bookmarks-Open([string]$Pane,[int]$Count,[bool]$Error=$false){
    $browser=Request @('browser','status',$Pane)
    [OptionsFixture]::Click([long]$browser.chrome_handle,[long]$browser.chrome.bookmarks_handle,$process.Id)
    return Bookmarks-Wait $Count $Error
}
function Bookmarks-Click([long]$Handle){[OptionsFixture]::Click([OptionsFixture]::Parent($Handle,$process.Id),$Handle,$process.Id)}
function Bookmarks-Close($Panel){
    [OptionsFixture]::PostEscape([long]$Panel.window,$process.Id);$clock=[Diagnostics.Stopwatch]::StartNew()
    do{if(-not (Tree).bookmarks.panel){return};if($clock.ElapsedMilliseconds -gt 5000){throw 'Bookmark popup did not close'};Start-Sleep -Milliseconds 20}while($true)
}
function Verify-Bookmarks([string]$Pane){
    $before=Request @('browser','status',$Pane);$panel=Bookmarks-Open $Pane 0
    if($panel.message -cne 'No bookmarks yet'){throw 'Bookmark empty state missing'}
    Bookmarks-Click ([long]$panel.add);$panel=Bookmarks-Wait 1 $false $panel.generation
    if(-not (Same-Text $panel.rows[0].title $oneTitle) -or $panel.rows[0].url -cne $origin+'/one'){throw 'Bookmark changed Korean title or URL'}
    $caption=[OptionsFixture]::Text([long]$panel.rows[0].open,$process.Id)
    if(-not (Same-Text $caption ($oneTitle+"`n"+$origin+'/one'))){throw 'Bookmark native two-line caption lost Unicode'}
    $image=Join-Path $directory 'bookmarks.bmp';$capture=Request @('chrome-capture',$image)
    if($capture.root_handle -ne $panel.window -or @($capture.controls|Where-Object {$_.handle -eq $panel.rows[0].open}).Count -ne 1 -or @($capture.controls|Where-Object {$_.handle -eq $panel.rows[0].remove}).Count -ne 1){throw 'Bookmark production paint omitted row or delete button'}
    $background=[ChromeFixture]::Pixel($image,1,1)
    foreach($handle in @($panel.rows[0].open,$panel.rows[0].remove)){$paint=@($capture.controls|Where-Object {$_.handle -eq $handle})[0].clip;if($paint.width*$paint.height-[ChromeFixture]::ColorCount($image,$paint.x,$paint.y,$paint.width,$paint.height,$background) -lt 8){throw 'Bookmark row or delete glyph was blank'}}
    Remove-Item -LiteralPath $image -Force
    Bookmarks-Click ([long]$panel.add);$panel=Bookmarks-Wait 1 $false $panel.generation
    Request @('browser','navigate',$Pane,($origin+'/two'))|Out-Null;Wait-Page $Pane '/two' $twoTitle|Out-Null
    Bookmarks-Click ([long]$panel.add);$panel=Bookmarks-Wait 2 $false $panel.generation
    if(-not (Same-Text $panel.rows[0].title $twoTitle)){throw 'Bookmark this page used stale metadata'}
    Bookmarks-Click ([long]$panel.rows[1].open);Wait-Page $Pane '/one' $oneTitle|Out-Null
    if((Tree).bookmarks.panel){throw 'Opening bookmark did not dismiss popup'}
    $panel=Bookmarks-Open $Pane 2;Bookmarks-Click ([long]$panel.rows[0].remove);$panel=Bookmarks-Wait 1 $false $panel.generation
    $path=Join-Path $directory 'state\browser-profile\bookmarks.json';$saved=[IO.File]::ReadAllBytes($path);$values=Get-Content -Raw -Encoding UTF8 -LiteralPath $path|ConvertFrom-Json
    if(@($values).Count -ne 1 -or -not (Same-Text $values[0].title $oneTitle)){throw 'Bookmark delete or persistence differs'}
    Bookmarks-Close $panel
    try{
        $invalid='{ broken bookmarks';[IO.File]::WriteAllText($path,$invalid,(New-Object Text.UTF8Encoding($false)))
        $panel=Bookmarks-Open $Pane 0 $true
        if([OptionsFixture]::Describe([long]$panel.add,$process.Id).Enabled -or [IO.File]::ReadAllText($path) -cne $invalid){throw 'Corrupt bookmarks enabled replacement or were overwritten'}
        Bookmarks-Close $panel
    }finally{[IO.File]::WriteAllBytes($path,$saved)}
    $panel=Bookmarks-Open $Pane 1;$lock=[IO.File]::Open([IO.Path]::ChangeExtension($path,'lock'),[IO.FileMode]::OpenOrCreate,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None)
    try{Bookmarks-Click ([long]$panel.add);$panel=Bookmarks-Wait 0 $true $panel.generation;if($panel.message -notlike '*Another window*' -or [Convert]::ToBase64String([IO.File]::ReadAllBytes($path)) -cne [Convert]::ToBase64String($saved)){throw 'Bookmark writer lock waited or changed committed bytes'}}finally{$lock.Dispose()}
    Bookmarks-Close $panel
    try{
        $rows=@(@{title='위험 링크';url='javascript:alert(1)'})+@(1..12|ForEach-Object {@{title=($oneTitle+' '+$_);url=($origin+'/one#'+$_)}})
        [IO.File]::WriteAllText($path,($rows|ConvertTo-Json -Compress),(New-Object Text.UTF8Encoding($false)))
        $panel=Bookmarks-Open $Pane 13
        if([OptionsFixture]::Describe([long]$panel.rows[0].open,$process.Id).Enabled -or -not [OptionsFixture]::Describe([long]$panel.rows[0].remove,$process.Id).Enabled){throw 'Unsafe stored bookmark was navigable or could not be removed'}
        [OptionsFixture]::Scroll([long]$panel.viewport,$process.Id,$true);$clock=[Diagnostics.Stopwatch]::StartNew()
        do{$panel=(Tree).bookmarks.panel;if($panel.first -gt 0){break};if($clock.ElapsedMilliseconds -gt 5000){throw 'Bookmark list did not scroll'};Start-Sleep -Milliseconds 20}while($true)
        Bookmarks-Click ([long]$panel.rows[12].remove);$panel=Bookmarks-Wait 12 $false $panel.generation
        if(@($panel.rows|Where-Object {$_.url -ceq ($origin+'/one#12')}).Count){throw 'Scrolled bookmark delete used the wrong row identity'}
        Bookmarks-Close $panel
    }finally{[IO.File]::WriteAllBytes($path,$saved)}
    $panel=Bookmarks-Open $Pane 1;Bookmarks-Close $panel
    $after=Request @('browser','status',$Pane)
    if($after.view_handle -ne $before.view_handle -or $after.holder.window -ne $before.holder.window -or (Tree).surfaces[0].pid -ne $terminal.pid){throw 'Bookmarks replaced browser or terminal'}
    $evidence.checks+=@{name='native_bookmarks_unicode_add_deduplicate_open_delete_scroll_corruption_writer_lock_and_safe_urls';passed=$true}
}
function Files-CloseRemaining([Diagnostics.Stopwatch]$Clock,[int]$Limit=3000) {
    $left=$Limit-$Clock.ElapsedMilliseconds;if($left -le 0){throw 'Files-close phase exceeded its bounded budget'};return [int][Math]::Min(5000,$left)
}
function Files-CloseListing([string]$Pane) {
    $clock=[Diagnostics.Stopwatch]::StartNew()
    do {
        $listing=Request @('files','status','--pane',$Pane) 0 (Files-CloseRemaining $clock 5000)
        if(-not $listing.loading -and -not $listing.stale -and -not $listing.last_error -and $listing.token){return $listing}
        Start-Sleep -Milliseconds 20
    }while($true)
}
function Verify-FilesClose {
    Add-Type -Path (Join-Path $PSScriptRoot 'EditorFixture.cs'),(Join-Path $PSScriptRoot 'FilesFixture.cs'),(Join-Path $PSScriptRoot 'EditorOpenLifetime.cs')
    $files=New-Object FilesFixture($directory);$pause=$null;$copy=$null;$copyOut=$null;$copyErr=$null
    try {
        $sourceName='닫기 중 원본 한 😀.txt';$destination='닫기 후 복사 한 😀.txt';$bytes=[EditorFixture]::Encode([EditorFixture]::Original,$true,$true)
        $sourcePath=$files.WriteBytes($sourceName,$bytes)
        $tree=Start-Owned @('--temporary','--shell=cmd','--cwd',$files.Root);$source=Request @('identify');$script:shells+=@($tree.surfaces.pid)
        $opener=(Request @('browser','open',($origin+'/one'),'--pane',$source.pane)).browser_pane_opened
        Wait-Page $opener.pane '/one' '첫째 한글 한 é 😀'|Out-Null
        if(-not (Eval-Page $opener.pane 'window.filesChild=window.open("/two");window.filesChild!==null')){throw 'Files-close child did not return a WindowProxy'}
        $clock=[Diagnostics.Stopwatch]::StartNew()
        do {
            $tree=Request @('tree') 0 (Files-CloseRemaining $clock 5000);$children=@($tree.browsers|Where-Object {$_.popup_opener -eq $opener.surface})
            if($children.Count -eq 1 -and $tree.popup.pending -eq 0){break}
            Start-Sleep -Milliseconds 20
        }while($true)
        $child=$children[0];$childPane=Location $tree $child.id;Wait-Page $childPane '/two' '둘째 한글 한 é 😀'|Out-Null
        Request @('detach-tab',$child.id)|Out-Null;$tree=Tree;$childPane=Location $tree $child.id;$frame=@($tree.detached_windows|Where-Object {$_.surface -eq $child.id})[0];$main=[long]$tree.window_handle
        [FindFixture]::PostClose($main,$process.Id);$clock=[Diagnostics.Stopwatch]::StartNew()
        do {
            if($process.HasExited){throw 'Main close killed the script-opened detached child'}
            $tree=Request @('tree') 0 (Files-CloseRemaining $clock 5000)
            if($tree.main_closed -and @($tree.browsers).Count -eq 1 -and @($tree.surfaces).Count -eq 0 -and -not $tree.state.saving){break}
            Start-Sleep -Milliseconds 20
        }while($true)
        [OptionsFixture]::Describe($main,$process.Id)|Out-Null;[OptionsFixture]::Describe([long]$frame.window_handle,$process.Id)|Out-Null
        Request @('files','show','--pane',$childPane,'--root',$files.Root)|Out-Null;$listing=Files-CloseListing $childPane
        $row=@($listing.rows|Where-Object {$_.path -ceq $sourceName});if($row.Count -ne 1){throw 'Files-close source row missing'}
        $warm=Request @('files','copy','--pane',$childPane,'--token',$listing.token,'--index',$row[0].index.ToString(),'--destination','warm.txt')
        if(-not $warm.operation.accepted -or -not $warm.operation.id){throw 'Files worker warm-up was not accepted'}
        $clock=[Diagnostics.Stopwatch]::StartNew()
        do {
            $receipt=Request @('files','operation-status','--id',$warm.operation.id) 0 (Files-CloseRemaining $clock 5000)
            if($receipt.operation.status -notin @('accepted','preparing')){break}
            Start-Sleep -Milliseconds 20
        }while($true)
        if($receipt.operation.status -ne 'succeeded' -or -not $files.BytesEqual($files.File('warm.txt'),$bytes)){throw 'Files worker warm-up did not finish with exact bytes'}
        $listing=Files-CloseListing $childPane;$row=@($listing.rows|Where-Object {$_.path -ceq $sourceName});if($row.Count -ne 1){throw 'Refreshed Files-close source row missing'}
        $pauseClock=[Diagnostics.Stopwatch]::StartNew()
        try {
            $pause=[EditorOpenWorkerPause]::new($process,$main,'flowmux-files-actions')
            Files-CloseRemaining $pauseClock|Out-Null
            $copy=[CliProbe]::Start($cli,@('--pipe',$pipeName,'--json','files','copy','--pane',$childPane,'--token',$listing.token,'--index',$row[0].index.ToString(),'--destination',$destination),$directory,$directory)
            $copyOut=$copy.StandardOutput.ReadToEndAsync();$copyErr=$copy.StandardError.ReadToEndAsync()
            do {
                $pending=Request @('files','status','--pane',$childPane) 0 (Files-CloseRemaining $pauseClock)
                if($pending.operation.status -eq 'preparing'){break}
                if($copy.HasExited){throw 'Paused Files copy exited before preparing was observed'}
                Start-Sleep -Milliseconds 10
            }while($true)
            $operationId=$pending.operation.id
            # Closing may invalidate its own eval callback. The real native close
            # event is required inside the same three-second pause budget.
            Request @('browser','eval',$childPane,'setTimeout(()=>window.close(),0);null') @(0,1) (Files-CloseRemaining $pauseClock)|Out-Null
            do {
                if($process.HasExited){throw 'Final browser close bypassed the pending Files operation'}
                $closed=Request @('tree') 0 (Files-CloseRemaining $pauseClock);$retained=@($closed.browsers|Where-Object {$_.id -eq $child.id})
                if($retained.Count -eq 1 -and $retained[0].native_closed){break}
                Start-Sleep -Milliseconds 10
            }while($true)
            $retainedFrames=@($closed.detached_windows|Where-Object {$_.surface -eq $child.id})
            if(-not $closed.main_closed -or $retainedFrames.Count -ne 1 -or $retainedFrames[0].window_handle -ne $frame.window_handle -or $process.HasExited -or $copy.HasExited){throw 'Native-closed final browser did not retain its frame and blocked copy'}
            [OptionsFixture]::Describe($main,$process.Id)|Out-Null;[OptionsFixture]::Describe([long]$frame.window_handle,$process.Id)|Out-Null
            $pending=Request @('files','status','--pane',$childPane) 0 (Files-CloseRemaining $pauseClock)
            if($pending.operation.id -ne $operationId -or $pending.operation.status -ne 'preparing'){throw 'Native browser close lost the active Files preparation'}
            $evidence.filesClose=@{operation=$operationId;surface=$child.id;frame=$frame.window_handle;nativeClosed=$true;worker=$pause.Description;thread=$pause.ThreadId;heldMs=$pauseClock.ElapsedMilliseconds}
            Files-CloseRemaining $pauseClock|Out-Null
        } finally {
            if($pause){$pause.Dispose()}
        }
        if(-not $pause.Resumed -or $pause.PreviousResumeCount -ne 1 -or $pauseClock.ElapsedMilliseconds -ge 3000){throw 'Files worker suspension was not balanced within three seconds'}
        $finish=[Diagnostics.Stopwatch]::StartNew()
        if(-not $copy.WaitForExit((Files-CloseRemaining $finish 5000)) -or -not $copyOut.Wait((Files-CloseRemaining $finish 5000)) -or -not $copyErr.Wait((Files-CloseRemaining $finish 5000)) -or $copy.ExitCode -ne 0){throw ('Resumed Files copy did not return acceptance: '+[CliProbe]::Output($copyErr))}
        $accepted=[CliProbe]::Output($copyOut)|ConvertFrom-Json
        if($accepted.operation.id -ne $operationId -or $accepted.operation.status -ne 'accepted' -or -not $accepted.operation.accepted){throw 'Resumed Files operation returned a different receipt'}
        if(-not $process.WaitForExit((Files-CloseRemaining $finish 5000)) -or $process.ExitCode -ne 0){throw 'Completed Files operation did not close the final native-closed browser normally'}
        if(-not $files.BytesEqual($sourcePath,$bytes) -or -not $files.BytesEqual($files.File($destination),$bytes)){throw 'Files-close completion changed source or destination bytes'}
        $evidence.checks+=@{name='final_script_browser_close_waits_for_active_files_copy_then_exits_with_exact_bytes';passed=$true;heldMs=$evidence.filesClose.heldMs}
        $evidence.hostExitCode=$process.ExitCode;if($stderr.Wait(1000)){$evidence.hostStderr=[CliProbe]::Output($stderr)}
        $process.Dispose();$script:process=$null;$script:pipeName=$null
    } finally {
        if($pause){$pause.Dispose()}
        if($copy){if(-not $copy.HasExited){$copy.Kill();[CliProbe]::WaitAfterKill($copy)};$copy.Dispose()}
        $files.Dispose()
    }
}
try {
    if($Case -eq 'all') {
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
    Verify-Bookmarks $first.pane
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
    $addressBefore=Request @('browser','status',$first.pane);$address=[long]$addressBefore.address_handle;$addressDraft=$origin+'/한글?q=한#😀'
    [OptionsFixture]::SetText([long]$addressBefore.chrome_handle,$address,$process.Id,$addressDraft)
    [OptionsFixture]::WindowCompositionGuard($address,$process.Id,$true)
    [OptionsFixture]::PostKey($address,$process.Id,13,$false,$false);Address-Unchanged $addressBefore $addressDraft
    [OptionsFixture]::PostKey($address,$process.Id,13,$true,$false);Address-Unchanged $addressBefore $addressDraft
    [OptionsFixture]::WindowCompositionGuard($address,$process.Id,$false)
    # Neither a held Enter nor its repeat bit can mask a missing settling guard;
    # releasing only a modifier must not settle the completed composition.
    [OptionsFixture]::PostKey($address,$process.Id,17,$false,$false);[OptionsFixture]::PostKey($address,$process.Id,17,$true,$false)
    [OptionsFixture]::PostKey($address,$process.Id,13,$false,$false);Address-Unchanged $addressBefore $addressDraft
    [OptionsFixture]::PostKey($address,$process.Id,13,$true,$false)
    [OptionsFixture]::PostKey($address,$process.Id,229,$false,$false)
    [OptionsFixture]::PostKey($address,$process.Id,13,$false,$false);Address-Unchanged $addressBefore $addressDraft
    [OptionsFixture]::PostKey($address,$process.Id,13,$true,$false);[OptionsFixture]::PostKey($address,$process.Id,229,$true,$false)
    foreach($modifier in @(17,18,16)){
        [OptionsFixture]::PostKey($address,$process.Id,$modifier,$false,$false)
        [OptionsFixture]::PostEnter($address,$process.Id);Address-Unchanged $addressBefore $addressDraft
        [OptionsFixture]::PostKey($address,$process.Id,$modifier,$true,$false)
    }
    [OptionsFixture]::PostEnter($address,$process.Id)
    $unicode=Wait-Page $first.pane '/%ED%95%9C%EA%B8%80' $oneTitle
    if ([BrowserFixture]::ReadText($unicode.address_handle) -ne $unicode.url -or -not (Same-Text (Eval-Page $first.pane 'decodeURI(location.href)') ($origin+'/한글?q=한#😀'))) {throw 'Unicode address changed codepoints'}
    $addressLoad=Eval-Page $first.pane 'window.fixtureLoad';$repeatDraft=$origin+'/two?repeat-한'
    [OptionsFixture]::SetText([long]$unicode.chrome_handle,$address,$process.Id,$repeatDraft)
    [OptionsFixture]::PostKey($address,$process.Id,13,$false,$true);Address-Unchanged $unicode $repeatDraft
    [OptionsFixture]::PostKey($address,$process.Id,13,$true,$false)
    if((Eval-Page $first.pane 'window.fixtureLoad') -ne $addressLoad){throw 'Repeated address Enter reloaded the document'}
    $evidence.checks+=@{name='native_address_enter_preserves_unicode_and_guards_composition_process_repeat_and_modifiers';passed=$true}
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
    $addressTree=Tree;$inactiveAddress=@($addressTree.browsers|Where-Object {$_.id -eq $first.surface})[0];$currentBefore=Request @('identify')
    if($inactiveAddress.visible){throw 'Inactive address fixture still has a visible browser surface'}
    $inactiveDraft=$origin+'/two?inactive-한'
    [OptionsFixture]::SetText([long]$inactiveAddress.chrome_handle,[long]$inactiveAddress.address_handle,$process.Id,$inactiveDraft)
    [OptionsFixture]::PostEnter([long]$inactiveAddress.address_handle,$process.Id)
    [OptionsFixture]::HostTick([long]$addressTree.window_handle,$process.Id)
    Address-Unchanged $inactiveAddress $inactiveDraft
    $addressTreeAfter=Tree;$currentAfter=Request @('identify')
    if($currentAfter.surface -ne $currentBefore.surface){throw 'Inactive address Enter changed the selected sibling surface'}
    foreach($other in @($addressTree.browsers|Where-Object {$_.id -ne $first.surface})){
        $same=@($addressTreeAfter.browsers|Where-Object {$_.id -eq $other.id})
        if($same.Count -ne 1 -or $same[0].url -cne $other.url -or $same[0].generation -ne $other.generation -or $same[0].view_handle -ne $other.view_handle){throw 'Inactive address Enter was routed to a sibling browser'}
    }
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
    if ($detachedFind.panel_handle -ne $mainFind.panel_handle -or $detachedFind.panel_query_handle -ne $mainFind.panel_query_handle -or -not (Same-Text $detachedFind.query $oneTitle)) {throw 'Detach replaced find controls or changed the executed query independently of the native draft'}
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
    if ($returnedFind.panel_handle -ne $detachedFind.panel_handle -or $returnedFind.panel_query_handle -ne $detachedFind.panel_query_handle -or -not (Same-Text $returnedFind.query $oneTitle)) {throw 'Reattach replaced find controls or changed the executed query independently of the native draft'}
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
    $bookmarks=Bookmarks-Open $restoredPane 1;if(-not (Same-Text $bookmarks.rows[0].title $oneTitle)){throw 'Bookmarks did not persist across process restart'};Bookmarks-Close $bookmarks
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
    $detachedDraft=$origin+'/one#address-enter-한글-한-é-😀'
    [OptionsFixture]::SetText([long]$controlled.chrome_handle,[long]$controlled.address_handle,$process.Id,$detachedDraft)
    [OptionsFixture]::PostEnter([long]$controlled.address_handle,$process.Id)
    $controlled=Wait-Page $solePane '#address-enter-' $oneTitle
    if($controlled.view_handle -ne $soleStatus.view_handle -or $controlled.holder.window -ne $soleStatus.holder.window -or -not (Same-Text (Eval-Page $solePane 'decodeURI(location.href)') $detachedDraft) -or -not (Same-Text (Eval-Page $solePane 'window.survivor') '분리 생존 한 é 😀')){throw 'Detached address Enter lost its original view, DOM or Unicode URL'}
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
    }
    Verify-FilesClose
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
