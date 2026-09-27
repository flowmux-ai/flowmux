# SPDX-License-Identifier: GPL-3.0-or-later
# Always hidden with isolated state. Never inject desktop input or show dialogs.
param([string]$BuildDirectory = "$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference = 'Stop'
$OutputEncoding = [Console]::OutputEncoding = New-Object Text.UTF8Encoding($false)
$BuildDirectory = (Resolve-Path $BuildDirectory).Path
$cli = Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor = (& $cli doctor | ConvertFrom-Json)
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) { throw 'A working debug build is required; no window was launched.' }
Add-Type -Path (Join-Path $PSScriptRoot 'NativeInput.cs')
$directory = Join-Path $PSScriptRoot ('..\dist\evidence\workspaces-' + [guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory) | Out-Null
$directory = (Resolve-Path $directory).Path
$hosts = New-Object 'System.Collections.Generic.List[System.Diagnostics.Process]'
$evidence = [ordered]@{ started=(Get-Date).ToString('o'); mode='background'; checks=@(); hosts=@() }
function Invoke-Flowmux([string[]]$Arguments) {
    if (-not $script:pipeName) { throw 'No verified test pipe; refusing discovery fallback' }
    $value = & $cli --pipe $script:pipeName --json @Arguments
    if ($LASTEXITCODE -ne 0) { throw "flowmuxctl failed: $Arguments" }
    return (($value -join "`n") | ConvertFrom-Json)
}
function Start-Owned([string[]]$LaunchArgs) {
    $launchedAt=(Get-Date).ToUniversalTime()
    $oldBackground=$env:FLOWMUX_TEST_BACKGROUND; $oldState=$env:FLOWMUX_TEST_STATE_DIR
    try {
        $env:FLOWMUX_TEST_BACKGROUND='1'; $env:FLOWMUX_TEST_STATE_DIR=Join-Path $directory 'state'
        $owned=Start-Process -FilePath (Join-Path $BuildDirectory 'flowmux.exe') -ArgumentList $LaunchArgs -PassThru
        $hosts.Add($owned)
    } finally { $env:FLOWMUX_TEST_BACKGROUND=$oldBackground; $env:FLOWMUX_TEST_STATE_DIR=$oldState }
    $discovery=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($owned.Id).json"
    $deadline=(Get-Date).AddSeconds(25)
    $script:pipeName=$null
    do {
        if ($owned.HasExited) { throw "Test host $($owned.Id) exited during startup ($($owned.ExitCode))" }
        if ((Get-Date) -gt $deadline) { throw "Test host $($owned.Id) did not publish a fresh discovery record" }
        $record=$null
        if ((Test-Path $discovery) -and (Get-Item $discovery).LastWriteTimeUtc -ge $launchedAt) {
            try { $record=Get-Content -Raw $discovery | ConvertFrom-Json } catch { $record=$null }
        }
        if ($record -and $record.pid -eq $owned.Id -and $record.pipe) { $script:pipeName=$record.pipe; break }
        Start-Sleep -Milliseconds 100
    } while ($true)
    $evidence.hosts+=@{ pid=$owned.Id; launchedUtc=$launchedAt.ToString('o'); discoveryWrittenUtc=(Get-Item $discovery).LastWriteTimeUtc.ToString('o'); pipe=$script:pipeName }
    Wait-Tree { param($t) @($t.surfaces | Where-Object { -not $_.ready -or -not $_.cwd_reported }).Count -eq 0 } | Out-Null
    if ((Invoke-Flowmux @('identify')).pid -ne $owned.Id) { throw 'Discovery resolved another process; refusing to mutate it' }
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
function Tabs($Pane) {
    if ($Pane.content) { foreach ($tab in $Pane.content.surfaces) { $tab } }
    else { Tabs $Pane.first; Tabs $Pane.second }
}
function Assert-Stopped([int[]]$Ids) {
    $deadline=(Get-Date).AddSeconds(10)
    do {
        $remaining=@($Ids | Where-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue })
        if ($remaining.Count -eq 0) { return }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    throw "Closed workspace processes survived: $remaining"
}
try {
    $owned=Start-Owned @('--cwd',('"'+$directory+'"'))
    $a=Invoke-Flowmux @('identify')
    $hangul=([string][char]0xD55C)+[char]0xAE00
    $name=$hangul+' '+[char]0x1112+[char]0x1161+[char]0x11AB+' e'+[char]0x301+' '+[char]::ConvertFromUtf32(0x1F600)+' & && name'
    $tabName='TAB-'+$name
    Invoke-Flowmux @('workspace','rename',('workspace:'+$a.workspace),$name) | Out-Null
    Invoke-Flowmux @('workspace','color',$a.workspace,'#12AbEF') | Out-Null
    Invoke-Flowmux @('rename-tab',$a.surface,$tabName) | Out-Null
    Send-Script $a.pane ('$Host.UI.RawUI.WindowTitle = ''AUTO-MUST-NOT-RENAME''; Write-Output ''LOCKED-TITLE-READY''')
    Wait-Find $a.surface 'LOCKED-TITLE-READY'
    Invoke-Flowmux @('new-workspace') | Out-Null
    $b=Invoke-Flowmux @('identify')
    Invoke-Flowmux @('new-tab') | Out-Null
    Invoke-Flowmux @('split','horizontal') | Out-Null
    Invoke-Flowmux @('new-workspace') | Out-Null
    $c=Invoke-Flowmux @('identify')
    $tree=Wait-Tree { param($t) @($t.surfaces | Where-Object { -not $_.cwd_reported }).Count -eq 0 }
    $initial=@($tree.surfaces | Sort-Object id | ForEach-Object { "$($_.id):$($_.pid)" }) -join ','
    $wsA=$tree.workspaces | Where-Object { $_.id -eq $a.workspace }
    $tabA=Tabs $wsA.root | Where-Object { $_.id -eq $a.surface }
    if (-not [string]::Equals($wsA.name,$name,[StringComparison]::Ordinal) -or $wsA.color -cne '#12abef' -or -not $tabA.title_locked -or -not [string]::Equals($tabA.title,$tabName,[StringComparison]::Ordinal)) { throw 'Unicode metadata or title lock was lost' }
    $window=[IntPtr]([long]$tree.window_handle)
    if ([NativeInput]::ButtonTitles($window) -notcontains (([string][char]0x25CB)+' '+$name.Replace('&','&&'))) { throw 'Native workspace caption did not preserve escaped ampersands/Unicode' }
    if ([NativeInput]::ChildTitles($window,'STATIC') -notcontains ([string][char]0x25A0)) { throw 'Native color swatch missing' }
    Invoke-Flowmux @('rename-tab',$a.surface,('hidden-'+$tabName)) | Out-Null
    if ((Invoke-Flowmux @('identify')).surface -ne $c.surface) { throw 'Inactive tab rename stole focus' }
    Invoke-Flowmux @('rename-tab',$a.surface,$tabName) | Out-Null
    $evidence.checks+=@{ name='korean_nfd_emoji_and_ampersands_preserved_in_metadata_and_native_captions'; workspaceName=$name; tabName=$tabName; color=$wsA.color; locked=$tabA.title_locked }
    $before=(Invoke-Flowmux @('tree')).workspaces | ConvertTo-Json -Depth 60 -Compress
    Reject @('workspace','rename',$a.workspace,"bad`nname")
    Reject @('workspace','rename',$a.workspace,('x'*257))
    Reject @('workspace','color',$a.workspace,'#12xyz9')
    Reject @('workspace','color',$a.workspace,'red')
    Reject @('workspace','reorder',$a.workspace,'3')
    Reject @('workspace','close',([guid]::NewGuid().ToString()))
    Reject @('rename-tab',$a.surface,"bad`rname")
    Reject @('rename-tab',([guid]::NewGuid().ToString()),'missing')
    if (((Invoke-Flowmux @('tree')).workspaces | ConvertTo-Json -Depth 60 -Compress) -ne $before) { throw 'Invalid request changed the model' }
    $evidence.checks+=@{ name='invalid_names_colors_reorders_and_missing_ids_leave_model_unchanged' }
    for ($cycle=0;$cycle -lt 20;$cycle++) {
        Invoke-Flowmux @('workspace','reorder',$c.workspace,'0') | Out-Null
        Invoke-Flowmux @('workspace','reorder',$c.workspace,'2') | Out-Null
    }
    Invoke-Flowmux @('workspace','reorder',$c.workspace,'0') | Out-Null
    $tree=Invoke-Flowmux @('tree')
    $list=(Invoke-Flowmux @('workspace','list')).workspaces
    if (($list.id -join ',') -ne (@($c.workspace,$a.workspace,$b.workspace) -join ',') -or $tree.active_workspace -ne $c.workspace -or (Invoke-Flowmux @('workspace','current')).workspace -ne $c.workspace) { throw 'Workspace order or active identity changed incorrectly' }
    if ((@($tree.surfaces | Sort-Object id | ForEach-Object { "$($_.id):$($_.pid)" }) -join ',') -ne $initial) { throw 'Reorder restarted a terminal' }
    $contextFile=Join-Path $directory 'context.json'
    Send-Script $a.pane ('& $env:FLOWMUX_BUNDLED_CLI_PATH --json workspace current | Set-Content -Encoding UTF8 ''{0}''' -f $contextFile)
    if ((Wait-File $contextFile).workspace -ne $a.workspace) { throw 'Hidden child resolved the focused workspace instead of its own' }
    $evidence.checks+=@{ name='forty_one_reorders_preserve_all_pids_active_identity_and_hidden_child_context'; count=41; order=$list.id }
    Invoke-Flowmux @('workspace','focus',$a.workspace) | Out-Null
    if ((Invoke-Flowmux @('identify')).surface -ne $a.surface) { throw 'Explicit workspace focus lost its selected pane/tab' }
    if ([NativeInput]::ButtonTitles($window) -notcontains (([string][char]0x25CF)+' '+$tabName.Replace('&','&&'))) { throw 'Native tab caption differs after showing the renamed tab' }
    $marker='PRESERVED-'+$hangul+'-'+[guid]::NewGuid().ToString().Substring(0,8)
    Send-Script $a.pane ("Write-Output '"+$marker+"'")
    Wait-Find $a.surface $marker
    # The close target has multiple panes/tabs and an owned three-level child tree.
    Invoke-Flowmux @('focus-tab',$b.surface) | Out-Null
    $probe=Join-Path $directory 'process-tree-probe.exe'
    Add-Type -Path (Join-Path $PSScriptRoot 'ProcessTreeProbe.cs') -OutputAssembly $probe -OutputType ConsoleApplication
    $pidFile=Join-Path $directory 'descendants.txt'
    Send-Script $b.pane ("& '"+$probe+"' '"+$pidFile+"' 2")
    $deadline=(Get-Date).AddSeconds(12)
    do {
        $descendants=if (Test-Path $pidFile) { @(Get-Content $pidFile | Where-Object { $_ -match '^\d+$' }) } else { @() }
        if ($descendants.Count -eq 3) { break }
        if ((Get-Date) -gt $deadline) { throw 'Descendant probe did not start' }
        Start-Sleep -Milliseconds 100
    } while ($true)
    $tree=Invoke-Flowmux @('tree')
    $closingIds=@(Tabs ($tree.workspaces | Where-Object { $_.id -eq $b.workspace }).root).id
    $closingPids=@($tree.surfaces | Where-Object { $closingIds -contains $_.id }).pid + @($descendants | ForEach-Object { [int]$_ })
    Invoke-Flowmux @('workspace','focus',$a.workspace) | Out-Null
    Invoke-Flowmux @('workspace','close',$b.workspace) | Out-Null
    Assert-Stopped $closingPids
    $tree=Invoke-Flowmux @('tree')
    if ($tree.surfaces.Count -ne 2 -or $tree.active_workspace -ne $a.workspace -or @($tree.surfaces | Where-Object { -not $_.running }).Count) { throw 'Closing inactive workspace damaged survivors' }
    foreach ($survivor in $tree.surfaces) {
        if ($initial.Split(',') -notcontains "$($survivor.id):$($survivor.pid)") { throw 'Survivor process restarted during close' }
    }
    Reject @('rename-tab',$b.surface,'stale target')
    Wait-Find $a.surface $marker
    $evidence.checks+=@{ name='inactive_workspace_close_terminates_all_three_terminals_and_three_descendants_only'; stoppedPids=$closingPids; survivorPids=$tree.surfaces.pid }
    $saved=Invoke-Flowmux @('save-state')
    $before=Invoke-Flowmux @('tree')
    Invoke-Flowmux @('quit') | Out-Null
    if (-not $owned.WaitForExit(10000)) { throw 'Original host did not exit' }
    $restored=Start-Owned @('--restore-window',$saved.window)
    $after=Invoke-Flowmux @('tree')
    if (($before.workspaces | ConvertTo-Json -Depth 60 -Compress) -ne ($after.workspaces | ConvertTo-Json -Depth 60 -Compress) -or $before.active_workspace -ne $after.active_workspace) { throw 'Metadata/order/title lock changed on restart' }
    Wait-Find $a.surface $marker
    Send-Script $a.pane ('$Host.UI.RawUI.WindowTitle = ''AFTER-RESTORE-AUTO''; Write-Output ''LOCK-SURVIVED-RESTORE''')
    Wait-Find $a.surface 'LOCK-SURVIVED-RESTORE'
    $tree=Invoke-Flowmux @('tree')
    if ((Tabs ($tree.workspaces | Where-Object { $_.id -eq $a.workspace }).root).title -cne $tabName) { throw 'Restored tab lost its custom title lock' }
    $evidence.checks+=@{ name='restart_restores_workspace_order_color_unicode_names_locked_tab_title_and_korean_history' }
    Invoke-Flowmux @('workspace','focus',$c.workspace) | Out-Null
    $closedPid=($tree.surfaces | Where-Object { $_.id -eq $c.surface }).pid
    Invoke-Flowmux @('workspace','close',$c.workspace) | Out-Null
    Assert-Stopped @($closedPid)
    if ((Invoke-Flowmux @('identify')).surface -ne $a.surface) { throw 'Closing active workspace did not select the adjacent survivor' }
    Invoke-Flowmux @('workspace','color',$a.workspace,'--clear') | Out-Null
    $before=Invoke-Flowmux @('tree')
    Reject @('workspace','close',$a.workspace)
    $after=Invoke-Flowmux @('tree')
    if ($after.workspaces[0].color -or $after.surfaces[0].pid -ne $before.surfaces[0].pid -or $after.workspaces.Count -ne 1) { throw 'Clear color or final workspace protection failed' }
    $evidence.checks+=@{ name='active_workspace_close_selects_survivor_color_clear_and_final_workspace_protection' }
    Invoke-Flowmux @('quit') | Out-Null
    if (-not $restored.WaitForExit(10000)) { throw 'Restored host did not exit' }
    $evidence.status='passed_background_workspace_subset'
    $evidence.pending='Native menus, edit dialogs, IME composition, color appearance, keyboard/DPI/accessibility, drag reordering and empty-window UI remain pending.'
} catch { $evidence.status='failed'; $evidence.error=$_.Exception.Message; throw }
finally {
    foreach ($ownedHost in $hosts) { if (-not $ownedHost.HasExited) { $ownedHost.Kill(); $ownedHost.WaitForExit() } }
    $evidence.finished=(Get-Date).ToString('o')
    $evidence | ConvertTo-Json -Depth 50 | Set-Content -Encoding UTF8 (Join-Path $directory 'native-workspaces-background.json')
}
$evidence | ConvertTo-Json -Depth 50
