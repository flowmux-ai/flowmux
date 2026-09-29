# SPDX-License-Identifier: GPL-3.0-or-later
# Exact owned hidden HWNDs only. 50s work + bounded cleanup, outer Job60s.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",[switch]$Capture)
$ErrorActionPreference='Stop';$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path;$cli=Join-Path $BuildDirectory 'flowmuxctl.exe';$gui=Join-Path $BuildDirectory 'flowmux.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'OptionsFixture.cs'),(Join-Path $PSScriptRoot 'EditorFixture.cs')
Add-Type -Path (Join-Path $PSScriptRoot 'ChromeFixture.cs') -ReferencedAssemblies System.Drawing
$base=if($env:FLOWMUX_TEST_ARTIFACT_ROOT){$env:FLOWMUX_TEST_ARTIFACT_ROOT}else{[IO.Path]::GetTempPath()};$directory=Join-Path $base ('palette-'+[guid]::NewGuid());[IO.Directory]::CreateDirectory($directory)|Out-Null
$terminalDirectory=Join-Path $directory '현재 경로 한 é 😀';[IO.Directory]::CreateDirectory($terminalDirectory)|Out-Null
$clock=[Diagnostics.Stopwatch]::StartNew();$owned=$null;$pipeName=$null;$hostOut=$null;$hostErr=$null;$checks=@();$failure=$null;$last=$null;$commandFailure=$null;$cleanupErrors=@();$cleaning=$false
function Require([bool]$Condition,[string]$Message){if(-not $Condition){throw $Message}}
function Budget([int]$Maximum=5000){if($cleaning){return $Maximum};$left=50000-$clock.ElapsedMilliseconds;Require ($left -gt 0) 'Palette work budget expired';return [int][Math]::Min($Maximum,$left)}
function Probe([string[]]$Arguments,[int]$Maximum=5000){
    Budget|Out-Null;$p=[CliProbe]::Start($cli,$Arguments,$directory,$directory);$out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
    try{Require ($p.WaitForExit((Budget $Maximum))) 'Owned CLI exceeded deadline; no retry';Require ($out.Wait(500) -and $err.Wait(500)) 'CLI output did not close';Require ($p.ExitCode -eq 0) ('CLI failed: '+[CliProbe]::Output($err));return ([CliProbe]::Output($out)|ConvertFrom-Json)}
    catch{$script:commandFailure=@{arguments=$Arguments;stdout=[CliProbe]::Output($out);stderr=[CliProbe]::Output($err)};throw}
    finally{if(-not $p.HasExited){$p.Kill();[CliProbe]::WaitAfterKill($p)};$p.Dispose()}
}
function Request([string[]]$Arguments,[int]$Maximum=5000){Require ([bool]$pipeName) 'Explicit owned pipe required';return Probe (@('--pipe',$pipeName,'--json')+$Arguments) $Maximum}
function Tree([int]$Maximum=5000){$t=Request @('tree') $Maximum;Require ($t.background_testing) 'Host is not hidden';[OptionsFixture]::Describe([long]$t.window_handle,$owned.Id)|Out-Null;$script:last=$t;return $t}
function Await([scriptblock]$Condition){$watch=[Diagnostics.Stopwatch]::StartNew();do{$left=5000-$watch.ElapsedMilliseconds;Require ($left -gt 0) 'Palette condition exceeded five seconds';$tree=Tree ([int]$left);if(& $Condition $tree){return $tree};Start-Sleep -Milliseconds 20}while($true)}
function Await-Settings([scriptblock]$Condition){$watch=[Diagnostics.Stopwatch]::StartNew();do{$left=5000-$watch.ElapsedMilliseconds;Require ($left -gt 0) 'Options condition exceeded five seconds';$status=Request @('settings','show') ([int]$left);if(& $Condition $status){return $status};Start-Sleep -Milliseconds 20}while($true)}
function Identities($Tree){return (@($Tree.surfaces|Sort-Object id|ForEach-Object {$_.id.ToString()+':'+$_.pid.ToString()}) -join ',')}
function Palette($Tree){$panel=$Tree.command_palette;Require ($panel -and $panel.open) 'Owned command palette is not open';[OptionsFixture]::Describe([long]$panel.window,$owned.Id)|Out-Null;return $panel}
function Query($Tree,[string]$Text){$panel=Palette $Tree;[OptionsFixture]::SetTextAndNotify([long]$panel.window,[long]$panel.query_handle,$owned.Id,$Text);return Await {param($t) $t.command_palette.open -and $t.command_palette.query -ceq $Text}}
function Select-Entry($Tree,[string]$Id){
    $tree=Await {param($t) $t.command_palette.open -and @($t.command_palette.filtered) -ccontains $Id};$panel=Palette $tree
    $index=[Array]::IndexOf(@($panel.filtered),$Id);$current=[int]$panel.selected_index;$key=if($current -gt $index){38}else{40}
    for($i=0;$i -lt [Math]::Abs($index-$current);$i++){Budget|Out-Null;[OptionsFixture]::PostKey([long]$panel.query_handle,$owned.Id,$key,$false,$false);[OptionsFixture]::PostKey([long]$panel.query_handle,$owned.Id,$key,$true,$false)}
    return Await {param($t) $t.command_palette.open -and $t.command_palette.selected -ceq $Id}
}
function Closed {return Await {param($t) -not $t.command_palette -or -not $t.command_palette.open}}
function Owner-Restored($Tree){Require ([OptionsFixture]::Describe([long]$Tree.window_handle,$owned.Id).Enabled) 'Main owner remains disabled'}
function Same-Identity($Expected){$actual=Request @('identify');Require ($actual.workspace -ceq $Expected.workspace -and $actual.pane -ceq $Expected.pane -and $actual.surface -ceq $Expected.surface) 'Palette selected the wrong workspace/pane/tab UUID'}
function Open-Palette([string]$Surface){$reply=Request @('test-shortcut',$Surface,'{"code":"KeyP","key":"p","ctrlKey":true,"shiftKey":true}');Require ($reply.surface -ceq $Surface -and -not $reply.forwarded) 'Ctrl+Shift+P was not handled by the actual renderer hook';return Await {param($t) $t.command_palette.open}}
function Menu-Open{$tree=Tree;$button=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'command_palette'});Require ($button.Count -eq 1) 'Command Palette toolbar entry missing';$handle=[long]$button[0].handle;[OptionsFixture]::Click([OptionsFixture]::Parent($handle,$owned.Id),$handle,$owned.Id);return Await {param($t) $t.command_palette.open}}
function Execute($Tree){$panel=Palette $Tree;[OptionsFixture]::PostEnter([long]$panel.query_handle,$owned.Id)}
function Dismiss($Tree){$panel=Palette $Tree;[OptionsFixture]::PostEscape([long]$panel.query_handle,$owned.Id);$tree=Await {param($t) -not $t.command_palette -or -not $t.command_palette.open};Require ([OptionsFixture]::Describe([long]$tree.window_handle,$owned.Id).Enabled) 'Palette dismissal left its main owner disabled';return $tree}
function Copy-Feedback([string]$Surface,[string]$Expected){
    $tree=Await {param($t) $t.copy_feedback -and $t.copy_feedback.source -ceq $Surface};$feedback=$tree.copy_feedback
    Require ($feedback.text -ceq $Expected -and $feedback.success -eq $false -and $feedback.message -match 'Clipboard access is disabled in background hosts' -and -not $feedback.native_visible -and $feedback.owner -eq $tree.window_handle) 'Copy path background guard, source or original payload differs'
    Require ([OptionsFixture]::Parent([long]$feedback.window,$owned.Id) -eq $feedback.owner -and [OptionsFixture]::Text([long]$feedback.window,$owned.Id) -ceq $feedback.message) 'Copy result is not the actual owned hidden native feedback';Owner-Restored $tree
    return $tree
}
function Editor-Bindings([string]$Surface){$revision=(Request @('settings','show')).document.revision;return Await {param($t) @($t.editors|Where-Object {$_.id -ceq $Surface -and $_.keybindings_revision -ceq $revision}).Count -eq 1}}
function Editor-Key([string]$Surface,[hashtable]$Event,[bool]$Forwarded){$reply=Request @('test-shortcut',$Surface,($Event|ConvertTo-Json -Compress));Require ($reply.surface -ceq $Surface -and $reply.forwarded -eq $Forwarded) 'Editor shortcut renderer ACK differs'}
function Editor-Copy([string]$Surface,[hashtable]$Event,[string]$Expected){$before=Tree;Editor-Key $Surface $Event $false;$null=Await {param($t) $t.copy_feedback.source -ceq $Surface -and $t.copy_feedback.window -ne $before.copy_feedback.window};return Copy-Feedback $Surface $Expected}
function Editor-NoAction([string]$Surface,[hashtable]$Event,[bool]$Forwarded){$before=Tree;Editor-Key $Surface $Event $Forwarded;$after=Tree;Require (-not $after.command_palette.open -and (Identities $after) -ceq (Identities $before) -and (-not $after.copy_feedback -or ($before.copy_feedback -and $after.copy_feedback.window -eq $before.copy_feedback.window -and $after.copy_feedback.source -ceq $before.copy_feedback.source -and $after.copy_feedback.text -ceq $before.copy_feedback.text))) 'Guarded or unbound editor shortcut dispatched a host action'}
function Workspace($Tree,[string]$Id){$found=@($Tree.workspaces|Where-Object {$_.id -ceq $Id});Require ($found.Count -eq 1) 'Metadata lost its stable workspace';return $found[0]}
function Workspace-Caption($Tree,[string]$Id,[string]$Expected){
    $rows=@($Tree.chrome.controls|Where-Object {$_.kind -ceq 'workspace' -and $_.workspace -ceq $Id -and $_.layout_visible});Require ($rows.Count -eq 1) 'Named workspace has no unique visible native sidebar row'
    $row=$rows[0];Require ([OptionsFixture]::Parent([long]$row.handle,$owned.Id) -eq $Tree.window_handle) 'Workspace caption belongs to another native parent'
    $caption=([OptionsFixture]::Text([long]$row.handle,$owned.Id) -split "`r?`n",2)[0]
    Require ($caption -ceq $Expected.Replace('&','&&')) 'Native workspace row differs from its Unicode name or Win32 ampersand escaping'
}
function Leaves($Node){if($Node.content){$Node}else{Leaves $Node.first;Leaves $Node.second}}
function Surface($Tree,[string]$Id){$found=@($Tree.workspaces|ForEach-Object {Leaves $_.root}|ForEach-Object {$_.content.surfaces}|Where-Object {$_.id -ceq $Id});Require ($found.Count -eq 1) 'Metadata lost its stable surface';return $found[0]}
function Metadata($Tree,[long]$Owner=0){
    if(-not $Owner){$Owner=[long]$Tree.window_handle}
    $panel=$Tree.metadata;Require ($panel -and $panel.open -and $panel.edit_id -and $panel.native_visible -eq $false) 'Metadata dialog is not the owned hidden active edit'
    $native=[OptionsFixture]::Describe([long]$panel.window,$owned.Id)
    Require ($panel.owner -eq $Owner -and $native.Owner -eq $Owner -and -not $native.OwnerEnabled -and $native.Enabled) 'Metadata dialog is not modal to its exact expected owner'
    foreach($key in @('input','apply','cancel')){Require ($panel.$key -and [OptionsFixture]::Parent([long]$panel.$key,$owned.Id) -eq $panel.window) ('Metadata control has the wrong parent: '+$key)}
    $controls=[ChromeFixture]::Read([long]$panel.window,$owned.Id)
    foreach($key in @('input','apply','cancel')){$control=@($controls|Where-Object {$_.Handle -eq [long]$panel.$key});Require ($control.Count -eq 1 -and $control[0].Font -ne 0 -and $control[0].Shown) ('Metadata native font or shown style is missing: '+$key);if($key -ne 'input'){Require (($control[0].Style -band 0xf) -eq 0xb) 'Metadata action is not painted by native chrome'}}
    return $panel
}
function Open-Metadata([string]$Id,[long]$Owner=0){
    $tree=Menu-Open;$entry=@($tree.command_palette.entries|Where-Object {$_.id -ceq $Id});Require ($entry.Count -eq 1) ('Metadata palette entry missing: '+$Id)
    $tree=Query $tree $entry[0].label;$tree=Select-Entry $tree $Id;Execute $tree
    $tree=Await {param($t) $t.metadata.open -and -not $t.command_palette.open};Metadata $tree $Owner|Out-Null;return $tree
}
function Metadata-Text($Panel,[string]$Value){[OptionsFixture]::SetTextAndNotify([long]$Panel.window,[long]$Panel.input,$owned.Id,$Value)}
function Metadata-Click($Panel,[ValidateSet('apply','cancel','picker')][string]$Action){[OptionsFixture]::Click([long]$Panel.window,[long]$Panel.$Action,$owned.Id)}
function Metadata-Closed{$tree=Await {param($t) -not $t.metadata -or -not $t.metadata.open};Owner-Restored $tree;Require ((Identities $tree) -ceq $identities) 'Metadata changed a terminal process';return $tree}
function Tab-Header($Tree,[string]$Id){
    $frames=@($Tree.detached_windows|Where-Object {$_.surface -ceq $Id})
    if($frames.Count){Require ($frames.Count -eq 1) 'Tab has multiple detached owners';$parent=[long]$frames[0].window_handle;$handle=[long]$frames[0].tab}
    else{$tabs=@($Tree.chrome.controls|Where-Object {$_.kind -ceq 'tab' -and $_.surface -ceq $Id -and $_.layout_visible});Require ($tabs.Count -eq 1) 'Tab has no unique visible header';$parent=[long]$Tree.window_handle;$handle=[long]$tabs[0].handle}
    $bounds=[OptionsFixture]::RelativeBounds($parent,$handle,$owned.Id);Require ($bounds.Width -gt 0 -and $bounds.Height -gt 0) 'Tab header has empty bounds'
    return @{owner=$parent;handle=$handle;bounds=$bounds}
}
function Double-Metadata($Tree,[string]$Id){
    $header=Tab-Header $Tree $Id
    [OptionsFixture]::TabDoubleClick($header.owner,$header.handle,$owned.Id,[int]($header.bounds.Width/2),[int]($header.bounds.Height/2))
    $tree=Await {param($t) $t.metadata.open};Metadata $tree $header.owner|Out-Null
    Require (-not $tree.chrome.tab_dragging) 'Double-click left a tab drag candidate'
    return $tree
}
function Passed([string]$Name){$script:checks+=$Name}
function Capture-Palette($Tree,[string]$Name,[string]$Foreground,[string]$Muted){
    $panel=Palette $Tree;$bounds=[OptionsFixture]::RelativeBounds([long]$panel.window,[long]$panel.list_handle,$owned.Id)
    $bmp=Join-Path $directory ($Name+'.bmp');$result=Request @('chrome-capture',$bmp)
    Require ($result.root_handle -eq $panel.window -and $result.subtree) 'Capture did not use the real owned palette'
    $scale=[OptionsFixture]::Describe([long]$panel.window,$owned.Id).Dpi/96.0;$height=[int](32*$scale);$half=[int]($bounds.Width/2)
    Require ([ChromeFixture]::ColorCount($bmp,$bounds.X,$bounds.Y,$half,$height,$Foreground) -gt 5) 'Palette painter omitted the command title'
    if($Muted){Require ([ChromeFixture]::ColorCount($bmp,($bounds.X+$half),$bounds.Y,($bounds.Width-$half),$height,$Muted) -gt 5) 'Palette painter omitted the separate right-side shortcut'}
    [ChromeFixture]::Png($bmp,(Join-Path $directory ($Name+'.png')));Remove-Item -LiteralPath $bmp
}
try {
    $doctor=Probe @('doctor');Require ($doctor.background_testing -and $doctor.status -eq 'ok') 'Working hidden debug build required'
    $started=[DateTime]::UtcNow;$startup=[Diagnostics.Stopwatch]::StartNew();$owned=[CliProbe]::Start($gui,@('--temporary','--shell=cmd','--cwd',$terminalDirectory),$terminalDirectory,$directory);$hostOut=$owned.StandardOutput.ReadToEndAsync();$hostErr=$owned.StandardError.ReadToEndAsync();
    $file=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($owned.Id).json"
    do {Require (-not $owned.HasExited -and $startup.ElapsedMilliseconds -lt 8000) 'Host discovery exceeded eight seconds or exited';if((Test-Path -LiteralPath $file) -and (Get-Item -LiteralPath $file).LastWriteTimeUtc -ge $started){$record=Get-Content -Raw -LiteralPath $file|ConvertFrom-Json;Require ($record.pid -eq $owned.Id -and [bool]$record.pipe) 'Wrong discovery owner';$pipeName=$record.pipe;break};Start-Sleep -Milliseconds 20}while($true)
    $left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup budget exhausted';Require ((Request @('identify') ([int][Math]::Min(5000,$left))).pid -eq $owned.Id) 'Pipe owner mismatch'
    do {$left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup readiness exceeded eight seconds';$tree=Tree ([int][Math]::Min(5000,$left));if(@($tree.surfaces).Count -eq 1 -and $tree.surfaces[0].ready -and $tree.surfaces[0].running){break};Start-Sleep -Milliseconds 20}while($true)
    $identities=Identities $tree;$initial=Request @('identify');$active=$initial.surface
    Require ($initial.cwd -ceq $terminalDirectory) 'Initial terminal lost its exact Korean/NFD current directory';$copyReply=Request @('test-shortcut',$initial.surface,'{"code":"KeyK","key":"k","ctrlKey":true,"shiftKey":true}');Require ($copyReply.surface -ceq $initial.surface -and -not $copyReply.forwarded) 'Copy path shortcut was not handled by the actual terminal renderer';$tree=Copy-Feedback $initial.surface $terminalDirectory;Require ((Identities $tree) -ceq $identities) 'Copy path shortcut changed terminal IDs or PIDs';Same-Identity $initial;Passed 'terminal-copy-path-shortcut-preserves-Korean-NFD-cwd-with-owned-hidden-clipboard-blocked-feedback'

    $workspaceName='한글 작업공간 한 é &';$tabName='원본 터미널 한글'
    Request @('workspace','rename',$initial.workspace,$workspaceName)|Out-Null;Request @('rename-tab',$active,$tabName)|Out-Null
    Request @('new-workspace','--cwd',$directory,'--shell=cmd')|Out-Null;$second=Request @('identify');Request @('workspace','rename',$second.workspace,'두번째 작업')|Out-Null;Request @('rename-tab',$second.surface,'한글 둘째 탭')|Out-Null
    Request @('split','horizontal','--shell=cmd')|Out-Null;$split=Request @('identify');Request @('rename-tab',$split.surface,'대상 분할 창')|Out-Null
    $tree=Await {param($t) @($t.surfaces).Count -eq 3 -and @($t.surfaces|Where-Object {-not $_.ready -or -not $_.running}).Count -eq 0};$identities=Identities $tree

    $tree=Open-Palette $split.surface;$panel=Palette $tree;$popup=[OptionsFixture]::Describe([long]$panel.window,$owned.Id)
    Require ($panel.modal -and -not $panel.native_visible -and -not $panel.owner_enabled -and $popup.Owner -eq $tree.window_handle -and -not $popup.OwnerEnabled) 'Palette is not the exact hidden modal owned by this host'
    Require ([OptionsFixture]::Parent([long]$panel.query_handle,$owned.Id) -eq $panel.window -and [OptionsFixture]::Parent([long]$panel.list_handle,$owned.Id) -eq $panel.window) 'Palette controls belong to another window'
    $listStyle=[OptionsFixture]::Describe([long]$panel.list_handle,$owned.Id).Style
    Require (($listStyle -band 0x50) -eq 0x50 -and ($listStyle -band 0x80) -eq 0) 'Palette must retain native accessible strings and draw distinct title/shortcut columns'
    Require (@($panel.entries|Where-Object {$_.id -ceq 'action:split-right' -and $_.shortcut -ceq 'Ctrl+Shift+Page Up'}).Count -eq 1) 'Palette exposes stored GTK syntax instead of readable shortcut labels'
    if($Capture){
        $originalSize=[ChromeFixture]::Size([long]$panel.window,$owned.Id);$tree=Query $tree 'split'
        Capture-Palette $tree 'palette-dark' '#f2f3f5' '#abb1bc'
        [ChromeFixture]::Resize([long]$panel.window,$owned.Id,400,300);$tree=Tree;Capture-Palette $tree 'palette-narrow-dark' '#f2f3f5' '#abb1bc'
        Request @('settings','set','theme','light')|Out-Null;$tree=Await {param($t) $t.chrome.theme -eq 'light'};Capture-Palette $tree 'palette-narrow-light' '#28282b' '#5f6269'
        Request @('settings','set','theme','dark')|Out-Null;$tree=Await {param($t) $t.chrome.theme -eq 'dark'}
        [ChromeFixture]::Resize([long]$panel.window,$owned.Id,$originalSize[0],$originalSize[1]);$tree=Query $tree ''
        Passed 'production-palette-title-and-shortcut-paint-at-default-and-narrow-widths-in-both-themes'
    }
    Require (@($panel.entries|Where-Object {$_.id -match '^action:workspace-[3-8]$'}).Count -eq 0) 'Palette exposes nonexistent numbered workspaces'
    Require (@($panel.entries|Where-Object {$_.id -ceq ('workspace:'+$initial.workspace) -and $_.label -ceq ('Workspace: '+$workspaceName)}).Count -eq 1) 'Korean/NFD workspace label changed'
    foreach($query in @('공간한&','공간한É&','공간한é&')){
        $tree=Query $tree $query;$panel=Palette $tree
        Require (@($panel.filtered) -ccontains ('workspace:'+$initial.workspace)) 'Canonical Hangul/accent fuzzy search missed the workspace'
        $entry=@($panel.entries|Where-Object {$_.id -ceq ('workspace:'+$initial.workspace)})[0]
        Require ([string]::Equals([OptionsFixture]::Text([long]$panel.query_handle,$owned.Id),$query,[StringComparison]::Ordinal) -and [string]::Equals($entry.label,('Workspace: '+$workspaceName),[StringComparison]::Ordinal) -and [string]::Equals((Workspace $tree $initial.workspace).name,$workspaceName,[StringComparison]::Ordinal)) 'Canonical filtering rewrote the native query, label or workspace name'
    }
    Passed 'canonical-Hangul-accent-fuzzy-search-preserves-native-query-label-and-workspace-name'
    $tree=Query $tree '한 작';$tree=Select-Entry $tree ('workspace:'+$initial.workspace);Execute $tree;$tree=Closed;Same-Identity $initial;Owner-Restored $tree
    Require ((Identities $tree) -ceq $identities) 'Workspace selection recreated a terminal';Passed 'terminal-shortcut-owned-modal-Korean-workspace-fuzzy-target'

    $tree=Menu-Open;$tree=Query $tree '대 분';$tree=Select-Entry $tree ('pane:'+$split.pane);Execute $tree;$tree=Closed;Same-Identity $split;Owner-Restored $tree
    $tree=Menu-Open;$tree=Query $tree '둘 탭';$tree=Select-Entry $tree ('surface:'+$second.surface);Execute $tree;$tree=Closed;Same-Identity $second
    Require ((Identities $tree) -ceq $identities) 'Pane/tab selection recreated a terminal';Passed 'toolbar-Korean-pane-tab-fuzzy-exact-identities'

    $tree=Menu-Open;$tree=Query $tree 'FLOWMUX_NO_MATCH_98cf3e';$tree=Await {param($t) $t.command_palette.empty -and @($t.command_palette.filtered).Count -eq 0}
    Execute $tree;$tree=Tree;Require ($tree.command_palette.open -and $tree.command_palette.empty -and (Identities $tree) -ceq $identities) 'No-match Enter executed or dismissed';Same-Identity $second
    $tree=Dismiss $tree;Same-Identity $second;Passed 'no-match-Enter-and-Escape-owner-restoration'

    $tree=Menu-Open;$tree=Select-Entry $tree 'action:new-surface';$panel=Palette $tree;$beforeFiltered=@($panel.filtered)-join ',';$raw='한글 한 é & composing'
    [OptionsFixture]::CompositionGuard([long]$panel.window,[long]$panel.query_handle,$owned.Id,$true)
    [OptionsFixture]::SetTextAndNotify([long]$panel.window,[long]$panel.query_handle,$owned.Id,$raw)
    $tree=Await {param($t) $t.command_palette.composing -and $t.command_palette.query -ceq $raw};Execute $tree;[OptionsFixture]::PostEscape([long]$panel.query_handle,$owned.Id)
    $tree=Tree;Require ($tree.command_palette.open -and $tree.command_palette.composing -and $tree.command_palette.query -ceq $raw -and (@($tree.command_palette.filtered)-join ',') -ceq $beforeFiltered -and (Identities $tree) -ceq $identities) 'Composing input ran a command, closed, filtered, or changed Unicode text'
    if($Capture){Capture-Palette $tree 'palette-composing' '#abb1bc' ''}
    [OptionsFixture]::CompositionGuard([long]$panel.window,[long]$panel.query_handle,$owned.Id,$false)
    $tree=Await {param($t) -not $t.command_palette.composing -and $t.command_palette.settling};[OptionsFixture]::PostKey([long]$panel.query_handle,$owned.Id,229,$true,$false)
    $tree=Await {param($t) -not $t.command_palette.settling};$tree=Query $tree '';$tree=Select-Entry $tree 'action:new-surface';$panel=Palette $tree
    [OptionsFixture]::PostKey([long]$panel.query_handle,$owned.Id,229,$false,$false);$tree=Await {param($t) $t.command_palette.settling};Execute $tree
    $tree=Await {param($t) -not $t.command_palette.settling};Require ($tree.command_palette.open -and $tree.command_palette.selected -ceq 'action:new-surface' -and $tree.command_palette.query -ceq '' -and (Identities $tree) -ceq $identities) 'PROCESS/229 guard allowed an actionable Enter'
    [OptionsFixture]::PostKey([long]$panel.query_handle,$owned.Id,229,$true,$false);$tree=Dismiss $tree;Passed 'controlled-composition-Unicode-filter-and-PROCESS229-guards'

    $tree=Menu-Open;$tree=Select-Entry $tree 'action:new-surface';Execute $tree;$tree=Await {param($t) -not $t.command_palette.open -and @($t.surfaces).Count -eq 4 -and @($t.surfaces|Where-Object {-not $_.ready -or -not $_.running}).Count -eq 0};Owner-Restored $tree
    $created=Request @('identify');Require ($created.workspace -ceq $second.workspace -and $created.pane -ceq $second.pane -and $created.surface -cne $second.surface) 'Existing New tab command executed against the wrong target'
    foreach($prior in $identities.Split(',')){Require ((Identities $tree).Split(',') -ccontains $prior) 'New tab command replaced an existing terminal'}
    Request @('rename-tab',$created.surface,'삭제 예정 한글 탭')|Out-Null;Passed 'existing-new-tab-command-retains-live-terminal-identities'

    $tree=Menu-Open;$tree=Query $tree '삭제 예';$tree=Select-Entry $tree ('surface:'+$created.surface)
    Request @('close-tab',$created.surface)|Out-Null;$tree=Await {param($t) @($t.surfaces).Count -eq 3};$survivor=Request @('identify');$survivors=Identities $tree
    Execute $tree;$tree=Await {param($t) $t.command_palette.open -and $t.command_palette.status -match 'no longer exists'}
    Require ($tree.command_palette.selected -ceq ('surface:'+$created.surface) -and (Identities $tree) -ceq $survivors) 'Stale target changed selection or surviving terminals';Same-Identity $survivor
    $tree=Dismiss $tree;Require ((Identities $tree) -ceq $identities) 'Stale-target cleanup damaged original terminal identities';Passed 'stale-target-revalidation-error-and-owner-restoration'

    $tree=Menu-Open;$panel=Palette $tree;$lastIndex=@($panel.filtered).Count-1
    [OptionsFixture]::PostKey([long]$panel.list_handle,$owned.Id,35,$false,$false);[OptionsFixture]::PostKey([long]$panel.list_handle,$owned.Id,35,$true,$false)
    $tree=Await {param($t) $t.command_palette.open -and $t.command_palette.selected_index -eq $lastIndex}
    [OptionsFixture]::PostKey([long]$panel.list_handle,$owned.Id,36,$false,$false);[OptionsFixture]::PostKey([long]$panel.list_handle,$owned.Id,36,$true,$false)
    $tree=Await {param($t) $t.command_palette.open -and $t.command_palette.selected_index -eq 0};Require ((Identities $tree) -ceq $identities) 'Home/End executed an entry instead of selecting'
    $tree=Query $tree 'Options';$tree=Await {param($t) $t.command_palette.open -and $t.command_palette.selected -ceq 'native:settings' -and $t.command_palette.selected_index -eq 0};$panel=Palette $tree
    [OptionsFixture]::ClickListRow([long]$panel.window,[long]$panel.list_handle,$owned.Id,0)
    $status=Await-Settings {param($s) $s.options.open};$tree=Closed;Owner-Restored $tree;Require ([OptionsFixture]::Describe([long]$status.options.window,$owned.Id).Owner -eq $tree.window_handle) 'Single-click Options used another owner'
    [OptionsFixture]::Click([long]$status.options.window,[long]$status.options.close,$owned.Id);$status=Await-Settings {param($s) -not $s.options.open};Passed 'Home-End-selection-only-and-first-selected-row-single-click-Options'

    $browser=(Request @('browser','open','about:blank','--pane',$survivor.pane)).browser_pane_opened;Request @('focus-pane',$browser.pane)|Out-Null
    $tree=Menu-Open;Require (@($tree.command_palette.entries|Where-Object {$_.id -ceq 'action:terminal-search'}).Count -eq 0) 'Browser palette offers terminal-only search'
    $tree=Dismiss $tree;Require ((Request @('identify')).surface -ceq $browser.surface) 'Browser palette changed its target'
    $browserBefore=@($tree.browsers|Where-Object {$_.id -ceq $browser.surface})[0];$browserTab=Surface $tree $browser.surface
    $tree=Menu-Open;$tree=Query $tree 'Copy focused pane path';$tree=Select-Entry $tree 'action:copy-pane-path';Execute $tree;$tree=Closed;$tree=Copy-Feedback $browser.surface $browserBefore.url;$copiedBrowser=@($tree.browsers|Where-Object {$_.id -ceq $browser.surface})[0];Require ($copiedBrowser.view_handle -eq $browserBefore.view_handle -and $copiedBrowser.url -ceq $browserBefore.url -and (Identities $tree) -ceq $identities -and (Request @('identify')).surface -ceq $browser.surface) 'Browser copy request changed its view, URL, selection or terminal PIDs';Passed 'browser-palette-copy-URL-uses-current-view-and-hidden-clipboard-blocked-feedback'
    $tree=Double-Metadata $tree $browser.surface;$panel=Metadata $tree
    Require ([OptionsFixture]::Text([long]$panel.input,$owned.Id) -ceq $browserTab.title) 'Browser double-click captured a different tab'
    Metadata-Text $panel '   ';Metadata-Click $panel 'apply';$tree=Metadata-Closed;$unchanged=Surface $tree $browser.surface
    Require ($unchanged.title -ceq $browserTab.title -and $unchanged.title_locked -eq $browserTab.title_locked) 'Whitespace-only browser rename changed its title or lock'
    $tree=Double-Metadata $tree $browser.surface;$panel=Metadata $tree;$browserName='브라우저 한 é 😀 & 이름';Metadata-Text $panel ('  '+$browserName+'  ');Metadata-Click $panel 'apply';$tree=Metadata-Closed
    $browserAfter=@($tree.browsers|Where-Object {$_.id -ceq $browser.surface})[0];$renamedBrowser=Surface $tree $browser.surface
    Require ($renamedBrowser.title -ceq $browserName -and $renamedBrowser.title_locked -and $browserAfter.view_handle -eq $browserBefore.view_handle -and $browserAfter.url -ceq $browserBefore.url -and (Request @('identify')).surface -ceq $browser.surface) 'Browser header rename changed view identity, URL, selection or Unicode title'
    Passed 'browser-header-doubleclick-trim-and-whitespace-noop-preserve-view-and-lock'
    Request @('close-tab',$browser.surface)|Out-Null
    $tree=Await {param($t) @($t.browsers).Count -eq 0};Owner-Restored $tree;Require ((Identities $tree) -ceq $identities) 'Browser palette or cleanup replaced a terminal';Passed 'contextual-action-availability-and-browser-toolbar-entry'

    Request @('focus-tab',$initial.surface)|Out-Null;$editorFixture=New-Object EditorFixture($directory)
    try{
        $editorPath=$editorFixture.Write('경로 한 문서.txt',[EditorFixture]::Original,$false,$false);$editor=(Request @('editor','open',$editorPath,'--pane',$initial.pane,'--root',$editorFixture.Root)).editor_opened
        $tree=Await {param($t) @($t.editors|Where-Object {$_.id -ceq $editor.surface -and $_.ready -and -not $_.dirty}).Count -eq 1};$editorBefore=@($tree.editors|Where-Object {$_.id -ceq $editor.surface})[0]
        $tree=Menu-Open;$tree=Query $tree 'Copy focused pane path';$tree=Select-Entry $tree 'action:copy-pane-path';Execute $tree;$tree=Closed;$tree=Copy-Feedback $editor.surface $editorBefore.workspace_root;$editorAfter=@($tree.editors|Where-Object {$_.id -ceq $editor.surface})[0]
        Require ($editorAfter.view_handle -eq $editorBefore.view_handle -and -not $editorAfter.dirty -and $editorAfter.workspace_root -ceq $editorBefore.workspace_root -and (Identities $tree) -ceq $identities -and $editorFixture.BytesEqual($editorPath,[EditorFixture]::Encode([EditorFixture]::Original,$false,$false))) 'Editor copy request changed its retained view, workspace root, document bytes or terminal PIDs'
        # Synthetic DOM events exercise the editor's production listener and
        # application guards; they do not establish physical keyboard/IME routing.
        $tree=Editor-Bindings $editor.surface;$readReply=Request @('editor','command',$editor.surface,'read');$editorReadBefore=if($readReply.psobject.Properties.Name -contains 'result'){$readReply.result}else{$readReply}
        Require ($editorReadBefore.search_open -eq $false) 'Editor search dialog was already open before the shortcut regression'
        $tree=Open-Palette $editor.surface;$tree=Dismiss $tree
        $copyKey=@{code='KeyK';key='k';ctrlKey=$true;shiftKey=$true};$tree=Editor-Copy $editor.surface $copyKey $editorBefore.workspace_root
        Request @('settings','keybindings','set','copy-pane-path','Ctrl+Alt+H')|Out-Null;$tree=Editor-Bindings $editor.surface;Editor-NoAction $editor.surface $copyKey $true
        $reboundCopy=@{code='KeyH';key='h';ctrlKey=$true;altKey=$true};$tree=Editor-Copy $editor.surface $reboundCopy $editorBefore.workspace_root
        Request @('settings','keybindings','set','copy-pane-path')|Out-Null;$tree=Editor-Bindings $editor.surface;Editor-NoAction $editor.surface $reboundCopy $true
        Request @('settings','keybindings','clear','copy-pane-path')|Out-Null;$tree=Editor-Bindings $editor.surface;$tree=Editor-Copy $editor.surface $copyKey $editorBefore.workspace_root
        $paletteKey=@{code='KeyP';key='p';ctrlKey=$true;shiftKey=$true}
        foreach($guard in @(@{repeat=$true;forwarded=$false},@{isComposing=$true;forwarded=$true},@{keyCode=229;forwarded=$true},@{altGraph=$true;forwarded=$true})){$event=$paletteKey.Clone();foreach($key in $guard.Keys){if($key -ne 'forwarded'){$event[$key]=$guard[$key]}};Editor-NoAction $editor.surface $event $guard.forwarded;Editor-Key $editor.surface @{type='keyup';code='KeyP';key='p'} $true}
        Request @('test-shortcut',$editor.surface,'{"type":"compositionstart"}')|Out-Null
        try{Editor-NoAction $editor.surface $paletteKey $true}finally{Request @('test-shortcut',$editor.surface,'{"type":"compositionend"}')|Out-Null}
        Editor-Key $editor.surface @{type='keyup';code='KeyP';key='p'} $true;$tree=Open-Palette $editor.surface;$tree=Dismiss $tree
        Request @('focus-tab',$initial.surface)|Out-Null;Same-Identity $initial;Editor-NoAction $editor.surface $paletteKey $false;Same-Identity $initial
        Request @('focus-tab',$editor.surface)|Out-Null;$tree=Open-Palette $editor.surface;$tree=Dismiss $tree
        Editor-Key $editor.surface @{code='KeyF';key='f';ctrlKey=$true;shiftKey=$true} $false
        $readReply=Request @('editor','command',$editor.surface,'read');$searchRead=if($readReply.psobject.Properties.Name -contains 'result'){$readReply.result}else{$readReply};Require ($searchRead.search_open -eq $true -and $searchRead.search_mode -ceq 'workspace') 'Ctrl+Shift+F did not open the actual editor workspace search dialog'
        Editor-NoAction $editor.surface $paletteKey $true
        $readReply=Request @('editor','command',$editor.surface,'read');$editorReadAfter=if($readReply.psobject.Properties.Name -contains 'result'){$readReply.result}else{$readReply};$tree=Tree;$editorAfter=@($tree.editors|Where-Object {$_.id -ceq $editor.surface})[0]
        Require ($editorReadAfter.search_open -eq $true -and $editorReadAfter.search_mode -ceq 'workspace') 'Blocked Ctrl+Shift+P closed or changed the editor workspace search dialog'
        Require ($editorReadBefore.content -ceq [EditorFixture]::Original -and $editorReadAfter.content -ceq $editorReadBefore.content -and $editorReadAfter.document_id -ceq $editorReadBefore.document_id -and $editorReadAfter.active_version -eq $editorReadBefore.active_version -and $editorReadAfter.document_focused -eq $false -and -not $editorAfter.dirty -and $editorAfter.view_handle -eq $editorBefore.view_handle -and (Identities $tree) -ceq $identities -and $editorFixture.BytesEqual($editorPath,[EditorFixture]::Encode([EditorFixture]::Original,$false,$false))) 'Editor shortcuts changed model content/version/focus, native view, disk bytes or terminal PIDs';Passed 'editor-renderer-palette-copy-rebind-unbind-reset-composition-229-AltGraph-repeat-and-inactive-source-guards-preserve-document'
        Request @('close-tab',$editor.surface)|Out-Null;$tree=Await {param($t) @($t.editors).Count -eq 0};Require ((Identities $tree) -ceq $identities) 'Closing the copy-path editor changed terminal PIDs';Passed 'editor-palette-copy-uses-workspace-root-and-preserves-clean-document-and-view'
    }finally{$editorFixture.Dispose()}

    # Native metadata dialogs reuse the palette, settings theme and original
    # model IDs. Synthetic composition messages test guards, not physical IME.
    Request @('focus-tab',$initial.surface)|Out-Null
    $tree=Open-Metadata 'metadata:workspace-name';$panel=Metadata $tree
    Require ([OptionsFixture]::Text([long]$panel.input,$owned.Id) -ceq $workspaceName) 'Workspace editor lost its starting UTF-16 text'
    $renamed='이름 한 é 😀 & 변경';Metadata-Text $panel $renamed
    [OptionsFixture]::PostEscape([long]$panel.input,$owned.Id)
    $tree=Metadata-Closed;Require ((Workspace $tree $initial.workspace).name -ceq $workspaceName) 'Input Escape did not cancel without changing the workspace'
    $tree=Open-Metadata 'metadata:workspace-name';$panel=Metadata $tree
    [OptionsFixture]::CompositionGuard([long]$panel.window,[long]$panel.input,$owned.Id,$true);Metadata-Text $panel $renamed
    [OptionsFixture]::PostEnter([long]$panel.input,$owned.Id);[OptionsFixture]::PostEscape([long]$panel.input,$owned.Id)
    Metadata-Click $panel 'apply'
    $tree=Await {param($t) $t.metadata.open -and $t.metadata.composing};Metadata $tree|Out-Null
    Require ((Workspace $tree $initial.workspace).name -ceq $workspaceName -and [OptionsFixture]::Text([long]$panel.input,$owned.Id) -ceq $renamed) 'Composition guard changed the draft or model'
    [OptionsFixture]::CompositionGuard([long]$panel.window,[long]$panel.input,$owned.Id,$false)
    $tree=Await {param($t) $t.metadata.open -and -not $t.metadata.composing -and $t.metadata.settling}
    foreach($key in @(13,27)){[OptionsFixture]::PostKey([long]$panel.input,$owned.Id,$key,$false,$false)}
    [OptionsFixture]::PostKey([long]$panel.input,$owned.Id,16,$true,$false)
    $tree=Tree;Metadata $tree|Out-Null;Require ($tree.metadata.settling -and (Workspace $tree $initial.workspace).name -ceq $workspaceName -and [OptionsFixture]::Text([long]$panel.input,$owned.Id) -ceq $renamed) 'Composition-ending Enter/Escape or modifier release applied, cancelled or changed the draft'
    [OptionsFixture]::PostKey([long]$panel.input,$owned.Id,13,$true,$false)
    $tree=Await {param($t) $t.metadata.open -and -not $t.metadata.settling}
    [OptionsFixture]::PostKey([long]$panel.input,$owned.Id,13,$false,$true)
    $tree=Tree;Metadata $tree|Out-Null;Require ((Workspace $tree $initial.workspace).name -ceq $workspaceName) 'Held Enter submitted the rename'
    [OptionsFixture]::PostEnter([long]$panel.input,$owned.Id)
    $tree=Metadata-Closed;Require ((Workspace $tree $initial.workspace).name -ceq $renamed) 'Input Enter did not retain the exact Korean/NFD/emoji/ampersand name';Same-Identity $initial;Workspace-Caption $tree $initial.workspace $renamed
    Passed 'metadata-input-Enter-Escape-IME-end-key-release-repeat-and-button-guard-preserve-Unicode-identity'

    $tree=Open-Metadata 'metadata:workspace-name';$panel=Metadata $tree;Metadata-Text $panel '취소할 조합'
    [OptionsFixture]::CompositionGuard([long]$panel.window,[long]$panel.input,$owned.Id,$true)
    [OptionsFixture]::CompositionGuard([long]$panel.window,[long]$panel.input,$owned.Id,$false)
    [OptionsFixture]::PostEscape([long]$panel.input,$owned.Id)
    $tree=Await {param($t) $t.metadata.open -and -not $t.metadata.settling};Metadata $tree|Out-Null
    [OptionsFixture]::PostEscape([long]$panel.input,$owned.Id)
    $tree=Metadata-Closed;Require ((Workspace $tree $initial.workspace).name -ceq $renamed) 'Post-composition Escape failed to cancel without changing the name'
    Passed 'metadata-IME-cancel-Escape-keeps-dialog-until-next-independent-Escape'

    $tree=Open-Metadata 'metadata:workspace-name';$panel=Metadata $tree
    $losing='UI 한 😀 draft';$winner='외부 한글 & winner';Metadata-Text $panel $losing
    Request @('workspace','rename',$initial.workspace,$winner)|Out-Null;Metadata-Click $panel 'apply'
    $tree=Await {param($t) $t.metadata.open -and $t.metadata.error -match 'changed elsewhere'};Metadata $tree|Out-Null
    Require ((Workspace $tree $initial.workspace).name -ceq $winner -and [OptionsFixture]::Text([long]$panel.input,$owned.Id) -ceq $losing) 'Concurrent IPC rename was overwritten or its pending UI draft was lost'
    Metadata-Click $panel 'cancel';$tree=Metadata-Closed;Require ((Workspace $tree $initial.workspace).name -ceq $winner) 'Conflict Cancel changed the external winner'
    Passed 'metadata-concurrent-rename-conflict-retains-draft-and-modal-owner'

    $tree=Open-Metadata 'metadata:tab-name';$panel=Metadata $tree;$newTab='탭 한 😀 & 고정';Metadata-Text $panel $newTab
    [OptionsFixture]::PostKey([long]$panel.input,$owned.Id,229,$false,$false)
    [OptionsFixture]::PostKey([long]$panel.input,$owned.Id,13,$false,$false)
    $tree=Await {param($t) $t.metadata.open -and $t.metadata.settling};Metadata $tree|Out-Null
    [OptionsFixture]::PostKey([long]$panel.input,$owned.Id,229,$true,$false)
    $tree=Await {param($t) $t.metadata.open -and -not $t.metadata.settling}
    [OptionsFixture]::PostEnter([long]$panel.input,$owned.Id)
    $tree=Metadata-Closed;$tab=Surface $tree $initial.surface
    Require ($tab.title -ceq $newTab -and $tab.title_locked) 'Native tab rename did not preserve its exact title and manual title lock';Same-Identity $initial
    Passed 'metadata-tab-name-PROCESS-key-release-then-Enter-locks-title-without-changing-live-surface-identity'

    Require ((Workspace $tree $initial.workspace).name_locked -eq $true -and (Workspace $tree $initial.workspace).name -ceq $winner) 'Manual workspace name followed a tab rename'
    Workspace-Caption $tree $initial.workspace $winner
    $tree=Open-Metadata 'metadata:workspace-name';$panel=Metadata $tree;Metadata-Text $panel '   ';Metadata-Click $panel 'apply'
    $tree=Metadata-Closed;$workspace=Workspace $tree $initial.workspace
    Require ($workspace.name_locked -eq $false -and $workspace.name -ceq $newTab) 'Whitespace workspace rename did not resume the current tab title';Workspace-Caption $tree $initial.workspace $newTab
    $automatic='자동 둘째 한 😀 & 탭';Request @('rename-tab',$initial.surface,$automatic)|Out-Null
    $tree=Await {param($t) $w=Workspace $t $initial.workspace;$w.name_locked -eq $false -and $w.name -ceq $automatic};Workspace-Caption $tree $initial.workspace $automatic
    $tree=Open-Metadata 'metadata:workspace-name';$panel=Metadata $tree;$trimmedDraft='  '+$winner+'  ';Metadata-Text $panel $trimmedDraft
    $duringEdit='편집 중 자동 한 😀 & 제목';Request @('rename-tab',$initial.surface,$duringEdit)|Out-Null
    $tree=Await {param($t) $w=Workspace $t $initial.workspace;$w.name_locked -eq $false -and $w.name -ceq $duringEdit};Metadata $tree|Out-Null
    Require ([OptionsFixture]::Text([long]$panel.input,$owned.Id) -ceq $trimmedDraft) 'Automatic title update replaced the open workspace draft'
    Metadata-Click $panel 'apply';$tree=Metadata-Closed
    Require ((Workspace $tree $initial.workspace).name_locked -eq $true -and (Workspace $tree $initial.workspace).name -ceq $winner) 'Automatic title update caused a false conflict or custom whitespace was not trimmed'
    Request @('rename-tab',$initial.surface,'고정 후 변경 한 😀 & 탭')|Out-Null;$tree=Tree
    Require ((Workspace $tree $initial.workspace).name_locked -eq $true -and (Workspace $tree $initial.workspace).name -ceq $winner) 'Restored custom workspace name continued following tab titles'
    Workspace-Caption $tree $initial.workspace $winner;Require ((Identities $tree) -ceq $identities) 'Workspace automatic naming changed terminal identities';Same-Identity $initial
    Passed 'workspace-custom-lock-whitespace-auto-title-sidebar-follow-and-edit-draft-trim-preservation'

    $tree=Open-Metadata 'metadata:workspace-color';$panel=Metadata $tree;$originalColor=(Workspace $tree $initial.workspace).color
    foreach($key in @('swatch','picker')){Require ($panel.$key -and [OptionsFixture]::Parent([long]$panel.$key,$owned.Id) -eq $panel.window) ('Color editor omitted its actual owned '+$key);$control=[OptionsFixture]::Describe([long]$panel.$key,$owned.Id);Require (($control.Style -band 0x10000000) -ne 0 -and ($control.Style -band 0xf) -eq 0xb) ('Color control is not shown and painted by native chrome: '+$key)}
    $colorRaw=[OptionsFixture]::Text([long]$panel.input,$owned.Id);$edit=$panel.edit_id;Metadata-Click $panel 'picker'
    $tree=Await {param($t) $t.metadata.open -and $t.metadata.error -match 'background|hidden'};$panel=Metadata $tree
    Require ($panel.edit_id -ceq $edit -and [OptionsFixture]::Text([long]$panel.input,$owned.Id) -ceq $colorRaw -and (Workspace $tree $initial.workspace).color -ceq $originalColor) 'Blocked hidden color chooser changed the popup, draft or model'
    Metadata-Text $panel '#12ABEF';Metadata-Click $panel 'apply';$tree=Metadata-Closed
    Require ((Workspace $tree $initial.workspace).color -ceq '#12abef') 'Color Apply did not store canonical hex'
    $tree=Open-Metadata 'metadata:workspace-color';$panel=Metadata $tree;Metadata-Text $panel '';Metadata-Click $panel 'apply';$tree=Metadata-Closed
    Require (-not (Workspace $tree $initial.workspace).color) 'Blank color did not clear the workspace override';Same-Identity $initial
    Passed 'metadata-color-native-swatch-hidden-chooser-guard-hex-Apply-and-empty-Clear'

    $header=Tab-Header $tree $initial.surface;$x=[int]($header.bounds.Width/2);$y=[int]($header.bounds.Height/2)
    [OptionsFixture]::TabPointerDown($header.owner,$header.handle,$owned.Id,$x,$y)
    [OptionsFixture]::HostPointer($header.owner,$owned.Id,0x202,($header.bounds.X+$x),($header.bounds.Y+$y))
    $tree=Await {param($t) -not $t.chrome.tab_dragging};Require (-not $tree.metadata.open -and (Identities $tree) -ceq $identities) 'Single header click opened metadata or changed a terminal';Same-Identity $initial
    $beforeTab=Surface $tree $initial.surface;$tree=Double-Metadata $tree $initial.surface;$panel=Metadata $tree
    Require ([OptionsFixture]::Text([long]$panel.input,$owned.Id) -ceq $beforeTab.title) 'Terminal double-click captured another title';Same-Identity $initial
    $headerName='헤더 한 é 😀 & 이름';Metadata-Text $panel ('  '+$headerName+'  ');Metadata-Click $panel 'apply';$tree=Metadata-Closed;$tab=Surface $tree $initial.surface
    Require ($tab.title -ceq $headerName -and $tab.title_locked) 'Header rename did not trim only outer whitespace and preserve Unicode';Same-Identity $initial
    $tree=Double-Metadata $tree $initial.surface;$panel=Metadata $tree;Metadata-Text $panel '   ';Metadata-Click $panel 'apply';$tree=Metadata-Closed;$tab=Surface $tree $initial.surface
    Require ($tab.title -ceq $headerName -and $tab.title_locked) 'Whitespace-only terminal rename changed the locked title';Same-Identity $initial
    Passed 'terminal-header-singleclick-no-dialog-doubleclick-Unicode-trim-and-empty-noop'

    # Closing the owner through IPC must dispose the modal panel before the
    # detached HWND dies, without disabling the surviving main workbench.
    Request @('new-tab','--shell=cmd')|Out-Null;$temporary=Request @('identify')
    $tree=Await {param($t) @($t.surfaces).Count -eq 4 -and @($t.surfaces|Where-Object {-not $_.ready -or -not $_.running}).Count -eq 0}
    # Capture this tab as the main palette target before it changes windows.
    # Detached terminal shortcuts intentionally do not open Command Palette.
    $tree=Menu-Open;$entry=@($tree.command_palette.entries|Where-Object {$_.id -ceq 'metadata:tab-name'});Require ($entry.Count -eq 1) 'Metadata tab rename palette entry missing'
    $tree=Query $tree $entry[0].label;$tree=Select-Entry $tree 'metadata:tab-name'
    Request @('detach-tab',$temporary.surface)|Out-Null;$tree=Await {param($t) @($t.detached_windows|Where-Object {$_.surface -ceq $temporary.surface}).Count -eq 1}
    $frame=@($tree.detached_windows|Where-Object {$_.surface -ceq $temporary.surface})[0]
    Require ($frame.window_handle -ne $tree.window_handle -and $frame.native_visible -eq $false) 'Owner-destruction check requires its own hidden detached window'
    Require ($tree.command_palette.open -and $tree.command_palette.selected -ceq 'metadata:tab-name') 'Detaching lost the selected palette target'
    Execute $tree;$tree=Await {param($t) $t.metadata.open -and -not $t.command_palette.open};$panel=Metadata $tree ([long]$frame.window_handle)
    Require (-not ([OptionsFixture]::Describe([long]$frame.window_handle,$owned.Id)).Enabled) 'Detached metadata owner was not disabled';Owner-Restored $tree
    $detachedIds=Identities $tree;Metadata-Click $panel 'cancel'
    $tree=Await {param($t) -not $t.metadata.open};Require ([OptionsFixture]::Describe([long]$frame.window_handle,$owned.Id).Enabled) 'Detached metadata Cancel left its owner disabled'
    $tree=Double-Metadata $tree $temporary.surface;$panel=Metadata $tree ([long]$frame.window_handle)
    $detachedName='분리 헤더 한 é 😀 & 이름';Metadata-Text $panel ('  '+$detachedName+'  ');Metadata-Click $panel 'apply'
    $tree=Await {param($t) -not $t.metadata.open};$tab=Surface $tree $temporary.surface
    Require ($tab.title -ceq $detachedName -and $tab.title_locked -and (Identities $tree) -ceq $detachedIds -and [OptionsFixture]::Describe([long]$frame.window_handle,$owned.Id).Enabled) 'Detached header rename changed title, terminal identities or owner restoration';Owner-Restored $tree
    Require ([OptionsFixture]::Text([long]$frame.tab,$owned.Id) -ceq $detachedName.Replace('&','&&')) 'Detached native header did not show the renamed title'
    Passed 'detached-header-doubleclick-uses-exact-modal-owner-and-retains-PIDs'
    $tree=Double-Metadata $tree $temporary.surface;$panel=Metadata $tree ([long]$frame.window_handle)
    Metadata-Text $panel '닫힐 한 😀 & draft';$oldEdit=$panel.edit_id
    Request @('close-tab',$temporary.surface)|Out-Null
    $tree=Await {param($t) $null -eq $t.metadata -and @($t.detached_windows|Where-Object {$_.surface -ceq $temporary.surface}).Count -eq 0 -and @($t.surfaces|Where-Object {$_.id -ceq $temporary.surface}).Count -eq 0}
    Require (-not $owned.HasExited -and (Identities $tree) -ceq $identities) 'Closing a metadata owner damaged surviving terminal sessions';Owner-Restored $tree
    Request @('focus-tab',$initial.surface)|Out-Null;$tree=Open-Metadata 'metadata:workspace-name';$panel=Metadata $tree
    Require ($panel.edit_id -cne $oldEdit -and [OptionsFixture]::Text([long]$panel.input,$owned.Id) -ceq $winner) 'Surviving main metadata reused a stale edit or lost its workspace name'
    Metadata-Click $panel 'cancel';$tree=Metadata-Closed;Same-Identity $initial
    Passed 'detached-metadata-owner-IPC-close-disposes-panel-and-main-can-reopen-and-Cancel'

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
    if($failure -or $cleanupErrors.Count){$path=Join-Path $directory 'failure.json';@{error=$failure;cleanupErrors=$cleanupErrors;passed=$checks;lastTree=$last;command=$commandFailure;host=$hostLog;scope='Hidden owned HWND messages; physical Korean IME/focus not exercised'}|ConvertTo-Json -Depth 30|Set-Content -Encoding UTF8 $path;Write-Output ('Failure diagnostics: '+$path)}
    elseif(-not $Capture){Remove-Item -LiteralPath $directory -Recurse -Force}
}
if($failure){throw $failure};if($cleanupErrors.Count){throw ($cleanupErrors -join '; ')}
Write-Output ('Command Palette passed: '+$checks.Count+' groups in '+[Math]::Round($clock.Elapsed.TotalSeconds,3)+'s; hidden guards only, no physical IME/focus acceptance')
