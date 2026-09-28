# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden owned hosts;110s inner budget,120s outer Job; no desktop input/dialogs.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path;$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'NativeInput.cs'),(Join-Path $PSScriptRoot 'CliProbe.cs')
$directory=Join-Path $PSScriptRoot ('..\dist\evidence\workspaces-'+[guid]::NewGuid());[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$hosts=New-Object 'System.Collections.Generic.List[System.Diagnostics.Process]';$hostIO=@{};$clientPids=@();$shellPids=@();$descendantPids=@();$cleanupErrors=@();$cleaning=$false;$clock=[Diagnostics.Stopwatch]::StartNew()
$evidence=[ordered]@{started=(Get-Date).ToString('o');mode='background';checks=@();hosts=@();observations=@();desktopInput=$false;clipboardAccess=$false}
function Budget([int]$Maximum=5000) {
    if($script:cleaning){return $Maximum};$left=110000-$clock.ElapsedMilliseconds
    if($left -le 0){throw 'Workspace verifier exhausted its110s inner budget'}
    return [int][Math]::Min($Maximum,$left)
}
function Invoke-Probe([string[]]$Arguments,[int]$ExpectedExit=0,[int]$Maximum=5000) {
    $p=[CliProbe]::Start($cli,$Arguments,$directory,$directory);$script:clientPids+=,$p.Id;$out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
    try {
        if(-not $p.WaitForExit((Budget $Maximum))){throw 'Owned workspace CLI exceeded its bounded deadline; no retry'}
        if(-not $out.Wait(500) -or -not $err.Wait(500)){throw 'Owned CLI output did not close'}
        if($p.ExitCode -ne $ExpectedExit){throw ('Owned CLI exit '+$p.ExitCode+': '+[CliProbe]::Output($err)+' '+[CliProbe]::Output($out))}
        $text=if($ExpectedExit -eq 0){[CliProbe]::Output($out)}else{[CliProbe]::Output($err)}
        return ($text|ConvertFrom-Json)
    } catch {$evidence.observations+=@{kind='command-failure';pid=$p.Id;arguments=$Arguments;error=$_.Exception.Message;stdout=[CliProbe]::Output($out);stderr=[CliProbe]::Output($err)};throw}
    finally {if(-not $p.HasExited){$p.Kill();[CliProbe]::WaitAfterKill($p)};$p.Dispose()}
}
function Invoke-Flowmux([string[]]$Arguments,[int]$Maximum=5000) {
    if(-not $script:pipeName){throw 'No verified test pipe; refusing discovery fallback'}
    return Invoke-Probe (@('--pipe',$script:pipeName,'--json')+$Arguments) 0 $Maximum
}
function Wait-Tree([scriptblock]$Condition,[int]$Maximum=5000) {
    $wait=[Diagnostics.Stopwatch]::StartNew()
    do {
        $left=$Maximum-$wait.ElapsedMilliseconds;if($left -le 0){throw 'Pane state did not converge within its condition budget'}
        $tree=Invoke-Flowmux @('tree') ([int][Math]::Min(5000,$left));$window=[IntPtr]([long]$tree.window_handle)
        if(-not $tree.background_testing -or [NativeInput]::IsWindowVisible($window) -or [NativeInput]::GetForegroundWindow() -eq $window){throw 'Test host became visible or foreground'}
        $script:shellPids+=@($tree.surfaces|Where-Object {$_.pid -and $script:shellPids -notcontains $_.pid}|ForEach-Object {$_.pid})
        if(& $Condition $tree){return $tree}
        Start-Sleep -Milliseconds ([int][Math]::Min(20,[Math]::Max(1,$Maximum-$wait.ElapsedMilliseconds)))
    }while($true)
}
function Start-Owned([string[]]$LaunchArgs) {
    $launchedAt=[DateTime]::UtcNow;$startup=[Diagnostics.Stopwatch]::StartNew()
    $owned=[CliProbe]::Start((Join-Path $BuildDirectory 'flowmux.exe'),$LaunchArgs,$directory,$directory);$hosts.Add($owned)
    $hostIO[$owned.Id]=@{output=$owned.StandardOutput.ReadToEndAsync();error=$owned.StandardError.ReadToEndAsync();pipe=$null}
    $discovery=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($owned.Id).json";$script:pipeName=$null
    do {
        Budget|Out-Null;if($owned.HasExited -or $startup.ElapsedMilliseconds -ge 8000){throw 'Owned host exited or exceeded eight-second startup budget'}
        if((Test-Path -LiteralPath $discovery) -and (Get-Item -LiteralPath $discovery).LastWriteTimeUtc -ge $launchedAt){
            $record=Get-Content -Raw -LiteralPath $discovery|ConvertFrom-Json
            if($record.pid -ne $owned.Id -or -not $record.pipe){throw 'Wrong discovery owner'}
            $script:pipeName=$record.pipe;$hostIO[$owned.Id].pipe=$record.pipe;break
        }
        Start-Sleep -Milliseconds 20
    }while($true)
    $left=8000-$startup.ElapsedMilliseconds;if($left -le 0){throw 'Startup budget exhausted'}
    if((Invoke-Flowmux @('identify') ([int][Math]::Min(5000,$left))).pid -ne $owned.Id){throw 'Discovery resolved another process'}
    $left=8000-$startup.ElapsedMilliseconds;if($left -le 0){throw 'Startup budget exhausted'}
    Wait-Tree {param($t) @($t.surfaces|Where-Object {-not $_.ready -or -not $_.cwd_reported}).Count -eq 0} ([int]$left)|Out-Null
    $evidence.hosts+=@{pid=$owned.Id;launchedUtc=$launchedAt.ToString('o');pipe=$script:pipeName;startupMs=$startup.ElapsedMilliseconds}
    return $owned
}
function Send-Script([string]$Pane,[string]$Source) {
    $encoded=[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Source))
    Invoke-Flowmux @('send-keys',$Pane,"Invoke-Expression ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$encoded')))")|Out-Null
    Invoke-Flowmux @('send-key','Enter','--pane',$Pane)|Out-Null
}
function Wait-File([string]$Path) {
    $wait=[Diagnostics.Stopwatch]::StartNew()
    while(-not (Test-Path -LiteralPath $Path)){Budget|Out-Null;if($wait.ElapsedMilliseconds -ge 5000){throw "Child file did not arrive: $Path"};Start-Sleep -Milliseconds 20}
    return (Get-Content -Raw -LiteralPath $Path|ConvertFrom-Json)
}
function Reject([string[]]$Arguments) {
    if(-not $script:pipeName){throw 'No verified rejection-test pipe'}
    $result=Invoke-Probe (@('--pipe',$script:pipeName,'--json')+$Arguments) 1
    if(-not $result.error){throw 'Rejected request omitted its error'}
}
function Wait-Find([string]$Surface,[string]$Query) {
    $wait=[Diagnostics.Stopwatch]::StartNew()
    do {
        $left=5000-$wait.ElapsedMilliseconds;if($left -le 0){throw "Hidden output missing: $Query"}
        $result=Invoke-Flowmux @('find',$Query,'--match-case','--surface',$Surface) ([int]$left)
        if($result.result.found -and $result.result.selection -ceq $Query){return}
        Start-Sleep -Milliseconds ([int][Math]::Min(20,[Math]::Max(1,5000-$wait.ElapsedMilliseconds)))
    }while($true)
}
function Tabs($Pane) {if($Pane.content){foreach($tab in $Pane.content.surfaces){$tab}}else{Tabs $Pane.first;Tabs $Pane.second}}
function Assert-Stopped([int[]]$Ids) {
    $wait=[Diagnostics.Stopwatch]::StartNew()
    do {Budget|Out-Null;$remaining=@($Ids|Where-Object {Get-Process -Id $_ -ErrorAction SilentlyContinue});if(-not $remaining.Count){return};if($wait.ElapsedMilliseconds -ge 5000){throw "Closed workspace processes survived: $remaining"};Start-Sleep -Milliseconds 20}while($true)
}
try {
    $doctor=Invoke-Probe @('doctor');if(-not $doctor.background_testing -or $doctor.status -ne 'ok'){throw 'A working hidden debug build is required; no host launched'}
    $owned=Start-Owned @('--cwd',$directory)
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
    $actualCwd=($tree.surfaces|Where-Object {$_.id -eq $a.surface}).cwd
    $caption=($name+"`n"+$actualCwd).Replace('&','&&')
    if ([NativeInput]::ButtonTitles($window) -notcontains $caption) { throw 'Native workspace caption lost its Unicode name/newline/actual cwd' }
    $chrome=@($tree.chrome.controls|Where-Object {$_.kind -eq 'workspace' -and $_.workspace -eq $a.workspace})
    if($chrome.Count -ne 1 -or -not [string]::Equals($chrome[0].label,$caption,[StringComparison]::Ordinal)){throw 'Chrome workspace metadata differs from the actual two-line caption'}
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
    if ([NativeInput]::ButtonTitles($window) -notcontains ($tabName.Replace('&','&&'))) { throw 'Native tab caption differs after showing the renamed tab' }
    $marker='PRESERVED-'+$hangul+'-'+[guid]::NewGuid().ToString().Substring(0,8)
    Send-Script $a.pane ("Write-Output '"+$marker+"'")
    Wait-Find $a.surface $marker
    # The close target has multiple panes/tabs and an owned three-level child tree.
    Invoke-Flowmux @('focus-tab',$b.surface) | Out-Null
    $probe=Join-Path $directory 'process-tree-probe.exe'
    Add-Type -Path (Join-Path $PSScriptRoot 'ProcessTreeProbe.cs') -OutputAssembly $probe -OutputType ConsoleApplication
    $pidFile=Join-Path $directory 'descendants.txt'
    Send-Script $b.pane ("& '"+$probe+"' '"+$pidFile+"' 2")
    $deadline=[Diagnostics.Stopwatch]::StartNew()
    do {
        $descendants=if (Test-Path $pidFile) { @(Get-Content $pidFile | Where-Object { $_ -match '^\d+$' }) } else { @() }
        if ($descendants.Count -eq 3) { break }
        if ($deadline.ElapsedMilliseconds -ge 5000) { throw 'Descendant probe did not start within five seconds' }; Budget|Out-Null
        Start-Sleep -Milliseconds 20
    } while ($true)
    $tree=Invoke-Flowmux @('tree')
    $closingIds=@(Tabs ($tree.workspaces | Where-Object { $_.id -eq $b.workspace }).root).id
    $script:descendantPids=@($descendants|ForEach-Object {[int]$_})
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
    if (-not $owned.WaitForExit((Budget 5000))) { throw 'Original host did not exit within five seconds' };if($owned.ExitCode -ne 0){throw 'Original host exited with failure'}
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
    if (-not $restored.WaitForExit((Budget 5000))) { throw 'Restored host did not exit within five seconds' };if($restored.ExitCode -ne 0){throw 'Restored host exited with failure'}
    $evidence.status='passed_background_workspace_subset'
    $evidence.pending='Native menus, edit dialogs, IME composition, color appearance, keyboard/DPI/accessibility, drag reordering and empty-window UI remain pending.'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
    $script:cleaning=$true
    foreach($ownedHost in $hosts){
        try {
            if(-not $ownedHost.HasExited){
                $io=$hostIO[$ownedHost.Id];$stop=[Diagnostics.Stopwatch]::StartNew()
                if($io.pipe){Invoke-Probe @('--pipe',$io.pipe,'--json','quit','--discard-state') 0 2500|Out-Null}
                $left=[int][Math]::Max(1,5000-$stop.ElapsedMilliseconds)
                if(-not $ownedHost.WaitForExit($left)){throw 'Owned host did not exit within five-second cleanup budget'}
            }
        }catch{$cleanupErrors+=$_.Exception.Message;if(-not $ownedHost.HasExited){$ownedHost.Kill();[CliProbe]::WaitAfterKill($ownedHost)}}
        finally{
            $io=$hostIO[$ownedHost.Id];$outDone=$io.output.Wait(500);$errDone=$io.error.Wait(500)
            $evidence.observations+=@{kind='host-exit';pid=$ownedHost.Id;exitCode=$ownedHost.ExitCode;stdoutComplete=$outDone;stderrComplete=$errDone;stdout=[CliProbe]::Output($io.output);stderr=[CliProbe]::Output($io.error)}
            $ownedHost.Dispose()
        }
    }
    if($cleanupErrors.Count){$evidence.status='failed';$evidence.cleanupErrors=$cleanupErrors}
    $evidence.clientPids=$clientPids;$evidence.shells=$shellPids;$evidence.descendants=$descendantPids;$evidence.elapsedMs=$clock.ElapsedMilliseconds;$evidence.finished=(Get-Date).ToString('o')
    $evidence|ConvertTo-Json -Depth 50|Set-Content -Encoding UTF8 (Join-Path $directory 'native-workspaces-background.json')
    Write-Output ('Evidence: '+$directory)
}
if($cleanupErrors.Count){throw ($cleanupErrors -join '; ')}
[ordered]@{status=$evidence.status;checks=$evidence.checks.Count;elapsedMs=$evidence.elapsedMs}|ConvertTo-Json -Compress
