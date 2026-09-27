# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden owned hosts, isolated shared settings/state, no desktop input.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path; $cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor=(& $cli doctor | ConvertFrom-Json)
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) { throw 'A working debug build is required; no host was launched.' }
Add-Type -Path (Join-Path $PSScriptRoot 'NativeInput.cs')
$directory=Join-Path $PSScriptRoot ('..\dist\evidence\shells-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory) | Out-Null
$directory=(Resolve-Path $directory).Path
$configDirectory=Join-Path $directory 'config'; $configPath=Join-Path $configDirectory 'config.json'
$hosts=New-Object 'System.Collections.Generic.List[object]'
$evidence=[ordered]@{ started=(Get-Date).ToString('o'); mode='background'; hosts=@(); checks=@() }
function Invoke-Owned($Owned,[string[]]$Arguments) {
    if (-not $Owned.pipe) { throw 'No owned pipe; refusing fallback' }
    $value=& $cli --pipe $Owned.pipe --json @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Owned request failed: $Arguments" }
    return (($value -join "`n") | ConvertFrom-Json)
}
function Same($Actual,[hashtable]$Expected) {
    if (-not $Actual) { return $false }
    foreach ($key in $Expected.Keys) {
        if ($Actual.$key -is [string]) { if (-not [string]::Equals($Actual.$key,[string]$Expected[$key],[StringComparison]::Ordinal)) { return $false } }
        elseif ($Actual.$key -ne $Expected[$key]) { return $false }
    }
    return $true
}
function Wait-Settings($Owned,[hashtable]$Expected,[string]$Revision='') {
    $deadline=(Get-Date).AddSeconds(15)
    do {
        $status=Invoke-Owned $Owned @('settings','show')
        $good=(Same $status.document.terminal $Expected) -and (-not $Revision -or $status.document.revision -eq $Revision)
        foreach ($surface in $status.surfaces) {
            $good=$good -and $surface.applied -and $surface.applied.revision -eq $status.document.revision -and (Same $surface.applied.terminal $Expected)
        }
        if ($good -and $status.surfaces.Count -gt 0) { return $status }
        Start-Sleep -Milliseconds 50
    } while ((Get-Date) -lt $deadline)
    $status | ConvertTo-Json -Depth 20 | Set-Content -Encoding UTF8 (Join-Path $directory 'failed-settings.json')
    throw 'Settings did not reach all terminal instances'
}
function Tree($Owned) {
    $tree=Invoke-Owned $Owned @('tree'); $window=[IntPtr]([long]$tree.window_handle)
    if (-not $tree.background_testing -or [NativeInput]::IsWindowVisible($window) -or [NativeInput]::GetForegroundWindow() -eq $window) { throw 'Host became visible or foreground' }
    return $tree
}
function Start-Owned([string[]]$Arguments) {
    $launch=(Get-Date).ToUniversalTime()
    $oldBackground=$env:FLOWMUX_TEST_BACKGROUND; $oldConfig=$env:FLOWMUX_TEST_CONFIG_DIR; $oldState=$env:FLOWMUX_TEST_STATE_DIR; $oldTrace=$env:FLOWMUX_TEST_SPAWN_TRACE
    try {
        $env:FLOWMUX_TEST_BACKGROUND='1'; $env:FLOWMUX_TEST_CONFIG_DIR=$configDirectory; $env:FLOWMUX_TEST_STATE_DIR=Join-Path $directory 'state'
        $env:FLOWMUX_TEST_SPAWN_TRACE=Join-Path $directory 'spawn-trace.txt'
        $process=Start-Process -FilePath (Join-Path $BuildDirectory 'flowmux.exe') -ArgumentList $Arguments -WorkingDirectory (Get-Location).ProviderPath -PassThru
    } finally { $env:FLOWMUX_TEST_BACKGROUND=$oldBackground; $env:FLOWMUX_TEST_CONFIG_DIR=$oldConfig; $env:FLOWMUX_TEST_STATE_DIR=$oldState; $env:FLOWMUX_TEST_SPAWN_TRACE=$oldTrace }
    $owned=@{process=$process;pipe=$null}; $hosts.Add($owned)
    $discovery=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($process.Id).json"; $deadline=(Get-Date).AddSeconds(25)
    do {
        if ($process.HasExited -or (Get-Date) -gt $deadline) { throw 'Owned host failed to start' }
        if ((Test-Path $discovery) -and (Get-Item $discovery).LastWriteTimeUtc -ge $launch) {
            $record=Get-Content -Raw $discovery | ConvertFrom-Json
            if ($record.pid -ne $process.Id) { throw 'Wrong discovery process' }
            $owned.pipe=$record.pipe; break
        }
        Start-Sleep -Milliseconds 50
    } while ($true)
    if ((Invoke-Owned $owned @('identify')).pid -ne $process.Id) { throw 'Connected to another host' }
    do {
        $tree=Tree $owned
        if (@($tree.surfaces | Where-Object { -not $_.ready }).Count -eq 0) { break }
        if ((Get-Date) -gt $deadline) { throw 'Terminals did not start' }
        Start-Sleep -Milliseconds 50
    } while ($true)
    Wait-Settings $owned @{} | Out-Null
    $evidence.hosts+=@{pid=$process.Id;pipe=$owned.pipe;launchedUtc=$launch.ToString('o')}
    return $owned
}
function Stop-Owned($Owned) { Invoke-Owned $Owned @('quit') | Out-Null; if (-not $Owned.process.WaitForExit(10000)) { throw 'Owned host did not quit' } }
function Reject($Owned,[string[]]$Arguments) {
    $old=$ErrorActionPreference
    try { $ErrorActionPreference='Continue'; $result=& $cli --pipe $Owned.pipe --json @Arguments 2>&1; $status=$LASTEXITCODE }
    finally { $ErrorActionPreference=$old }
    if ($status -eq 0) { throw "Expected settings rejection: $Arguments" }
}
function Wait-Find($Owned,[string]$Surface,[string]$Query) {
    $deadline=(Get-Date).AddSeconds(12)
    do {
        $found=Invoke-Owned $Owned @('find',$Query,'--match-case','--surface',$Surface)
        if ($found.result.found -and $found.result.selection -ceq $Query) { return }
        Start-Sleep -Milliseconds 50
    } while ((Get-Date) -lt $deadline)
    throw "Terminal output not retained: $Query"
}
function Raw($Owned,$Command) {
    if ($Owned.process.HasExited -or -not $Owned.pipe) { throw 'Owned host is not live' }
    $stream=[IO.Pipes.NamedPipeClientStream]::new('.',$Owned.pipe.Substring(9),[IO.Pipes.PipeDirection]::InOut,[IO.Pipes.PipeOptions]::Asynchronous,[Security.Principal.TokenImpersonationLevel]::Identification)
    try {
        $stream.Connect(3000)
        $writer=[IO.StreamWriter]::new($stream,(New-Object Text.UTF8Encoding($false)),4096,$true)
        $reader=[IO.StreamReader]::new($stream,(New-Object Text.UTF8Encoding($false)),$false,4096,$true)
        try {
            $writer.WriteLine(($Command | ConvertTo-Json -Depth 20 -Compress)); $writer.Flush()
            $read=$reader.ReadLineAsync(); if (-not $read.Wait(15000)) { throw 'Owned raw request timed out; not retried' }
            $value=$read.Result | ConvertFrom-Json
            if ($value.error) { throw $value.error }; return $value
        } finally { $writer.Dispose();$reader.Dispose() }
    } finally { $stream.Dispose() }
}
function Wait-Surface($Owned,[string]$Id,[scriptblock]$Condition) {
    $deadline=(Get-Date).AddSeconds(20)
    do {
        $surface=(Tree $Owned).surfaces | Where-Object {$_.id -eq $Id}
        if ($surface -and (& $Condition $surface)) { return $surface }
        Start-Sleep -Milliseconds 50
    } while ((Get-Date) -lt $deadline)
    $surface | ConvertTo-Json -Depth 20 | Set-Content -Encoding UTF8 (Join-Path $directory 'failed-surface.json')
    throw "Terminal did not reach expected shell state: $Id"
}
function Send-Line($Owned,[string]$Pane,[string]$Line) {
    Raw $Owned @{method='send_keys';pane=$Pane;text=$Line} | Out-Null
    Invoke-Owned $Owned @('send-key','Enter','--pane',$Pane) | Out-Null
}
function New-Tab($Owned,[string[]]$Options) {
    Invoke-Owned $Owned (@('new-tab')+$Options) | Out-Null
    return (Invoke-Owned $Owned @('identify'))
}
function Assert-Shell($Owned,$Identity,[string]$Program,[string]$Image) {
    $s=Wait-Surface $Owned $Identity.surface {param($s) $s.ready -and $s.running -and $s.cwd_reported}
    if ($s.shell.program -cne $Program -or [IO.Path]::GetFileName((Get-Process -Id $s.pid).Path) -ine $Image) { throw "Wrong shell process for $Program" }
    return $s
}
function Node-Title($Node,[string]$Id) {
    foreach ($tab in $Node.content.surfaces) {if ($tab.id -eq $Id) {return $tab.title}}
    foreach ($child in @($Node.first,$Node.second)) {
        if ($child) {$title=Node-Title $child $Id; if ($null -ne $title) {return $title}}
    }
    return $null
}
function Tab-Title($Owned,[string]$Id) {
    foreach ($workspace in (Tree $Owned).workspaces) {$title=Node-Title $workspace.root $Id; if ($null -ne $title) {return $title}}
    throw 'Tab title missing'
}
try {
    $unicode=[string][char]0xD55C+[char]0xAE00+' '+[char]0x1112+[char]0x1161+[char]0x11AB+' '+[char]::ConvertFromUtf32(0x1F600)
    $cwd=Join-Path $directory ('cwd '+$unicode+' & #')
    [IO.Directory]::CreateDirectory($cwd) | Out-Null
    $a=Start-Owned @('--cwd',('"'+$directory+'"'))
    $original=Invoke-Owned $a @('identify'); $originalSurface=Assert-Shell $a $original 'powershell' 'powershell.exe'
    $profiles=Invoke-Owned $a @('shells'); $evidence.profiles=$profiles.profiles
    Invoke-Owned $a @('settings','shell','cmd') | Out-Null
    $cmd=New-Tab $a @('--cwd',$cwd)
    $cmdSurface=Assert-Shell $a $cmd 'cmd' 'cmd.exe'
    if (-not [string]::Equals($cmdSurface.cwd,$cwd,[StringComparison]::Ordinal)) { throw 'CMD Unicode startup cwd differs' }
    if ((Wait-Surface $a $original.surface {param($s)$s.running}).pid -ne $originalSurface.pid) { throw 'Default shell change replaced a live process' }
    $evidence.checks+=@{name='default_shell_only_changes_future_tabs_and_cmd_reports_exact_unicode_startup_directory';powershellPid=$originalSurface.pid;cmdPid=$cmdSurface.pid;cwd=$cwd}

    $next=Join-Path $cwd 'next space'; [IO.Directory]::CreateDirectory($next) | Out-Null
    Send-Line $a $cmd.pane ('cd /d "'+$next+'"')
    Wait-Surface $a $cmd.surface {param($s) $s.cwd -ceq $next} | Out-Null
    $marker='CMD_'+$unicode.Replace(' ','_')
    Send-Line $a $cmd.pane ('echo '+$marker); Wait-Find $a $cmd.surface $marker
    Invoke-Owned $a @('split','horizontal') | Out-Null; $split=Invoke-Owned $a @('identify'); $splitSurface=Assert-Shell $a $split 'cmd' 'cmd.exe'
    if (-not [string]::Equals($splitSurface.cwd,$next,[StringComparison]::Ordinal)) {throw 'Split did not inherit CMD cwd'}
    $explicit=New-Tab $a @('--shell','powershell','--shell-arg','-NoProfile'); Assert-Shell $a $explicit 'powershell' 'powershell.exe' | Out-Null
    $evidence.checks+=@{name='cmd_prompt_tracks_changed_unicode_cwd_split_inherits_shell_and_tab_override_starts_powershell';marker=$marker;split=$split.surface}

    $before=Tree $a; $active=(Invoke-Owned $a @('identify')).surface
    Reject $a @('new-tab','--shell','flowmux-missing-shell.exe')
    Reject $a @('new-tab','--cwd',(Join-Path $directory 'absent'))
    Reject $a @('settings','shell','flowmux-missing-shell.exe')
    Reject $a @('retry-shell','--surface',$original.surface,'--shell','cmd')
    $configHash=(Get-FileHash $configPath).Hash; $rejected=$false
    try {Raw $a @{method='settings';op=@{action='shell';program='cmd';args=@(([string][char]0xD55C)*25000)}} | Out-Null}
    catch {if ($_.Exception.Message -notlike '*64 KiB*') {throw};$rejected=$true}
    if (-not $rejected -or (Get-FileHash $configPath).Hash -ne $configHash) {throw 'Oversized UTF-8 settings changed the saved default'}
    if ((Tree $a).surfaces.Count -ne $before.surfaces.Count -or (Invoke-Owned $a @('identify')).surface -ne $active) {throw 'Rejected shell launch changed layout or focus'}
    $evidence.checks+=@{name='invalid_program_directory_default_running_retry_and_oversized_utf8_settings_preserve_file_layout_and_focus'}

    $relativeCwd=Join-Path $directory 'relative'; [IO.Directory]::CreateDirectory($relativeCwd) | Out-Null
    Push-Location $directory
    try {$relative=New-Tab $a @('--cwd','relative')} finally {Pop-Location}
    $relativeSurface=Assert-Shell $a $relative 'cmd' 'cmd.exe'
    if (-not [string]::Equals($relativeSurface.cwd,$relativeCwd,[StringComparison]::Ordinal)) {throw 'Relative CLI cwd resolved against the GUI or active terminal'}
    $evidence.checks+=@{name='relative_cli_cwd_resolves_at_invocation_instead_of_gui_or_source_directory';cwd=$relativeSurface.cwd}

    $probe=Join-Path $cwd 'shell probe.exe'; $argumentFile=Join-Path $directory 'argv.txt'
    Add-Type -Path (Join-Path $PSScriptRoot 'ShellProbe.cs') -OutputAssembly $probe -OutputType ConsoleApplication
    $sentinel=Join-Path $directory 'must-not-execute.txt'
    $arguments=@('',($unicode+' quote" slash\'),('C:\trailing space\'),('& echo bad > "'+$sentinel+'"'))
    Raw $a @{method='new_tab';cwd=$cwd;shell=$probe;shell_args=(@($argumentFile)+$arguments)} | Out-Null
    $custom=Invoke-Owned $a @('identify')
    $probeSurface=Wait-Surface $a $custom.surface {param($s) $s.exit_code -eq 17 -and $s.resources_released}
    if (-not (Test-Path $argumentFile)) {throw 'Custom program did not write argv'}
    $received=@([IO.File]::ReadAllLines($argumentFile) | ForEach-Object {[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($_))})
    if ($received.Count -ne $arguments.Count+2 -or -not [string]::Equals($received[0],$cwd,[StringComparison]::Ordinal) -or $received[1] -ne $custom.surface) {throw 'Custom program cwd/environment changed'}
    for ($i=0;$i -lt $arguments.Count;$i++) {if (-not [string]::Equals($arguments[$i],$received[$i+2],[StringComparison]::Ordinal)) {throw "Argument $i changed"}}
    if (Test-Path $sentinel) {throw 'A shell interpreted a custom executable argument'}
    Wait-Find $a $custom.surface ('SHELL_PROBE_'+[char]0xD55C+[char]0xAE00)
    $evidence.checks+=@{name='unicode_executable_path_empty_quoted_backslash_and_metacharacter_argv_arrive_exactly_without_command_interpretation';argv=$arguments;cwd=$received[0];exitCode=$probeSurface.exit_code}

    $bad=Join-Path $directory 'invalid-image.exe'; [IO.File]::WriteAllText($bad,'not an executable')
    $failed=New-Tab $a @('--shell',$bad)
    $failure=Wait-Surface $a $failed.surface {param($s) $s.ready -and $s.startup_error -and -not $s.running}
    Invoke-Owned $a @('save-state') | Out-Null
    Invoke-Owned $a @('retry-shell','--surface',$failed.surface,'--shell','cmd') | Out-Null
    $recovered=Assert-Shell $a $failed 'cmd' 'cmd.exe'
    if ($recovered.startup_error -or $recovered.id -ne $failure.id) {throw 'Failed startup retry lost identity or retained error'}
    if ((Tab-Title $a $failed.surface) -ceq $bad) {throw 'Recovered shell still displays the failed executable title'}
    $evidence.checks+=@{name='failed_createprocess_retains_saveable_terminal_and_explicit_command_prompt_retry_keeps_identity';surface=$failure.id;failure=$failure.startup_error;newPid=$recovered.pid}

    $saved=Invoke-Owned $a @('save-state'); $window=(Tree $a).state.window
    $shellsBefore=@((Tree $a).surfaces | Sort-Object id | ForEach-Object {@{id=$_.id;shell=$_.shell}}) | ConvertTo-Json -Depth 10 -Compress
    Stop-Owned $a; $a=Start-Owned @('--restore-window',$window)
    $shellsAfter=@((Tree $a).surfaces | Sort-Object id | ForEach-Object {@{id=$_.id;shell=$_.shell}}) | ConvertTo-Json -Depth 10 -Compress
    if ($shellsBefore -cne $shellsAfter) {throw 'Restart changed terminal shell/argv'}
    Assert-Shell $a $original 'powershell' 'powershell.exe' | Out-Null; Assert-Shell $a $cmd 'cmd' 'cmd.exe' | Out-Null
    Wait-Find $a $cmd.surface $marker
    $evidence.checks+=@{name='restart_preserves_each_terminal_shell_argv_cwd_identity_and_korean_history';window=$window}

    Stop-Owned $a; [IO.File]::Delete($probe)
    $a=Start-Owned @('--restore-window',$window)
    Wait-Surface $a $custom.surface {param($s) $s.ready -and $s.startup_error -and -not $s.running} | Out-Null
    Assert-Shell $a $cmd 'cmd' 'cmd.exe' | Out-Null
    $lockedTitle='Recovered '+$unicode+' &'
    Invoke-Owned $a @('rename-tab',$custom.surface,$lockedTitle) | Out-Null
    Invoke-Owned $a @('retry-shell','--surface',$custom.surface,'--shell','cmd') | Out-Null
    Assert-Shell $a $custom 'cmd' 'cmd.exe' | Out-Null
    if (-not [string]::Equals((Tab-Title $a $custom.surface),$lockedTitle,[StringComparison]::Ordinal)) {throw 'Shell retry replaced a user-locked title'}
    Wait-Find $a $custom.surface ('SHELL_PROBE_'+[char]0xD55C+[char]0xAE00)
    $evidence.checks+=@{name='missing_restored_executable_leaves_other_shells_running_and_history_available_after_explicit_retry'}

    Push-Location $directory
    try {$b=Start-Owned @('--new-window','--shell','powershell','--cwd','.')} finally {Pop-Location}
    $legacy=Invoke-Owned $b @('identify'); $saved=Invoke-Owned $b @('save-state'); $legacyWindow=(Tree $b).state.window
    Stop-Owned $b
    $old=Get-Content -Raw -Encoding UTF8 $saved.path | ConvertFrom-Json; $old.PSObject.Properties.Remove('shells')
    if (-not [string]::Equals($old.workspaces[0].cwd,$directory,[StringComparison]::Ordinal)) {throw 'Launch stored a relative working directory'}
    [IO.File]::WriteAllText($saved.path,($old | ConvertTo-Json -Depth 50),(New-Object Text.UTF8Encoding($false)))
    $b=Start-Owned @('--restore-window',$legacyWindow); Assert-Shell $b $legacy 'powershell' 'powershell.exe' | Out-Null
    $future=New-Tab $b @(); Assert-Shell $b $future 'cmd' 'cmd.exe' | Out-Null
    $evidence.checks+=@{name='legacy_checkpoint_defaults_to_original_powershell_while_new_tabs_use_shared_cmd_default'}

    if (@($profiles.profiles | Where-Object {$_.program -eq 'pwsh' -and $_.available}).Count) {
        $pwsh=New-Tab $b @('--shell','pwsh'); Assert-Shell $b $pwsh 'pwsh' 'pwsh.exe' | Out-Null
        $evidence.checks+=@{name='installed_powershell7_profile_starts_and_reports_cwd'}
    } else {$evidence.pwsh='Not installed/discoverable on this machine; actual PowerShell 7 remains pending.'}
    foreach ($owned in @($a,$b)) {Tree $owned | Out-Null;Stop-Owned $owned}
    $evidence.status='passed_shell_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
    foreach ($owned in $hosts) {if (-not $owned.process.HasExited) {$owned.process.Kill();$owned.process.WaitForExit()}}
    $evidence.finished=(Get-Date).ToString('o')
    $evidence | ConvertTo-Json -Depth 30 | Set-Content -Encoding UTF8 (Join-Path $directory 'shells.json')
}
$evidence | ConvertTo-Json -Depth 30
