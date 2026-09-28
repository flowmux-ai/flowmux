# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden debug hosts and a unique state directory only. No desktop input.
param([string]$BuildDirectory = "$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",
    [ValidateSet('all','detached')][string]$Case='all')
if (-not $env:FLOWMUX_TEST_ARTIFACT_ROOT) { throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.' }
$ErrorActionPreference = 'Stop'
$OutputEncoding = [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$BuildDirectory = (Resolve-Path $BuildDirectory).Path
$cli = Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor = (& $cli doctor | ConvertFrom-Json)
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) { throw 'A working debug build is required; no window was launched.' }
Add-Type -Path (Join-Path $PSScriptRoot 'NativeInput.cs')
if ($Case -eq 'detached') {
    Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs'),(Join-Path $PSScriptRoot 'BrowserFixture.cs'),(Join-Path $PSScriptRoot 'EditorFixture.cs'),(Join-Path $PSScriptRoot 'FindFixture.cs')
    Add-Type -AssemblyName System.Drawing
    Add-Type -ReferencedAssemblies System.Drawing -Path (Join-Path $PSScriptRoot 'ChromeFixture.cs')
}
$directory = Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT ('state-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $directory -Force | Out-Null
$directory = (Resolve-Path $directory).Path
$stateDirectory = Join-Path $directory 'state'
$hosts = New-Object 'System.Collections.Generic.List[System.Diagnostics.Process]'
$evidence = [ordered]@{ started=(Get-Date).ToString('o'); mode='background'; checks=@(); hosts=@() }
function Start-Owned([string[]]$LaunchArgs = @()) {
    $priorBackground = $env:FLOWMUX_TEST_BACKGROUND
    $priorState = $env:FLOWMUX_TEST_STATE_DIR
    try {
        $env:FLOWMUX_TEST_BACKGROUND = '1'
        $env:FLOWMUX_TEST_STATE_DIR = $stateDirectory
        $options = @{ FilePath=(Join-Path $BuildDirectory 'flowmux.exe'); PassThru=$true }
        if ($LaunchArgs.Count -gt 0) { $options.ArgumentList = $LaunchArgs }
        $hostProcess = Start-Process @options
        $hosts.Add($hostProcess)
        return $hostProcess
    } finally {
        $env:FLOWMUX_TEST_BACKGROUND = $priorBackground
        $env:FLOWMUX_TEST_STATE_DIR = $priorState
    }
}
function Invoke-Flowmux([string[]]$Arguments) {
    if ($Case -eq 'detached') {
        $probe=[CliProbe]::Start($cli,(@('--pipe',$script:pipeName,'--json')+$Arguments),$directory,$directory)
        try {
            $out=$probe.StandardOutput.ReadToEndAsync();$err=$probe.StandardError.ReadToEndAsync()
            if (-not $probe.WaitForExit(5000)) {throw 'Owned state CLI exceeded five seconds'}
            if (-not $out.Wait(500) -or -not $err.Wait(500)) {throw 'Owned state CLI output did not close'}
            if ($probe.ExitCode -ne 0) {throw ('State CLI failed: '+($Arguments -join ' ')+' '+[CliProbe]::Output($err))}
            return ([CliProbe]::Output($out)|ConvertFrom-Json)
        } finally {if (-not $probe.HasExited) {$probe.Kill();[CliProbe]::WaitAfterKill($probe)};$probe.Dispose()}
    }
    $output = & $cli --pipe $script:pipeName --json @Arguments
    if ($LASTEXITCODE -ne 0) { throw "flowmuxctl failed: $Arguments" }
    return (($output -join "`n") | ConvertFrom-Json)
}
function Connect-Owned($Process) {
    $discovery = Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($Process.Id).json"
    $deadline = (Get-Date).AddSeconds($(if ($Case -eq 'detached') {5} else {30}))
    while (-not (Test-Path $discovery)) {
        if ($Process.HasExited -or (Get-Date) -gt $deadline) { throw 'Test host did not start' }
        Start-Sleep -Milliseconds 100
    }
    $record=Get-Content -Raw $discovery|ConvertFrom-Json
    if ($Case -eq 'detached' -and $record.pid -ne $Process.Id) {throw 'Detached discovery process owner differs'}
    $script:pipeName = $record.pipe
    if ($Case -eq 'detached' -and (Invoke-Flowmux @('identify')).pid -ne $Process.Id) {throw 'Detached pipe process owner differs'}
    do {
        $tree = Invoke-Flowmux @('tree')
        if (@($tree.surfaces | Where-Object { -not $_.ready }).Count -eq 0) {
            $window = [IntPtr]([long]$tree.window_handle)
            if (-not $tree.background_testing -or [NativeInput]::IsWindowVisible($window) -or [NativeInput]::GetForegroundWindow() -eq $window) {
                throw 'Background host became visible or foreground'
            }
            return $tree
        }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    throw 'Terminal restore/readiness timed out'
}
function Stop-Owned($Process) {
    Invoke-Flowmux @('quit') | Out-Null
    if (-not $Process.WaitForExit($(if ($Case -eq 'detached') {5000} else {10000})) -or $Process.ExitCode -ne 0) { throw 'Host did not close cleanly' }
}
function Write-Marker([string]$Surface, [string]$Marker) {
    Invoke-Flowmux @('focus-tab', $Surface) | Out-Null
    $pane = (Invoke-Flowmux @('identify')).pane
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes("Write-Host '$Marker' -ForegroundColor Green"))
    Invoke-Flowmux @('send-keys', $pane, "Invoke-Expression ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$encoded')))") | Out-Null
    Invoke-Flowmux @('send-key', 'Enter', '--pane', $pane) | Out-Null
    $deadline = (Get-Date).AddSeconds($(if ($Case -eq 'detached') {5} else {12}))
    do {
        $screen = Invoke-Flowmux @('read-screen', '--surface', $Surface)
        if ($screen.text.Replace("`n", '').Contains($Marker)) { return }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    throw "Marker was not rendered: $Marker"
}
function Load-State([string]$Path) { return ([IO.File]::ReadAllText($Path, [Text.Encoding]::UTF8) | ConvertFrom-Json) }
function Save-Json([string]$Path, $Value) { [IO.File]::WriteAllText($Path, ($Value | ConvertTo-Json -Depth 60), (New-Object Text.UTF8Encoding($false))) }
function State-Leaves($Node) {if ($Node.content) {$Node} else {State-Leaves $Node.first;State-Leaves $Node.second}}
function State-Topology($Tree) {
    return (@($Tree.workspaces|ForEach-Object {$workspace=$_.id;State-Leaves $_.root|ForEach-Object {$pane=$_.id;$_.content.surfaces|ForEach-Object {$workspace+':'+$pane+':'+$_.id}}}|Sort-Object)-join ';')
}
function Detached-Tree {
    $tree=Invoke-Flowmux @('tree');$evidence.lastTree=$tree
    foreach ($handle in @($tree.window_handle)+@($tree.detached_windows.window_handle)) {
        if ($handle) {[ChromeFixture]::Placement([long]$handle,$script:detachedProcess.Id)|Out-Null}
    }
    if (-not $tree.background_testing) {throw 'Detached state host is not isolated and hidden'}
    return $tree
}
function Detached-Ready {
    $deadline=(Get-Date).AddSeconds(5)
    do {
        $tree=Detached-Tree
        if (@($tree.surfaces|Where-Object {-not $_.ready}).Count -eq 0 -and @($tree.editors|Where-Object {-not $_.ready}).Count -eq 0 -and @($tree.browsers|Where-Object {$_.loading}).Count -eq 0) {return $tree}
        if ((Get-Date) -gt $deadline) {throw 'Mixed detached state did not become ready within five seconds'}
        Start-Sleep -Milliseconds 25
    } while ($true)
}
function Detached-Start([string[]]$LaunchArgs) {
    $script:detachedProcess=[CliProbe]::Start((Join-Path $BuildDirectory 'flowmux.exe'),$LaunchArgs,$directory,$directory)
    $hosts.Add($script:detachedProcess)
    $script:detachedOutput[$script:detachedProcess.Id]=@{out=$script:detachedProcess.StandardOutput.ReadToEndAsync();err=$script:detachedProcess.StandardError.ReadToEndAsync()}
    Connect-Owned $script:detachedProcess|Out-Null
    return Detached-Ready
}
function Detached-EditorText([string]$Surface) {
    $response=Invoke-Flowmux @('editor','command',$Surface,'read')
    $read=if ($response.psobject.Properties.Name -contains 'result') {$response.result} else {$response}
    if ($read.document_focused -ne $false -or $read.dirty -or $read.content_truncated -or -not [string]::Equals($read.content,[EditorFixture]::Original,[StringComparison]::Ordinal)) {throw 'Detached editor did not restore its exact clean Korean document'}
}
function Detached-BrowserText([string]$Surface) {
    Invoke-Flowmux @('focus-tab',$Surface)|Out-Null;$pane=(Invoke-Flowmux @('identify')).pane
    $value=(Invoke-Flowmux @('browser','eval',$pane,'({text:document.querySelector("#label").textContent,saved:localStorage.getItem("detached-state"),focused:document.hasFocus()})')).result
    if (-not [string]::Equals($value.text,'첫째 한글 한 é 😀',[StringComparison]::Ordinal) -or -not [string]::Equals($value.saved,'복원 한 é 😀',[StringComparison]::Ordinal) -or $value.focused -ne $false) {throw 'Detached browser URL/profile Korean DOM or hidden focus differs'}
}
function Detached-Placements($Tree,$Expected) {
    $keys=@($Expected.psobject.Properties.Name)
    if (@($Tree.detached_windows).Count -ne $keys.Count) {throw 'Saved detached frame count changed'}
    foreach ($surface in $keys) {
        $frames=@($Tree.detached_windows|Where-Object {$_.surface -eq $surface})
        if ($frames.Count -ne 1) {throw 'Saved detached surface identity is missing or duplicated'}
        $frame=$frames[0];$native=[ChromeFixture]::Placement([long]$frame.window_handle,$script:detachedProcess.Id)
        foreach ($key in @('left','top','width','height')) {
            if ($null -eq $frame.placement.$key -or $frame.placement.$key -ne $Expected.$surface.$key -or $native.$key -ne $Expected.$surface.$key) {throw ('Restored actual detached placement differs: '+$surface+' '+$key)}
        }
        if ($frame.placement.maximized -ne $Expected.$surface.maximized -or $native.Maximized) {throw 'Hidden restore lost maximized intent or actually maximized a native window'}
        if ($frame.sidebar.width_dip -ne $Expected.$surface.sidebar_width_dip -or $frame.placement.sidebar_width_dip -ne $Expected.$surface.sidebar_width_dip -or $frame.area.x -ne ($frame.sidebar.gutter.x+$frame.sidebar.gutter.width)) {throw 'Restored independent sidebar width or live content offset differs'}
        $tabs=@($Tree.workspaces|Where-Object {$_.id -eq $frame.workspace}|ForEach-Object {State-Leaves $_.root}|ForEach-Object {$_.content.surfaces})
        if ($tabs.Count -ne 1 -or $tabs[0].id -ne $surface) {throw 'Restored detached workspace is not its original single surface'}
        $views=@(@($Tree.surfaces)+@($Tree.browsers)+@($Tree.editors)|Where-Object {$_.id -eq $surface})
        if ($views.Count -ne 1 -or $views[0].holder.parent -ne $frame.window_handle -or $views[0].holder.root -ne $frame.window_handle -or $frame.native_visible -ne $false -or $views[0].holder.native_visible -ne $false) {throw 'Restored detached holder is not under its hidden saved frame'}
        foreach ($key in @('x','y','width','height')) {if ($views[0].holder.bounds.$key -ne $frame.area.$key) {throw ('Native retained surface overlaps its restored sidebar: '+$key)}}
    }
}
function State-Name($Tree,[string]$Workspace,[string]$Expected,[bool]$Locked) {
    $matches=@($Tree.workspaces|Where-Object {$_.id -eq $Workspace})
    if ($matches.Count -ne 1 -or $matches[0].name_locked -ne $Locked -or -not [string]::Equals($matches[0].name,$Expected,[StringComparison]::Ordinal)) {throw ('Saved workspace name/mode differs: '+$Workspace)}
}
function Detached-Name($Tree,[string]$Surface,[string]$Expected,[bool]$Locked) {
    $frames=@($Tree.detached_windows|Where-Object {$_.surface -eq $Surface})
    if ($frames.Count -ne 1) {throw 'Named detached surface has no unique native frame'}
    State-Name $Tree $frames[0].workspace $Expected $Locked
    $rows=@([ChromeFixture]::Read([long]$frames[0].window_handle,$script:detachedProcess.Id)|Where-Object {$_.Handle -eq [long]$frames[0].sidebar.workspace_row})
    if ($rows.Count -ne 1 -or -not $rows[0].Shown -or -not [string]::Equals($rows[0].Text,$Expected.Replace('&','&&'),[StringComparison]::Ordinal)) {throw 'Restored native detached row lost its Unicode name or ampersand escaping'}
}
function Detached-State {
    $tree=Detached-Start @('--new-window','--cwd',$directory);$anchor=Invoke-Flowmux @('identify')
    Invoke-Flowmux @('new-tab')|Out-Null;$terminal=Invoke-Flowmux @('identify');Detached-Ready|Out-Null
    $marker='DETACHED-HISTORY-한글-한-é-😀';Write-Marker $terminal.surface $marker
    $browser=(Invoke-Flowmux @('browser','open',($script:stateBrowser.Origin+'/one'),'--pane',$anchor.pane)).browser_pane_opened
    Detached-Ready|Out-Null
    Invoke-Flowmux @('browser','eval',$browser.pane,'localStorage.setItem("detached-state","복원 한 é 😀");true')|Out-Null
    $file=$script:stateEditor.Write('복원 문서 한 é 😀.txt',[EditorFixture]::Original,$true,$false)
    $editor=(Invoke-Flowmux @('editor','open',$file,'--pane',$anchor.pane,'--root',$script:stateEditor.Root)).editor_opened
    Detached-Ready|Out-Null;Detached-EditorText $editor.surface
    $ids=@($terminal.surface,$browser.surface,$editor.surface)
    for ($index=0;$index -lt $ids.Count;$index++) {
        Invoke-Flowmux @('detach-tab',$ids[$index])|Out-Null
        $frame=@((Detached-Tree).detached_windows|Where-Object {$_.surface -eq $ids[$index]})[0]
        $dip=200+60*$index;$dpi=[ChromeFixture]::GetDpiForWindow([IntPtr][long]$frame.window_handle)
        $x=[int]$frame.sidebar.gutter.x+1;$target=[int][Math]::Round($dip*[Math]::Max(96,$dpi)/96)+1
        [ChromeFixture]::Pointer([long]$frame.window_handle,$script:detachedProcess.Id,'down',$x,120)
        [ChromeFixture]::Pointer([long]$frame.window_handle,$script:detachedProcess.Id,'move',$target,120)
        [ChromeFixture]::Pointer([long]$frame.window_handle,$script:detachedProcess.Id,'up',$target,120)
        $deadline=(Get-Date).AddSeconds(5)
        do {
            $frame=@((Detached-Tree).detached_windows|Where-Object {$_.surface -eq $ids[$index]})[0]
            if ($frame.sidebar.width_dip -eq $dip -and -not $frame.sidebar.dragging) {break}
            if ((Get-Date) -gt $deadline) {throw 'Detached sidebar drag did not settle within five seconds'}
            Start-Sleep -Milliseconds 20
        } while ($true)
        [ChromeFixture]::Position([long]$frame.window_handle,$script:detachedProcess.Id,(60+30*$index),(700+40*$index),(460+30*$index))
    }
    $nameTree=Detached-Tree;$terminalWorkspace=@($nameTree.detached_windows|Where-Object {$_.surface -eq $terminal.surface})[0].workspace;$browserWorkspace=@($nameTree.detached_windows|Where-Object {$_.surface -eq $browser.surface})[0].workspace
    $customName='저장 고정 한 😀 & 작업공간';$browserName='첫째 한글 한 é 😀'
    Invoke-Flowmux @('workspace','rename',$terminalWorkspace,$customName)|Out-Null
    Invoke-Flowmux @('workspace','rename',$browserWorkspace,'   ')|Out-Null
    Invoke-Flowmux @('focus-tab',$browser.surface)|Out-Null
    $before=Detached-Tree;$topology=State-Topology $before;$oldPid=@($before.surfaces|Where-Object {$_.id -eq $terminal.surface})[0].pid
    $saved=Invoke-Flowmux @('save-state');$snapshot=Load-State $saved.path
    if (@($snapshot.detached_windows.psobject.Properties).Count -ne 3 -or $snapshot.main_closed -or $snapshot.detached_focus -ne $browser.surface) {throw 'Mixed state omitted detached placement/focus or closed its main window'}
    Detached-Placements $before $snapshot.detached_windows
    Detached-Name $before $terminal.surface $customName $true;Detached-Name $before $browser.surface $browserName $false
    State-Name $snapshot $terminalWorkspace $customName $true;State-Name $snapshot $browserWorkspace $browserName $false
    if (-not $snapshot.screens.($terminal.surface).data.Contains($marker)) {throw 'Detached Korean history was not saved'}
    Stop-Owned $script:detachedProcess
    # State-input recovery probe only: no fixture displays or maximizes a window.
    $snapshot=Load-State $saved.path
    $snapshot.detached_windows.($browser.surface).left=999999;$snapshot.detached_windows.($browser.surface).top=999999
    $snapshot.detached_windows.($terminal.surface).maximized=$true
    # Missing name_locked is the actual legacy state shape, not an explicit mode.
    $legacy=@($snapshot.workspaces|Where-Object {$_.id -eq $anchor.workspace});if ($legacy.Count -ne 1) {throw 'Legacy migration probe requires the attached anchor workspace'}
    $legacyName='이전 저장 한 😀 & 이름';$legacy[0].name=$legacyName;$legacy[0].psobject.Properties.Remove('name_locked')
    Save-Json $saved.path $snapshot
    $restored=Detached-Start @('--restore-window',$saved.window)
    if ((State-Topology $restored) -cne $topology -or $restored.main_closed) {throw 'Mixed detached restart changed workspace/pane/surface identities'}
    $corrected=@($restored.detached_windows|Where-Object {$_.surface -eq $browser.surface})[0]
    if (-not $corrected -or -not [ChromeFixture]::InWorkArea([long]$corrected.window_handle,$script:detachedProcess.Id) -or $corrected.placement.left -eq 999999 -or $corrected.placement.top -eq 999999) {throw 'Offscreen browser placement was not clamped into the native work area'}
    $snapshot.detached_windows.($browser.surface)=$corrected.placement
    Detached-Placements $restored $snapshot.detached_windows
    $newPid=@($restored.surfaces|Where-Object {$_.id -eq $terminal.surface})[0].pid
    if (-not $newPid -or $newPid -eq $oldPid -or (Invoke-Flowmux @('identify')).surface -ne $browser.surface) {throw 'Detached restart did not restore focused browser and a fresh terminal process'}
    $screen=Invoke-Flowmux @('read-screen','--surface',$terminal.surface,'--recent')
    if (-not $screen.text.Replace("`r",'').Replace("`n",'').Contains($marker)) {throw 'Detached restart lost visible Korean terminal history'}
    Detached-BrowserText $browser.surface;Detached-EditorText $editor.surface
    Detached-Name $restored $terminal.surface $customName $true;Detached-Name $restored $browser.surface $browserName $false
    State-Name $restored $anchor.workspace $legacyName $true
    $browserRenamed='복원 뒤 자동 한 😀 & 브라우저'
    Invoke-Flowmux @('rename-tab',$browser.surface,$browserRenamed)|Out-Null
    Invoke-Flowmux @('rename-tab',$terminal.surface,'복원 뒤 탭 변경 한 😀 &')|Out-Null
    $restored=Detached-Tree;Detached-Name $restored $browser.surface $browserRenamed $false;Detached-Name $restored $terminal.surface $customName $true
    State-Name $restored $anchor.workspace $legacyName $true
    $correctedSave=Invoke-Flowmux @('save-state');$correctedSnapshot=Load-State $correctedSave.path
    Detached-Placements $restored $correctedSnapshot.detached_windows
    State-Name $correctedSnapshot $terminalWorkspace $customName $true;State-Name $correctedSnapshot $browserWorkspace $browserRenamed $false;State-Name $correctedSnapshot $anchor.workspace $legacyName $true
    $evidence.checks+='Workspace naming saves and restores custom/automatic modes, follows browser tab rename in the native sidebar, retains custom terminal name, and migrates missing legacy mode to locked'
    if (-not $correctedSnapshot.detached_windows.($terminal.surface).maximized) {throw 'Hidden save lost the restored maximized intent'}
    $evidence.checks+='Mixed detached restart preserves identities, actual placements, focus, Korean history with fresh PTY, browser profile/editor text; offscreen browser is clamped and maximized intent is saved without native maximization'
    [FindFixture]::PostClose([long]$restored.window_handle,$script:detachedProcess.Id)
    $deadline=(Get-Date).AddSeconds(5)
    do {
        $broker=Detached-Tree
        if ($broker.main_closed -and -not $broker.state.saving -and -not $broker.editor_synchronizing) {break}
        if ((Get-Date) -gt $deadline) {throw 'Main close did not retain detached windows as a broker within five seconds'}
        Start-Sleep -Milliseconds 25
    } while ($true)
    if (@($broker.surfaces).Count -ne 1 -or @($broker.browsers).Count -ne 1 -or @($broker.editors).Count -ne 1 -or @($broker.surfaces|Where-Object {$_.id -eq $anchor.surface}).Count) {throw 'Main close did not remove only the attached anchor terminal'}
    $brokerSaved=Invoke-Flowmux @('save-state');$brokerSnapshot=Load-State $brokerSaved.path
    if (-not $brokerSnapshot.main_closed -or @($brokerSnapshot.detached_windows.psobject.Properties).Count -ne 3) {throw 'Broker-only state omitted its closed-main flag or detached windows'}
    Stop-Owned $script:detachedProcess
    $brokerRestored=Detached-Start @('--restore-window',$saved.window)
    if (-not $brokerRestored.main_closed -or (State-Topology $brokerRestored) -cne (State-Topology $broker)) {throw 'Broker-only restart recreated a main workspace or lost surface identities'}
    Detached-Placements $brokerRestored $brokerSnapshot.detached_windows
    Detached-BrowserText $browser.surface;Detached-EditorText $editor.surface
    Detached-Name $brokerRestored $terminal.surface $customName $true;Detached-Name $brokerRestored $browser.surface $browserRenamed $false
    $evidence.checks+='Closed-main broker restart restores only the three detached windows and their native placements'
    Invoke-Flowmux @('new-workspace','--cwd',$directory)|Out-Null;$destination=Invoke-Flowmux @('identify');Detached-Ready|Out-Null
    foreach ($surface in $ids) {Invoke-Flowmux @('move-tab',$surface,'--to-pane',$destination.pane)|Out-Null}
    $joined=Detached-Tree
    if ($joined.main_closed -or @($joined.detached_windows).Count -ne 0) {throw 'Reattachment did not reopen main or remove detached frames'}
    foreach ($surface in $ids) {
        $view=@(@($joined.surfaces)+@($joined.browsers)+@($joined.editors)|Where-Object {$_.id -eq $surface})
        if ($view.Count -ne 1 -or $view[0].holder.root -ne $joined.window_handle) {throw 'Reattachment lost a saved surface or its main-window parent'}
    }
    $joinedSaved=Invoke-Flowmux @('save-state');$joinedSnapshot=Load-State $joinedSaved.path
    if ($joinedSnapshot.main_closed -or $joinedSnapshot.detached_focus -or ($joinedSnapshot.detached_windows -and @($joinedSnapshot.detached_windows.psobject.Properties).Count -ne 0)) {throw 'Reattachment persisted stale detached map/focus/main-closed state'}
    Stop-Owned $script:detachedProcess
    $evidence.checks+='Reattachment preserves original surface IDs and removes detached map/focus from the saved main-window state'
}
try {
    if ($Case -eq 'detached') {
        $script:detachedOutput=@{};$script:stateBrowser=New-Object BrowserFixture;$script:stateEditor=New-Object EditorFixture($directory)
        Detached-State
    } else {
    $first = Start-Owned @('--cwd', ('"' + $directory + '"'))
    Connect-Owned $first | Out-Null
    Invoke-Flowmux @('new-tab') | Out-Null
    Invoke-Flowmux @('split', 'vertical') | Out-Null
    Invoke-Flowmux @('new-workspace', '--cwd', $directory) | Out-Null
    $before = Connect-Owned $first
    $markers = @{}
    $korean = ([string][char]0xD55C) + [char]0xAE00 + [char]0xAE30 + [char]0xB85D
    foreach ($surface in $before.surfaces) {
        $markers[$surface.id] = "HISTORY-$korean-$($surface.id.Substring(0, 8))"
        Write-Marker $surface.id $markers[$surface.id]
    }
    $before = Invoke-Flowmux @('tree')
    $saved = Invoke-Flowmux @('save-state')
    $path = $saved.path
    $windowId = $saved.window
    $snapshot = Load-State $path
    foreach ($surface in $before.surfaces) {
        $data = $snapshot.screens.($surface.id).data
        if (-not $data.Contains($markers[$surface.id]) -or -not $data.Contains(([string][char]27) + '[92m')) { throw 'Styled Korean history was not saved' }
    }
    $evidence.checks += 'Four terminal histories, including inactive tabs/workspaces, saved with Korean and green SGR'
    $hash = (Get-FileHash $path -Algorithm SHA256).Hash
    $locked = Start-Owned @('--restore-window', $windowId)
    if (-not $locked.WaitForExit(10000) -or $locked.ExitCode -eq 0) { throw 'A second process claimed a live window state' }
    $second = Start-Owned
    $otherTree = Connect-Owned $second
    if ($otherTree.state.window -eq $windowId) { throw 'Default launch reused a live window identity' }
    Stop-Owned $second
    if ((Get-FileHash $path -Algorithm SHA256).Hash -ne $hash) { throw 'Second window overwrote first state' }
    $script:pipeName = (Get-Content -Raw (Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($first.Id).json") | ConvertFrom-Json).pipe
    Stop-Owned $first
    $evidence.checks += 'Live-window lease rejected explicit duplicate restore; another window saved independently'

    # A saved display that looks like a command must never execute on restore.
    $snapshot = Load-State $path
    $sentinel = Join-Path $directory 'must-not-execute.txt'
    $one = $before.surfaces[0].id
    $snapshot.screens.$one.data += "`r`nSet-Content -LiteralPath '$sentinel' -Value executed`r`n" + [char]27 + '[5n'
    Save-Json $path $snapshot
    $restored = Start-Owned @('--restore-window', $windowId)
    $after = Connect-Owned $restored
    if (($before.workspaces | ConvertTo-Json -Depth 60 -Compress) -ne ($after.workspaces | ConvertTo-Json -Depth 60 -Compress) -or $before.active_workspace -ne $after.active_workspace) {
        throw 'Restored layout, focus, titles or working directories differ'
    }
    foreach ($surface in $before.surfaces) {
        $new = $after.surfaces | Where-Object { $_.id -eq $surface.id }
        if (-not $new -or $new.pid -eq $surface.pid) { throw 'Restored surface did not create a fresh shell' }
    }
    $savedAgain = Invoke-Flowmux @('save-state')
    $roundtrip = Load-State $savedAgain.path
    foreach ($surface in $before.surfaces) {
        $data = $roundtrip.screens.($surface.id).data
        if (-not $data.Contains($markers[$surface.id]) -or -not $data.Contains(([string][char]27) + '[92m')) { throw 'Styled history was lost after fresh ConPTY startup' }
    }
    if (Test-Path $sentinel) { throw 'Historical command was executed' }
    $evidence.checks += 'Clean restart kept workspace/pane/surface identity, layout, focus, cwd, Korean and SGR with fresh process IDs'
    $evidence.checks += 'Historical command text and VT status query replayed to display only; sentinel was not created'
    $evidence.hosts += @{ first=$first.Id; restored=$restored.Id; originalShells=@($before.surfaces.pid); restoredShells=@($after.surfaces.pid); window=$windowId }

    # Simulate an atomic replacement failure using a handle that denies deletion.
    $hash = (Get-FileHash $path -Algorithm SHA256).Hash
    $guard = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        $previousErrorPreference = $ErrorActionPreference
        try {
            $ErrorActionPreference = 'Continue'
            $raw = & $cli --pipe $script:pipeName --json quit 2>&1
            $failedExit = $LASTEXITCODE
        } finally { $ErrorActionPreference = $previousErrorPreference }
        if ($failedExit -eq 0 -or $restored.HasExited) { throw 'Failed save closed the window or reported success' }
    } finally { $guard.Dispose() }
    if ((Get-FileHash $path -Algorithm SHA256).Hash -ne $hash) { throw 'Failed replacement damaged the previous checkpoint' }
    $evidence.checks += 'Denied atomic replace retained old checkpoint and kept window/processes alive after quit failed'
    Invoke-Flowmux @('save-state') | Out-Null
    $restored.Kill(); $restored.WaitForExit()
    $crashRestored = Start-Owned @('--restore-window', $windowId)
    $crashTree = Connect-Owned $crashRestored
    if ($crashTree.surfaces.Count -ne 4) { throw 'Crash recovery lost terminals' }
    $automaticMarker = "AUTO-$korean-$([guid]::NewGuid().ToString().Substring(0, 8))"
    Write-Marker $one $automaticMarker
    $deadline = (Get-Date).AddSeconds(40)
    do {
        $automatic = Load-State $path
        if ($automatic.screens.$one.data.Contains($automaticMarker)) { break }
        if ((Get-Date) -gt $deadline) { throw 'Periodic checkpoint did not persist new output' }
        Start-Sleep -Milliseconds 250
    } while ($true)
    $evidence.checks += 'Thirty-second background checkpoint persisted later Korean output without an explicit save'
    Stop-Owned $crashRestored
    $evidence.checks += 'Forced termination released the OS lease; explicit restart recovered the last checkpoint'
    $automaticRestore = Start-Owned
    $automaticTree = Connect-Owned $automaticRestore
    if ($automaticTree.state.window -ne $windowId) { throw 'Default launch did not select the latest closed window' }
    $hash = (Get-FileHash $path -Algorithm SHA256).Hash
    $guard = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        Invoke-Flowmux @('quit', '--discard-state') | Out-Null
        if (-not $automaticRestore.WaitForExit(10000)) { throw 'Explicit close without saving did not close' }
    } finally { $guard.Dispose() }
    if ((Get-FileHash $path -Algorithm SHA256).Hash -ne $hash) { throw 'Discard-state modified the completed checkpoint' }
    $temporary = Start-Owned @('--temporary')
    $temporaryTree = Connect-Owned $temporary
    if ($temporaryTree.state.window -or $temporaryTree.surfaces.Count -ne 1) { throw 'Temporary host restored persistent state' }
    Stop-Owned $temporary
    if ((Get-FileHash $path -Algorithm SHA256).Hash -ne $hash) { throw 'Temporary host modified persistent state' }
    $evidence.checks += 'Default launch restored latest closed window; explicit discard and temporary mode preserved its checkpoint'
    }
    $evidence.finished = (Get-Date).ToString('o')
} catch {
    $evidence.status='failed';$evidence.error=$_.Exception.Message
    if ($Case -eq 'detached' -and $script:detachedOutput) {
        $evidence.hostOutput=@($hosts|ForEach-Object {$output=$script:detachedOutput[$_.Id];@{pid=$_.Id;stdout=[CliProbe]::Output($output.out);stderr=[CliProbe]::Output($output.err)}})
    }
    Save-Json (Join-Path $directory 'native-state-background.json') $evidence
    throw
} finally {
    foreach ($owned in $hosts) { if (-not $owned.HasExited) { $owned.Kill(); if ($Case -eq 'detached') {[CliProbe]::WaitAfterKill($owned)} else {$owned.WaitForExit()} } }
    if ($Case -eq 'detached') {if ($script:stateBrowser) {$script:stateBrowser.Dispose()};if ($script:stateEditor) {$script:stateEditor.Dispose()}}
}
[ordered]@{status='passed';case=$Case;checks=$evidence.checks.Count}|ConvertTo-Json -Compress
