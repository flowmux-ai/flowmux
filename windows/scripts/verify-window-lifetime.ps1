# SPDX-License-Identifier: GPL-3.0-or-later
# Owned hidden processes only; never writes PATH/registry or sends desktop input.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",[switch]$ExpectCoupled)
if (-not $env:FLOWMUX_TEST_ARTIFACT_ROOT) { throw 'Run this verifier through windows/scripts/run-check.ps1 so temporary artifacts are cleaned automatically.' }
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path
$gui=Join-Path $BuildDirectory 'flowmux.exe'; $control=Join-Path $BuildDirectory 'flowmuxctl.exe'
$console=Join-Path $BuildDirectory 'flowmux.com'
$doctor=(& $control doctor | ConvertFrom-Json)
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) {throw 'A working debug build is required; no host was launched.'}
Copy-Item (Join-Path $BuildDirectory 'flowmux-command.exe') $console
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs')
$directory=Join-Path $env:FLOWMUX_TEST_ARTIFACT_ROOT ('window-lifetime-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory) | Out-Null; $directory=(Resolve-Path $directory).Path
$cwd=Join-Path $directory '한글 한 😀 space & #';[IO.Directory]::CreateDirectory($cwd)|Out-Null
$hosts=New-Object 'System.Collections.Generic.List[object]'
$evidence=[ordered]@{started=(Get-Date).ToString('o');mode='background';checks=@();hosts=@();pathext=$env:PATHEXT}
function Invoke-Program([string]$File,[string[]]$Arguments,[int]$Expected=0) {
    $p=[CliProbe]::Start($File,$Arguments,$cwd,$directory)
    try {
        $out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
        if (-not $p.WaitForExit(30000)) {$p.Kill();$p.WaitForExit();throw 'Owned command exceeded 30 seconds; not retried'}
        if (-not $out.Wait(5000) -or -not $err.Wait(5000)) {throw 'A descendant retained the command output pipe after process exit'}
        $result=@{code=$p.ExitCode;stdout=$out.Result;stderr=$err.Result}
        if ($result.code -ne $Expected) {throw "Unexpected exit $($result.code) for $File $Arguments : $($result.stderr)"}
        return $result
    } finally {$p.Dispose()}
}
function Request($Owned,[string[]]$Arguments,[string]$File=$console) {
    if (-not $Owned.pipe) {throw 'Owned pipe missing; refusing discovery fallback'}
    return ((Invoke-Program $File (@('--pipe',$Owned.pipe,'--json')+$Arguments)).stdout | ConvertFrom-Json)
}
function Tree($Owned) {
    $tree=Request $Owned @('tree')
    $window=[IntPtr]([long]$tree.window_handle)
    if (-not $tree.background_testing -or [CliProbe]::IsWindowVisible($window) -or [CliProbe]::GetForegroundWindow() -eq $window) {throw 'Owned host became visible or foreground'}
    return $tree
}
function Start-Owned([bool]$ViaConsole,[string[]]$Arguments) {
    $launch=(Get-Date).ToUniversalTime()
    if ($ViaConsole) {
        $receipt=(Invoke-Program $console (@('--json')+$Arguments)).stdout | ConvertFrom-Json
        $p=Get-Process -Id $receipt.spawned_pid
        if ($p.Path -ne $gui -or $p.StartTime.ToUniversalTime() -lt $launch) {throw 'Console receipt does not identify the new sibling GUI'}
        $owned=@{process=$p;pipe=$null;via='console'}
    } else {
        $p=[CliProbe]::Start($gui,$Arguments,$cwd,$directory)
        $owned=@{process=$p;pipe=$null;via='gui';out=$p.StandardOutput.ReadToEndAsync();err=$p.StandardError.ReadToEndAsync()}
    }
    $hosts.Add($owned)
    $discovery=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($p.Id).json"; $deadline=(Get-Date).AddSeconds(25)
    do {
        if ($p.HasExited -or (Get-Date) -gt $deadline) {throw 'Owned GUI failed to become ready'}
        if ((Test-Path $discovery) -and (Get-Item $discovery).LastWriteTimeUtc -ge $launch) {
            $record=Get-Content -Raw $discovery | ConvertFrom-Json
            if ($record.pid -ne $p.Id) {throw 'Wrong discovery owner'}
            $owned.pipe=$record.pipe;break
        }
        Start-Sleep -Milliseconds 50
    } while ($true)
    if ((Request $owned @('identify')).pid -ne $p.Id) {throw 'Wrong pipe target'}
    do {
        $tree=Tree $owned
        if (@($tree.surfaces|Where-Object {-not $_.ready}).Count -eq 0) {break}
        if ((Get-Date) -gt $deadline) {throw 'Owned surface readiness timeout'}
        Start-Sleep -Milliseconds 50
    } while ($true)
    $evidence.hosts+=@{pid=$p.Id;pipe=$owned.pipe;via=$owned.via}
    return $owned
}
function Send-Line($Owned,[string]$Pane,[string]$Line) {
    Request $Owned @('send-keys',$Pane,$Line)|Out-Null
    Request $Owned @('send-key','Enter','--pane',$Pane)|Out-Null
}
function Wait-Receipt([string]$Path,[datetime]$Started) {
    $deadline=(Get-Date).AddSeconds(25)
    do {
        if (Test-Path $Path) {
            try {$receipt=Get-Content -Raw $Path|ConvertFrom-Json} catch {$receipt=$null}
            if ($receipt.spawned_pid) {break}
        }
        if ((Get-Date) -gt $deadline) {throw 'Launch receipt did not arrive'}
        Start-Sleep -Milliseconds 50
    } while ($true)
    $p=Get-Process -Id $receipt.spawned_pid
    if ($p.Path -ne $gui -or $p.StartTime.ToUniversalTime() -lt $Started) {throw 'Receipt does not identify our new GUI'}
    $null=$p.Handle
    $owned=@{process=$p;pipe=$null;via='terminal'};$hosts.Add($owned)
    $discovery=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($p.Id).json"
    do {
        if ($p.HasExited -or (Get-Date) -gt $deadline) {throw 'New GUI failed before discovery'}
        if ((Test-Path $discovery) -and (Get-Item $discovery).LastWriteTimeUtc -ge $Started) {
            $record=Get-Content -Raw $discovery|ConvertFrom-Json
            if ($record.pid -ne $p.Id) {throw 'New GUI discovery differs'}
            $owned.pipe=$record.pipe;break
        }
        Start-Sleep -Milliseconds 50
    } while ($true)
    do {
        $tree=Tree $owned
        if (@($tree.surfaces|Where-Object {-not $_.ready}).Count -eq 0) {break}
        if ((Get-Date) -gt $deadline) {throw 'New GUI terminal not ready'}
        Start-Sleep -Milliseconds 50
    } while ($true)
    if ((Request $owned @('identify')).pid -ne $p.Id) {throw 'New GUI target differs'}
    $evidence.hosts+=@{pid=$p.Id;pipe=$owned.pipe;via='terminal'}
    return $owned
}

function Ps-String([string]$Value) { return "'"+$Value.Replace("'","''")+"'" }
function Wait-Json([string]$Path) {
    $deadline=(Get-Date).AddSeconds(15)
    do {
        if (Test-Path $Path) {
            try {$value=Get-Content -Raw $Path|ConvertFrom-Json} catch {$value=$null}
            if ($value) {return $value}
        }
        if ((Get-Date) -gt $deadline) {throw ('Owned JSON not written: '+$Path)}
        Start-Sleep -Milliseconds 50
    } while ($true)
}
function Managed-Launch([string]$Program,[string[]]$Arguments,[string]$From,[string]$Receipt) {
    $argsText=(@($Arguments|ForEach-Object {Ps-String $_}) -join ',')
    return 'if (-not (''CliProbe'' -as [type])) {Add-Type -Path '+(Ps-String (Join-Path $PSScriptRoot 'CliProbe.cs'))+'}; $p=[CliProbe]::Start('+(Ps-String $Program)+',[string[]]@('+$argsText+'),'+(Ps-String $From)+','+(Ps-String $directory)+'); $r=$p.StandardOutput.ReadToEnd(); $err=$p.StandardError.ReadToEnd(); $p.WaitForExit(); [IO.File]::WriteAllText('+(Ps-String $Receipt)+',$r,[Text.Encoding]::UTF8); [IO.File]::WriteAllText('+(Ps-String ($Receipt+'.stderr'))+',$err,[Text.Encoding]::UTF8); $p.Dispose()'
}
function Capture-Context($Owned,[string]$Path) {
    $id=Request $Owned @('identify')
    $line='$i=& '+(Ps-String $console)+' --json identify | ConvertFrom-Json; $x=[ordered]@{value=$env:FLOWMUX_LIFETIME_PROBE;cwd=(Get-Location).ProviderPath;pipe=$env:FLOWMUX_PIPE_NAME;surface=$env:FLOWMUX_SURFACE_ID;hostPid=$i.pid;reportedSurface=$i.surface}; [IO.File]::WriteAllText('+(Ps-String $Path)+',($x|ConvertTo-Json -Compress),[Text.Encoding]::UTF8)'
    Send-Line $Owned $id.pane $line
    return Wait-Json $Path
}
function Raw-Reject($Owned,$Body) {
    $stream=[IO.Pipes.NamedPipeClientStream]::new('.',$Owned.pipe.Substring(9),[IO.Pipes.PipeDirection]::InOut,[IO.Pipes.PipeOptions]::Asynchronous,[Security.Principal.TokenImpersonationLevel]::Identification)
    try {
        $stream.Connect(3000)
        $writer=[IO.StreamWriter]::new($stream,(New-Object Text.UTF8Encoding($false)),4096,$true)
        $reader=[IO.StreamReader]::new($stream,(New-Object Text.UTF8Encoding($false)),$false,4096,$true)
        try {
            $writer.WriteLine(($Body|ConvertTo-Json -Depth 20 -Compress));$writer.Flush()
            $read=$reader.ReadLineAsync();if (-not $read.Wait(15000)) {throw 'Owned rejection timed out'}
            $result=$read.Result|ConvertFrom-Json
            if (-not $result.error) {throw 'Malformed launch unexpectedly succeeded'}
            return $result.error
        } finally {$writer.Dispose();$reader.Dispose()}
    } finally {$stream.Dispose()}
}
function Units([string]$Value) {return ,([int[]]@($Value.ToCharArray()|ForEach-Object {[int]$_}))}
try {
    $a=Start-Owned $false @('--temporary',('--cwd='+$cwd))
    Request $a @('new-tab')|Out-Null
    $source=Request $a @('identify')
    $receipt=Join-Path $directory 'receipt.json'
    $started=(Get-Date).ToUniversalTime()
    $line='$r=& '''+$console.Replace("'","''")+''' --json --new-window --temporary --shell=cmd --cwd '''+$cwd.Replace("'","''")+'''; [IO.File]::WriteAllText('''+$receipt.Replace("'","''")+''',($r -join "`n"),[Text.Encoding]::UTF8)'
    Send-Line $a $source.pane $line
    $b=Wait-Receipt $receipt $started
    $sourcePid=((Tree $a).surfaces|Where-Object {$_.id -eq $source.surface}).pid
    $childPid=(Tree $b).surfaces[0].pid
    $evidence.before=@{sourceHost=$a.process.Id;sourceSurface=$source.surface;sourceShell=$sourcePid;childHost=$b.process.Id;childShell=$childPid}
    Request $a @('close-tab',$source.surface)|Out-Null
    $closed=$b.process.WaitForExit(1500)
    $evidence.after=@{childExited=$closed;sourceShellAlive=[bool](Get-Process -Id $sourcePid -ErrorAction SilentlyContinue);childShellAlive=[bool](Get-Process -Id $childPid -ErrorAction SilentlyContinue)}
    if ($ExpectCoupled) {
        if (-not $closed) {throw 'Expected old coupled lifetime did not reproduce'}
        $evidence.status='reproduced_coupled_lifetime'
    } else {
        if ($closed) {throw 'Closing source tab killed the independently launched GUI'}
        Tree $b|Out-Null

        if ($evidence.after.sourceShellAlive -or -not $evidence.after.childShellAlive) {throw 'Shell ownership after tab close differs'}
        $evidence.checks+=@{name='closing_source_tab_terminates_its_shell_but_preserves_new_gui_and_shell';passed=$true}

        $source=Request $a @('identify')
        $sourceTree=Tree $a;$sourceShell=$sourceTree.surfaces[0].pid
        $parent=Join-Path $cwd 'parent 한글';$relative='child 한 😀 & #';$childCwd=Join-Path $parent $relative
        [IO.Directory]::CreateDirectory($childCwd)|Out-Null
        $value="ENV_한글 한 😀 & #`nsecond line"
        $receipt=Join-Path $directory 'gui-receipt.json';$started=(Get-Date).ToUniversalTime()
        $encodedValue=[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($value))
        $line='$env:FLOWMUX_LIFETIME_PROBE=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('+(Ps-String $encodedValue)+')); Set-Location -LiteralPath '+(Ps-String $parent)+'; '
        $line+=Managed-Launch $gui @('--json','--new-window','--temporary','--shell=powershell','--shell-arg=-NoProfile','--cwd',$relative) $parent $receipt
        Send-Line $a $source.pane $line
        $c=Wait-Receipt $receipt $started
        $context=Capture-Context $c (Join-Path $directory 'context-before.json')
        if (-not [string]::Equals($context.value,$value,[StringComparison]::Ordinal) -or $context.cwd -cne $childCwd -or $context.pipe -ne $c.pipe -or $context.hostPid -ne $c.process.Id -or $context.surface -ne $context.reportedSurface) {throw 'Delegated GUI lost caller cwd/env or retained old routing IDs'}
        $evidence.checks+=@{name='direct_gui_entry_brokers_relative_cwd_unicode_environment_and_fresh_child_cli_context';passed=$true;cwd=$context.cwd;surface=$context.surface}

        # Shell resolution must use the caller's updated PATH, not the broker's original environment.
        $bin=Join-Path $parent 'bin 한글';[IO.Directory]::CreateDirectory($bin)|Out-Null
        $probe=Join-Path $bin 'flowmux-context-probe.exe'
        Add-Type -Path (Join-Path $PSScriptRoot 'ShellProbe.cs') -OutputAssembly $probe -OutputType ConsoleApplication
        $record=Join-Path $directory 'probe.txt';$receipt=Join-Path $directory 'path-receipt.json';$started=(Get-Date).ToUniversalTime()
        $probeArgs=@('', '한글 한 😀 quote" slash\', 'C:\last space\')
        $launchArgs=@('--json','--new-window','--temporary','--shell=flowmux-context-probe','--cwd',$relative,('--shell-arg='+$record))
        foreach ($arg in $probeArgs) {$launchArgs+=('--shell-arg='+$arg)}
        $line='$env:PATH='+(Ps-String ($bin+';'))+'+$env:PATH; '+(Managed-Launch $console $launchArgs $parent $receipt)
        # Managed ProcessStartInfo preserves argv inside the source terminal job;
        # PowerShell 5's native embedded-quote marshalling is not this assertion.
        Send-Line $a $source.pane $line
        $d=Wait-Receipt $receipt $started
        $deadline=(Get-Date).AddSeconds(10)
        while (-not (Test-Path $record)) {if ((Get-Date) -gt $deadline) {throw 'Caller-PATH shell did not run'};Start-Sleep -Milliseconds 50}
        $fields=@([IO.File]::ReadAllLines($record)|ForEach-Object {[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($_))})
        if ($fields[0] -cne $childCwd -or $fields.Length -ne $probeArgs.Length+2) {throw 'Caller PATH/cwd/argument count differs'}
        for ($i=0;$i -lt $probeArgs.Length;$i++) {if (-not [string]::Equals($fields[$i+2],$probeArgs[$i],[StringComparison]::Ordinal)) {throw ('Broker changed argv at '+$i)}}
        $dt=Tree $d
        if ($dt.surfaces[0].startup_error) {throw 'Caller PATH was not used for shell startup'}
        $evidence.checks+=@{name='caller_updated_path_selects_custom_shell_and_preserves_empty_quoted_trailing_unicode_arguments';passed=$true;cwd=$fields[0]}

        # Start another generation from the retained CMD host, then close that host.
        $bi=Request $b @('identify');$receipt=Join-Path $directory 'nested-receipt.json';$started=(Get-Date).ToUniversalTime()
        $line='"'+$console+'" --json --new-window --temporary --shell=cmd --cwd "'+$cwd+'" > "'+$receipt+'"'
        Send-Line $b $bi.pane $line
        $e=Wait-Receipt $receipt $started
        $eShell=(Tree $e).surfaces[0].pid
        Request $b @('quit','--discard-state')|Out-Null
        if (-not $b.process.WaitForExit(10000) -or $e.process.HasExited -or (Tree $e).surfaces[0].pid -ne $eShell) {throw 'Nested new window did not survive source window close'}
        $evidence.checks+=@{name='cmd_launch_and_nested_new_window_survive_clean_source_window_close';passed=$true}

        # Known stale context must be rejected, never redirected or launched locally.
        $oldPipe=$env:FLOWMUX_PIPE_NAME;$oldSurface=$env:FLOWMUX_SURFACE_ID
        try {
            foreach ($file in @($console,$gui)) {
                $env:FLOWMUX_PIPE_NAME='\\.\pipe\flowmux-999999-'+[guid]::NewGuid();$env:FLOWMUX_SURFACE_ID=[guid]::NewGuid().ToString()
                $r=Invoke-Program $file @('--json','--temporary') 1
                if ($r.stdout -or -not ($r.stderr|ConvertFrom-Json).error) {throw 'Missing broker did not reject launch'}
                $env:FLOWMUX_PIPE_NAME=$a.pipe;$env:FLOWMUX_SURFACE_ID=$evidence.before.sourceSurface
                $r=Invoke-Program $file @('--json','--temporary') 1
                if ($r.stdout -or -not ($r.stderr|ConvertFrom-Json).error.Contains('Calling terminal no longer exists')) {throw 'Stale source did not reject launch'}
            }
        } finally {$env:FLOWMUX_PIPE_NAME=$oldPipe;$env:FLOWMUX_SURFACE_ID=$oldSurface}
        $evidence.checks+=@{name='stale_pipe_and_closed_source_surface_reject_without_fallback_for_both_launchers';passed=$true}

        $ctx=@{arguments=@();directory=(Units $parent);environment=(Units "FLOWMUX_TEST_BACKGROUND=1`0`0")}
        $request=@{method='launch_window';caller_surface=$source.surface;context=$ctx}
        $errors=@()
        $ctx.arguments=,(Units 'tree');$errors+=Raw-Reject $a $request
        $ctx.arguments=@();$ctx.directory=Units 'relative';$errors+=Raw-Reject $a $request
        $ctx.directory=Units $parent;$ctx.environment=@(65,0,0);$errors+=Raw-Reject $a $request
        $ctx.environment=Units "FLOWMUX_TEST_BACKGROUND=1`0`0";$ctx.arguments=,(Units "bad`0arg");$errors+=Raw-Reject $a $request
        $expected=@('Only window launch options can be delegated','Launch directory must be an existing absolute path','malformed launch environment entry','launch arguments contain NUL or exceed the Windows limit')
        for ($i=0;$i -lt $expected.Length;$i++) {if ($errors[$i] -cne $expected[$i]) {throw ('Wrong rejection boundary: '+$errors[$i])}}
        $evidence.checks+=@{name='internal_launch_rejects_cli_commands_relative_directories_bad_environment_and_nul';passed=$true;errors=$errors}

        $before=Tree $c;$cShell=$before.surfaces[0].pid
        $a.process.Kill();if (-not $a.process.WaitForExit(10000)) {throw 'Owned source host did not terminate'}
        if (Get-Process -Id $sourceShell -ErrorAction SilentlyContinue) {throw 'Source terminal shell survived its owned host kill'}
        foreach ($owned in @($c,$d,$e)) {if ($owned.process.HasExited) {throw 'Independent new GUI died with original host'};Tree $owned|Out-Null}
        if ((Tree $c).surfaces[0].pid -ne $cShell) {throw 'Surviving window replaced its shell'}
        $context=Capture-Context $c (Join-Path $directory 'context-after.json')
        if ($context.hostPid -ne $c.process.Id -or $context.value -cne $value) {throw 'New window CLI/env failed after original host exit'}
        $evidence.checks+=@{name='forced_original_host_exit_cleans_source_shell_and_preserves_independent_windows_processes_and_cli';passed=$true;retainedShell=$cShell}
        foreach ($owned in @($c,$d,$e)) {Request $owned @('quit','--discard-state')|Out-Null;if (-not $owned.process.WaitForExit(10000)) {throw 'Owned child did not quit'}}
        $evidence.status='passed_window_lifetime_subset'

    }
} catch {
    $evidence.status='failed';$evidence.error=$_.Exception.Message
    foreach ($owned in $hosts) {
        if (-not $owned.process.HasExited -and $owned.pipe) {
            try {(Request $owned @('read-screen')).text|Set-Content -Encoding UTF8 (Join-Path $directory ('failure-screen-'+$owned.process.Id+'.txt'))} catch {}
        }
    }
    throw
}
finally {
    # A GUI may already exist when a caller fails before receiving its receipt.
    # Only inspect children of our known GUI PIDs and the exact test executable.
    $parentIds=@($hosts|ForEach-Object {$_.process.Id})
    foreach ($child in @(Get-CimInstance Win32_Process -Filter "Name='flowmux.exe'"|Where-Object {$_.ParentProcessId -in $parentIds -and $_.ExecutablePath -eq $gui -and $_.CreationDate -ge [datetime]$evidence.started})) {
        if ($child.ProcessId -notin $parentIds) {
            $record=Join-Path $env:LOCALAPPDATA ('flowmux\windows\instances\'+$child.ProcessId+'.json')
            if (Test-Path $record) {
                try {
                    $pipe=(Get-Content -Raw $record|ConvertFrom-Json).pipe
                    $identity=Request @{pipe=$pipe} @('identify')
                    if ($identity.pid -eq $child.ProcessId -and $identity.cwd.StartsWith($directory,[StringComparison]::OrdinalIgnoreCase)) {
                        Request @{pipe=$pipe} @('quit','--discard-state')|Out-Null
                        $evidence.cleanedUnreceipted+=@($child.ProcessId)
                    }
                } catch {}
            }
        }
    }
    foreach ($owned in $hosts) {if (-not $owned.process.HasExited) {$owned.process.Kill();$owned.process.WaitForExit()};$owned.process.Dispose()}
    $evidence.finished=(Get-Date).ToString('o')
    if($evidence.status -eq 'failed'){$evidence|ConvertTo-Json -Depth 20|Set-Content -Encoding UTF8 (Join-Path $directory 'window-lifetime.json')}
}
[ordered]@{status=$evidence.status;checks=$evidence.checks.Count}|ConvertTo-Json -Compress
