# SPDX-License-Identifier: GPL-3.0-or-later
# Exact owned hidden HWNDs only. 50s work + bounded cleanup, outer Job60s.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference='Stop';$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path;$cli=Join-Path $BuildDirectory 'flowmuxctl.exe';$gui=Join-Path $BuildDirectory 'flowmux.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'OptionsFixture.cs')
$base=if($env:FLOWMUX_TEST_ARTIFACT_ROOT){$env:FLOWMUX_TEST_ARTIFACT_ROOT}else{[IO.Path]::GetTempPath()};$directory=Join-Path $base ('tab-drag-'+[guid]::NewGuid());[IO.Directory]::CreateDirectory($directory)|Out-Null
$clock=[Diagnostics.Stopwatch]::StartNew();$owned=$null;$pipeName=$null;$hostOut=$null;$hostErr=$null;$checks=@();$failure=$null;$last=$null;$commandFailure=$null;$cleanupErrors=@();$cleaning=$false
function Require([bool]$Condition,[string]$Message){if(-not $Condition){throw $Message}}
function Budget([int]$Maximum=5000){if($cleaning){return $Maximum};$left=50000-$clock.ElapsedMilliseconds;Require ($left -gt 0) 'Tab drag work budget expired';return [int][Math]::Min($Maximum,$left)}
function Probe([string[]]$Arguments,[int]$Maximum=5000){
    Budget|Out-Null;$p=[CliProbe]::Start($cli,$Arguments,$directory,$directory);$out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
    try{Require ($p.WaitForExit((Budget $Maximum))) 'Owned CLI exceeded deadline; no retry';Require ($out.Wait(500) -and $err.Wait(500)) 'CLI output did not close';Require ($p.ExitCode -eq 0) ('CLI failed: '+[CliProbe]::Output($err));return ([CliProbe]::Output($out)|ConvertFrom-Json)}
    catch{$script:commandFailure=@{arguments=$Arguments;stdout=[CliProbe]::Output($out);stderr=[CliProbe]::Output($err)};throw}
    finally{if(-not $p.HasExited){$p.Kill();[CliProbe]::WaitAfterKill($p)};$p.Dispose()}
}
function Request([string[]]$Arguments,[int]$Maximum=5000){Require ([bool]$pipeName) 'Explicit owned pipe required';return Probe (@('--pipe',$pipeName,'--json')+$Arguments) $Maximum}
function Tree([int]$Maximum=5000){$t=Request @('tree') $Maximum;Require ($t.background_testing) 'Host is not hidden';[OptionsFixture]::Describe([long]$t.window_handle,$owned.Id)|Out-Null;$script:last=$t;return $t}
function Await([scriptblock]$Condition){$watch=[Diagnostics.Stopwatch]::StartNew();do{$left=5000-$watch.ElapsedMilliseconds;Require ($left -gt 0) 'Tab drag condition exceeded five seconds';$tree=Tree ([int]$left);if(& $Condition $tree){return $tree};Start-Sleep -Milliseconds 20}while($true)}
function Leaves($Node){if($Node.content){$Node}else{Leaves $Node.first;Leaves $Node.second}}
function Pane($Tree,[string]$Id){$found=@($Tree.workspaces|ForEach-Object {Leaves $_.root}|Where-Object {$_.id -ceq $Id});Require ($found.Count -eq 1) ('Missing pane '+$Id);return $found[0]}
function Order($Tree,[string]$Id){return (@((Pane $Tree $Id).content.surfaces.id)-join ',')}
function Mapping($Tree){return (@($Tree.workspaces|ForEach-Object {$w=$_.id;Leaves $_.root|ForEach-Object {$w+':'+$_.id+':'+(@($_.content.surfaces.id)-join ',')}})-join ';')}
function Identities($Tree){return (@($Tree.surfaces|Sort-Object id|ForEach-Object {$_.id.ToString()+':'+$_.pid.ToString()}) -join ',')}
function Tab($Tree,[string]$Id){$c=@($Tree.chrome.controls|Where-Object {$_.kind -ceq 'tab' -and $_.surface -ceq $Id -and $_.layout_visible});Require ($c.Count -eq 1) ('Visible owned tab missing: '+$Id);[OptionsFixture]::RelativeBounds([long]$Tree.window_handle,[long]$c[0].handle,$owned.Id)|Out-Null;return $c[0]}
function Pane-Rect($Tree,[string]$Id){foreach($entry in $Tree.layout.panes){if($entry[0] -ceq $Id){return $entry[1]}};throw ('Pane layout missing '+$Id)}
function Body-Rect($Tree,[string]$Id){$r=Pane-Rect $Tree $Id;$bar=[int][Math]::Round(28*$Tree.chrome.dpi/96.0,[MidpointRounding]::AwayFromZero);return @{x=$r.x;y=$r.y+$bar;width=$r.width;height=$r.height-$bar}}
function Body($Tree,[string]$Id,[string]$Zone='append'){$r=Body-Rect $Tree $Id;$fx=if($Zone -ceq 'right'){0.75}else{0.25};$fy=if($Zone -ceq 'down'){0.75}else{0.25};return @{x=[int]($r.x+$r.width*$fx);y=[int]($r.y+$r.height*$fy)}}
function Location($Tree,[string]$Surface){$found=@($Tree.workspaces|ForEach-Object {Leaves $_.root}|Where-Object {@($_.content.surfaces.id) -ccontains $Surface});Require ($found.Count -eq 1) ('Surface location is ambiguous '+$Surface);return $found[0].id}
function Topology($Tree){return ($Tree.workspaces|ForEach-Object {@{id=$_.id;root=$_.root}}|ConvertTo-Json -Depth 30 -Compress)}
function No-Preview($Tree){Require (-not $Tree.chrome.tab_drop_preview -or -not $Tree.chrome.tab_drop_preview.active) 'Split drop preview was not cleared'}
function Preview($Tree,[string]$Pane,[string]$Zone,$Body){
    $p=$Tree.chrome.tab_drop_preview;Require ($p -and $p.active -and $p.zone -ceq $Zone -and $p.pane -ceq $Pane -and -not $p.native_visible) 'Wrong or visible split preview'
    $popup=[OptionsFixture]::Describe([long]$p.window,$owned.Id);Require ($popup.Owner -eq $Tree.window_handle -and -not $popup.Enabled -and [OptionsFixture]::Parent([long]$p.window,$owned.Id) -eq $Tree.window_handle) 'Split preview has another owner or accepts input'
    $native=[OptionsFixture]::RelativeBounds([long]$Tree.window_handle,[long]$p.window,$owned.Id)
    $expected=if($Zone -ceq 'right'){@{x=$Body.x+[Math]::Floor($Body.width/2);y=$Body.y;width=$Body.width-[Math]::Floor($Body.width/2);height=$Body.height}}else{@{x=$Body.x;y=$Body.y+[Math]::Floor($Body.height/2);width=$Body.width;height=$Body.height-[Math]::Floor($Body.height/2)}}
    foreach($key in @('x','y','width','height')){Require ([Math]::Abs($p.rect.$key-$expected.$key) -le 1 -and [Math]::Abs($native.$key-$p.rect.$key) -le 1) ('Split preview half-body bounds differ: '+$key)}
}
function Split-Drag($Tree,[string]$Source,[string]$Target,[string]$Zone){$body=Body-Rect $Tree $Target;$point=Body $Tree $Target $Zone;$tree=Begin $Tree $Source;Motion $tree $point;$tree=Await {param($t) $t.chrome.tab_drop_preview -and $t.chrome.tab_drop_preview.active};Preview $tree $Target $Zone $body;$tree=Release $tree $point;No-Preview $tree;return $tree}
function Tab-Point($Tree,[string]$Id,[bool]$After){$c=Tab $Tree $Id;$right=$c.rect.x+$c.rect.width;$close=@($Tree.chrome.controls|Where-Object {$_.kind -ceq 'tab_close' -and $_.surface -ceq $Id -and $_.layout_visible});if($close.Count -eq 1){[OptionsFixture]::RelativeBounds([long]$Tree.window_handle,[long]$close[0].handle,$owned.Id)|Out-Null;$right=[Math]::Max($right,$close[0].rect.x+$close[0].rect.width)};return @{x=[int]$(if($After){$right-3}else{$c.rect.x+3});y=[int]($c.rect.y+$c.rect.height/2)}}
function Begin($Tree,[string]$Id){$c=Tab $Tree $Id;$x=[int]($c.rect.width/2);$y=[int]($c.rect.height/2);$script:press=@{x=[int]($c.rect.x+$x);y=[int]($c.rect.y+$y)};[OptionsFixture]::TabPointerDown([long]$Tree.window_handle,[long]$c.handle,$owned.Id,$x,$y);return Await {param($t) $t.chrome.tab_dragging}}
function Motion($Tree,$Point){[OptionsFixture]::HostPointer([long]$Tree.window_handle,$owned.Id,0x200,[int]$Point.x,[int]$Point.y)}
function Release($Tree,$Point){[OptionsFixture]::HostPointer([long]$Tree.window_handle,$owned.Id,0x202,[int]$Point.x,[int]$Point.y);return Await {param($t) -not $t.chrome.tab_dragging -and -not $t.chrome.workspace_dragging -and (-not $t.chrome.tab_drop_preview -or -not $t.chrome.tab_drop_preview.active)}}
function Drag($Tree,[string]$Source,$Point){$tree=Begin $Tree $Source;Motion $tree $Point;return Release $tree $Point}
function Workspace-Row($Tree,[string]$Id){$rows=@($Tree.chrome.controls|Where-Object {$_.kind -ceq 'workspace' -and $_.workspace -ceq $Id -and $_.layout_visible});Require ($rows.Count -eq 1) ('Visible workspace row missing '+$Id);[OptionsFixture]::RelativeBounds([long]$Tree.window_handle,[long]$rows[0].handle,$owned.Id)|Out-Null;return $rows[0]}
function Workspace-Point($Tree,[string]$Id,[bool]$After){$r=(Workspace-Row $Tree $Id).rect;return @{x=[int]($r.x+$r.width/2);y=[int]$(if($After){$r.y+$r.height-3}else{$r.y+3})}}
function Begin-Workspace($Tree,[string]$Id){$row=Workspace-Row $Tree $Id;$x=[int]($row.rect.width/2);$y=[int]($row.rect.height/2);$script:press=@{x=[int]($row.rect.x+$x);y=[int]($row.rect.y+$y)};[OptionsFixture]::TabPointerDown([long]$Tree.window_handle,[long]$row.handle,$owned.Id,$x,$y);return Await {param($t) $t.chrome.workspace_dragging -and -not $t.chrome.tab_dragging}}
function Drag-Workspace($Tree,[string]$Source,$Point){$tree=Begin-Workspace $Tree $Source;Motion $tree $Point;return Release $tree $Point}
function Workspace-Order($Tree){return (@($Tree.workspaces.id)-join ',')}
function Workspace-Metadata($Tree){return ($Tree.workspaces|Sort-Object id|ForEach-Object {[ordered]@{id=$_.id;name=$_.name;color=$_.color}}|ConvertTo-Json -Compress)}
function Stable-Workspaces($Tree){Stable $Tree;Require ((Workspace-Metadata $Tree) -ceq $workspaceMetadata) 'Workspace drag altered Unicode names or colors';No-Preview $Tree}
function Stable($Tree){Require ((Identities $Tree) -ceq $identities) 'Tab drag replaced a terminal process';foreach($item in $names.GetEnumerator()){$tabs=@($Tree.workspaces|ForEach-Object {Leaves $_.root}|ForEach-Object {$_.content.surfaces}|Where-Object {$_.id -ceq $item.Key});Require ($tabs.Count -eq 1 -and $tabs[0].title -ceq $item.Value) 'Drag changed a Unicode title or lost a tab'}}
function Passed([string]$Name){$script:checks+=$Name}
function Screen-Contains([string]$Surface,[string]$Text){$watch=[Diagnostics.Stopwatch]::StartNew();do{$left=5000-$watch.ElapsedMilliseconds;Require ($left -gt 0) 'Owned terminal output exceeded five seconds';$screen=Request @('read-screen','--surface',$Surface,'--recent') ([int]$left);if($screen.text.Contains($Text)){return};Start-Sleep -Milliseconds 20}while($true)}
try {
    $doctor=Probe @('doctor');Require ($doctor.background_testing -and $doctor.status -eq 'ok') 'Working hidden debug build required'
    $started=[DateTime]::UtcNow;$startup=[Diagnostics.Stopwatch]::StartNew();$owned=[CliProbe]::Start($gui,@('--temporary','--shell=cmd','--cwd',$directory),$directory,$directory);$hostOut=$owned.StandardOutput.ReadToEndAsync();$hostErr=$owned.StandardError.ReadToEndAsync();
    $file=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($owned.Id).json"
    do {Require (-not $owned.HasExited -and $startup.ElapsedMilliseconds -lt 8000) 'Host discovery exceeded eight seconds or exited';if((Test-Path -LiteralPath $file) -and (Get-Item -LiteralPath $file).LastWriteTimeUtc -ge $started){$record=Get-Content -Raw -LiteralPath $file|ConvertFrom-Json;Require ($record.pid -eq $owned.Id -and [bool]$record.pipe) 'Wrong discovery owner';$pipeName=$record.pipe;break};Start-Sleep -Milliseconds 20}while($true)
    $left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup budget exhausted';Require ((Request @('identify') ([int][Math]::Min(5000,$left))).pid -eq $owned.Id) 'Pipe owner mismatch'
    do {$left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup readiness exceeded eight seconds';$tree=Tree ([int][Math]::Min(5000,$left));if(@($tree.surfaces).Count -eq 1 -and $tree.surfaces[0].ready -and $tree.surfaces[0].running){break};Start-Sleep -Milliseconds 20}while($true)
    $a=Request @('identify');$names=@{};$marker='FM_DRAG_한글_한_é_925b';$names[$a.surface]='한글 한 é & 원본'
    Request @('rename-tab',$a.surface,$names[$a.surface])|Out-Null;Request @('send-keys',$a.pane,('echo '+$marker))|Out-Null;Request @('send-key','Enter','--pane',$a.pane)|Out-Null;Screen-Contains $a.surface $marker
    Request @('new-tab','--shell=cmd')|Out-Null;$b=Request @('identify');$names[$b.surface]='둘째 한글';Request @('rename-tab',$b.surface,$names[$b.surface])|Out-Null
    Request @('new-tab','--shell=cmd')|Out-Null;$c=Request @('identify');$names[$c.surface]='셋째 한 é & 탭';Request @('rename-tab',$c.surface,$names[$c.surface])|Out-Null
    $tree=Await {param($t) @($t.surfaces).Count -eq 3 -and @($t.surfaces|Where-Object {-not $_.ready -or -not $_.running}).Count -eq 0};$identities=Identities $tree;$original=Order $tree $a.pane
    Request @('send-keys',$c.pane,('echo '+$marker))|Out-Null;Request @('send-key','Enter','--surface',$c.surface)|Out-Null;Screen-Contains $c.surface $marker

    $tree=Begin $tree $a.surface;$tree=Release $tree $press;Require ((Order $tree $a.pane) -ceq $original -and (Request @('identify')).surface -ceq $a.surface) 'Ordinary tab click failed or reordered'
    $tree=Begin $tree $b.surface;$near=@{x=$press.x+1;y=$press.y+1};Motion $tree $near;$tree=Release $tree $near
    Require ((Order $tree $a.pane) -ceq $original -and (Request @('identify')).surface -ceq $b.surface) 'Below-threshold movement reordered or failed click selection';Stable $tree;Passed 'ordinary-click-and-below-threshold-selection'

    $point=Tab-Point $tree $a.surface $false;$tree=Drag $tree $c.surface $point;Require ((Order $tree $a.pane) -ceq (@($c.surface,$a.surface,$b.surface)-join ',')) 'Left insertion order differs'
    $point=Tab-Point $tree $b.surface $true;$tree=Drag $tree $c.surface $point;Require ((Order $tree $a.pane) -ceq $original) 'Right insertion did not adjust source removal index'
    foreach($after in @($false,$true)){$point=Tab-Point $tree $c.surface $after;$tree=Drag $tree $c.surface $point;Require ((Order $tree $a.pane) -ceq $original) 'Dropping on the source tab changed order'}
    Stable $tree;Passed 'same-pane-left-right-index-adjustment-and-self-drop'

    Request @('split','vertical','--shell=cmd')|Out-Null;$d=Request @('identify');$names[$d.surface]='분할 대상';Request @('rename-tab',$d.surface,$names[$d.surface])|Out-Null
    $tree=Await {param($t) @($t.surfaces).Count -eq 4 -and @($t.surfaces|Where-Object {-not $_.ready -or -not $_.running}).Count -eq 0};$identities=Identities $tree
    Request @('focus-tab',$c.surface)|Out-Null;$tree=Tree;$tree=Drag $tree $c.surface (Body $tree $d.pane)
    Require ((Order $tree $a.pane) -ceq (@($a.surface,$b.surface)-join ',') -and (Order $tree $d.pane) -ceq (@($d.surface,$c.surface)-join ',')) 'Cross-pane body drop did not append'
    $tree=Drag $tree $d.surface (Body $tree $a.pane);$tree=Drag $tree $c.surface (Body $tree $a.pane)
    Require (@($tree.layout.panes).Count -eq 1 -and (Order $tree $a.pane) -ceq (@($a.surface,$b.surface,$d.surface,$c.surface)-join ',')) 'Final source tab did not collapse its empty pane';Stable $tree;Passed 'cross-pane-append-and-last-source-collapse'

    Request @('new-workspace','--cwd',$directory,'--shell=cmd')|Out-Null;$e=Request @('identify');Request @('workspace','rename',$e.workspace,'한글 도착 공간')|Out-Null
    Request @('split','vertical','--shell=cmd')|Out-Null;$f=Request @('identify');$tree=Await {param($t) @($t.surfaces).Count -eq 6 -and @($t.surfaces|Where-Object {-not $_.ready -or -not $_.running}).Count -eq 0};$identities=Identities $tree
    Request @('workspace','focus',$a.workspace)|Out-Null;Request @('focus-tab',$c.surface)|Out-Null;$tree=Tree;$row=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'workspace' -and $_.workspace -ceq $e.workspace -and $_.layout_visible});Require ($row.Count -eq 1) 'Destination sidebar row is unavailable'
    [OptionsFixture]::RelativeBounds([long]$tree.window_handle,[long]$row[0].handle,$owned.Id)|Out-Null;$r=$row[0].rect;$point=@{x=[int]($r.x+$r.width/2);y=[int]($r.y+$r.height/2)};$tree=Drag $tree $c.surface $point
    Require ((Order $tree $f.pane) -ceq (@($f.surface,$c.surface)-join ',') -and (Order $tree $e.pane) -ceq $e.surface -and $tree.active_workspace -ceq $e.workspace) 'Sidebar drop ignored destination workspace focused pane';Stable $tree;Passed 'sidebar-workspace-focused-pane-append'

    foreach($cancel in @('escape','cancelmode','capturechanged')){
        $before=Mapping $tree;$point=Tab-Point $tree $f.surface $false;$tree=Begin $tree $c.surface;Motion $tree $point
        if($cancel -ceq 'escape'){[OptionsFixture]::PostEscape([long]$tree.window_handle,$owned.Id)}else{$message=if($cancel -ceq 'cancelmode'){0x1f}else{0x215};[OptionsFixture]::HostPointer([long]$tree.window_handle,$owned.Id,$message,0,0)}
        $tree=Await {param($t) -not $t.chrome.tab_dragging};$tree=Release $tree $point;Require ((Mapping $tree) -ceq $before) ('Cancelled drag committed: '+$cancel)
    };Stable $tree;Passed 'Escape-cancelmode-capturechanged-cancel'

    $before=Mapping $tree;$outside=@{x=-20;y=-20};$tree=Drag $tree $c.surface $outside;Require ((Mapping $tree) -ceq $before) 'Outside-client drop changed tabs';Stable $tree;Passed 'outside-client-drop-noop'

    $point=Tab-Point $tree $f.surface $false;$tree=Begin $tree $c.surface;Motion $tree $point
    Request @('move-tab',$c.surface,'--to-pane',$a.pane)|Out-Null;$tree=Await {param($t) -not $t.chrome.tab_dragging};$afterMutation=Mapping $tree;$tree=Release $tree $point
    Require ((Mapping $tree) -ceq $afterMutation -and (Order $tree $a.pane) -ceq (@($a.surface,$b.surface,$d.surface,$c.surface)-join ',')) 'Stale pointer release overrode a structural mutation';Stable $tree;Screen-Contains $a.surface $marker;Screen-Contains $c.surface $marker;Passed 'stale-rebuild-cancel-and-Unicode-PID-output-preservation'

    # Split the original pane using its existing live tabs; no extra PTYs.
    $tree=Split-Drag $tree $c.surface $a.pane 'right';$right=Location $tree $c.surface;$ws=@($tree.workspaces|Where-Object {$_.id -ceq $a.workspace})[0]
    Require ($right -cne $a.pane -and @($tree.layout.panes).Count -eq 2 -and $ws.root.direction -ceq 'vertical' -and $ws.root.first.id -ceq $a.pane -and $ws.root.second.id -ceq $right -and (Order $tree $right) -ceq $c.surface -and (Order $tree $a.pane) -ceq (@($a.surface,$b.surface,$d.surface)-join ',')) 'Same-pane right split has wrong identities or structure'
    Request @('focus-tab',$b.surface)|Out-Null;$tree=Tree;$tree=Split-Drag $tree $b.surface $a.pane 'down';$down=Location $tree $b.surface;$ws=@($tree.workspaces|Where-Object {$_.id -ceq $a.workspace})[0]
    Require (@($tree.layout.panes).Count -eq 3 -and $ws.root.direction -ceq 'vertical' -and $ws.root.first.direction -ceq 'horizontal' -and $ws.root.first.first.id -ceq $a.pane -and $ws.root.first.second.id -ceq $down -and $ws.root.second.id -ceq $right -and (Order $tree $a.pane) -ceq (@($a.surface,$d.surface)-join ',')) 'Nested down split has wrong ordering or nesting';Stable $tree;Passed 'same-pane-right-and-nested-down-splits-with-owned-half-body-preview'

    $tree=Split-Drag $tree $c.surface $a.pane 'right';$relocated=Location $tree $c.surface;$ws=@($tree.workspaces|Where-Object {$_.id -ceq $a.workspace})[0]
    Require (@($tree.layout.panes).Count -eq 3 -and @($tree.workspaces|ForEach-Object {Leaves $_.root}|Where-Object {$_.id -ceq $right}).Count -eq 0 -and $relocated -cne $right -and $ws.root.direction -ceq 'horizontal' -and $ws.root.first.direction -ceq 'vertical' -and $ws.root.first.first.id -ceq $a.pane -and $ws.root.first.second.id -ceq $relocated -and $ws.root.second.id -ceq $down) 'Sole source tab split did not collapse its original pane'
    Request @('focus-tab',$b.surface)|Out-Null;$tree=Tree;$before=Topology $tree
    foreach($zone in @('right','down')){$point=Body $tree $down $zone;$tree=Begin $tree $b.surface;Motion $tree $point;$tree=Tree;No-Preview $tree;$tree=Release $tree $point;Require ((Topology $tree) -ceq $before) 'Single-tab self split mutated the model'}
    Stable $tree;Passed 'other-pane-sole-source-collapse-and-single-tab-self-split-noop'

    Request @('focus-tab',$d.surface)|Out-Null;$tree=Tree
    foreach($cancel in @('escape','cancelmode','outside')){
        $before=Topology $tree;$body=Body-Rect $tree $a.pane;$point=Body $tree $a.pane 'right';$tree=Begin $tree $d.surface;Motion $tree $point;$tree=Await {param($t) $t.chrome.tab_drop_preview -and $t.chrome.tab_drop_preview.active};Preview $tree $a.pane 'right' $body
        if($cancel -ceq 'escape'){[OptionsFixture]::PostEscape([long]$tree.window_handle,$owned.Id)}elseif($cancel -ceq 'cancelmode'){[OptionsFixture]::HostPointer([long]$tree.window_handle,$owned.Id,0x1f,0,0)}else{$point=@{x=-20;y=-20};Motion $tree $point}
        $tree=Await {param($t) -not $t.chrome.tab_drop_preview -or -not $t.chrome.tab_drop_preview.active};$tree=Release $tree $point;No-Preview $tree;Require ((Topology $tree) -ceq $before) ('Split preview cancellation changed model: '+$cancel)
    };Stable $tree;Screen-Contains $a.surface $marker;Screen-Contains $c.surface $marker;Passed 'split-preview-Escape-cancel-outside-cleanup-and-Unicode-output-preservation'

    # Reuse both existing workspaces and all six PTYs for sidebar ordering.
    Request @('workspace','rename',$a.workspace,'원본 한 é & 작업공간')|Out-Null;Request @('workspace','color',$a.workspace,'#12abef')|Out-Null;Request @('workspace','color',$e.workspace,'#e67129')|Out-Null
    $tree=Tree;$workspaceMetadata=Workspace-Metadata $tree;$workspaceOrder=Workspace-Order $tree;$workspaceActive=$tree.active_workspace;Require (@($tree.workspaces).Count -eq 2 -and $workspaceActive -ceq $a.workspace) 'Workspace drag fixture changed'
    $point=Workspace-Point $tree $a.workspace $false;$tree=Drag-Workspace $tree $e.workspace $point
    Require ((Workspace-Order $tree) -ceq (@($e.workspace,$a.workspace)-join ',') -and $tree.active_workspace -ceq $workspaceActive) 'Moving workspace upward changed active workspace or insertion index'
    $point=Workspace-Point $tree $a.workspace $true;$tree=Drag-Workspace $tree $e.workspace $point
    Require ((Workspace-Order $tree) -ceq $workspaceOrder -and $tree.active_workspace -ceq $workspaceActive) 'Moving workspace downward did not correct removal index'
    foreach($after in @($false,$true)){$point=Workspace-Point $tree $e.workspace $after;$tree=Drag-Workspace $tree $e.workspace $point;Require ((Workspace-Order $tree) -ceq $workspaceOrder -and $tree.active_workspace -ceq $workspaceActive) 'Workspace self drop reordered or activated its row'}
    Stable-Workspaces $tree;Passed 'workspace-up-down-index-adjustment-self-drop-and-active-preservation'

    $tree=Begin-Workspace $tree $e.workspace;$tree=Release $tree $press;Require ($tree.active_workspace -ceq $e.workspace -and (Workspace-Order $tree) -ceq $workspaceOrder) 'Normal workspace row click did not activate without reordering'
    $tree=Begin-Workspace $tree $a.workspace;$near=@{x=$press.x+1;y=$press.y+1};Motion $tree $near;$tree=Release $tree $near
    Require ($tree.active_workspace -ceq $a.workspace -and (Workspace-Order $tree) -ceq $workspaceOrder) 'Below-threshold workspace movement reordered or failed activation';Stable-Workspaces $tree;Passed 'workspace-click-and-below-threshold-activation'

    foreach($cancel in @('escape','cancelmode','capturechanged','outside')){
        $point=Workspace-Point $tree $a.workspace $false;$tree=Begin-Workspace $tree $e.workspace;Motion $tree $point
        if($cancel -ceq 'outside'){$point=@{x=-20;y=-20};Motion $tree $point}else{if($cancel -ceq 'escape'){[OptionsFixture]::PostEscape([long]$tree.window_handle,$owned.Id)}else{$message=if($cancel -ceq 'cancelmode'){0x1f}else{0x215};[OptionsFixture]::HostPointer([long]$tree.window_handle,$owned.Id,$message,0,0)};$tree=Await {param($t) -not $t.chrome.workspace_dragging}}
        $tree=Release $tree $point;Require ((Workspace-Order $tree) -ceq $workspaceOrder -and $tree.active_workspace -ceq $workspaceActive) ('Cancelled workspace drag changed order or active workspace: '+$cancel)
    };Stable-Workspaces $tree;Passed 'workspace-Escape-cancelmode-capturechanged-outside-noop'

    $point=Workspace-Point $tree $a.workspace $false;$tree=Begin-Workspace $tree $e.workspace;Motion $tree $point
    Request @('workspace','reorder',$a.workspace,'1')|Out-Null;$tree=Await {param($t) -not $t.chrome.workspace_dragging};$winner=Workspace-Order $tree;$tree=Release $tree $point
    Require ($winner -ceq (@($e.workspace,$a.workspace)-join ',') -and (Workspace-Order $tree) -ceq $winner -and $tree.active_workspace -ceq $workspaceActive) 'Stale workspace release overwrote the CLI reorder winner'
    Stable-Workspaces $tree;Screen-Contains $a.surface $marker;Screen-Contains $c.surface $marker;Passed 'workspace-stale-rebuild-cancel-and-name-color-PTY-output-preservation'
}catch{$failure=$_.Exception.Message}
finally{
    $cleaning=$true
    if($owned){
        try{if(-not $owned.HasExited){$stop=[Diagnostics.Stopwatch]::StartNew();if($pipeName){Request @('quit','--discard-state') 2500|Out-Null};Require ($owned.WaitForExit([int][Math]::Max(1,5000-$stop.ElapsedMilliseconds))) 'Owned host cleanup deadline exceeded'}}catch{$cleanupErrors+=$_.Exception.Message}
        if(-not $owned.HasExited){try{$owned.Kill();[CliProbe]::WaitAfterKill($owned)}catch{$cleanupErrors+=$_.Exception.Message}}
        if(-not $owned.HasExited -or $owned.ExitCode -ne 0){$cleanupErrors+='Owned host did not exit with code zero'}
        try{if(-not $hostOut.Wait(500) -or -not $hostErr.Wait(500)){$cleanupErrors+='Host output did not complete'}}catch{$cleanupErrors+=$_.Exception.Message}
        $hostLog=@{pid=$owned.Id;stdout=[CliProbe]::Output($hostOut);stderr=[CliProbe]::Output($hostErr)};$owned.Dispose()
    }
    if($failure -or $cleanupErrors.Count){$path=Join-Path $directory 'failure.json';@{error=$failure;cleanupErrors=$cleanupErrors;passed=$checks;lastTree=$last;command=$commandFailure;host=$hostLog;scope='Owned hidden tab/host messages only; physical pointer capture and Korean IME not exercised'}|ConvertTo-Json -Depth 30|Set-Content -Encoding UTF8 $path;Write-Output ('Failure diagnostics: '+$path)}
    else{Remove-Item -LiteralPath $directory -Recurse -Force}
}
if($failure){throw $failure};if($cleanupErrors.Count){throw ($cleanupErrors -join '; ')}
Write-Output ('Tab drag passed: '+$checks.Count+' groups in '+[Math]::Round($clock.Elapsed.TotalSeconds,3)+'s; hidden guards only, no physical IME/focus acceptance')
