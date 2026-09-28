# SPDX-License-Identifier: GPL-3.0-or-later
# Owned hidden native chrome only. Run under a60s run-check.ps1 Job.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",[ValidateSet('details','overflow')][string]$Case='details')
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path;$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
Add-Type -ReferencedAssemblies System.Drawing -Path (Join-Path $PSScriptRoot 'ChromeFixture.cs')
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs')
$directory=Join-Path $PSScriptRoot ('..\dist\evidence\chrome-'+[guid]::NewGuid());[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$cwd=Join-Path $directory 'workspace 한글';[IO.Directory]::CreateDirectory($cwd)|Out-Null
$clock=[Diagnostics.Stopwatch]::StartNew();$hostProcess=$null;$pipeName=$null;$clients=@();$shells=@();$cleanup=$false;$cleanupErrors=@();$hostOut=$null;$hostErr=$null
$evidence=[ordered]@{started=[DateTime]::UtcNow.ToString('o');mode='hidden-native-chrome';case=$Case;baseline=$false;checks=@();observations=@();artifacts=@();desktopInput=$false;clipboardAccess=$false;webviewCapture=$false;deferred=@('No physical input, IME, foreground focus, accessibility, per-monitor DPI or full WebView screenshot acceptance.','PNG uses the production native button renderer and live HWND geometry on an offscreen DIB; it is not a composed desktop/GPU screenshot. WebView pixels are absent.')}
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
function Capture([string]$Name,$Tree) {
    Budget|Out-Null;$handle=[long]$Tree.window_handle;$controls=@([ChromeFixture]::Read($handle,$hostProcess.Id));$size=[ChromeFixture]::Size($handle,$hostProcess.Id)
    $path=Join-Path $directory ($Name+'.png');$bmp=Join-Path $directory ($Name+'.bmp')
    $capture=Request @('chrome-capture',$bmp)
    [ChromeFixture]::Png($bmp,$path);$background=[ChromeFixture]::Pixel($path,1,100)
    $record=[ordered]@{name=$Name;path=$path;window=$handle;client=$size;dpi=[ChromeFixture]::GetDpiForWindow([IntPtr]$handle);controls=$controls;background=$background;layout=$Tree.layout;chrome=$Tree.chrome;terminalIdentities=(Identities $Tree);capture=$capture}
    $script:evidence.observations+=$record;$script:evidence.artifacts+=@{path=$path;bytes=(Get-Item -LiteralPath $path).Length;sha256=(Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant();scope='owned hidden native chrome only'}
    Require ($background -eq $(if($Tree.chrome.theme -eq 'light'){'#f2f1f0'}else{'#24272e'})) 'Actual native background pixels do not match the configured palette'
    $scale=[Math]::Max(96,$record.dpi)/96.0;$shown=@($controls|Where-Object {$_.Shown})
    Require (@($shown|Where-Object {$_.Text -eq 'Workspaces'}).Count -eq 1) 'Workspaces header missing or duplicated'
    $workspace=@($Tree.workspaces|Where-Object {$_.id -eq $Tree.active_workspace})[0]
    $rowInfo=@($Tree.chrome.controls|Where-Object {$_.kind -eq 'workspace' -and $_.workspace -eq $workspace.id})[0]
    $row=@($shown|Where-Object {$_.Handle -eq $rowInfo.handle})
    Require ($Tree.chrome.sidebar_width_dip -eq 260 -and $Tree.chrome.workspace_row_height_dip -eq 58) 'Sidebar dimensions differ from the Linux alignment baseline'
    Require ($row.Count -eq 1 -and $row[0].Text.StartsWith($workspace.name.Replace('&','&&')+"`n")) 'Active workspace is hidden or lost its two-line native caption'
    $workspaceIndex=[Array]::IndexOf(@($Tree.workspaces.id),$workspace.id)
    Require ([Math]::Abs($row[0].Y-(40+58*($workspaceIndex-$Tree.chrome.sidebar_offset))*$scale) -le 2) 'Workspace row position differs from visible sidebar order'
    $selectionPixel=[ChromeFixture]::Pixel($path,($row[0].X+$row[0].Width-8),($row[0].Y+12))
    Require ($selectionPixel -eq $(if($Tree.chrome.theme -eq 'light'){'#dae6f5'}else{'#313741'})) 'Selected workspace was not actually painted in the owned hidden capture'
    $muted=if($Tree.chrome.theme -eq 'light'){'#5f6269'}else{'#abb1bc'}
    Require ([ChromeFixture]::ColorCount($path,($row[0].X+12),($row[0].Y+[int](27*$scale)),($row[0].Width-24),([int](20*$scale)),$muted) -gt 5) 'Native second-line path text was not painted'
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
        $area=$pane[1];$tabs=@($shown|Where-Object {$_.X -ge $area.x -and $_.X -lt $area.x+$area.width -and [Math]::Abs($_.Y-$area.y) -le 1})
        $menu=@($tabs|Where-Object {$_.Text -eq 'Pane actions' -or $_.Text -eq 'More'});$plus=@($tabs|Where-Object {$_.Text -eq '+'})
        Require ($menu.Count -eq 1 -and $plus.Count -eq 1 -and $plus[0].X -lt $menu[0].X -and [Math]::Abs($menu[0].X+$menu[0].Width-($area.x+$area.width)) -le 4*$scale) 'Pane +/More controls are not aligned at the right edge'
        Require ($tabs.Count -ge 4 -and @($tabs|Where-Object {$_.Height -gt 30*$scale}).Count -eq 0) 'Pane tab strip is not compact'
    }
    return $record
}
function FocusPaint($Tree,$Capture) {
    $selected=@($Tree.chrome.controls|Where-Object {$_.kind -eq 'tab' -and $_.selected -and $_.layout_visible})
    Require (@($selected|Where-Object {$_.focused}).Count -eq 1) 'Exactly one visible selected tab must be in the focused pane'
    foreach($tab in $selected){
        $r=$tab.rect;$actual=[ChromeFixture]::Pixel($Capture.path,($r.x+[int]($r.width/2)),($r.y+$r.height-1))
        $expected=if($Tree.chrome.theme -eq 'light'){if($tab.focused){'#2066ba'}else{'#d1d1d3'}}else{if($tab.focused){'#78aeed'}else{'#454a55'}}
        Require ($actual -eq $expected) 'Native active-tab accent does not distinguish the focused pane'
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
    return Capture $Name $tree
}
try {
    $doctor=Probe @('doctor');Require ($doctor.background_testing -and $doctor.status -eq 'ok') 'A working hidden debug build is required; no host launched'
    $started=[DateTime]::UtcNow;$startup=[Diagnostics.Stopwatch]::StartNew()
    $hostProcess=[CliProbe]::Start($gui,@('--temporary','--shell=cmd','--cwd',$cwd),$cwd,$directory);$hostOut=$hostProcess.StandardOutput.ReadToEndAsync();$hostErr=$hostProcess.StandardError.ReadToEndAsync()
    $file=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($hostProcess.Id).json"
    do {
        Require (-not $hostProcess.HasExited -and $startup.ElapsedMilliseconds -lt 8000) 'Owned host discovery exceeded eight seconds or exited'
        if((Test-Path -LiteralPath $file) -and (Get-Item -LiteralPath $file).LastWriteTimeUtc -ge $started){$record=Get-Content -Raw -LiteralPath $file|ConvertFrom-Json;Require ($record.pid -eq $hostProcess.Id) 'Discovery belongs to another process';$pipeName=$record.pipe;break}
        Start-Sleep -Milliseconds 20
    } while($true)
    $left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup budget exhausted';$identity=Request @('identify') ([int][Math]::Min(5000,$left));Require ($identity.pid -eq $hostProcess.Id) 'IPC pipe belongs to another process'
    $left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup budget exhausted';$tree=Ready 1 ([int]$left)
    $evidence.observations+=@{kind='startup';pid=$hostProcess.Id;elapsedMs=$startup.ElapsedMilliseconds;pipe=$pipeName}
    $initial=Capture 'initial' $tree
    if($Case -eq 'details') {
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
        Require ($row.Count -eq 1 -and [ChromeFixture]::Pixel($metadata.path,($row[0].X+1),($row[0].Y+15)) -eq '#12abef') 'Live Unicode path/name or actual model color stripe was lost'
        $evidence.checks+=@{name='unicode_cwd_metadata_and_color_update_preserve_native_handles_and_terminal_process';passed=$true}
        Request @('split','vertical','--shell=cmd')|Out-Null;$tree=Ready 2
        Require ((Identities $tree).Contains($original)) 'Split replaced the original terminal process'
        $split=Capture 'split' $tree;FocusPaint $tree $split;$stable=Identities $tree;$controls=ControlIds $tree
        Request @('focus-pane',$source.pane)|Out-Null;$tree=Tree
        Require ((ControlIds $tree) -eq $controls -and (Identities $tree) -eq $stable) 'Pane focus replaced native controls or terminal processes'
        $focused=Capture 'focus-original' $tree;FocusPaint $tree $focused
        $light=Theme 'light' $stable;$dark=Theme 'dark' $stable
        Require ($light.background -ne $dark.background) 'Light/dark setting did not change actual native background pixels'
        $evidence.checks+=@{name='hidden_native_workspace_tab_geometry_owner_draw_and_theme_pixels_preserve_terminal_processes';passed=$true}
        $browser=(Request @('browser','open','about:blank','--pane',$source.pane)).browser_pane_opened
        $file=Join-Path $changed '편집 한글.txt';[IO.File]::WriteAllText($file,'한글 editor chrome fixture', (New-Object Text.UTF8Encoding($false)))
        Request @('editor','open',$file,'--root',$changed,'--pane',$source.pane)|Out-Null
        $tree=Tree;Require (@($tree.browsers).Count -eq 1 -and @($tree.editors).Count -eq 1 -and (Identities $tree) -eq $stable) 'Mixed surface chrome changed terminal identities or lost a surface'
        $mixed=Capture 'terminal-browser-editor' $tree;FocusPaint $tree $mixed
        $evidence.checks+=@{name='terminal_browser_editor_native_chrome_capture_and_terminal_identity_preservation';passed=$true}
    } else {
        [ChromeFixture]::Resize([long]$tree.window_handle,$hostProcess.Id,900,400);$tree=Tree
        $dpi=[ChromeFixture]::GetDpiForWindow([IntPtr]([long]$tree.window_handle));$scale=[Math]::Max(96,$dpi)/96.0
        $rows=[int][Math]::Floor((400-160*$scale)/(58*$scale));Require ($rows -ge 1 -and $rows -le 6) 'Owned resize did not produce a bounded sidebar overflow case'
        $first=$tree.active_workspace
        for($i=0;$i -lt $rows;$i++){Request @('new-workspace','--cwd',$cwd,'--shell=cmd')|Out-Null}
        $tree=Ready ($rows+1);$last=$tree.active_workspace;$stable=Identities $tree
        $lastCapture=Capture 'overflow-last' $tree
        Require ($tree.chrome.sidebar_offset -eq 1) 'Last active workspace did not scroll into view'
        Require (@($lastCapture.controls|Where-Object {$_.Text -eq 'Next' -and $_.Shown -and -not $_.Enabled}).Count -eq 1) 'Last-page Next control should be disabled'
        Request @('workspace','focus',$first)|Out-Null;$tree=Tree;$firstCapture=Capture 'overflow-first' $tree
        Require ($tree.chrome.sidebar_offset -eq 0 -and (Identities $tree) -eq $stable) 'First active workspace did not scroll into view or restarted a terminal'
        Require (@($firstCapture.controls|Where-Object {$_.Text -eq 'Previous' -and $_.Shown -and -not $_.Enabled}).Count -eq 1) 'First-page Previous control should be disabled'
        $evidence.checks+=@{name='overflow_active_workspace_visibility_footer_bounds_and_paging_endpoints';passed=$true;visibleRows=$rows;workspaces=$rows+1;first=$first;last=$last}
    }
    Request @('quit','--discard-state')|Out-Null;Require ($hostProcess.WaitForExit((Budget 5000))) 'Owned host did not quit within five seconds';Require ($hostProcess.ExitCode -eq 0) 'Owned host exited with failure'
    $evidence.status='passed_background_chrome_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
    $cleanup=$true
    if($hostProcess){
        try {if(-not $hostProcess.HasExited -and $pipeName){Request @('quit','--discard-state')|Out-Null};if(-not $hostProcess.WaitForExit(5000)){$hostProcess.Kill();[CliProbe]::WaitAfterKill($hostProcess);$cleanupErrors+='Owned host required forced cleanup'}} catch {$cleanupErrors+=$_.Exception.Message;if(-not $hostProcess.HasExited){$hostProcess.Kill();[CliProbe]::WaitAfterKill($hostProcess)}}
        $outDone=$hostOut.Wait(500);$errDone=$hostErr.Wait(500);$evidence.hosts=@($hostProcess.Id);$evidence.observations+=@{kind='host-exit';exitCode=$hostProcess.ExitCode;stdoutComplete=$outDone;stderrComplete=$errDone;stdout=[CliProbe]::Output($hostOut);stderr=[CliProbe]::Output($hostErr)};$hostProcess.Dispose()
    }
    if($cleanupErrors.Count){$evidence.status='failed';$evidence.cleanupErrors=$cleanupErrors}
    $evidence.shells=$shells;$evidence.clientPids=$clients;$evidence.elapsedMs=$clock.ElapsedMilliseconds;$evidence.finished=[DateTime]::UtcNow.ToString('o')
    $evidence|ConvertTo-Json -Depth 24|Set-Content -Encoding UTF8 (Join-Path $directory 'native-chrome-background.json');Write-Output ('Evidence: '+$directory)
}
if($cleanupErrors.Count){throw ($cleanupErrors -join '; ')}
[ordered]@{status=$evidence.status;baseline=$false;checks=$evidence.checks.Count;elapsedMs=$evidence.elapsedMs}|ConvertTo-Json -Compress
