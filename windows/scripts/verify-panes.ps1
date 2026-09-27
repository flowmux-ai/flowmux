# SPDX-License-Identifier: GPL-3.0-or-later
# Always hidden, isolated state. No desktop input, native clicks or IME automation.
param([string]$BuildDirectory = "$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference = 'Stop'
$OutputEncoding = [Console]::OutputEncoding = New-Object Text.UTF8Encoding($false)
$BuildDirectory = (Resolve-Path $BuildDirectory).Path
$cli = Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor = (& $cli doctor | ConvertFrom-Json)
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) { throw 'A working debug build is required; no window was launched.' }
Add-Type -Path (Join-Path $PSScriptRoot 'NativeInput.cs')
$directory = Join-Path $PSScriptRoot ('..\dist\evidence\panes-' + [guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory) | Out-Null
$directory = (Resolve-Path $directory).Path
$hosts = New-Object 'System.Collections.Generic.List[System.Diagnostics.Process]'
$evidence = [ordered]@{ started=(Get-Date).ToString('o'); mode='background'; checks=@() }
function Invoke-Flowmux([string[]]$Arguments) {
    $value = & $cli --pipe $script:pipeName --json @Arguments
    if ($LASTEXITCODE -ne 0) { throw "flowmuxctl failed: $Arguments" }
    return (($value -join "`n") | ConvertFrom-Json)
}
function Start-Owned([string[]]$LaunchArgs) {
    $oldBackground=$env:FLOWMUX_TEST_BACKGROUND; $oldState=$env:FLOWMUX_TEST_STATE_DIR
    try {
        $env:FLOWMUX_TEST_BACKGROUND='1'; $env:FLOWMUX_TEST_STATE_DIR=Join-Path $directory 'state'
        $owned=Start-Process -FilePath (Join-Path $BuildDirectory 'flowmux.exe') -ArgumentList $LaunchArgs -PassThru
        $hosts.Add($owned)
    } finally { $env:FLOWMUX_TEST_BACKGROUND=$oldBackground; $env:FLOWMUX_TEST_STATE_DIR=$oldState }
    $discovery=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($owned.Id).json"
    $deadline=(Get-Date).AddSeconds(25)
    while (-not (Test-Path $discovery)) {
        if ($owned.HasExited -or (Get-Date) -gt $deadline) { throw 'Hidden host failed to start' }
        Start-Sleep -Milliseconds 100
    }
    $script:pipeName=(Get-Content -Raw $discovery | ConvertFrom-Json).pipe
    Wait-Tree { param($t) @($t.surfaces | Where-Object { -not $_.ready -or -not $_.cwd_reported }).Count -eq 0 } | Out-Null
    return $owned
}
function Wait-Tree([scriptblock]$Condition) {
    $deadline=(Get-Date).AddSeconds(15)
    do {
        $tree=Invoke-Flowmux @('tree')
        $window=[IntPtr]([long]$tree.window_handle)
        if (-not $tree.background_testing -or [NativeInput]::IsWindowVisible($window) -or [NativeInput]::GetForegroundWindow() -eq $window) { throw 'Test host became visible or foreground' }
        if (& $Condition $tree) { return $tree }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    $tree | ConvertTo-Json -Depth 50 | Set-Content -Encoding UTF8 (Join-Path $directory 'failed-tree.json')
    throw 'Pane state did not converge'
}
function Send-Script([string]$Pane, [string]$Source) {
    $encoded=[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Source))
    Invoke-Flowmux @('send-keys',$Pane,"Invoke-Expression ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$encoded')))") | Out-Null
    Invoke-Flowmux @('send-key','Enter','--pane',$Pane) | Out-Null
}
function Wait-File([string]$Path) {
    $deadline=(Get-Date).AddSeconds(12)
    while (-not (Test-Path $Path)) {
        if ((Get-Date) -gt $deadline) { throw "Child file did not arrive: $Path" }
        Start-Sleep -Milliseconds 50
    }
    return (Get-Content -Raw $Path | ConvertFrom-Json)
}
function Probe-Size($Identity) {
    $file=Join-Path $directory ([guid]::NewGuid().ToString()+'.json')
    Send-Script $Identity.pane ('@{{ cols=[Console]::WindowWidth; rows=[Console]::WindowHeight; pid=$PID }} | ConvertTo-Json | Set-Content -Encoding UTF8 ''{0}''' -f $file)
    $size=Wait-File $file
    $surface=(Invoke-Flowmux @('tree')).surfaces | Where-Object { $_.id -eq $Identity.surface }
    if ($size.cols -ne $surface.cols -or $size.rows -ne $surface.rows -or $size.pid -ne $surface.pid) { throw "ConPTY and xterm dimensions differ: $($size | ConvertTo-Json -Compress) / $($surface | ConvertTo-Json -Compress)" }
    return $size
}
function Rect-Of($Tree,[string]$Pane) {
    foreach ($entry in $Tree.layout.panes) { if ($entry[0] -eq $Pane) { return $entry[1] } }
    throw 'Pane missing from rendered layout'
}
function Reject([string[]]$Arguments) {
    $old=$ErrorActionPreference
    try { $ErrorActionPreference='Continue'; $result=& $cli --pipe $script:pipeName --json @Arguments 2>&1; $status=$LASTEXITCODE }
    finally { $ErrorActionPreference=$old }
    if ($status -eq 0) { throw "Expected rejection: $Arguments" }
}
function Wait-Find([string]$Surface,[string]$Query) {
    $deadline=(Get-Date).AddSeconds(12)
    do {
        $result=Invoke-Flowmux @('find',$Query,'--match-case','--surface',$Surface)
        if ($result.result.found -and $result.result.selection -ceq $Query) { return }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    throw "Hidden output missing: $Query"
}
function Check-Focus([string]$Source,[string]$Direction,[string]$Expected) {
    $before=(Invoke-Flowmux @('identify')).pane
    $result=Invoke-Flowmux @('focus-direction',$Direction,'--pane',$Source)
    if ($Expected) {
        if (-not $result.focused -or $result.pane -ne $Expected -or (Invoke-Flowmux @('identify')).pane -ne $Expected) { throw 'Directional focus went to the wrong pane' }
    } elseif ($result.focused -or (Invoke-Flowmux @('identify')).pane -ne $before) { throw 'Missing neighbor changed focus' }
}
try {
    $owned=Start-Owned @('--cwd',('"'+$directory+'"'))
    $left=Invoke-Flowmux @('identify')
    if ((Invoke-Flowmux @('toggle-pane-zoom')).zoomed_pane) { throw 'Single pane was zoomed' }
    Invoke-Flowmux @('split','vertical') | Out-Null
    $top=Invoke-Flowmux @('identify')
    Invoke-Flowmux @('split','horizontal') | Out-Null
    $bottom=Invoke-Flowmux @('identify')
    $tree=Wait-Tree { param($t) @($t.surfaces | Where-Object { -not $_.cwd_reported }).Count -eq 0 }
    $originalPids=@($tree.surfaces | Sort-Object id | ForEach-Object { "$($_.id):$($_.pid)" }) -join ','
    $root=$tree.workspaces[0].root.id
    $inner=$tree.workspaces[0].root.second.id
    Invoke-Flowmux @('resize-pane',$root,'--ratio','0.6') | Out-Null
    Invoke-Flowmux @('resize-pane',$bottom.pane,'--ratio','0.35') | Out-Null
    $tree=Wait-Tree { param($t) $a=$t.surfaces | Where-Object { $_.id -eq $left.surface }; $b=$t.surfaces | Where-Object { $_.id -eq $top.surface }; $c=$t.surfaces | Where-Object { $_.id -eq $bottom.surface }; $a.cols -gt $b.cols -and $b.rows -lt $c.rows }
    $beforeLayout=$tree.layout | ConvertTo-Json -Depth 20 -Compress
    $sizes=@(Probe-Size $left; Probe-Size $top; Probe-Size $bottom)
    foreach ($identity in @($left,$top,$bottom)) {
        $r=Rect-Of $tree $identity.pane
        $s=$tree.surfaces | Where-Object { $_.id -eq $identity.surface }
        if ($s.bounds.x -ne $r.x -or $s.bounds.width -ne $r.width -or ($s.bounds.y+$s.bounds.height) -ne ($r.y+$r.height)) { throw 'Native WebView bounds do not match the pane geometry' }
    }
    $evidence.checks+=@{ name='nested_ratios_update_native_webview_and_real_conpty_dimensions'; sizes=$sizes; layout=$tree.layout }
    $before=$tree.workspaces | ConvertTo-Json -Depth 50 -Compress
    foreach ($ratio in @('0','1','-1','2')) { Reject @('resize-pane',$root,'--ratio',$ratio) }
    Reject @('resize-pane',([guid]::NewGuid().ToString()),'--ratio','0.5')
    if (((Invoke-Flowmux @('tree')).workspaces | ConvertTo-Json -Depth 50 -Compress) -ne $before) { throw 'Invalid resize mutated state' }
    if ([math]::Abs((Invoke-Flowmux @('resize-pane',$root,'--ratio','0.001')).ratio - 0.05) -gt 0.0001) { throw 'Lower clamp differs' }
    if ([math]::Abs((Invoke-Flowmux @('resize-pane',$root,'--ratio','0.999')).ratio - 0.95) -gt 0.0001) { throw 'Upper clamp differs' }
    Invoke-Flowmux @('resize-pane',$root,'--ratio','0.6') | Out-Null
    $evidence.checks+=@{ name='invalid_ratios_and_ids_do_not_mutate_state_and_extremes_clamp_to_five_percent' }
    Check-Focus $top.pane 'down' $bottom.pane
    Check-Focus $bottom.pane 'up' $top.pane
    Check-Focus $bottom.pane 'left' $left.pane
    Check-Focus $top.pane 'right' ''
    $evidence.checks+=@{ name='directional_focus_stays_in_workspace_and_missing_neighbors_preserve_focus' }
    Invoke-Flowmux @('toggle-pane-zoom',$top.pane) | Out-Null
    $zoom=Wait-Tree { param($t) $s=$t.surfaces | Where-Object { $_.id -eq $top.surface }; $t.zoomed_pane -eq $top.pane -and $s.cols -gt $sizes[1].cols -and $s.rows -gt $sizes[1].rows }
    if (@($zoom.surfaces | Where-Object visible).Count -ne 1 -or $zoom.layout.dividers.Count -ne 0) { throw 'Maximize did not hide siblings/dividers' }
    if ([NativeInput]::ButtonTitles([IntPtr]([long]$zoom.window_handle)) -notcontains 'Restore pane') { throw 'Native restore control missing' }
    $zoomSize=Probe-Size $top
    $marker='HIDDEN-'+([string][char]0xD55C)+[char]0xAE00+'-'+[guid]::NewGuid().ToString().Substring(0,8)
    Send-Script $bottom.pane ("Write-Output '"+$marker+"'")
    Wait-Find $bottom.surface $marker
    if ((Invoke-Flowmux @('identify')).surface -ne $top.surface) { throw 'Hidden output/find changed focus' }
    Invoke-Flowmux @('toggle-pane-zoom',$top.pane) | Out-Null
    $restored=Wait-Tree { param($t) $s=$t.surfaces | Where-Object { $_.id -eq $top.surface }; -not $t.zoomed_pane -and $s.cols -eq $sizes[1].cols -and $s.rows -eq $sizes[1].rows }
    if (($restored.layout | ConvertTo-Json -Depth 20 -Compress) -ne $beforeLayout) { throw 'Restore changed split geometry' }
    $evidence.checks+=@{ name='maximize_restore_preserves_ratios_hidden_korean_output_and_processes'; maximizedSize=$zoomSize }
    for ($cycle=0;$cycle -lt 20;$cycle++) {
        Invoke-Flowmux @('toggle-pane-zoom',$top.pane) | Out-Null
        Wait-Tree { param($t) $s=$t.surfaces | Where-Object { $_.id -eq $top.surface }; $s.cols -eq $zoomSize.cols -and $s.rows -eq $zoomSize.rows } | Out-Null
        Invoke-Flowmux @('toggle-pane-zoom',$top.pane) | Out-Null
        Wait-Tree { param($t) $s=$t.surfaces | Where-Object { $_.id -eq $top.surface }; $s.cols -eq $sizes[1].cols -and $s.rows -eq $sizes[1].rows } | Out-Null
    }
    $tree=Wait-Tree { param($t) $s=$t.surfaces | Where-Object { $_.id -eq $top.surface }; $s.cols -eq $sizes[1].cols -and $s.rows -eq $sizes[1].rows }
    if ((@($tree.surfaces | Sort-Object id | ForEach-Object { "$($_.id):$($_.pid)" }) -join ',') -ne $originalPids -or @($tree.surfaces | Where-Object { -not $_.running }).Count) { throw 'Repeated zoom restarted or lost a shell' }
    Invoke-Flowmux @('toggle-pane-zoom',$top.pane) | Out-Null
    Check-Focus $top.pane 'down' $bottom.pane
    if ((Invoke-Flowmux @('tree')).zoomed_pane) { throw 'Directional navigation did not restore hidden destination' }
    Invoke-Flowmux @('toggle-pane-zoom',$top.pane) | Out-Null
    Invoke-Flowmux @('resize-pane',$inner,'--ratio','0.35') | Out-Null
    if ((Invoke-Flowmux @('tree')).zoomed_pane) { throw 'Resize did not restore layout' }
    $evidence.checks+=@{ name='forty_zoom_transitions_keep_sessions_and_directional_focus_or_resize_restores_layout'; transitions=40 }
    # A caller in a hidden sibling still resolves its own pane by stable surface ID.
    Invoke-Flowmux @('toggle-pane-zoom',$top.pane) | Out-Null
    $contextFile=Join-Path $directory 'hidden-caller.json'
    Send-Script $bottom.pane ('& $env:FLOWMUX_BUNDLED_CLI_PATH --json focus-direction up | Set-Content -Encoding UTF8 ''{0}''' -f $contextFile)
    $context=Wait-File $contextFile
    if ($context.pane -ne $top.pane -or -not $context.focused) { throw 'Hidden caller directional context was lost' }
    Invoke-Flowmux @('new-workspace') | Out-Null
    $other=Invoke-Flowmux @('identify')
    if ((Invoke-Flowmux @('tree')).zoomed_pane) { throw 'Workspace switch retained old zoom' }
    Invoke-Flowmux @('resize-pane',$root,'--ratio','0.61') | Out-Null
    if ((Invoke-Flowmux @('identify')).workspace -ne $other.workspace) { throw 'Inactive workspace resize stole focus' }
    Invoke-Flowmux @('focus-tab',$top.surface) | Out-Null
    Invoke-Flowmux @('toggle-pane-zoom') | Out-Null
    Invoke-Flowmux @('split','vertical') | Out-Null
    $extra=Invoke-Flowmux @('identify')
    if ((Invoke-Flowmux @('tree')).zoomed_pane) { throw 'Split retained zoom' }
    Invoke-Flowmux @('close-tab',$extra.surface) | Out-Null
    Invoke-Flowmux @('toggle-pane-zoom',$bottom.pane) | Out-Null
    Invoke-Flowmux @('move-tab',$other.surface,'--to-pane',$bottom.pane) | Out-Null
    if ((Invoke-Flowmux @('tree')).zoomed_pane) { throw 'Move retained zoom' }
    Invoke-Flowmux @('toggle-pane-zoom',$bottom.pane) | Out-Null
    Invoke-Flowmux @('close-tab',$other.surface) | Out-Null
    if ((Invoke-Flowmux @('tree')).zoomed_pane) { throw 'Close retained zoom' }
    $evidence.checks+=@{ name='stable_hidden_caller_context_and_workspace_split_move_close_zoom_transitions' }
    Invoke-Flowmux @('toggle-pane-zoom',$top.pane) | Out-Null
    Wait-Tree { param($t) @($t.surfaces | Where-Object { -not $_.cwd_reported }).Count -eq 0 } | Out-Null
    $saved=Invoke-Flowmux @('save-state')
    $before=Invoke-Flowmux @('tree')
    Invoke-Flowmux @('quit') | Out-Null
    if (-not $owned.WaitForExit(10000) -or $owned.ExitCode -ne 0) { throw 'Original host did not close' }
    $restoredHost=Start-Owned @('--restore-window',$saved.window)
    $after=Invoke-Flowmux @('tree')
    if ($after.zoomed_pane -or $after.layout.panes.Count -ne 3 -or ($after.workspaces | ConvertTo-Json -Depth 50 -Compress) -ne ($before.workspaces | ConvertTo-Json -Depth 50 -Compress)) { throw 'Saved ratios/layout did not restore normally' }
    Wait-Find $bottom.surface $marker
    $evidence.checks+=@{ name='restart_restores_nested_ratios_focus_and_korean_history_with_zoom_cleared'; stateWindow=$saved.window }
    Invoke-Flowmux @('quit') | Out-Null
    if (-not $restoredHost.WaitForExit(10000) -or $restoredHost.ExitCode -ne 0) { throw 'Restored host did not close' }
    $evidence.status='passed_background_pane_subset'
    $evidence.pending='Physical divider drag, shortcuts, IME during transitions, DPI and accessibility remain unverified.'
} catch { $evidence.status='failed'; $evidence.error=$_.Exception.Message; throw }
finally {
    foreach ($ownedHost in $hosts) { if (-not $ownedHost.HasExited) { $ownedHost.Kill(); $ownedHost.WaitForExit() } }
    $evidence.finished=(Get-Date).ToString('o')
    $evidence | ConvertTo-Json -Depth 50 | Set-Content -Encoding UTF8 (Join-Path $directory 'native-panes-background.json')
}
$evidence | ConvertTo-Json -Depth 50
