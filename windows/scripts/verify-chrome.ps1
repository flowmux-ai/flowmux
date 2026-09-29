# SPDX-License-Identifier: GPL-3.0-or-later
# Owned hidden native chrome only. Run under a60s run-check.ps1 Job.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",[ValidateSet('details','overflow','resize')][string]$Case='details')
if (-not $env:FLOWMUX_TEST_ARTIFACT_ROOT) { throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.' }
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path;$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
Add-Type -ReferencedAssemblies System.Drawing -Path (Join-Path $PSScriptRoot 'ChromeFixture.cs')
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs')
$directory=Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT ('chrome-'+[guid]::NewGuid());[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$cwd=Join-Path $directory 'workspace 한글';[IO.Directory]::CreateDirectory($cwd)|Out-Null
$clock=[Diagnostics.Stopwatch]::StartNew();$hostProcess=$null;$pipeName=$null;$clients=@();$shells=@();$cleanup=$false;$cleanupErrors=@();$hostOut=$null;$hostErr=$null
$evidence=[ordered]@{started=[DateTime]::UtcNow.ToString('o');mode='hidden-native-chrome';case=$Case;baseline=$false;hosts=@();checks=@();observations=@();lastCapture=$null;desktopInput=$false;clipboardAccess=$false;webviewCapture=$false;deferred=@('No physical input, IME, foreground focus, accessibility, per-monitor DPI or full WebView screenshot acceptance.','PNG uses the production native button renderer and live HWND geometry on an offscreen DIB; it is not a composed desktop/GPU screenshot. WebView pixels are absent.')}
function Require([bool]$Condition,[string]$Message) {if(-not $Condition){throw $Message}}
function Budget([int]$Maximum=5000) {
    $remaining=55000-$clock.ElapsedMilliseconds
    if(-not $cleanup){Require ($remaining -gt 0) 'Chrome verifier exhausted its55s inner budget';return [int][Math]::Min($Maximum,$remaining)}
    return $Maximum
}
function Probe([string[]]$Arguments,[int]$Maximum=5000) {
    $p=[CliProbe]::Start($cli,$Arguments,$cwd,$directory);$script:clients+=,$p.Id;$out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
    try {
        Require ($p.WaitForExit((Budget $Maximum))) 'Owned chrome CLI exceeded its bounded deadline; no retry'
        Require ($out.Wait(500) -and $err.Wait(500)) 'Owned CLI output did not close'
        Require ($p.ExitCode -eq 0) ('Owned CLI failed: '+[CliProbe]::Output($err)+' '+[CliProbe]::Output($out))
        return ([CliProbe]::Output($out)|ConvertFrom-Json)
    } finally {if(-not $p.HasExited){$p.Kill();[CliProbe]::WaitAfterKill($p)};$p.Dispose()}
}
function Request([string[]]$Arguments,[int]$Maximum=5000) {Require ([bool]$script:pipeName) 'Explicit owned pipe required';return Probe (@('--pipe',$script:pipeName,'--json')+$Arguments) $Maximum}
function Tree([int]$Maximum=5000) {
    $tree=Request @('tree') $Maximum;$handle=[IntPtr]([long]$tree.window_handle)
    Require ($tree.background_testing -and -not [CliProbe]::IsWindowVisible($handle) -and [CliProbe]::GetForegroundWindow() -ne $handle) 'Expected exact hidden background host'
    [ChromeFixture]::Size([long]$tree.window_handle,$hostProcess.Id)|Out-Null
    return $tree
}
function Ready([int]$Count,[int]$Maximum=5000) {
    $wait=[Diagnostics.Stopwatch]::StartNew()
    do {
        $left=$Maximum-$wait.ElapsedMilliseconds;Require ($left -gt 0) 'Owned terminal readiness exceeded condition budget'
        $tree=Tree ([int][Math]::Min(5000,$left))
        if(@($tree.surfaces).Count -eq $Count -and @($tree.surfaces|Where-Object {-not $_.ready -or -not $_.running -or -not $_.pid}).Count -eq 0){$script:shells=@($tree.surfaces|ForEach-Object {$_.pid});return $tree}
        Start-Sleep -Milliseconds ([int][Math]::Min(20,[Math]::Max(1,$Maximum-$wait.ElapsedMilliseconds)))
    } while($true)
}
function Identities($Tree) {return (@($Tree.surfaces|Sort-Object id|ForEach-Object {$_.id.ToString()+':'+$_.pid.ToString()}) -join ',')}
function ControlIds($Tree) {return (@($Tree.chrome.controls|Sort-Object handle|ForEach-Object {$_.handle.ToString()}) -join ',')}
function Start-Owned([string[]]$LaunchArgs) {
    $started=[DateTime]::UtcNow;$startup=[Diagnostics.Stopwatch]::StartNew();$script:pipeName=$null
    $script:hostProcess=[CliProbe]::Start($gui,$LaunchArgs,$cwd,$directory);$script:hostOut=$hostProcess.StandardOutput.ReadToEndAsync();$script:hostErr=$hostProcess.StandardError.ReadToEndAsync()
    $evidence.hosts+=,$hostProcess.Id
    $file=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($hostProcess.Id).json"
    do {
        Require (-not $hostProcess.HasExited -and $startup.ElapsedMilliseconds -lt 8000) 'Owned host discovery exceeded eight seconds or exited'
        if((Test-Path -LiteralPath $file) -and (Get-Item -LiteralPath $file).LastWriteTimeUtc -ge $started){$record=Get-Content -Raw -LiteralPath $file|ConvertFrom-Json;Require ($record.pid -eq $hostProcess.Id) 'Discovery belongs to another process';$script:pipeName=$record.pipe;break}
        Start-Sleep -Milliseconds 20
    } while($true)
    $left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup budget exhausted';$identity=Request @('identify') ([int][Math]::Min(5000,$left));Require ($identity.pid -eq $hostProcess.Id) 'IPC pipe belongs to another process'
    $left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup budget exhausted';$tree=Ready 1 ([int]$left)
    $evidence.observations+=@{kind='startup';pid=$hostProcess.Id;elapsedMs=$startup.ElapsedMilliseconds;pipe=$pipeName}
    return $tree
}
function Record-Exit {
    $outDone=$hostOut.Wait(500);$errDone=$hostErr.Wait(500);$evidence.observations+=@{kind='host-exit';pid=$hostProcess.Id;exitCode=$hostProcess.ExitCode;stdoutComplete=$outDone;stderrComplete=$errDone;stdout=[CliProbe]::Output($hostOut);stderr=[CliProbe]::Output($hostErr)}
    $hostProcess.Dispose();$script:hostProcess=$null;$script:pipeName=$null
}
function Await-Width([int]$Width,[bool]$Dragging) {
    $wait=[Diagnostics.Stopwatch]::StartNew()
    do {
        $left=5000-$wait.ElapsedMilliseconds;Require ($left -gt 0) 'Sidebar drag did not converge within five seconds'
        $tree=Tree ([int]$left)
        Require ([ChromeFixture]::CaptureHandle([long]$tree.window_handle,$hostProcess.Id) -eq 0) 'Hidden drag captured the desktop pointer'
        if($tree.chrome.sidebar_width_dip -eq $Width -and $tree.chrome.sidebar_dragging -eq $Dragging){return $tree}
        Start-Sleep -Milliseconds 20
    }while($true)
}
function Capture([string]$Name,$Tree) {
    Budget|Out-Null;$handle=[long]$Tree.window_handle;$controls=@([ChromeFixture]::Read($handle,$hostProcess.Id));$size=[ChromeFixture]::Size($handle,$hostProcess.Id)
    $path=Join-Path $directory ($Name+'.png');$bmp=Join-Path $directory ($Name+'.bmp')
    $capture=Request @('chrome-capture',$bmp)
    [ChromeFixture]::Png($bmp,$path);$background=[ChromeFixture]::Pixel($path,1,100)
    $record=[ordered]@{name=$Name;path=$path;window=$handle;client=$size;dpi=[ChromeFixture]::GetDpiForWindow([IntPtr]$handle);controls=$controls;background=$background;layout=$Tree.layout;chrome=$Tree.chrome;terminalIdentities=(Identities $Tree);capture=$capture}
    $script:evidence.lastCapture=$record
    Require ($background -eq $(if($Tree.chrome.theme -eq 'light'){'#f2f1f0'}else{'#24272e'})) 'Actual native background pixels do not match the configured palette'
    $scale=[Math]::Max(96,$record.dpi)/96.0;$shown=@($controls|Where-Object {$_.Shown})
    Require (@($shown|Where-Object {$_.Text -eq 'Workspaces'}).Count -eq 1) 'Workspaces header missing or duplicated'
    $workspace=@($Tree.workspaces|Where-Object {$_.id -eq $Tree.active_workspace})[0]
    $rowInfo=@($Tree.chrome.controls|Where-Object {$_.kind -eq 'workspace' -and $_.workspace -eq $workspace.id})[0]
    $row=@($shown|Where-Object {$_.Handle -eq $rowInfo.handle})
    Require ($Tree.chrome.sidebar_width_dip -ge 160 -and $Tree.chrome.sidebar_width_dip -le 640 -and $Tree.chrome.workspace_row_height_dip -eq 58) 'Sidebar dimensions escaped supported bounds'
    Require ($row.Count -eq 1 -and $row[0].Text.StartsWith($workspace.name.Replace('&','&&')+"`n")) 'Active workspace is hidden or lost its native metadata caption'
    $rowHandles=@($Tree.chrome.controls|Where-Object {$_.kind -eq 'workspace' -and $_.layout_visible}|ForEach-Object {$_.handle})
    $rows=@($shown|Where-Object {$rowHandles -contains $_.Handle}|Sort-Object Y);$expectedY=$Tree.chrome.sidebar_list_top
    foreach($item in $rows){
        $lineCount=@($item.Text -split "`n").Count-1
        Require ($lineCount -ge 1 -and $lineCount -le 3) 'Workspace metadata does not contain one to three pane lines'
        $height=[int][Math]::Round((58+20*($lineCount-1))*$scale)
        Require ([Math]::Abs($item.Y-$expectedY) -le 1 -and [Math]::Abs($item.Height-($height-[Math]::Round(2*$scale))) -le 1) 'Variable workspace rows overlap or retain a fixed height'
        $expectedY+=$height
    }
    $selectionPixel=[ChromeFixture]::Pixel($path,($row[0].X+$row[0].Width-4),($row[0].Y+$row[0].Height-4))
    Require ($selectionPixel -eq $background) 'Selected workspace does not retain the sidebar background'
    $accent=if($Tree.chrome.theme -eq 'light'){'#2066ba'}else{'#78aeed'}
    $stripe=[int][Math]::Max(1,[Math]::Round(5*$scale))
    Require ([ChromeFixture]::ColorCount($path,$row[0].X,$row[0].Y,$stripe,$row[0].Height,$accent) -eq $stripe*$row[0].Height) 'Selected workspace lacks its full-height accent stripe independent of workspace color'
    $muted=if($Tree.chrome.theme -eq 'light'){'#5f6269'}else{'#abb1bc'}
    Require ([ChromeFixture]::ColorCount($path,($row[0].X+12),($row[0].Y+[int](27*$scale)),($row[0].Width-24),([int](20*$scale)),$muted) -gt 5) 'Native second-line path text was not painted'
    $textLeft=$row[0].X+[int][Math]::Round((15+$(if($workspace.color){10}else{0}))*$scale)
    $gutterX=$textLeft+[int][Math]::Round(6*$scale);$lineCount=@($row[0].Text -split "`n").Count-1
    for($line=0;$line -lt $lineCount;$line++){
        $top=$row[0].Y+[int][Math]::Round((27+20*$line)*$scale);$middle=$top+[int][Math]::Floor([Math]::Round(20*$scale)/2)
        Require ([ChromeFixture]::Pixel($path,$gutterX,($top+1)) -eq $muted -and [ChromeFixture]::Pixel($path,($gutterX+2),$middle) -eq $muted) 'Workspace metadata tree connector missing'
        if($line+1 -eq $lineCount){Require ([ChromeFixture]::Pixel($path,$gutterX,($middle+3)) -eq $background) 'Last metadata branch continues below its leaf'}
        else{Require ([ChromeFixture]::Pixel($path,$gutterX,($middle+3)) -eq $muted) 'Intermediate metadata branch is disconnected'}
    }
    $footerHandles=@($Tree.chrome.controls|Where-Object {$_.kind -in @('settings','files','search_all','open_file')}|ForEach-Object {$_.handle})
    $footer=@($shown|Where-Object {$footerHandles -contains $_.Handle -and $_.X -lt $Tree.chrome.sidebar_actual_width})
    $footerCount=if($Tree.chrome.sidebar_actual_width -lt 136*$scale){0}elseif($Tree.chrome.sidebar_actual_width -lt 232*$scale){3}else{4}
    Require ($footer.Count -eq $footerCount -and @($footer|Where-Object {[Math]::Abs($_.Y-($size[1]-32*$scale)) -gt 2 -or [Math]::Abs($_.Width-28*$scale) -gt 2}).Count -eq 0) 'Footer actions do not follow compact row visibility or geometry'
    foreach($button in $footer){Require ([ChromeFixture]::ColorCount($path,$button.X,$button.Y,$button.Width,$button.Height,$muted) -gt 5) 'Footer glyph was not painted'}
    if($Case -eq 'details'){foreach($button in $footer){
        $info=@($Tree.chrome.controls|Where-Object {$_.handle -eq $button.Handle})[0]
        Require ($info.tooltip -ceq $button.Text) 'Native tooltip text differs from its accessible button caption'
    }}
    $bell=@($shown|Where-Object {$_.Text -match '^Notifications \(\d+\)$'})
    Require ($bell.Count -eq 1 -and [Math]::Abs($bell[0].Y-5*$scale) -le 2 -and [Math]::Abs($bell[0].X+$bell[0].Width-($Tree.chrome.sidebar_actual_width-4*$scale)) -le 2) 'Notification bell is not at the header right edge'
    foreach($control in $shown){
        Require ($control.X -ge 0 -and $control.Y -ge 0 -and $control.Width -gt 0 -and $control.Height -gt 0 -and $control.X+$control.Width -le $size[0]+1 -and $control.Y+$control.Height -le $size[1]+1) 'Native chrome control escaped client bounds'
        Require ($control.Text -notmatch '[●○]') 'Legacy circle markers remain in native chrome text'
        if($control.Class -eq 'Button'){Require (($control.Style -band 15) -eq 11 -and $control.Font -ne 0) 'Expected ownerdraw BUTTON with an assigned font'}
    }
    for($i=0;$i -lt $shown.Count;$i++){for($j=$i+1;$j -lt $shown.Count;$j++){
        $a=$shown[$i];$b=$shown[$j]
        Require (-not ($a.X -lt $b.X+$b.Width -and $b.X -lt $a.X+$a.Width -and $a.Y -lt $b.Y+$b.Height -and $b.Y -lt $a.Y+$a.Height)) ('Native controls overlap: '+$a.Text+' / '+$b.Text)
    }}
    foreach($pane in @($Tree.layout.panes)){
        $area=$pane[1];$tabs=@($shown|Where-Object {$_.X -ge $area.x -and $_.X -lt $area.x+$area.width -and [Math]::Abs($_.Y-($area.y+[Math]::Round(4*$scale))) -le 1})
        $menu=@($tabs|Where-Object {$_.Text -eq 'Pane actions' -or $_.Text -eq 'More'});$plus=@($tabs|Where-Object {$_.Text -eq '+'})
        Require ($menu.Count -eq 1 -and $plus.Count -eq 1 -and $plus[0].X -lt $menu[0].X -and [Math]::Abs($menu[0].X+$menu[0].Width-($area.x+$area.width)) -le 4*$scale) 'Pane +/More controls are not aligned at the right edge'
        Require ($tabs.Count -ge 4 -and @($tabs|Where-Object {$_.Height -gt 24*$scale}).Count -eq 0) 'Pane tab strip is not compact'
        $toolHandles=@($Tree.chrome.controls|Where-Object {$_.pane -eq $pane[0] -and $_.kind -like 'pane_*'}|ForEach-Object {$_.handle})
        Require (@($tabs|Where-Object {$toolHandles -contains $_.Handle -and ([Math]::Abs($_.Height-22*$scale) -gt 1 -or [Math]::Abs($_.Width-22*$scale) -gt 1)}).Count -eq 0) 'Pane tools are not22DIP at the inset header position'
    }
    return $record
}
function Workspace-Unread($Tree,$Capture,[string]$Workspace,[bool]$Unread) {
    $info=@($Tree.chrome.controls|Where-Object {$_.kind -eq 'workspace' -and $_.workspace -eq $Workspace})
    Require ($info.Count -eq 1 -and $info[0].layout_visible -and $info[0].unread -eq $Unread) 'Workspace unread ownership or row visibility differs'
    $row=@($Capture.controls|Where-Object {$_.Handle -eq $info[0].handle})[0]
    $model=@($Tree.workspaces|Where-Object {$_.id -eq $Workspace})[0]
    Require ($row.Text.StartsWith($model.name.Replace('&','&&')+"`n") -and $row.Text -notmatch '^\[\d+\] ') 'Unread count changed the original workspace title'
    $scale=[Math]::Max(96,$Capture.dpi)/96.0;$diameter=[int][Math]::Max(1,[Math]::Round(7*$scale));$margin=$diameter
    $x=$row.X+$row.Width-$margin-$diameter;$y=$row.Y+$margin
    $accent=if($Tree.chrome.theme -eq 'light'){'#2066ba'}else{'#78aeed'}
    $count=[ChromeFixture]::ColorCount($Capture.path,$x,$y,$diameter,$diameter,$accent)
    if($Unread){
        Require ($count -gt $diameter*$diameter/3 -and $count -lt $diameter*$diameter -and [ChromeFixture]::Pixel($Capture.path,($x+[int][Math]::Floor($diameter/2)),($y+[int][Math]::Floor($diameter/2))) -eq $accent) 'Unread workspace dot was not painted as a7DIP circle'
    }else{Require ($count -eq 0) 'Workspace retained a stale unread dot'}
}
function FocusPaint($Tree,$Capture) {
    $scale=[Math]::Max(96,$Capture.dpi)/96.0;$line=[int][Math]::Max(1,[Math]::Round(2*$scale));$one=[int][Math]::Max(1,[Math]::Round($scale));$bar=[int][Math]::Round(28*$scale)
    $surface=if($Tree.chrome.theme -eq 'light'){'#ffffff'}else{'#282c34'}
    $accent=if($Tree.chrome.theme -eq 'light'){'#2066ba'}else{'#78aeed'}
    $border=if($Tree.chrome.theme -eq 'light'){'#d1d1d3'}else{'#454a55'}
    $selectedColor=if($Tree.chrome.theme -eq 'light'){'#dae6f5'}else{'#313741'}
    $selected=@($Tree.chrome.controls|Where-Object {$_.kind -eq 'tab' -and $_.selected -and $_.layout_visible})
    Require (@($selected|Where-Object {$_.focused}).Count -eq 1) 'Exactly one visible selected tab must be in the focused pane'
    $panes=@($Tree.layout.panes);$solo=@($Tree.chrome.controls|Where-Object {$_.kind -eq 'tab'}).Count -eq 1
    foreach($pane in $panes){
        $area=$pane[1];$active=@($selected|Where-Object {$_.pane -eq $pane[0]})
        Require ($active.Count -eq 1) 'A visible pane has no single active native tab'
        $expected=if(-not $solo -and $active[0].focused){$accent}else{$surface}
        Require ([ChromeFixture]::ColorCount($Capture.path,$area.x,$area.y,$area.width,$line,$expected) -eq $area.width*$line) 'Pane focus line is missing, partial or visible in a single-surface workspace'
        Require ([ChromeFixture]::Pixel($Capture.path,($area.x+[int]($area.width/2)),($area.y+$line)) -eq $surface) 'Pane focus line exceeds2DIP'
        Require ([ChromeFixture]::ColorCount($Capture.path,$area.x,($area.y+$bar-$one),$area.width,$one,$border) -eq $area.width*$one) 'Pane header bottom separator is missing or covered by a tab'
    }
    foreach($tab in $selected){
        $r=$tab.rect;$close=@($Tree.chrome.controls|Where-Object {$_.kind -eq 'tab_close' -and $_.surface -eq $tab.surface -and $_.layout_visible})
        Require ($close.Count -eq 1) 'Selected tab has no visible paired close control'
        $c=$close[0].rect;$multiple=@($Tree.chrome.controls|Where-Object {$_.kind -eq 'tab' -and $_.pane -eq $tab.pane}).Count -gt 1
        Require ($r.x+$r.width -eq $c.x -and $r.y -eq $c.y -and $r.height -eq $c.height -and [Math]::Abs($r.height-23*$scale) -le 1) 'Tab body and close are not a contiguous23DIP shape'
        $top=if($multiple){$accent}else{$selectedColor}
        Require ([ChromeFixture]::ColorCount($Capture.path,($c.x-1),$r.y,2,$line,$top) -eq 2*$line) 'Active tab top line does not continue across body/close, or appears on a single tab'
        foreach($x in @(($c.x-1),$c.x,($c.x+$c.width-2))){
            Require ([ChromeFixture]::Pixel($Capture.path,$x,($r.y+$r.height-2)) -eq $selectedColor) 'Selected tab body and close do not share one background'
        }
        Require ([ChromeFixture]::Pixel($Capture.path,$r.x,$r.y) -eq $surface -and [ChromeFixture]::Pixel($Capture.path,($c.x+$c.width-1),$c.y) -eq $surface) 'Active tab outer top corners are not rounded against the header surface'
        $radius=[int][Math]::Max(1,[Math]::Round(4*$scale))
        Require ([ChromeFixture]::Pixel($Capture.path,($r.x+$radius+1),$r.y) -eq $top -and [ChromeFixture]::Pixel($Capture.path,($c.x+$c.width-$radius-2),$c.y) -eq $top) 'Rounded corners consumed more than the intended4DIP tab inset'
    }
}
function Theme([string]$Name,[string]$Identities) {
    Request @('settings','set','theme',$Name)|Out-Null;$wait=[Diagnostics.Stopwatch]::StartNew()
    do {
        $left=5000-$wait.ElapsedMilliseconds;Require ($left -gt 0) 'Theme application exceeded five-second condition budget'
        $settings=Request @('settings','show') ([int]$left)
        $pending=@($settings.surfaces|Where-Object {-not $_.applied -or $_.applied.revision -ne $settings.document.revision -or $_.applied.terminal.theme -ne $Name})
        if($settings.document.terminal.theme -eq $Name -and $pending.Count -eq 0){
            $expected=if($Name -eq 'dark'){'#282c34'}else{'#ffffff'}
            Require (@($settings.surfaces|Where-Object {$_.applied.background -ne $expected}).Count -eq 0) 'Live terminal palette differs from Linux baseline'
            break
        }
        Start-Sleep -Milliseconds ([int][Math]::Min(20,[Math]::Max(1,5000-$wait.ElapsedMilliseconds)))
    } while($true)
    $tree=Tree;Require ((Identities $tree) -eq $Identities) 'Theme change replaced a live terminal identity or PID'
    $capture=Capture $Name $tree
    if($Case -eq 'details'){FocusPaint $tree $capture}
    return $capture
}
try {
    $doctor=Probe @('doctor');Require ($doctor.background_testing -and $doctor.status -eq 'ok') 'A working hidden debug build is required; no host launched'
    $launch=@('--shell=cmd','--cwd',$cwd);if($Case -ne 'resize'){$launch+=,'--temporary'}
    $tree=Start-Owned $launch
    # Pin this fixture name before separate IPC/native reads; startup titles arrive asynchronously.
    Request @('workspace','rename',$tree.active_workspace,'workspace 한글')|Out-Null;$tree=Tree
    $initial=Capture 'initial' $tree
    if($Case -eq 'details') {
        FocusPaint $tree $initial
        $notification=(Request @('notify','--global','--title','한글 알림','chrome fixture')).id
        $noticeTree=Tree;$notice=Capture 'unread-bell' $noticeTree
        $bell=@($notice.controls|Where-Object {$_.Text -eq 'Notifications (1)'})[0]
        Require ([ChromeFixture]::ColorCount($notice.path,$bell.X,$bell.Y,$bell.Width,$bell.Height,'#78aeed') -gt 5) 'Unread notification did not accent the actual bell glyph'
        Require (@($noticeTree.chrome.controls|Where-Object {$_.handle -eq $bell.Handle -and $_.tooltip -ceq 'Notifications (1)'}).Count -eq 1) 'Native bell tooltip did not update unread count'
        Request @('notifications','mark-read',$notification)|Out-Null;$tree=Tree
        Require (@($tree.chrome.controls|Where-Object {$_.handle -eq $bell.Handle -and $_.tooltip -ceq 'Notifications (0)'}).Count -eq 1) 'Native bell tooltip retained a stale unread count'
        $evidence.checks+=@{name='compact_footer_icons_native_tooltips_and_live_header_notification_accent';passed=$true}
        $source=Request @('identify');$nfc='한글 & 표시';$nfd=([string][char]0x1112)+[char]0x1161+[char]0x11AB+[char]0x1100+[char]0x1173+[char]0x11AF+' & 표시'
        Request @('workspace','rename',$source.workspace,$nfc)|Out-Null;Request @('rename-tab',$source.surface,$nfc)|Out-Null
        $nfcCapture=Capture 'hangul-nfc' (Tree)
        Request @('workspace','rename',$source.workspace,$nfd)|Out-Null;Request @('rename-tab',$source.surface,$nfd)|Out-Null
        $tree=Tree;$nfdCapture=Capture 'hangul-nfd' $tree
        Require ([string]::Equals($tree.workspaces[0].name,$nfd,[StringComparison]::Ordinal)) 'Display normalization changed the original model name'
        Require (@($nfdCapture.controls|Where-Object {[string]::Equals($_.Text,$nfd.Replace('&','&&'),[StringComparison]::Ordinal)}).Count -eq 1) 'Display normalization changed the original native tab caption'
        Require ((Get-FileHash -LiteralPath $nfcCapture.path -Algorithm SHA256).Hash -eq (Get-FileHash -LiteralPath $nfdCapture.path -Algorithm SHA256).Hash) 'Canonically equivalent Hangul captions do not paint identical native chrome'
        $compat='ㅎㅏㄴㄱㅡㄹ & 표시';Request @('workspace','rename',$source.workspace,$compat)|Out-Null;Request @('rename-tab',$source.surface,$compat)|Out-Null
        $compatCapture=Capture 'hangul-compatibility-jamo' (Tree)
        Require ((Get-FileHash -LiteralPath $compatCapture.path -Algorithm SHA256).Hash -ne (Get-FileHash -LiteralPath $nfcCapture.path -Algorithm SHA256).Hash) 'Display path incorrectly composed independent compatibility Jamo'
        $evidence.checks+=@{name='nfc_nfd_hangul_native_render_equivalence_preserves_original_model_and_hwnd_codepoints';passed=$true;nfc=$nfc;nfd=$nfd;compatibilityJamoRemainDistinct=$true;displayOnly=$true}
        $original=Identities $tree;$controls=ControlIds $tree;$source=Request @('identify');$name='한글 '+[char]0x1112+[char]0x1161+[char]0x11AB+' '+[char]::ConvertFromUtf32(0x1F600)+' & 작업'
        Request @('workspace','rename',$source.workspace,$name)|Out-Null;Request @('workspace','color',$source.workspace,'#12abef')|Out-Null
        $changed=Join-Path $cwd '경로 & 변경';[IO.Directory]::CreateDirectory($changed)|Out-Null
        Request @('send-keys',$source.pane,('cd /d "'+$changed+'"'))|Out-Null;Request @('send-key','Enter','--pane',$source.pane)|Out-Null
        $wait=[Diagnostics.Stopwatch]::StartNew()
        do {
            $left=5000-$wait.ElapsedMilliseconds;Require ($left -gt 0) 'CWD metadata did not converge within five seconds'
            $tree=Tree ([int]$left)
            if($tree.surfaces[0].cwd -eq $changed -and $tree.surfaces[0].cwd_reported){break};Start-Sleep -Milliseconds 20
        }while($true)
        Require ((Identities $tree) -eq $original -and (ControlIds $tree) -eq $controls) 'Metadata change replaced a terminal or native chrome control'
        $metadata=Capture 'unicode-cwd' $tree;$row=@($metadata.controls|Where-Object {$_.Text -eq ($name+"`n"+$changed).Replace('&','&&')})
        $scale=[Math]::Max(96,$metadata.dpi)/96.0;$colorX=$row[0].X+[int][Math]::Round(15*$scale);$colorWidth=[int][Math]::Max(1,[Math]::Round(4*$scale));$colorY=$row[0].Y+[int]($row[0].Height/2)
        Require ($row.Count -eq 1 -and [ChromeFixture]::ColorCount($metadata.path,$colorX,$colorY,$colorWidth,1,'#12abef') -eq $colorWidth -and [ChromeFixture]::Pixel($metadata.path,($colorX-1),$colorY) -eq $metadata.background -and [ChromeFixture]::Pixel($metadata.path,($colorX+$colorWidth),$colorY) -eq $metadata.background) 'Live Unicode path/name or separate4DIP workspace color bar was lost'
        $evidence.checks+=@{name='unicode_cwd_metadata_and_color_update_preserve_native_handles_and_terminal_process';passed=$true}
        Request @('new-tab','--shell=cmd','--cwd',$changed)|Out-Null;$tree=Ready 2
        Require (@($tree.layout.panes).Count -eq 1 -and (Identities $tree).Contains($original)) 'Multi-tab fixture changed its original pane or terminal'
        $multiple=Capture 'single-pane-multiple-tabs' $tree;FocusPaint $tree $multiple
        $beforeMove=Identities $tree;$surfaceNotice=Request @('notify','--surface',$source.surface,'--title','한글 한 알림','workspace unread ownership')
        Require ($surfaceNotice.accepted) 'Owned surface notification was not accepted'
        $tree=Tree;$unread=Capture 'workspace-unread' $tree;Workspace-Unread $tree $unread $source.workspace $true
        Request @('new-workspace','--shell=cmd','--cwd',$changed)|Out-Null;$tree=Ready 3;$destination=Request @('identify');$moving=Identities $tree
        Request @('workspace','rename',$destination.workspace,'알림 이동 작업공간')|Out-Null
        Request @('move-tab',$source.surface,'--to-pane',$destination.pane)|Out-Null;$tree=Tree
        Require ((Identities $tree) -eq $moving) 'Moving an unread surface changed a terminal PID'
        $moved=Capture 'workspace-unread-moved' $tree;Workspace-Unread $tree $moved $source.workspace $false;Workspace-Unread $tree $moved $destination.workspace $true
        Request @('notifications','mark-read',$surfaceNotice.id)|Out-Null;$tree=Tree
        $read=Capture 'workspace-read' $tree;Workspace-Unread $tree $read $source.workspace $false;Workspace-Unread $tree $read $destination.workspace $false
        Request @('move-tab',$source.surface,'--to-pane',$source.pane,'--index','0')|Out-Null
        Request @('workspace','close',$destination.workspace)|Out-Null;Request @('focus-tab',$source.surface)|Out-Null;$tree=Ready 2
        Require ((Identities $tree) -eq $beforeMove) 'Workspace unread roundtrip did not preserve the original terminal IDs and PIDs'
        $evidence.checks+=@{name='workspace_unread_dot_moves_with_surface_and_mark_read_preserves_raw_title_and_processes';passed=$true}
        Request @('split','vertical','--shell=cmd')|Out-Null;$tree=Ready 3
        Require ((Identities $tree).Contains($original)) 'Split replaced the original terminal process'
        $split=Capture 'split' $tree;FocusPaint $tree $split;$stable=Identities $tree;$controls=ControlIds $tree
        Request @('focus-pane',$source.pane)|Out-Null;$tree=Tree
        Require ((ControlIds $tree) -eq $controls -and (Identities $tree) -eq $stable) 'Pane focus replaced native controls or terminal processes'
        $focused=Capture 'focus-original' $tree;FocusPaint $tree $focused
        Request @('toggle-pane-zoom',$source.pane)|Out-Null;$tree=Tree
        Require ($tree.zoomed_pane -eq $source.pane -and @($tree.layout.panes).Count -eq 1 -and (Identities $tree) -eq $stable -and (ControlIds $tree) -eq $controls) 'Zoom changed native controls, processes or the selected pane'
        $zoom=Capture 'zoom-retains-focus-line' $tree;FocusPaint $tree $zoom
        Request @('toggle-pane-zoom',$source.pane)|Out-Null;$tree=Tree
        Require (-not $tree.zoomed_pane -and @($tree.layout.panes).Count -eq 2 -and (Identities $tree) -eq $stable -and (ControlIds $tree) -eq $controls) 'Unzoom did not restore panes without rebuilding native controls'
        $evidence.checks+=@{name='solo_multitab_split_focus_and_zoom_header_lines_joined_tab_shape';passed=$true}
        $light=Theme 'light' $stable;$dark=Theme 'dark' $stable
        Require ($light.background -ne $dark.background) 'Light/dark setting did not change actual native background pixels'
        $evidence.checks+=@{name='hidden_native_workspace_tab_geometry_owner_draw_and_theme_pixels_preserve_terminal_processes';passed=$true}
        $browser=(Request @('browser','open','about:blank','--pane',$source.pane)).browser_pane_opened
        $file=Join-Path $changed '편집 한글.txt';[IO.File]::WriteAllText($file,'한글 editor chrome fixture', (New-Object Text.UTF8Encoding($false)))
        Request @('editor','open',$file,'--root',$changed,'--pane',$source.pane)|Out-Null
        $tree=Tree;Require (@($tree.browsers).Count -eq 1 -and @($tree.editors).Count -eq 1 -and (Identities $tree) -eq $stable) 'Mixed surface chrome changed terminal identities or lost a surface'
        $mixed=Capture 'terminal-browser-editor' $tree;FocusPaint $tree $mixed
        $row=@($tree.chrome.controls|Where-Object {$_.kind -eq 'workspace' -and $_.workspace -eq $source.workspace})[0];$rowHandle=$row.handle
        $lines=@($row.label -split "`n")
        Require ($lines.Count -eq 4 -and $lines[1].StartsWith('Editor-') -and $lines[2].StartsWith('Browser-') -and $lines[3] -ceq $changed.Replace('&','&&')) 'Mixed pane metadata does not follow most-recent focus or include all three active surfaces'
        Request @('focus-pane',$browser.pane)|Out-Null;$tree=Tree
        $row=@($tree.chrome.controls|Where-Object {$_.kind -eq 'workspace' -and $_.workspace -eq $source.workspace})[0]
        Require ($row.handle -eq $rowHandle -and @($row.label -split "`n")[1].StartsWith('Browser-') -and @($row.label -split "`n")[2].StartsWith('Editor-') -and (Identities $tree) -eq $stable) 'Focusing a pane recreated the row or lost metadata MRU order'
        Capture 'metadata-mru' $tree|Out-Null
        Request @('close-pane',$browser.pane)|Out-Null;$tree=Tree
        $shrunk=Capture 'metadata-close-pane' $tree
        $row=@($tree.chrome.controls|Where-Object {$_.kind -eq 'workspace' -and $_.workspace -eq $source.workspace})[0]
        Require (@($row.label -split "`n").Count -eq 3 -and $row.label -notmatch 'Browser-' -and (Identities $tree) -eq $stable) 'Closed pane metadata or its row height was retained'
        $evidence.checks+=@{name='three_native_metadata_lines_tree_connectors_focus_mru_and_close_resize_preserve_processes';passed=$true}
        $evidence.checks+=@{name='terminal_browser_editor_native_chrome_capture_and_terminal_identity_preservation';passed=$true}
    } elseif($Case -eq 'overflow') {
        [ChromeFixture]::Resize([long]$tree.window_handle,$hostProcess.Id,900,400);$tree=Tree
        $dpi=[ChromeFixture]::GetDpiForWindow([IntPtr]([long]$tree.window_handle));$scale=[Math]::Max(96,$dpi)/96.0
        $withoutPager=[int][Math]::Floor((400-76*$scale)/(58*$scale));Require ($withoutPager -ge 1 -and $withoutPager -le 6) 'Owned resize did not produce a bounded sidebar overflow case'
        $first=$tree.active_workspace
        for($i=0;$i -lt $withoutPager;$i++){Request @('new-workspace','--cwd',$cwd,'--shell=cmd')|Out-Null}
        $tree=Ready ($withoutPager+1);$last=$tree.active_workspace
        Request @('split','vertical','--shell=cmd')|Out-Null;Request @('split','horizontal','--shell=cmd')|Out-Null;Request @('split','vertical','--shell=cmd')|Out-Null
        $tree=Ready ($withoutPager+4);$stable=Identities $tree
        $metadata=@($tree.chrome.controls|Where-Object {$_.kind -eq 'workspace' -and $_.workspace -eq $last})[0]
        Require (@($tree.layout.panes).Count -eq 4 -and @($metadata.label -split "`n").Count -eq 4) 'Four panes were not capped to three sidebar metadata lines'
        $rows=1+[int][Math]::Floor((400-202*$scale)/(58*$scale))
        $lastCapture=Capture 'overflow-last' $tree
        Require ($tree.chrome.sidebar_offset -eq $withoutPager+1-$rows) 'Last active workspace did not scroll into view'
        Require (@($lastCapture.controls|Where-Object {$_.Text -eq 'Next' -and $_.Shown -and -not $_.Enabled}).Count -eq 1) 'Last-page Next control should be disabled'
        [ChromeFixture]::Resize([long]$tree.window_handle,$hostProcess.Id,900,350);$tree=Tree
        Capture 'overflow-shorter' $tree|Out-Null
        $shortRows=1+[int][Math]::Floor((350-202*$scale)/(58*$scale))
        Require ($tree.chrome.sidebar_offset -eq $withoutPager+1-$shortRows -and (Identities $tree) -eq $stable) 'Shrinking variable rows hid the active workspace or restarted a terminal'
        [ChromeFixture]::Resize([long]$tree.window_handle,$hostProcess.Id,900,400)
        Request @('workspace','focus',$first)|Out-Null;$tree=Tree;$firstCapture=Capture 'overflow-first' $tree
        Require ($tree.chrome.sidebar_offset -eq 0 -and (Identities $tree) -eq $stable) 'First active workspace did not scroll into view or restarted a terminal'
        Require (@($firstCapture.controls|Where-Object {$_.Text -eq 'Previous' -and $_.Shown -and -not $_.Enabled}).Count -eq 1) 'First-page Previous control should be disabled'
        $evidence.checks+=@{name='overflow_active_workspace_visibility_footer_bounds_and_paging_endpoints';passed=$true;visibleRows=$rows;workspaces=$withoutPager+1;first=$first;last=$last}
    } else {
        Require ($tree.chrome.sidebar_width_dip -eq 260) 'Fresh isolated host did not use default sidebar width'
        $stable=Identities $tree;$controls=ControlIds $tree;$originalWidth=$tree.layout.panes[0][1].width
        $dpi=[ChromeFixture]::GetDpiForWindow([IntPtr]([long]$tree.window_handle));$scale=[Math]::Max(96,$dpi)/96.0
        foreach($width in @(340,200)){
            [ChromeFixture]::Pointer([long]$tree.window_handle,$hostProcess.Id,'down',([int]$tree.chrome.sidebar_actual_width+1),100)
            $tree=Await-Width ([int]$tree.chrome.sidebar_width_dip) $true
            [ChromeFixture]::Pointer([long]$tree.window_handle,$hostProcess.Id,'move',([int]($width*$scale)+1),100)
            $tree=Await-Width $width $true
            [ChromeFixture]::Pointer([long]$tree.window_handle,$hostProcess.Id,'up',([int]($width*$scale)+1),100)
            $tree=Await-Width $width $false
            Require ((Identities $tree) -eq $stable -and (ControlIds $tree) -eq $controls) 'Sidebar drag recreated native controls or terminal processes'
            Capture ('width-'+$width) $tree|Out-Null
        }
        Require ($tree.layout.panes[0][1].width -gt $originalWidth) 'Narrower sidebar did not enlarge live terminal bounds'
        [ChromeFixture]::Pointer([long]$tree.window_handle,$hostProcess.Id,'down',([int]$tree.chrome.sidebar_actual_width+1),100);$tree=Await-Width 200 $true
        [ChromeFixture]::Pointer([long]$tree.window_handle,$hostProcess.Id,'cancel',0,0);$tree=Await-Width 200 $false
        [ChromeFixture]::Pointer([long]$tree.window_handle,$hostProcess.Id,'move',400,100);$tree=Await-Width 200 $false
        [ChromeFixture]::Resize([long]$tree.window_handle,$hostProcess.Id,480,400);$tree=Tree
        Require ($tree.chrome.sidebar_width_dip -eq 200 -and $tree.chrome.sidebar_actual_width -le 160*$scale) 'Small-window clamp overwrote preferred sidebar width'
        Capture 'width-clamped' $tree|Out-Null
        $edge=[int]$tree.chrome.sidebar_actual_width+1
        [ChromeFixture]::Pointer([long]$tree.window_handle,$hostProcess.Id,'down',$edge,100);$tree=Await-Width 200 $true
        [ChromeFixture]::Pointer([long]$tree.window_handle,$hostProcess.Id,'up',$edge,100);$tree=Await-Width 200 $false
        [ChromeFixture]::Resize([long]$tree.window_handle,$hostProcess.Id,1000,600);$tree=Tree
        Require ($tree.chrome.sidebar_width_dip -eq 200 -and [Math]::Abs($tree.chrome.sidebar_actual_width-200*$scale) -le 1) 'Sidebar width did not recover after window expansion'
        $saved=Request @('save-state');Request @('quit')|Out-Null;Require ($hostProcess.WaitForExit((Budget 5000))) 'Width host did not save/quit within five seconds';Require ($hostProcess.ExitCode -eq 0) 'Width host failed on exit';Record-Exit
        $tree=Start-Owned @('--restore-window',$saved.window)
        Require ($tree.chrome.sidebar_width_dip -eq 200) 'Preferred sidebar width was lost on restart'
        Capture 'width-restored' $tree|Out-Null
        $evidence.checks+=@{name='owned_hidden_sidebar_drag_preserves_controls_processes_bounds_and_cancellation';passed=$true}
        $evidence.checks+=@{name='preferred_sidebar_width_survives_narrow_window_clamp_and_restart';passed=$true}
    }
    Request @('quit','--discard-state')|Out-Null;Require ($hostProcess.WaitForExit((Budget 5000))) 'Owned host did not quit within five seconds';Require ($hostProcess.ExitCode -eq 0) 'Owned host exited with failure'
    $evidence.status='passed_background_chrome_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
    $cleanup=$true
    if($hostProcess){
        try {if(-not $hostProcess.HasExited -and $pipeName){Request @('quit','--discard-state')|Out-Null};if(-not $hostProcess.WaitForExit(5000)){$hostProcess.Kill();[CliProbe]::WaitAfterKill($hostProcess);$cleanupErrors+='Owned host required forced cleanup'}} catch {$cleanupErrors+=$_.Exception.Message;if(-not $hostProcess.HasExited){$hostProcess.Kill();[CliProbe]::WaitAfterKill($hostProcess)}}
        Record-Exit
    }
    if($cleanupErrors.Count){$evidence.status='failed';$evidence.cleanupErrors=$cleanupErrors}
    $evidence.shells=$shells;$evidence.clientPids=$clients;$evidence.elapsedMs=$clock.ElapsedMilliseconds;$evidence.finished=[DateTime]::UtcNow.ToString('o')
    if ($evidence.status -eq 'failed') { $evidence|ConvertTo-Json -Depth 24|Set-Content -Encoding UTF8 (Join-Path $directory 'native-chrome-background.json') };if ($evidence.status -eq 'failed') { Write-Output ('Evidence: '+$directory) }
}
if($cleanupErrors.Count){throw ($cleanupErrors -join '; ')}
[ordered]@{status=$evidence.status;baseline=$false;checks=$evidence.checks.Count;elapsedMs=$evidence.elapsedMs}|ConvertTo-Json -Compress
