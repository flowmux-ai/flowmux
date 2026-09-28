# SPDX-License-Identifier: GPL-3.0-or-later
# Own hidden host only. Run under run-check.ps1 -TimeoutSeconds 60.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",[ValidateSet('pane-tools','terminal-menu')][string]$Case='pane-tools')
if (-not $env:FLOWMUX_TEST_ARTIFACT_ROOT) { throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.' }
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path;$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
Add-Type -ReferencedAssemblies System.Drawing -Path (Join-Path $PSScriptRoot 'ChromeFixture.cs')
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'EditorFixture.cs'),(Join-Path $PSScriptRoot 'PaneToolsFixture.cs'),(Join-Path $PSScriptRoot 'OptionsFixture.cs')
$directory=Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT ('pane-tools-한글-한-'+[guid]::NewGuid());[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$fixture=New-Object EditorFixture($directory);$path=$fixture.Write('pane close 한글.txt',[EditorFixture]::Original,$false,$false)
$clock=[Diagnostics.Stopwatch]::StartNew();$hostProcess=$null;$pipeName=$null;$clients=@();$shells=@();$cleanup=$false;$out=$null;$err=$null;$editor=$null
$evidence=[ordered]@{started=[DateTime]::UtcNow.ToString('o');mode='hidden-native-pane-tools';case=$Case;checks=@();observations=@();desktopInput=$false;physicalIme=$false;deferred=@('Owned WM_COMMAND validates direct production buttons. Desktop pointer/menu navigation and composed GPU pixels are not covered.','Tab menus use owned hidden HWND messages; clipboard and folder launch are never invoked. Pane/workspace menus use the same modeless native controller.','The dirty document is acknowledged before close; this does not force an unsynchronized edit debounce race.')}
function Require([bool]$Condition,[string]$Message){if(-not $Condition){throw $Message}}
function Budget([int]$Max=5000){if($cleanup){return $Max};$left=55000-$clock.ElapsedMilliseconds;Require ($left -gt 0) 'Pane tools exceeded55s inner budget';return [int][Math]::Min($Max,$left)}
function Probe([string[]]$Arguments,[int]$Exit=0,[int]$Max=5000){
 $p=[CliProbe]::Start($cli,$Arguments,$directory,$directory);$script:clients+=,$p.Id;$o=$p.StandardOutput.ReadToEndAsync();$e=$p.StandardError.ReadToEndAsync()
 try {Require ($p.WaitForExit((Budget $Max))) ('Owned CLI exceeded bounded deadline; no retry: '+($Arguments -join ' '));Require ($o.Wait(500)-and $e.Wait(500)) 'CLI pipes did not close';$text=[CliProbe]::Output($o);Require ($p.ExitCode -eq $Exit) ('CLI exit differs: '+[CliProbe]::Output($e)+' '+$text);if($Exit -ne 0){return ([CliProbe]::Output($e)|ConvertFrom-Json)};return ($text|ConvertFrom-Json)}
 finally {if(-not $p.HasExited){$p.Kill();[CliProbe]::WaitAfterKill($p)};$p.Dispose()}
}
function Request([string[]]$Arguments,[int]$Exit=0,[int]$Max=5000){Require ([bool]$pipeName) 'Explicit owned pipe required';return Probe (@('--pipe',$pipeName,'--json')+$Arguments) $Exit $Max}
function Tree([int]$Max=5000){$t=Request @('tree') 0 $Max;Require ($t.background_testing) 'Hidden debug host required';[ChromeFixture]::Size([long]$t.window_handle,$hostProcess.Id)|Out-Null;$evidence.lastTree=$t;return $t}
function Ready([int]$Count,[int]$Max=5000){$watch=[Diagnostics.Stopwatch]::StartNew();do{$left=$Max-$watch.ElapsedMilliseconds;Require ($left -gt 0) 'Terminal readiness exceeded condition budget';$t=Tree ([int]$left);if(@($t.surfaces).Count -eq $Count -and @($t.surfaces|Where-Object {-not $_.ready -or -not $_.pid -or -not $_.running}).Count -eq 0){$script:shells+=@($t.surfaces.pid);return $t};Start-Sleep -Milliseconds 20}while($true)}
function Stable($Tree,$Before){foreach($s in $Before){$now=@($Tree.surfaces|Where-Object {$_.id -eq $s.id});Require ($now.Count -eq 1 -and $now[0].pid -eq $s.pid -and $now[0].running) 'Pane UI replaced an existing terminal process'}}
function Tool($Tree,[string]$Pane,[string]$Kind){$items=@($Tree.chrome.controls|Where-Object {$_.pane -eq $Pane -and $_.kind -eq $Kind});Require ($items.Count -eq 1 -and $items[0].layout_visible) ('Missing visible tool '+$Kind);return $items[0]}
function Click([string]$Pane,[string]$Kind){$t=Tree;$button=Tool $t $Pane $Kind;[PaneToolsFixture]::Command($hostProcess,[long]$t.window_handle,[long]$button.handle);$evidence.observations+=@{name='native-command';pane=$Pane;kind=$Kind;button=$button}}
function Check-Geometry($Tree){
 $native=@([ChromeFixture]::Read([long]$Tree.window_handle,$hostProcess.Id))
 foreach($entry in @($Tree.layout.panes)){$pane=$entry[0];$r=$entry[1];$controls=@($Tree.chrome.controls|Where-Object {$_.pane -eq $pane -and $_.layout_visible});foreach($c in $controls){$n=@($native|Where-Object {$_.Handle -eq $c.handle});Require ($n.Count -eq 1) 'Reported control missing from native HWND tree';$v=$n[0];Require ($v.X -ge $r.x -and $v.Y -ge $r.y -and $v.X+$v.Width -le $r.x+$r.width+1 -and $v.Y+$v.Height -le $r.y+$r.height+1) 'Pane header spills outside owning pane'};for($i=0;$i -lt $controls.Count;$i++){for($j=$i+1;$j -lt $controls.Count;$j++){$a=$controls[$i].rect;$b=$controls[$j].rect;Require (-not ($a.x -lt $b.x+$b.width -and $b.x -lt $a.x+$a.width -and $a.y -lt $b.y+$b.height -and $b.y -lt $a.y+$a.height)) 'Pane header controls overlap'}}}
 $evidence.observations+=@{name='native-header-geometry';tree=$Tree;native=$native}
}
function Await([scriptblock]$Condition){$watch=[Diagnostics.Stopwatch]::StartNew();do{$left=5000-$watch.ElapsedMilliseconds;Require ($left -ge 100) 'Native menu condition deadline reached (five-second limit; fewer than 100ms remain)';$t=Tree ([int]$left);if(& $Condition $t){return $t};Start-Sleep -Milliseconds 20}while($true)}
function Menu-Panel($Panel,[long]$Owner){
 Require ($Panel -and $Panel.id -and $Panel.native_visible -eq $false) 'Missing hidden tab menu generation'
 $native=[OptionsFixture]::Describe([long]$Panel.window,$hostProcess.Id);Require ($Panel.owner -eq $Owner -and $native.Owner -eq $Owner -and $native.Enabled -and $native.OwnerEnabled) 'Tab menu has wrong owner or disabled its modeless owner'
 $size=[ChromeFixture]::Size([long]$Panel.window,$hostProcess.Id);$bottom=0
 foreach($row in @($Panel.rows)){$bounds=[OptionsFixture]::RelativeBounds([long]$Panel.window,[long]$row.window,$hostProcess.Id);$control=[OptionsFixture]::Describe([long]$row.window,$hostProcess.Id)
  if($row.separator){Require (-not $row.enabled -and $row.label -ceq '' -and -not $control.Enabled -and ($control.Style -band 0x1f) -eq 0xd -and [Math]::Abs($bounds.Height-9*$native.Dpi/96.0) -le 1) 'Menu separator is not a disabled nine-DIP ownerdraw STATIC'}else{Require ($control.Enabled -eq [bool]$row.enabled -and ($control.Style -band 0xf) -eq 0xb) 'Menu row native state/style differs from diagnostics'}
  Require ($bounds.X -eq $row.bounds.x -and $bounds.Y -eq $row.bounds.y -and $bounds.Width -eq $row.bounds.width -and $bounds.Height -eq $row.bounds.height) 'Menu row diagnostics differ from native client bounds'
  if($row.layout_visible){Require ($bounds.Width -gt 0 -and $bounds.Height -gt 0 -and $bounds.X -ge 0 -and $bounds.Y -ge $bottom -and $bounds.X+$bounds.Width -le $size[0] -and $bounds.Y+$bounds.Height -le $size[1]) 'Visible menu rows overlap or leave popup client bounds';$bottom=$bounds.Y+$bounds.Height}
 }
 return $Panel
}
function Menu-Open([string]$Surface){
 $t=Tree;$frames=@($t.detached_windows|Where-Object {$_.surface -ceq $Surface})
 if($frames.Count){Require ($frames.Count -eq 1) 'Context source has multiple detached owners';$owner=[long]$frames[0].window_handle;$tab=[long]$frames[0].tab}
 else{$tabs=@($t.chrome.controls|Where-Object {$_.kind -ceq 'tab' -and $_.surface -ceq $Surface -and $_.layout_visible});Require ($tabs.Count -eq 1) 'Context source has no unique visible tab header';$owner=[long]$t.window_handle;$tab=[long]$tabs[0].handle}
 [OptionsFixture]::ContextMenu($owner,$tab,$hostProcess.Id)
 $t=Await {param($value) $value.tab_menu -and $value.tab_menu.surface -ceq $Surface};Menu-Panel $t.tab_menu.menu $owner|Out-Null;return $t
}
function Menu-Click($Panel,[string]$Label){$rows=@($Panel.rows|Where-Object {$_.label -ceq $Label});Require ($rows.Count -eq 1 -and $rows[0].enabled) ('Menu action unavailable: '+$Label);[OptionsFixture]::ClickMenu([long]$Panel.window,[long]$rows[0].window,$hostProcess.Id)}
function Menu-Dismiss($Tree){[OptionsFixture]::PostEscape([long]$Tree.tab_menu.menu.window,$hostProcess.Id);$t=Await {param($value) -not $value.tab_menu};Require ([OptionsFixture]::Describe([long]$t.window_handle,$hostProcess.Id).Enabled) 'Context dismissal disabled the main owner';return $t}
function Menu-Move($Tree){Menu-Click $Tree.tab_menu.menu 'Move';$t=Await {param($value) $value.tab_menu.submenu};Menu-Panel $t.tab_menu.submenu ([long]$t.tab_menu.menu.window)|Out-Null;return $t}
function Workspace-Menu([string]$Id){
 $t=Tree;$rows=@($t.chrome.controls|Where-Object {$_.kind -ceq 'workspace' -and $_.workspace -ceq $Id -and $_.layout_visible});Require ($rows.Count -eq 1) 'Workspace context source has no unique visible HWND'
 [OptionsFixture]::ContextMenu([long]$t.window_handle,[long]$rows[0].handle,$hostProcess.Id)
 $t=Await {param($value) $value.tab_menu.kind -ceq 'workspace' -and $value.tab_menu.workspace -ceq $Id};Menu-Panel $t.tab_menu.menu ([long]$t.window_handle)|Out-Null;return $t
}
function Menu-Metadata($Tree,[string]$Label){
 Menu-Click $Tree.tab_menu.menu $Label;$t=Await {param($value) $value.metadata.open -and -not $value.tab_menu};$panel=$t.metadata;$native=[OptionsFixture]::Describe([long]$panel.window,$hostProcess.Id)
 Require ($panel.native_visible -eq $false -and $panel.owner -eq $t.window_handle -and $native.Owner -eq $t.window_handle -and $native.Enabled -and -not $native.OwnerEnabled) 'Workspace menu opened metadata with the wrong modal owner'
 foreach($key in @('input','apply','cancel')){Require ([OptionsFixture]::Parent([long]$panel.$key,$hostProcess.Id) -eq $panel.window) 'Workspace metadata control has the wrong parent'}
 return $panel
}
function Finish-Metadata($Panel,[string]$Action){[OptionsFixture]::Click([long]$Panel.window,[long]$Panel.$Action,$hostProcess.Id);$t=Await {param($value) -not $value.metadata.open};Require ([OptionsFixture]::Describe([long]$t.window_handle,$hostProcess.Id).Enabled) 'Metadata left its main owner disabled';return $t}
function Body-Menu([string]$Surface,$Event,[int]$Max=5000){
 $reply=Request @('test-terminal-menu',$Surface,($Event|ConvertTo-Json -Compress)) 0 $Max
 Require ($reply.surface -ceq $Surface -and $reply.request -and $reply.state) 'Terminal menu hook returned the wrong owned source or no state'
 $evidence.lastTerminalMenu=$reply;return $reply.state
}
function Body-Open([string]$Surface,[scriptblock]$Condition){
 $watch=[Diagnostics.Stopwatch]::StartNew()
 do{$left=5000-$watch.ElapsedMilliseconds;Require ($left -ge 100) 'Terminal menu configuration exceeded five seconds';$state=Body-Menu $Surface @{action='open';x=99999;y=99999} ([int]$left);if(& $Condition $state){return $state};Start-Sleep -Milliseconds 20}while($true)
}
function Pane-Area($Tree,[string]$Id){$bounds=$null;$count=0;foreach($entry in $Tree.layout.panes){if($entry[0] -ceq $Id){$bounds=$entry[1];$count++}};Require ($count -eq 1) 'Terminal menu lost its pane geometry';return $bounds}
function Editor-Command([string]$Action,[string[]]$Options=@()){$r=Request (@('editor','command',$editor.surface,$Action)+$Options);if($r.psobject.Properties.Name -contains 'result'){return $r.result};return $r}
try {
 $doctor=Probe @('doctor');Require ($doctor.background_testing -and $doctor.status -eq 'ok') 'Debug background build required'
 $utc=[DateTime]::UtcNow;$startup=[Diagnostics.Stopwatch]::StartNew();$hostProcess=[CliProbe]::Start($gui,@('--temporary','--shell=cmd','--cwd',$fixture.Root),$directory,$directory);$out=$hostProcess.StandardOutput.ReadToEndAsync();$err=$hostProcess.StandardError.ReadToEndAsync()
 $discovery=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($hostProcess.Id).json"
 do {Budget|Out-Null;Require (-not $hostProcess.HasExited -and $startup.ElapsedMilliseconds -lt 8000) 'Startup exceeded eight seconds';if((Test-Path $discovery)-and(Get-Item $discovery).LastWriteTimeUtc -ge $utc){$record=Get-Content -Raw $discovery|ConvertFrom-Json;Require ($record.pid -eq $hostProcess.Id) 'Wrong discovery PID';$pipeName=$record.pipe;break};Start-Sleep -Milliseconds 20}while($true)
 $left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup budget exhausted';$tree=Ready 1 ([int]$left);$a=Request @('identify');$original=@($tree.surfaces)
 if($Case -eq 'terminal-menu'){
  [ChromeFixture]::Resize([long]$tree.window_handle,$hostProcess.Id,1500,850)
  $tree=Await {param($t) @($t.surfaces|Where-Object {$_.id -ceq $a.surface -and $_.cwd_reported}).Count -eq 1}
  Require (@($tree.surfaces|Where-Object {$_.id -ceq $a.surface})[0].cwd -ceq $fixture.Root) 'Terminal body menu fixture lost its live Unicode/NFD CWD'
  $original=@($tree.surfaces)
  Request @('send-keys',$a.pane,'echo FLOWMUX_BODY_MENU_SURVIVOR')|Out-Null;Request @('send-key','Enter','--pane',$a.pane)|Out-Null
  $watch=[Diagnostics.Stopwatch]::StartNew()
  do{$left=5000-$watch.ElapsedMilliseconds;Require ($left -ge 100) 'Terminal marker exceeded five seconds';$screen=Request @('read-screen','--surface',$a.surface) 0 ([int]$left);if($screen.text.Contains('FLOWMUX_BODY_MENU_SURVIVOR')){break};Start-Sleep -Milliseconds 20}while($true)
  $expected='copy,paste,separator,split_right,split_down,separator,copy_path,separator,close_pane'
  $html=Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot '..\assets\index.html');$markup=[regex]::Match($html,'(?s)<div id="terminal-menu".*?(?=<form id="search")').Value
  $sourceRows=@([regex]::Matches($markup,'data-action="([^"]+)"|role="separator"')|ForEach-Object {if($_.Groups[1].Success){$_.Groups[1].Value}else{'separator'}})
  Require (($sourceRows -join ',') -ceq $expected) 'Production terminal HTML action/separator order differs from Linux'
  $menu=Body-Open $a.surface {param($s) -not $s.hidden -and @($s.rows|Where-Object {$_.action -ceq 'split_right' -and $_.enabled}).Count -eq 1}
  Require ((@($menu.rows.action)-join ',') -ceq $expected -and @($menu.rows|Where-Object {$_.action -ceq 'separator' -and $_.label -ceq '' -and -not $_.enabled}).Count -eq 3) 'Live terminal menu rows/separators differ from production HTML'
  Require ($menu.rows[0].label -match '^Copy(?:\s|$)' -and $menu.rows[1].label -match '^Paste(?:\s|$)' -and $menu.rows[3].label -ceq 'Split Right' -and $menu.rows[4].label -ceq 'Split Down' -and $menu.rows[6].label -ceq 'Copy path' -and $menu.rows[8].label -ceq 'Close Pane') 'Terminal menu labels differ from Linux'
  Require (-not $menu.rows[0].enabled -and $menu.rows[1].enabled -and $menu.rows[6].enabled -and -not $menu.rows[8].enabled) 'Initial selection, input, CWD or final-pane menu state differs'
  Require ($menu.rect.width -gt 0 -and $menu.rect.height -gt 0 -and $menu.rect.left -ge 0 -and $menu.rect.top -ge 0 -and $menu.rect.left+$menu.rect.width -le $menu.viewport.width+1 -and $menu.rect.top+$menu.rect.height -le $menu.viewport.height+1) 'Terminal menu escaped its actual renderer viewport'
  $menu=Body-Menu $a.surface @{action='key';key='Escape';isComposing=$true};Require (-not $menu.hidden) 'Synthetic composing Escape dismissed the terminal menu'
  $menu=Body-Menu $a.surface @{action='key';key='Escape';keyCode=229};Require (-not $menu.hidden) 'PROCESS/229 Escape dismissed the terminal menu'
  $menu=Body-Menu $a.surface @{action='key';key='ArrowDown'};Require (-not $menu.hidden) 'Arrow navigation dismissed terminal menu'
  $menu=Body-Menu $a.surface @{action='key';key='Escape'};Require ($menu.hidden) 'Escape did not dismiss the terminal menu';Stable (Tree) $original
  $evidence.checks+=@{name='terminal_body_Linux_HTML_live_rows_separators_viewport_clamp_and_synthetic_composition_Escape_guards';passed=$true}

  Body-Open $a.surface {param($s) -not $s.hidden -and $s.rows[3].enabled}|Out-Null
  Body-Menu $a.surface @{action='click';item='split_right'}|Out-Null;$tree=Ready 2;$b=Request @('identify');Require ($b.pane -cne $a.pane) 'Body Split Right did not create a different pane'
  $left=Pane-Area $tree $a.pane;$right=Pane-Area $tree $b.pane;Require ($right.x -ge $left.x+$left.width -and $right.y -eq $left.y -and $right.height -eq $left.height) 'Body Split Right used the wrong direction';Stable $tree $original
  Request @('focus-tab',$a.surface)|Out-Null
  Body-Open $b.surface {param($s) -not $s.hidden -and $s.rows[4].enabled}|Out-Null
  Require ((Request @('identify')).surface -ceq $a.surface) 'Source-routing case requires the visible B menu while A remains current'
  Body-Menu $b.surface @{action='click';item='split_down'}|Out-Null;$tree=Ready 3;$c=Request @('identify')
  $top=Pane-Area $tree $b.pane;$bottom=Pane-Area $tree $c.pane;Require ($c.pane -cne $b.pane -and $bottom.y -ge $top.y+$top.height -and $bottom.x -eq $top.x -and $bottom.width -eq $top.width) 'Body Split Down used the wrong target or direction';Stable $tree $original
  $unchanged=Pane-Area $tree $a.pane;foreach($key in @('x','y','width','height')){Require ($unchanged.$key -eq $left.$key) 'Non-current B menu split changed A pane geometry'}
  $retained=@($tree.surfaces|Where-Object {$_.id -in @($a.surface,$c.surface)})
  $evidence.checks+=@{name='terminal_body_Split_Right_and_Down_create_actual_nested_panes_without_restarting_original_PTY';passed=$true}

  Request @('focus-tab',$b.surface)|Out-Null;Request @('new-tab','--cwd',$fixture.Root,'--shell=cmd')|Out-Null;$tree=Ready 4;$d=Request @('identify');Require ($d.pane -ceq $b.pane) 'Whole-pane close requires two tabs in its source pane'
  $closedPids=@($tree.surfaces|Where-Object {$_.id -in @($b.surface,$d.surface)}|ForEach-Object {$_.pid});Require ($closedPids.Count -eq 2) 'Whole-pane close target omitted a terminal process'
  Body-Open $d.surface {param($s) -not $s.hidden -and $s.rows[8].enabled}|Out-Null
  # Debug click ACK captures pre-click state; the actual production action follows.
  Body-Menu $d.surface @{action='click';item='close_pane'}|Out-Null
  $tree=Await {param($t) @($t.layout.panes).Count -eq 2 -and @($t.surfaces).Count -eq 2 -and @($t.surfaces|Where-Object {$_.id -in @($b.surface,$d.surface)}).Count -eq 0 -and @(Get-Process -Id $closedPids -ErrorAction SilentlyContinue).Count -eq 0};Stable $tree $retained
  foreach($prior in $retained){$now=@($tree.surfaces|Where-Object {$_.id -ceq $prior.id})[0];Require ($now.view_handle -eq $prior.view_handle -and $now.holder.window -eq $prior.holder.window) 'Body Close Pane recreated a surviving WebView or holder'}
  $screen=Request @('read-screen','--surface',$a.surface);Require ($screen.text.Contains('FLOWMUX_BODY_MENU_SURVIVOR')) 'Body Close Pane erased original terminal output'
  $evidence.checks+=@{name='terminal_body_Close_Pane_terminates_every_source_tab_preserves_surviving_PID_WebView_holder_and_output';passed=$true}

  Request @('focus-tab',$c.surface)|Out-Null;Body-Open $c.surface {param($s) -not $s.hidden -and $s.rows[8].enabled}|Out-Null;Body-Menu $c.surface @{action='click';item='close_pane'}|Out-Null
  $tree=Await {param($t) @($t.layout.panes).Count -eq 1 -and @($t.surfaces).Count -eq 1};Stable $tree $original
  Request @('focus-tab',$a.surface)|Out-Null;$menu=Body-Open $a.surface {param($s) -not $s.hidden -and -not $s.rows[8].enabled};Require ($menu.rows[3].enabled -and $menu.rows[4].enabled) 'Final pane lost its available split actions'
  $menu=Body-Menu $a.surface @{action='key';key='Escape'};Require ($menu.hidden) 'Final-pane menu did not dismiss';Stable (Tree) $original
  $evidence.checks+=@{name='terminal_body_menu_updates_final_pane_Close_disabled_and_keeps_splits_available';passed=$true}
 }else{
 [ChromeFixture]::Resize([long]$tree.window_handle,$hostProcess.Id,1500,850);$tree=Tree;Check-Geometry $tree
 $order=@($tree.chrome.controls|Where-Object {$_.pane -eq $a.pane -and $_.kind -like 'pane_*' -and $_.layout_visible}|Sort-Object {$_.rect.x}|ForEach-Object {$_.kind})
 Require (($order -join ',') -eq 'pane_zoom,pane_split_right,pane_split_down,pane_add,pane_browser,pane_menu') 'Direct tool order differs from Linux'
 Click $a.pane 'pane_split_right';$tree=Ready 2;$b=Request @('identify');Require ($b.pane -ne $a.pane) 'Split right did not create a pane';Stable $tree $original;Check-Geometry $tree
 Click $b.pane 'pane_split_down';$tree=Ready 3;$c=Request @('identify');Require ($c.pane -ne $b.pane) 'Split down did not create a pane';$beforeZoom=@($tree.surfaces);$layout=($tree.layout|ConvertTo-Json -Depth 15 -Compress)
 Click $c.pane 'pane_zoom';$tree=Tree;Require ($tree.zoomed_pane -eq $c.pane) 'Native maximize did not zoom target';Require ((Tool $tree $c.pane 'pane_zoom').label -eq 'Restore pane') 'Zoom button caption did not change';Stable $tree $beforeZoom
 Click $c.pane 'pane_zoom';$tree=Tree;Require (-not $tree.zoomed_pane -and ($tree.layout|ConvertTo-Json -Depth 15 -Compress) -ceq $layout) 'Restore changed split geometry';Stable $tree $beforeZoom
 Click $c.pane 'pane_add';$tree=Ready 4;$newTab=Request @('identify');Require ($newTab.pane -eq $c.pane) 'Add tab split or retargeted source';$stable=@($tree.surfaces)
 Click $c.pane 'pane_browser';$watch=[Diagnostics.Stopwatch]::StartNew();do{$tree=Tree;Require ($watch.ElapsedMilliseconds -lt 5000) 'Browser creation exceeded five seconds';if(@($tree.browsers).Count -eq 1){break};Start-Sleep -Milliseconds 20}while($true)
 $browser=Request @('identify');Require ($browser.pane -eq $c.pane) 'Browser tool opened in the wrong pane';Stable $tree $stable;Check-Geometry $tree
 Click $c.pane 'pane_menu';$tree=Await {param($t) $t.tab_menu.kind -ceq 'pane'};Menu-Panel $tree.tab_menu.menu ([long]$tree.window_handle)|Out-Null;Require ($tree.tab_menu.pane -ceq $c.pane -and (@($tree.tab_menu.menu.rows.label)-join '|') -ceq 'Close Pane' -and $tree.tab_menu.menu.rows[0].enabled) 'Pane tool did not expose its captured Close Pane action';$tree=Menu-Dismiss $tree;Stable $tree $stable;Require (@($tree.layout.panes).Count -eq 3) 'Pane menu Escape changed layout'
 [ChromeFixture]::Resize([long]$tree.window_handle,$hostProcess.Id,420,400);$tree=Tree;Check-Geometry $tree;Stable $tree $stable
 [ChromeFixture]::Resize([long]$tree.window_handle,$hostProcess.Id,1500,850);$tree=Tree;Check-Geometry $tree
 $evidence.checks+=@{name='native_direct_tools_preserve_existing_terminal_pids_zoom_layout_and_bounded_header_geometry';passed=$true}
 $editor=(Request @('editor','open',$path,'--pane',$c.pane,'--root',$fixture.Root)).editor_opened;Require ([bool]$editor.surface) 'Editor open omitted identity';$watch=[Diagnostics.Stopwatch]::StartNew();do{$status=Request @('editor','status',$editor.surface);Require ($watch.ElapsedMilliseconds -lt 5000) 'Editor readiness exceeded five seconds';if($status.ready){break};Start-Sleep -Milliseconds 20}while($true)
 Editor-Command 'replace-text' @('--text',[EditorFixture]::Edited)|Out-Null;Request @('focus-pane',$a.pane)|Out-Null;$before=Tree;$identity=Request @('identify')
 $rejected=Request @('close-pane',$c.pane) 1;$evidence.observations+=@{name='dirty-close-response';response=$rejected};Require ($rejected.error -like '*unsaved changes*') 'Dirty close did not reject through editor barrier';$after=Tree;$afterIdentity=Request @('identify');Stable $after $stable
 Require (($before.workspaces|ConvertTo-Json -Depth 30 -Compress) -ceq ($after.workspaces|ConvertTo-Json -Depth 30 -Compress) -and $identity.surface -eq $afterIdentity.surface) 'Rejected whole-pane close changed tabs or logical focus'
 $read=Editor-Command 'read';Require ($read.dirty -and $read.content -ceq [EditorFixture]::Edited -and -not $read.document_focused) 'Rejected close lost dirty text or focused hidden editor'
 $evidence.observations+=@{name='dirty-pane-rejected';response=$rejected;before=$before;after=$after;editorRead=$read}
 Click $c.pane 'pane_menu';$tree=Await {param($t) $t.tab_menu.kind -ceq 'pane'};Menu-Click $tree.tab_menu.menu 'Close Pane'
 $tree=Await {param($t) $t.editor_close_dialog -and -not $t.tab_menu};$dialog=$tree.editor_close_dialog;$native=[OptionsFixture]::Describe([long]$dialog.window,$hostProcess.Id)
 Require ($dialog.owner -eq $tree.window_handle -and $native.Owner -eq $tree.window_handle -and -not $native.OwnerEnabled -and $native.Enabled -and $dialog.native_visible -eq $false -and -not $dialog.busy) 'Pane Close did not use the owned dirty-editor decision barrier'
 Require ([OptionsFixture]::Parent([long]$dialog.cancel,$hostProcess.Id) -eq $dialog.window -and $dialog.body.Contains([IO.Path]::GetFileName($path))) 'Pane Close dialog lost its Unicode document or Cancel owner'
 [OptionsFixture]::Click([long]$dialog.window,[long]$dialog.cancel,$hostProcess.Id);$tree=Await {param($t) -not $t.editor_close_dialog -and -not $t.editor_synchronizing};Stable $tree $stable
 $read=Editor-Command 'read';Require ($read.dirty -and $read.content -ceq [EditorFixture]::Edited -and @($tree.layout.panes).Count -eq 3 -and [OptionsFixture]::Describe([long]$tree.window_handle,$hostProcess.Id).Enabled) 'Pane Close Cancel lost dirty content, removed a pane or kept its owner disabled'
 Editor-Command 'save'|Out-Null;Click $c.pane 'pane_menu';$tree=Await {param($t) $t.tab_menu.kind -ceq 'pane'};Menu-Click $tree.tab_menu.menu 'Close Pane';$after=Await {param($t) -not $t.tab_menu -and @($t.layout.panes).Count -eq 2 -and -not $t.editor_synchronizing};$editor=$null
 $retained=@($stable|Where-Object {$_.id -in @($a.surface,$b.surface)});Stable $after $retained
 Require (@($after.layout.panes).Count -eq 2 -and @($after.surfaces).Count -eq 2 -and @($after.browsers).Count -eq 0 -and @($after.editors).Count -eq 0) 'Successful close did not remove every tab in the target pane'
 Require ($fixture.BytesEqual($path,[EditorFixture]::Encode([EditorFixture]::Edited,$false,$false))) 'Saved editor bytes changed during whole-pane close'
 Request @('close-pane',$b.pane)|Out-Null;$tree=Tree;$final=Request @('close-pane',$a.pane) 1;Require ($final.error -like '*final pane*') 'Final pane close was not refused';Stable (Tree) $original
 $evidence.observations+=@{name='clean-whole-pane-close-and-final-refusal';tree=$tree;final=$final}
 $evidence.checks+=@{name='whole_pane_close_seals_all_editors_rejects_dirty_without_partial_removal_and_preserves_final_pane';passed=$true}
 Click $a.pane 'pane_menu';$tree=Await {param($t) $t.tab_menu.kind -ceq 'pane'};Require (@($tree.tab_menu.menu.rows).Count -eq 1 -and -not $tree.tab_menu.menu.rows[0].enabled) 'Final pane Close is not disabled';$tree=Menu-Dismiss $tree
 $tree=Workspace-Menu $a.workspace;$workspaceMenu=$tree.tab_menu.menu
 Require ((@($workspaceMenu.rows.label)-join '|') -ceq 'New workspace|New SSH Workspace|New window||Change tab name|Change color…||Close tab|Close all tabs||Show in folder|Copy path') 'Workspace menu differs from Linux action and separator order'
 foreach($label in @('New SSH Workspace')){$row=@($workspaceMenu.rows|Where-Object {$_.label -ceq $label});Require ($row.Count -eq 1 -and $row[0].enabled) ('SSH workspace action unavailable: '+$label)}
 foreach($label in @('Close tab','Close all tabs')){$row=@($workspaceMenu.rows|Where-Object {$_.label -ceq $label});Require ($row.Count -eq 1 -and $row[0].enabled) ('Workspace close action missing: '+$label)}
 Require ($tree.tab_menu.folder -ceq $fixture.Root -and $tree.tab_menu.copy_text -ceq $fixture.Root -and @($workspaceMenu.rows|Where-Object {$_.separator}).Count -eq 3) 'Workspace context lost its Unicode CWD or native separators'
 [OptionsFixture]::PostKey([long]$workspaceMenu.window,$hostProcess.Id,40,$false,$false);$tree=Await {param($t) $t.tab_menu.menu.selected -eq 1};Require ($tree.tab_menu.menu.id -ceq $workspaceMenu.id) 'Menu navigation replaced the workspace target'
 [OptionsFixture]::PostKey([long]$workspaceMenu.window,$hostProcess.Id,35,$false,$false);$tree=Await {param($t) $t.tab_menu.menu.selected -eq (@($workspaceMenu.rows).Count-1)}
 $bitmap=Join-Path $directory 'workspace-menu.bmp';$capture=Request @('chrome-capture',$bitmap);Require ($capture.root_handle -eq $workspaceMenu.window) 'Workspace bitmap captured another popup'
 $background=[ChromeFixture]::Pixel($bitmap,0,0)
 foreach($row in @($workspaceMenu.rows|Where-Object {$_.separator -and $_.layout_visible})){$r=$row.bounds;$plain=[ChromeFixture]::ColorCount($bitmap,$r.x,$r.y,$r.width,$r.height,$background);Require ($plain -lt $r.width*$r.height-5) 'Native separator painter omitted its divider'}
 $tree=Tree;Require ($tree.tab_menu.menu.id -ceq $workspaceMenu.id) 'Workspace bitmap changed menu generation';Remove-Item -LiteralPath $bitmap -Force;$tree=Menu-Dismiss $tree;Stable $tree $original
 $blankY=[int]($tree.chrome.sidebar_list_top+2*$tree.chrome.workspace_row_height_dip*$tree.chrome.dpi/96)
 [OptionsFixture]::ContextMenuAt([long]$tree.window_handle,$hostProcess.Id,12,$blankY);$tree=Await {param($t) $t.tab_menu.kind -ceq 'creation'}
 Require ((@($tree.tab_menu.menu.rows.label)-join '|') -ceq 'New workspace|New SSH Workspace|New window') 'Empty sidebar context did not use the creation menu';$tree=Menu-Dismiss $tree
 [OptionsFixture]::ContextMenuAt([long]$tree.window_handle,$hostProcess.Id,([int]$tree.chrome.sidebar_actual_width+20),$blankY);$tree=Tree;Require (-not $tree.tab_menu) 'Workspace creation menu escaped the sidebar bounds'
 $evidence.checks+=@{name='workspace_Linux_menu_order_Close_and_SSH_actions_native_separators_and_modeless_Escape';passed=$true}
 # Tab context menus remain modeless while the normal IPC loop answers Tree.
 Request @('workspace','rename',$a.workspace,'원본 한글 한')|Out-Null;Request @('focus-tab',$a.surface)|Out-Null
 $tree=Menu-Open $a.surface;$rootMenu=$tree.tab_menu
 Require ((@($rootMenu.menu.rows.label)-join '|') -ceq 'Show in folder|Copy path|Move' -and $rootMenu.pane -ceq $a.pane -and $rootMenu.workspace -ceq $a.workspace) 'Terminal tab menu entries or captured target differ'
 Require ($rootMenu.copy_text -ceq $fixture.Root -and $rootMenu.folder -ceq $fixture.Root) 'Terminal menu did not capture its actual Unicode CWD'
 $move=@($rootMenu.menu.rows|Where-Object {$_.label -ceq 'Move'});Require ($move.Count -eq 1 -and -not $move[0].enabled) 'Single-workspace Move is not disabled'
 $bitmap=Join-Path $directory 'tab-menu.bmp';$capture=Request @('chrome-capture',$bitmap)
 Require ($capture.root_handle -eq $rootMenu.menu.window) 'Native capture selected a different root than the open tab menu'
 $painted=@([ChromeFixture]::Read([long]$rootMenu.menu.window,$hostProcess.Id));$foreground=if($tree.chrome.theme -eq 'light'){'#28282b'}else{'#f2f3f5'}
 foreach($row in @($rootMenu.menu.rows|Where-Object {$_.layout_visible -and $_.enabled})){$actual=@($painted|Where-Object {$_.Handle -eq $row.window});Require ($actual.Count -eq 1 -and $actual[0].Font -ne 0 -and $actual[0].Text -ceq $row.label.Replace('&','&&')) 'Native captured menu label or chrome font differs';$r=$row.bounds;Require ([ChromeFixture]::ColorCount($bitmap,$r.x,$r.y,$r.width,$r.height,$foreground) -gt 5) 'Production menu painter omitted actual foreground text'}
 $tree=Tree;Require ($tree.tab_menu.menu.id -ceq $rootMenu.menu.id -and $tree.tab_menu.surface -ceq $a.surface) 'Native capture changed the menu generation or target';Remove-Item -LiteralPath $bitmap -Force
 $evidence.checks+=@{name='owned_menu_production_bitmap_has_actual_labels_fonts_and_foreground_text';passed=$true}
 $tree=Menu-Dismiss $tree;Stable $tree $original
 $evidence.checks+=@{name='terminal_context_message_owned_geometry_CWD_and_single_workspace_Move_disabled';passed=$true}

 $editor=(Request @('editor','open',$path,'--pane',$a.pane,'--root',$fixture.Root)).editor_opened
 $tree=Await {param($t) @($t.editors|Where-Object {$_.id -ceq $editor.surface -and $_.ready}).Count -eq 1};$editorBefore=@($tree.editors|Where-Object {$_.id -ceq $editor.surface})[0]
 $tree=Menu-Open $editor.surface;Require ((@($tree.tab_menu.menu.rows.label)-join '|') -ceq 'Copy URL|Move' -and $tree.tab_menu.copy_text -ceq $editorBefore.workspace_root) 'Editor menu differs from Linux Copy URL/root contract'
 $tree=Menu-Dismiss $tree;Require (@($tree.editors|Where-Object {$_.id -ceq $editor.surface -and $_.view_handle -eq $editorBefore.view_handle}).Count -eq 1) 'Editor context menu recreated its WebView'
 Request @('close-tab',$editor.surface)|Out-Null;$editor=$null
 $opened=(Request @('browser','open','about:blank','--pane',$a.pane)).browser_pane_opened
 $tree=Await {param($t) @($t.browsers|Where-Object {$_.id -ceq $opened.surface -and -not $_.loading}).Count -eq 1};$browserBefore=@($tree.browsers|Where-Object {$_.id -ceq $opened.surface})[0]
 $tree=Menu-Open $opened.surface;Require ((@($tree.tab_menu.menu.rows.label)-join '|') -ceq 'Copy URL|Move' -and $tree.tab_menu.copy_text -ceq $browserBefore.url) 'Browser menu URL or entries differ'
 $tree=Menu-Dismiss $tree;Stable $tree $original
 $evidence.checks+=@{name='editor_and_browser_context_URL_snapshots_without_clipboard_or_folder_launch';passed=$true}

 $firstName='첫째 목적 한 &';$thirdName='셋째 목적 😀'
 Request @('new-workspace','--cwd',$fixture.Root,'--shell=cmd')|Out-Null;$first=Request @('identify');Request @('workspace','rename',$first.workspace,$firstName)|Out-Null
 Request @('new-workspace','--cwd',$fixture.Root,'--shell=cmd')|Out-Null;$third=Request @('identify');Request @('workspace','rename',$third.workspace,$thirdName)|Out-Null
 $tree=Ready 3;$menuTerminals=@($tree.surfaces);Request @('workspace','reorder',$a.workspace,'1')|Out-Null;Request @('focus-tab',$opened.surface)|Out-Null
 $tree=Menu-Open $opened.surface;$tree=Menu-Move $tree;$rootId=$tree.tab_menu.menu.id
 Require ((@($tree.tab_menu.submenu.rows.label)-join '|') -ceq ('1. '+$firstName+'|3. '+$thirdName)) 'Move submenu renumbered destinations or included its source workspace'
 [OptionsFixture]::PostKey([long]$tree.tab_menu.submenu.window,$hostProcess.Id,37,$false,$false)
 $tree=Await {param($t) $t.tab_menu -and -not $t.tab_menu.submenu};Require ($tree.tab_menu.menu.id -ceq $rootId) 'Left dismissed or replaced the root context menu'
 $moveIndex=@($tree.tab_menu.menu.rows).Count-1;Require ($tree.tab_menu.menu.selected -eq $moveIndex -and $tree.tab_menu.menu.rows[$moveIndex].label -ceq 'Move') 'Left did not retain the selected Move row'
 [OptionsFixture]::PostKey([long]$tree.tab_menu.menu.window,$hostProcess.Id,36,$false,$false);$tree=Await {param($t) $null -ne $t.tab_menu -and $t.tab_menu.menu.selected -eq 0}
 [OptionsFixture]::PostKey([long]$tree.tab_menu.menu.window,$hostProcess.Id,35,$false,$false)
 $tree=Await {param($t) $t.tab_menu.menu.selected -eq $moveIndex};[OptionsFixture]::PostKey([long]$tree.tab_menu.menu.window,$hostProcess.Id,39,$false,$false)
 $tree=Await {param($t) $t.tab_menu.submenu};$menuWindow=[long]$tree.tab_menu.menu.window;$subWindow=[long]$tree.tab_menu.submenu.window;$subId=$tree.tab_menu.submenu.id;$lastDestination=@($tree.tab_menu.submenu.rows).Count-1
 [OptionsFixture]::PostKey($subWindow,$hostProcess.Id,35,$false,$false);$tree=Await {param($t) $t.tab_menu.submenu.selected -eq $lastDestination}
 [OptionsFixture]::PostKey($subWindow,$hostProcess.Id,36,$false,$false);$tree=Await {param($t) $null -ne $t.tab_menu.submenu -and $t.tab_menu.submenu.selected -eq 0}
 [OptionsFixture]::DeactivateMenu($menuWindow,$hostProcess.Id,$subWindow);$tree=Tree
 Require ($tree.tab_menu.menu.id -ceq $rootId -and $tree.tab_menu.submenu.id -ceq $subId) 'Root deactivation to its own submenu dismissed the menu chain'
 [OptionsFixture]::DeactivateMenu($subWindow,$hostProcess.Id,$menuWindow);$tree=Await {param($t) $t.tab_menu -and -not $t.tab_menu.submenu}
 Require ($tree.tab_menu.menu.id -ceq $rootId -and $tree.tab_menu.menu.selected -eq $moveIndex) 'Submenu deactivation to root lost the root menu or Move selection'
 [OptionsFixture]::PostKey($menuWindow,$hostProcess.Id,39,$false,$false);$tree=Await {param($t) $t.tab_menu.submenu}
 [OptionsFixture]::DeactivateMenu([long]$tree.tab_menu.submenu.window,$hostProcess.Id,0);$tree=Await {param($t) -not $t.tab_menu}
 Require ([OptionsFixture]::Describe([long]$tree.window_handle,$hostProcess.Id).Enabled) 'Outside deactivation left main owner disabled';Stable $tree $menuTerminals
 $evidence.checks+=@{name='owned_End_Right_Home_End_navigation_and_deactivation_messages_preserve_or_dismiss_menu_chain';passed=$true}
 $tree=Menu-Open $opened.surface
 $tree=Menu-Move $tree;$destination=$tree.tab_menu.submenu.rows[1];$submenu=$tree.tab_menu.submenu
 Request @('workspace','rename',$third.workspace,'이름변경 한 목적')|Out-Null;Request @('workspace','reorder',$third.workspace,'0')|Out-Null
 $tree=Tree;Require ($tree.tab_menu.submenu.id -ceq $submenu.id) 'Rename/reorder replaced the captured destination menu'
 [OptionsFixture]::ClickMenu([long]$submenu.window,[long]$destination.window,$hostProcess.Id)
 $tree=Await {param($t) -not $t.tab_menu -and @($t.browsers|Where-Object {$_.id -ceq $opened.surface}).Count -eq 1};$moved=Request @('identify');$browserAfter=@($tree.browsers|Where-Object {$_.id -ceq $opened.surface})[0]
 Require ($moved.surface -ceq $opened.surface -and $moved.workspace -ceq $third.workspace -and $moved.pane -ceq $third.pane -and $browserAfter.view_handle -eq $browserBefore.view_handle -and $browserAfter.url -ceq $browserBefore.url) 'Captured Move destination retargeted after reorder or replaced the browser WebView'
 Stable $tree $menuTerminals;Require ([OptionsFixture]::Describe([long]$tree.window_handle,$hostProcess.Id).Enabled) 'Move left main owner disabled'
 $evidence.checks+=@{name='Move_submenu_Left_sidebar_indices_and_renamed_reordered_UUID_target_preserve_browser_view';passed=$true}

 Request @('focus-tab',$a.surface)|Out-Null;$tree=Menu-Open $a.surface;$tree=Menu-Move $tree
 $destinations=@((Request @('workspace','list')).workspaces);$destinationWorkspace=@($destinations|Where-Object {$_.id -ceq $first.workspace})[0];$label=([int]$destinationWorkspace.index+1).ToString()+'. '+$destinationWorkspace.name
 Menu-Click $tree.tab_menu.submenu $label;$tree=Await {param($t) -not $t.tab_menu};$moved=Request @('identify')
 Require ($moved.surface -ceq $a.surface -and $moved.workspace -ceq $first.workspace -and $moved.pane -ceq $first.pane) 'Terminal Move targeted a different destination identity';Stable $tree $menuTerminals
 $tree=Menu-Open $a.surface;$tree=Menu-Move $tree;$tree=Menu-Dismiss $tree;Stable $tree $menuTerminals
 $evidence.checks+=@{name='terminal_Move_keeps_surface_and_PIDs_and_Escape_dismisses_context_chain';passed=$true}
 $terminalBefore=@($tree.surfaces|Where-Object {$_.id -ceq $a.surface})[0]
 Request @('detach-tab',$a.surface)|Out-Null;$tree=Await {param($t) @($t.detached_windows|Where-Object {$_.surface -ceq $a.surface}).Count -eq 1};$frame=@($tree.detached_windows|Where-Object {$_.surface -ceq $a.surface})[0]
 $tree=Menu-Open $a.surface;Require ($tree.tab_menu.menu.owner -eq $frame.window_handle -and $tree.tab_menu.workspace -ceq $frame.workspace) 'Detached tab context menu used the main frame or stale workspace';$tree=Menu-Move $tree
 $destinations=@((Request @('workspace','list')).workspaces);$destinationWorkspace=@($destinations|Where-Object {$_.id -ceq $first.workspace})[0];$label=([int]$destinationWorkspace.index+1).ToString()+'. '+$destinationWorkspace.name
 Menu-Click $tree.tab_menu.submenu $label;$tree=Await {param($t) -not $t.tab_menu -and @($t.detached_windows|Where-Object {$_.surface -ceq $a.surface}).Count -eq 0};$returned=Request @('identify');$terminalAfter=@($tree.surfaces|Where-Object {$_.id -ceq $a.surface})[0]
 Require ($returned.surface -ceq $a.surface -and $returned.workspace -ceq $first.workspace -and $returned.pane -ceq $first.pane -and $terminalAfter.view_handle -eq $terminalBefore.view_handle -and $terminalAfter.holder.window -eq $terminalBefore.holder.window -and $terminalAfter.holder.parent -eq $tree.window_handle -and $terminalAfter.holder.root -eq $tree.window_handle) 'Detached context Move did not return the same WebView/holder to the original main workspace'
 Require (@($tree.detached_windows|Where-Object {$_.window_handle -eq $frame.window_handle}).Count -eq 0 -and [OptionsFixture]::Parent([long]$terminalAfter.holder.window,$hostProcess.Id) -eq $tree.window_handle -and [OptionsFixture]::Describe([long]$tree.window_handle,$hostProcess.Id).Enabled) 'Detached context Move retained the old frame or wrong native owner';Stable $tree $menuTerminals
 $evidence.checks+=@{name='detached_tab_context_exact_owner_Move_reattaches_same_HWND_and_PID_and_removes_frame';passed=$true}
 # Header creation uses the shared creation menu; existing PTYs survive.
 $tree=Tree;$header=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'workspace_header' -and $_.layout_visible});Require ($header.Count -eq 1) 'Workspace header entry missing'
 [OptionsFixture]::Click([long]$tree.window_handle,[long]$header[0].handle,$hostProcess.Id);$tree=Await {param($t) $t.tab_menu.kind -ceq 'creation'};Menu-Panel $tree.tab_menu.menu ([long]$tree.window_handle)|Out-Null
 Require ((@($tree.tab_menu.menu.rows.label)-join '|') -ceq 'New workspace|New SSH Workspace|New window' -and $tree.tab_menu.menu.rows[0].enabled -and $tree.tab_menu.menu.rows[1].enabled) 'Creation menu order or SSH availability differs'
 Menu-Click $tree.tab_menu.menu 'New workspace';$tree=Ready 4;$created=Request @('identify');Require ($created.workspace -notin @($first.workspace,$third.workspace) -and -not $tree.tab_menu) 'Native creation reused an existing workspace';Stable $tree $menuTerminals
 # Pin the new automatic name before comparing unrelated workspace metadata.
 Request @('workspace','rename',$created.workspace,'새 생성 고정 한 😀')|Out-Null;$tree=Tree
 $allTerminals=@($tree.surfaces);$oldNames=@{};$oldLocks=@{};foreach($workspace in $tree.workspaces){Require ($workspace.name_locked) 'Workspace comparison requires explicit locked names';$oldNames[$workspace.id]=$workspace.name;$oldLocks[$workspace.id]=$workspace.name_locked}
 $tree=Workspace-Menu $first.workspace;$captured=$tree.tab_menu.menu.id;Require ((Request @('identify')).workspace -ceq $created.workspace) 'Right-click selected the inactive workspace'
 Request @('workspace','reorder',$first.workspace,'0')|Out-Null;Request @('focus-tab',$third.surface)|Out-Null;$tree=Tree;Require ($tree.tab_menu.menu.id -ceq $captured -and $tree.tab_menu.workspace -ceq $first.workspace) 'Reorder/focus replaced or retargeted the workspace context'
 $panel=Menu-Metadata $tree 'Change tab name';Require ([OptionsFixture]::Text([long]$panel.input,$hostProcess.Id) -ceq $oldNames[$first.workspace]) 'Rename opened for the active workspace instead of its captured UUID'
 $newName='이름 메뉴 한 😀 &';[OptionsFixture]::SetTextAndNotify([long]$panel.window,[long]$panel.input,$hostProcess.Id,('  '+$newName+'  '));$tree=Finish-Metadata $panel 'apply'
 $changed=@($tree.workspaces|Where-Object {$_.id -ceq $first.workspace})[0];Require ($changed.name -ceq $newName -and $changed.name_locked) 'Workspace menu rename did not trim and lock its Unicode title'
 Require (@($tree.workspaces).Count -eq $oldNames.Count) 'Captured rename changed the workspace identity set';foreach($workspace in $tree.workspaces|Where-Object {$_.id -cne $first.workspace}){Require ($oldNames.ContainsKey($workspace.id) -and $workspace.name -ceq $oldNames[$workspace.id] -and $workspace.name_locked -eq $oldLocks[$workspace.id]) 'Captured rename modified another workspace name or lock'}
 $row=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'workspace' -and $_.workspace -ceq $first.workspace})[0];Require ([OptionsFixture]::Text([long]$row.handle,$hostProcess.Id).StartsWith($newName.Replace('&','&&')+"`n")) 'Live workspace row did not receive the Unicode name'
 $tree=Workspace-Menu $first.workspace;$panel=Menu-Metadata $tree 'Change tab name';[OptionsFixture]::SetTextAndNotify([long]$panel.window,[long]$panel.input,$hostProcess.Id,'취소 한');$tree=Finish-Metadata $panel 'cancel';Require (@($tree.workspaces|Where-Object {$_.id -ceq $first.workspace})[0].name -ceq $newName) 'Rename Cancel changed the captured workspace'
 $tree=Workspace-Menu $first.workspace;$panel=Menu-Metadata $tree 'Change color…';[OptionsFixture]::SetTextAndNotify([long]$panel.window,[long]$panel.input,$hostProcess.Id,'#2684c7');$tree=Finish-Metadata $panel 'apply'
 Require (@($tree.workspaces|Where-Object {$_.id -ceq $first.workspace})[0].color -ceq '#2684c7') 'Workspace color Apply changed the wrong target'
 $tree=Workspace-Menu $first.workspace;$panel=Menu-Metadata $tree 'Change color…';[OptionsFixture]::SetTextAndNotify([long]$panel.window,[long]$panel.input,$hostProcess.Id,'#a15c33');$tree=Finish-Metadata $panel 'cancel'
 Require (@($tree.workspaces|Where-Object {$_.id -ceq $first.workspace})[0].color -ceq '#2684c7') 'Workspace color Cancel overwrote the saved color';Stable $tree $allTerminals
 $evidence.checks+=@{name='header_creation_preserves_PIDs_and_inactive_workspace_context_captures_UUID_across_reorder_focus_Unicode_rename_color_and_Cancel';passed=$true}
 }
 Request @('quit','--discard-state')|Out-Null;Require ($hostProcess.WaitForExit((Budget 5000))) 'Owned host quit timed out';Require ($hostProcess.ExitCode -eq 0) 'Owned host exit failed';$evidence.status='passed_background_pane_tools_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
 $cleanup=$true
 if($hostProcess){try{if(-not $hostProcess.HasExited -and $pipeName){if($editor){Editor-Command 'discard-document'|Out-Null};Request @('quit','--discard-state')|Out-Null};if(-not $hostProcess.WaitForExit(5000)){$hostProcess.Kill();[CliProbe]::WaitAfterKill($hostProcess);$evidence.status='failed';$evidence.cleanupError='Owned host required forced cleanup'}}catch{$evidence.status='failed';$evidence.cleanupError=$_.Exception.Message;if(-not $hostProcess.HasExited){$hostProcess.Kill();[CliProbe]::WaitAfterKill($hostProcess)}}
  $outDone=$out.Wait(500);$errDone=$err.Wait(500);$evidence.hosts=@($hostProcess.Id);$evidence.observations+=@{kind='host-exit';exitCode=$hostProcess.ExitCode;stdoutComplete=$outDone;stderrComplete=$errDone;stdout=[CliProbe]::Output($out);stderr=[CliProbe]::Output($err)};$hostProcess.Dispose()}
 $fixture.Dispose();$evidence.shells=@($shells|Select-Object -Unique);$evidence.clientPids=$clients;$evidence.elapsedMs=$clock.ElapsedMilliseconds;$evidence.finished=[DateTime]::UtcNow.ToString('o');if($evidence.status -eq 'failed'){$evidence|ConvertTo-Json -Depth 35|Set-Content -Encoding UTF8 (Join-Path $directory 'native-pane-tools-background.json')};if($evidence.status -eq 'failed'){Write-Output ('Evidence: '+$directory)}
}
if($evidence.status -ne 'passed_background_pane_tools_subset'){throw 'Pane tools verification failed'}
[ordered]@{status=$evidence.status;checks=$evidence.checks.Count;elapsedMs=$evidence.elapsedMs}|ConvertTo-Json -Compress
