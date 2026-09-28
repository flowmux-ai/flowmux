# SPDX-License-Identifier: GPL-3.0-or-later
# Own hidden host only. Run under run-check.ps1 -TimeoutSeconds 60.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path;$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
Add-Type -ReferencedAssemblies System.Drawing -Path (Join-Path $PSScriptRoot 'ChromeFixture.cs')
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'EditorFixture.cs'),(Join-Path $PSScriptRoot 'PaneToolsFixture.cs'),(Join-Path $PSScriptRoot 'OptionsFixture.cs')
$directory=Join-Path $(if ($env:FLOWMUX_TEST_ARTIFACT_ROOT) { $env:FLOWMUX_TEST_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\dist\evidence' }) ('pane-tools-한글-한-'+[guid]::NewGuid());[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$fixture=New-Object EditorFixture($directory);$path=$fixture.Write('pane close 한글.txt',[EditorFixture]::Original,$false,$false)
$clock=[Diagnostics.Stopwatch]::StartNew();$hostProcess=$null;$pipeName=$null;$clients=@();$shells=@();$cleanup=$false;$out=$null;$err=$null;$editor=$null
$evidence=[ordered]@{started=[DateTime]::UtcNow.ToString('o');mode='hidden-native-pane-tools';checks=@();observations=@();desktopInput=$false;physicalIme=$false;deferred=@('Owned WM_COMMAND validates direct production buttons. Desktop pointer/menu navigation and composed GPU pixels are not covered.','Tab menus use owned hidden HWND messages; clipboard and folder launch are never invoked. Legacy pane popup selection remains outside this suite.','The dirty document is acknowledged before close; this does not force an unsynchronized edit debounce race.')}
function Require([bool]$Condition,[string]$Message){if(-not $Condition){throw $Message}}
function Budget([int]$Max=5000){if($cleanup){return $Max};$left=55000-$clock.ElapsedMilliseconds;Require ($left -gt 0) 'Pane tools exceeded55s inner budget';return [int][Math]::Min($Max,$left)}
function Probe([string[]]$Arguments,[int]$Exit=0,[int]$Max=5000){
 $p=[CliProbe]::Start($cli,$Arguments,$directory,$directory);$script:clients+=,$p.Id;$o=$p.StandardOutput.ReadToEndAsync();$e=$p.StandardError.ReadToEndAsync()
 try {Require ($p.WaitForExit((Budget $Max))) 'Owned CLI exceeded bounded deadline; no retry';Require ($o.Wait(500)-and $e.Wait(500)) 'CLI pipes did not close';$text=[CliProbe]::Output($o);Require ($p.ExitCode -eq $Exit) ('CLI exit differs: '+[CliProbe]::Output($e)+' '+$text);if($Exit -ne 0){return ([CliProbe]::Output($e)|ConvertFrom-Json)};return ($text|ConvertFrom-Json)}
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
function Await([scriptblock]$Condition){$watch=[Diagnostics.Stopwatch]::StartNew();do{$left=5000-$watch.ElapsedMilliseconds;Require ($left -gt 0) 'Tab menu condition exceeded five seconds';$t=Tree ([int]$left);if(& $Condition $t){return $t};Start-Sleep -Milliseconds 20}while($true)}
function Menu-Panel($Panel,[long]$Owner){
 Require ($Panel -and $Panel.id -and $Panel.native_visible -eq $false) 'Missing hidden tab menu generation'
 $native=[OptionsFixture]::Describe([long]$Panel.window,$hostProcess.Id);Require ($Panel.owner -eq $Owner -and $native.Owner -eq $Owner -and $native.Enabled -and $native.OwnerEnabled) 'Tab menu has wrong owner or disabled its modeless owner'
 $size=[ChromeFixture]::Size([long]$Panel.window,$hostProcess.Id);$bottom=0
 foreach($row in @($Panel.rows)){$bounds=[OptionsFixture]::RelativeBounds([long]$Panel.window,[long]$row.window,$hostProcess.Id);$control=[OptionsFixture]::Describe([long]$row.window,$hostProcess.Id)
  Require ($control.Enabled -eq [bool]$row.enabled -and ($control.Style -band 0xf) -eq 0xb) 'Menu row native state/style differs from diagnostics'
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
function Editor-Command([string]$Action,[string[]]$Options=@()){$r=Request (@('editor','command',$editor.surface,$Action)+$Options);if($r.psobject.Properties.Name -contains 'result'){return $r.result};return $r}
try {
 $doctor=Probe @('doctor');Require ($doctor.background_testing -and $doctor.status -eq 'ok') 'Debug background build required'
 $utc=[DateTime]::UtcNow;$startup=[Diagnostics.Stopwatch]::StartNew();$hostProcess=[CliProbe]::Start($gui,@('--temporary','--shell=cmd','--cwd',$fixture.Root),$directory,$directory);$out=$hostProcess.StandardOutput.ReadToEndAsync();$err=$hostProcess.StandardError.ReadToEndAsync()
 $discovery=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($hostProcess.Id).json"
 do {Budget|Out-Null;Require (-not $hostProcess.HasExited -and $startup.ElapsedMilliseconds -lt 8000) 'Startup exceeded eight seconds';if((Test-Path $discovery)-and(Get-Item $discovery).LastWriteTimeUtc -ge $utc){$record=Get-Content -Raw $discovery|ConvertFrom-Json;Require ($record.pid -eq $hostProcess.Id) 'Wrong discovery PID';$pipeName=$record.pipe;break};Start-Sleep -Milliseconds 20}while($true)
 $left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup budget exhausted';$tree=Ready 1 ([int]$left);$a=Request @('identify');$original=@($tree.surfaces)
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
 Click $c.pane 'pane_menu';$tree=Tree;Stable $tree $stable;Require (@($tree.layout.panes).Count -eq 3) 'Hidden pane menu changed layout'
 [ChromeFixture]::Resize([long]$tree.window_handle,$hostProcess.Id,420,400);$tree=Tree;Check-Geometry $tree;Stable $tree $stable
 [ChromeFixture]::Resize([long]$tree.window_handle,$hostProcess.Id,1500,850);$tree=Tree;Check-Geometry $tree
 $evidence.checks+=@{name='native_direct_tools_preserve_existing_terminal_pids_zoom_layout_and_bounded_header_geometry';passed=$true}
 $editor=(Request @('editor','open',$path,'--pane',$c.pane,'--root',$fixture.Root)).editor_opened;Require ([bool]$editor.surface) 'Editor open omitted identity';$watch=[Diagnostics.Stopwatch]::StartNew();do{$status=Request @('editor','status',$editor.surface);Require ($watch.ElapsedMilliseconds -lt 5000) 'Editor readiness exceeded five seconds';if($status.ready){break};Start-Sleep -Milliseconds 20}while($true)
 Editor-Command 'replace-text' @('--text',[EditorFixture]::Edited)|Out-Null;Request @('focus-pane',$a.pane)|Out-Null;$before=Tree;$identity=Request @('identify')
 $rejected=Request @('close-pane',$c.pane) 1;$evidence.observations+=@{name='dirty-close-response';response=$rejected};Require ($rejected.error -like '*unsaved changes*') 'Dirty close did not reject through editor barrier';$after=Tree;$afterIdentity=Request @('identify');Stable $after $stable
 Require (($before.workspaces|ConvertTo-Json -Depth 30 -Compress) -ceq ($after.workspaces|ConvertTo-Json -Depth 30 -Compress) -and $identity.surface -eq $afterIdentity.surface) 'Rejected whole-pane close changed tabs or logical focus'
 $read=Editor-Command 'read';Require ($read.dirty -and $read.content -ceq [EditorFixture]::Edited -and -not $read.document_focused) 'Rejected close lost dirty text or focused hidden editor'
 $evidence.observations+=@{name='dirty-pane-rejected';response=$rejected;before=$before;after=$after;editorRead=$read}
 Editor-Command 'save'|Out-Null;Request @('close-pane',$c.pane)|Out-Null;$editor=$null;$after=Tree
 $retained=@($stable|Where-Object {$_.id -in @($a.surface,$b.surface)});Stable $after $retained
 Require (@($after.layout.panes).Count -eq 2 -and @($after.surfaces).Count -eq 2 -and @($after.browsers).Count -eq 0 -and @($after.editors).Count -eq 0) 'Successful close did not remove every tab in the target pane'
 Require ($fixture.BytesEqual($path,[EditorFixture]::Encode([EditorFixture]::Edited,$false,$false))) 'Saved editor bytes changed during whole-pane close'
 Request @('close-pane',$b.pane)|Out-Null;$tree=Tree;$final=Request @('close-pane',$a.pane) 1;Require ($final.error -like '*final pane*') 'Final pane close was not refused';Stable (Tree) $original
 $evidence.observations+=@{name='clean-whole-pane-close-and-final-refusal';tree=$tree;final=$final}
 $evidence.checks+=@{name='whole_pane_close_seals_all_editors_rejects_dirty_without_partial_removal_and_preserves_final_pane';passed=$true}
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
