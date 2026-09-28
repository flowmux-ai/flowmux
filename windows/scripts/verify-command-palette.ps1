# SPDX-License-Identifier: GPL-3.0-or-later
# Exact owned hidden HWNDs only. 50s work + bounded cleanup, outer Job60s.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference='Stop';$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path;$cli=Join-Path $BuildDirectory 'flowmuxctl.exe';$gui=Join-Path $BuildDirectory 'flowmux.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'OptionsFixture.cs')
Add-Type -Path (Join-Path $PSScriptRoot 'ChromeFixture.cs') -ReferencedAssemblies System.Drawing
$base=if($env:FLOWMUX_TEST_ARTIFACT_ROOT){$env:FLOWMUX_TEST_ARTIFACT_ROOT}else{[IO.Path]::GetTempPath()};$directory=Join-Path $base ('palette-'+[guid]::NewGuid());[IO.Directory]::CreateDirectory($directory)|Out-Null
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
function Open-Palette([string]$Surface){$reply=Request @('test-shortcut',$Surface,'{"code":"KeyP","key":"p","ctrlKey":true,"shiftKey":true}');Require ($reply.surface -ceq $Surface -and -not $reply.forwarded) 'Ctrl+Shift+P was not handled by the actual terminal hook';return Await {param($t) $t.command_palette.open}}
function Menu-Open{$tree=Tree;$button=@($tree.chrome.controls|Where-Object {$_.kind -ceq 'command_palette'});Require ($button.Count -eq 1) 'Command Palette toolbar entry missing';$handle=[long]$button[0].handle;[OptionsFixture]::Click([OptionsFixture]::Parent($handle,$owned.Id),$handle,$owned.Id);return Await {param($t) $t.command_palette.open}}
function Execute($Tree){$panel=Palette $Tree;[OptionsFixture]::PostEnter([long]$panel.query_handle,$owned.Id)}
function Dismiss($Tree){$panel=Palette $Tree;[OptionsFixture]::PostEscape([long]$panel.query_handle,$owned.Id);$tree=Await {param($t) -not $t.command_palette -or -not $t.command_palette.open};Require ([OptionsFixture]::Describe([long]$tree.window_handle,$owned.Id).Enabled) 'Palette dismissal left its main owner disabled';return $tree}
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
function Passed([string]$Name){$script:checks+=$Name}
try {
    $doctor=Probe @('doctor');Require ($doctor.background_testing -and $doctor.status -eq 'ok') 'Working hidden debug build required'
    $started=[DateTime]::UtcNow;$startup=[Diagnostics.Stopwatch]::StartNew();$owned=[CliProbe]::Start($gui,@('--temporary','--shell=cmd','--cwd',$directory),$directory,$directory);$hostOut=$owned.StandardOutput.ReadToEndAsync();$hostErr=$owned.StandardError.ReadToEndAsync();
    $file=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($owned.Id).json"
    do {Require (-not $owned.HasExited -and $startup.ElapsedMilliseconds -lt 8000) 'Host discovery exceeded eight seconds or exited';if((Test-Path -LiteralPath $file) -and (Get-Item -LiteralPath $file).LastWriteTimeUtc -ge $started){$record=Get-Content -Raw -LiteralPath $file|ConvertFrom-Json;Require ($record.pid -eq $owned.Id -and [bool]$record.pipe) 'Wrong discovery owner';$pipeName=$record.pipe;break};Start-Sleep -Milliseconds 20}while($true)
    $left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup budget exhausted';Require ((Request @('identify') ([int][Math]::Min(5000,$left))).pid -eq $owned.Id) 'Pipe owner mismatch'
    do {$left=8000-$startup.ElapsedMilliseconds;Require ($left -gt 0) 'Startup readiness exceeded eight seconds';$tree=Tree ([int][Math]::Min(5000,$left));if(@($tree.surfaces).Count -eq 1 -and $tree.surfaces[0].ready -and $tree.surfaces[0].running){break};Start-Sleep -Milliseconds 20}while($true)
    $identities=Identities $tree;$initial=Request @('identify');$active=$initial.surface


    $workspaceName='한글 작업공간 한 &';$tabName='원본 터미널 한글'
    Request @('workspace','rename',$initial.workspace,$workspaceName)|Out-Null;Request @('rename-tab',$active,$tabName)|Out-Null
    Request @('new-workspace','--cwd',$directory,'--shell=cmd')|Out-Null;$second=Request @('identify');Request @('workspace','rename',$second.workspace,'두번째 작업')|Out-Null;Request @('rename-tab',$second.surface,'한글 둘째 탭')|Out-Null
    Request @('split','horizontal','--shell=cmd')|Out-Null;$split=Request @('identify');Request @('rename-tab',$split.surface,'대상 분할 창')|Out-Null
    $tree=Await {param($t) @($t.surfaces).Count -eq 3 -and @($t.surfaces|Where-Object {-not $_.ready -or -not $_.running}).Count -eq 0};$identities=Identities $tree

    $tree=Open-Palette $split.surface;$panel=Palette $tree;$popup=[OptionsFixture]::Describe([long]$panel.window,$owned.Id)
    Require ($panel.modal -and -not $panel.native_visible -and -not $panel.owner_enabled -and $popup.Owner -eq $tree.window_handle -and -not $popup.OwnerEnabled) 'Palette is not the exact hidden modal owned by this host'
    Require ([OptionsFixture]::Parent([long]$panel.query_handle,$owned.Id) -eq $panel.window -and [OptionsFixture]::Parent([long]$panel.list_handle,$owned.Id) -eq $panel.window) 'Palette controls belong to another window'
    Require (@($panel.entries|Where-Object {$_.id -match '^action:workspace-[3-8]$'}).Count -eq 0) 'Palette exposes nonexistent numbered workspaces'
    Require (@($panel.entries|Where-Object {$_.id -ceq ('workspace:'+$initial.workspace) -and $_.label -ceq ('Workspace: '+$workspaceName)}).Count -eq 1) 'Korean/NFD workspace label changed'
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
    $tree=Dismiss $tree;Require ((Request @('identify')).surface -ceq $browser.surface) 'Browser palette changed its target';Request @('close-tab',$browser.surface)|Out-Null
    $tree=Await {param($t) @($t.browsers).Count -eq 0};Owner-Restored $tree;Require ((Identities $tree) -ceq $identities) 'Browser palette or cleanup replaced a terminal';Passed 'contextual-action-availability-and-browser-toolbar-entry'

    # Native metadata dialogs reuse the palette, settings theme and original
    # model IDs. Synthetic composition messages test guards, not physical IME.
    Request @('focus-tab',$initial.surface)|Out-Null
    $tree=Open-Metadata 'metadata:workspace-name';$panel=Metadata $tree
    Require ([OptionsFixture]::Text([long]$panel.input,$owned.Id) -ceq $workspaceName) 'Workspace editor lost its starting UTF-16 text'
    $renamed='이름 한 é 😀 & 변경';Metadata-Text $panel $renamed
    [OptionsFixture]::PostEnter([long]$panel.input,$owned.Id);[OptionsFixture]::PostEscape([long]$panel.input,$owned.Id)
    $tree=Tree;Metadata $tree|Out-Null;Require ((Workspace $tree $initial.workspace).name -ceq $workspaceName -and [OptionsFixture]::Text([long]$panel.input,$owned.Id) -ceq $renamed) 'Plain EDIT Enter/Escape applied, cancelled or changed raw input'
    Metadata-Click $panel 'cancel';$tree=Metadata-Closed;Require ((Workspace $tree $initial.workspace).name -ceq $workspaceName) 'Metadata Cancel changed the workspace'
    $tree=Open-Metadata 'metadata:workspace-name';$panel=Metadata $tree
    [OptionsFixture]::CompositionGuard([long]$panel.window,[long]$panel.input,$owned.Id,$true);Metadata-Text $panel $renamed
    [OptionsFixture]::PostEnter([long]$panel.input,$owned.Id);[OptionsFixture]::PostEscape([long]$panel.input,$owned.Id)
    $tree=Await {param($t) $t.metadata.open -and $t.metadata.composing};Metadata $tree|Out-Null
    Require ((Workspace $tree $initial.workspace).name -ceq $workspaceName -and [OptionsFixture]::Text([long]$panel.input,$owned.Id) -ceq $renamed) 'Composition guard changed the draft or model'
    [OptionsFixture]::CompositionGuard([long]$panel.window,[long]$panel.input,$owned.Id,$false)
    $tree=Await {param($t) $t.metadata.open -and -not $t.metadata.composing};[OptionsFixture]::PostEnter([long]$panel.apply,$owned.Id)
    $tree=Metadata-Closed;Require ((Workspace $tree $initial.workspace).name -ceq $renamed) 'Native Apply did not retain the exact Korean/NFD/emoji/ampersand name';Same-Identity $initial
    Passed 'metadata-owned-themed-name-Cancel-composition-guard-and-explicit-Apply-preserve-Unicode-identity'

    $tree=Open-Metadata 'metadata:workspace-name';$panel=Metadata $tree
    $losing='UI 한 😀 draft';$winner='외부 한글 & winner';Metadata-Text $panel $losing
    Request @('workspace','rename',$initial.workspace,$winner)|Out-Null;Metadata-Click $panel 'apply'
    $tree=Await {param($t) $t.metadata.open -and $t.metadata.error -match 'changed elsewhere'};Metadata $tree|Out-Null
    Require ((Workspace $tree $initial.workspace).name -ceq $winner -and [OptionsFixture]::Text([long]$panel.input,$owned.Id) -ceq $losing) 'Concurrent IPC rename was overwritten or its pending UI draft was lost'
    Metadata-Click $panel 'cancel';$tree=Metadata-Closed;Require ((Workspace $tree $initial.workspace).name -ceq $winner) 'Conflict Cancel changed the external winner'
    Passed 'metadata-concurrent-rename-conflict-retains-draft-and-modal-owner'

    $tree=Open-Metadata 'metadata:tab-name';$panel=Metadata $tree;$newTab='탭 한 😀 & 고정';Metadata-Text $panel $newTab;Metadata-Click $panel 'apply'
    $tree=Metadata-Closed;$tab=Surface $tree $initial.surface
    Require ($tab.title -ceq $newTab -and $tab.title_locked) 'Native tab rename did not preserve its exact title and manual title lock';Same-Identity $initial
    Passed 'metadata-tab-name-Apply-locks-title-without-changing-live-surface-identity'

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
    else{Remove-Item -LiteralPath $directory -Recurse -Force}
}
if($failure){throw $failure};if($cleanupErrors.Count){throw ($cleanupErrors -join '; ')}
Write-Output ('Command Palette passed: '+$checks.Count+' groups in '+[Math]::Round($clock.Elapsed.TotalSeconds,3)+'s; hidden guards only, no physical IME/focus acceptance')
