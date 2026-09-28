# SPDX-License-Identifier: GPL-3.0-or-later
# Owned hidden processes only; never writes PATH/registry or sends desktop input.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path
$gui=Join-Path $BuildDirectory 'flowmux.exe'; $control=Join-Path $BuildDirectory 'flowmuxctl.exe'
$console=Join-Path $BuildDirectory 'flowmux.com'
$doctor=(& $control doctor | ConvertFrom-Json)
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) {throw 'A working debug build is required; no host was launched.'}
Copy-Item (Join-Path $BuildDirectory 'flowmux-command.exe') $console
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs')
$directory=Join-Path $(if ($env:FLOWMUX_TEST_ARTIFACT_ROOT) { $env:FLOWMUX_TEST_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\dist\evidence' }) ('entrypoints-'+[guid]::NewGuid())
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
function Encoded([string]$Script) {[Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($Script))}
function Wait-Text($Owned,[string]$Surface,[string]$Text) {
    $deadline=(Get-Date).AddSeconds(10)
    do {
        $result=Request $Owned @('read-screen','--surface',$Surface)
        if ($result.text.Contains($Text)) {return}
        Start-Sleep -Milliseconds 50
    } while ((Get-Date) -lt $deadline)
    throw "Owned terminal did not print $Text"
}
try {
    foreach ($file in @($gui,$console,$control)) {
        foreach ($arg in @('--help','--version')) {
            $r=Invoke-Program $file @($arg)
            if (-not $r.stdout -or $r.stderr) {throw 'Help/version did not use stdout exclusively'}
        }
        $r=Invoke-Program $file @('unknown-command') 2
        if ($r.stdout -or -not $r.stderr.Contains('error:')) {throw 'Parse error did not use stderr exclusively'}
        $d=(Invoke-Program $file @('doctor')).stdout | ConvertFrom-Json
        if ($d.webview2 -ne $doctor.webview2) {throw 'Entrypoint doctor result differs'}
    }
    $help=(Invoke-Program $console @('--help')).stdout
    if (-not $help.Contains('--shell') -or -not $help.Contains('read-screen')) {throw 'Unified help omits launch or CLI options'}
    $evidence.checks+=@{name='three_entrypoints_help_version_parse_error_and_doctor_use_standard_streams';passed=$true}

    $missing='\\.\pipe\flowmux-999999-'+[guid]::NewGuid()
    foreach ($file in @($gui,$console,$control)) {
        $r=Invoke-Program $file @('--json','--pipe',$missing,'identify') 1
        if ($r.stdout -or -not ($r.stderr|ConvertFrom-Json).error) {throw 'Runtime error was not JSON on stderr'}
    }
    Invoke-Program $console @('--temporary','tree') 2 | Out-Null
    $evidence.checks+=@{name='runtime_errors_are_nonzero_json_stderr_without_dialogs_and_mixed_launch_cli_is_rejected';passed=$true}

    $a=Start-Owned $false @('--shell=cmd','--temporary',('--cwd='+$cwd))
    $at=Tree $a
    if ($at.surfaces[0].shell.program -ne 'cmd' -or $at.surfaces[0].cwd -cne $cwd) {throw 'Leading --shell or equals cwd routing failed'}
    $b=Start-Owned $true @('--shell=powershell','--temporary',('--cwd='+$cwd))
    $bt=Tree $b
    if ($bt.surfaces[0].shell.program -ne 'powershell' -or $bt.surfaces[0].cwd -cne $cwd) {throw 'Console launcher lost shell or Unicode cwd'}
    $evidence.checks+=@{name='leading_shell_equals_options_and_console_spawn_receipt_start_distinct_hidden_native_shells';passed=$true;cwd=$cwd}

    # These child shells inherit only process-local PATH changes. Registry/user PATH stays untouched.
    $oldPath=$env:PATH
    try {
        $env:PATH=$BuildDirectory+';'+$oldPath
        $ps=Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
        $cmd=Join-Path $env:SystemRoot 'System32\cmd.exe'
        $script='(Get-Command flowmux).Source; flowmux --json --pipe '''+$b.pipe+''' identify; exit $LASTEXITCODE'
        $r=Invoke-Program $ps @('-NoProfile','-EncodedCommand',(Encoded $script))
        $lines=$r.stdout -split "`r?`n",2
        if ($lines[0] -cne $console -or ($lines[1]|ConvertFrom-Json).pid -ne $b.process.Id) {throw 'PowerShell did not select console entry or await JSON'}
        $r=Invoke-Program $cmd @('/D','/C',('flowmux --json --pipe '+$a.pipe+' identify'))
        if (($r.stdout|ConvertFrom-Json).pid -ne $a.process.Id) {throw 'CMD did not await JSON response'}
        Invoke-Program $cmd @('/D','/C',('flowmux --json --pipe '+$missing+' identify')) 1 | Out-Null
        $script='flowmux --json --pipe '''+$missing+''' identify; exit $LASTEXITCODE'
        Invoke-Program $ps @('-NoProfile','-EncodedCommand',(Encoded $script)) 1 | Out-Null
    } finally {$env:PATH=$oldPath}
    $evidence.checks+=@{name='cmd_and_powershell_resolve_bare_flowmux_to_console_and_preserve_exit_codes_and_pipeline_json';passed=$true}

    $name='CLI 한글 한 😀 & " quote';$surface=$bt.surfaces[0].id
    Request $b @('rename-tab',$surface,$name)|Out-Null
    if ((Tree $b).workspaces[0].root -eq $null) {throw 'Tree response missing workspace'}
    $saved=Request $b @('tree') $gui
    if (-not (($saved|ConvertTo-Json -Depth 30).Contains('CLI 한글'))) {throw 'GUI delegation lost Unicode command'}
    Request $b @('new-tab','--shell=cmd')|Out-Null
    if ((Tree $b).surfaces.Count -ne 2 -or (Tree $a).surfaces.Count -ne 1) {throw 'Multi-window delegation affected the wrong host'}
    $pane=(Request $b @('identify')).pane
    # Explicit target uses the original PowerShell surface rather than the new CMD tab.
    Request $b @('focus-tab',$surface)|Out-Null
    $pane=(Request $b @('identify')).pane
    $marker='ENTRY_한글_한_😀'
    Request $b @('send-keys',$pane,("Write-Output '"+$marker+"'"))|Out-Null
    Request $b @('send-key','Enter','--pane',$pane)|Out-Null
    Wait-Text $b $surface $marker
    $plain=(Invoke-Program $console @('--pipe',$b.pipe,'read-screen','--surface',$surface)).stdout
    if (-not $plain.Contains($marker) -or $plain.StartsWith('{')) {throw 'Plain read-screen output differs from JSON text'}
    $evidence.checks+=@{name='unicode_argv_plain_and_json_screen_output_and_multiwindow_commands_preserve_target';passed=$true}

    $oldPath=$env:PATH
    try {
        $env:PATH=$BuildDirectory+';'+$oldPath
        $file=Join-Path $directory 'redirect.json';$errorFile=Join-Path $directory 'redirect.stderr'
        $line='flowmux --json --pipe '+$a.pipe+' identify > "'+$file+'" 2> "'+$errorFile+'"'
        Invoke-Program $cmd @('/D','/C',$line)|Out-Null
        if ((Get-Content -Raw $file|ConvertFrom-Json).pid -ne $a.process.Id -or (Get-Item $errorFile).Length -ne 0) {throw 'CMD file redirection failed'}
    } finally {$env:PATH=$oldPath}
    $evidence.checks+=@{name='cmd_file_redirection_keeps_json_and_stderr_separate';passed=$true}
    $aPane=(Request $a @('identify')).pane
    Request $a @('send-keys',$aPane,'cls')|Out-Null
    Request $a @('send-key','Enter','--pane',$aPane)|Out-Null
    $line='start "" /b /wait "'+$gui+'" --json --pipe '+$b.pipe+' identify >NUL & echo NUL_REDIRECT_DONE'
    Request $a @('send-keys',$aPane,$line)|Out-Null
    Request $a @('send-key','Enter','--pane',$aPane)|Out-Null
    Wait-Text $a $at.surfaces[0].id 'NUL_REDIRECT_DONE'
    # A trailing prompt establishes that the child exited; merely finding the
    # echoed command line would not prove it ran.
    $deadline=(Get-Date).AddSeconds(10)
    do {
        $screen=(Request $a @('read-screen','--surface',$at.surfaces[0].id)).text
        if ([regex]::Matches($screen,'NUL_REDIRECT_DONE').Count -ge 2) {break}
        if ((Get-Date) -gt $deadline) {throw 'Redirected GUI CLI did not return to CMD'}
        Start-Sleep -Milliseconds 50
    } while ($true)
    if ($screen.Contains('"platform": "windows"')) {throw 'GUI CLI replaced NUL with its parent console'}
    $evidence.checks+=@{name='gui_cli_preserves_nul_redirection_inside_real_owned_conpty';passed=$true}

    $probe=Join-Path $cwd 'probe 한글.exe';$record=Join-Path $directory 'argv.txt'
    Add-Type -Path (Join-Path $PSScriptRoot 'ShellProbe.cs') -OutputAssembly $probe -OutputType ConsoleApplication
    $arguments=@('', '한글 한 😀 quote" slash\', 'C:\trailing space\', '& echo should-be-an-argument')
    $launchArgs=@('--temporary',('--cwd='+$cwd),('--shell='+$probe),('--shell-arg='+$record))
    foreach ($argument in $arguments) {$launchArgs+=('--shell-arg='+$argument)}
    $c=Start-Owned $true $launchArgs
    $deadline=(Get-Date).AddSeconds(10)
    while (-not (Test-Path $record)) {if ((Get-Date) -gt $deadline) {throw 'Launch argv probe did not write'};Start-Sleep -Milliseconds 50}
    $values=@([IO.File]::ReadAllLines($record)|ForEach-Object {[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($_))})
    if ($values[0] -cne $cwd -or $values.Length -ne $arguments.Length+2) {throw 'Launch argv count/cwd differs'}
    for ($i=0;$i -lt $arguments.Length;$i++) {if (-not [string]::Equals($values[$i+2],$arguments[$i],[StringComparison]::Ordinal)) {throw "Launch argv differs at $i"}}
    $evidence.checks+=@{name='console_to_gui_to_conpty_preserves_unicode_executable_empty_quotes_backslashes_and_literal_metacharacters';passed=$true;argv=$arguments}

    $isolated=Join-Path $directory 'missing-gui 한글';[IO.Directory]::CreateDirectory($isolated)|Out-Null
    $standalone=Join-Path $isolated 'flowmux.com';Copy-Item $console $standalone
    $r=Invoke-Program $standalone @('--json','--temporary') 1
    if (-not ($r.stderr|ConvertFrom-Json).error) {throw 'Missing sibling did not report JSON error'}
    [IO.File]::WriteAllText((Join-Path $isolated 'flowmux.exe'),'not an executable')
    $r=Invoke-Program $standalone @('--json','--temporary') 1
    if (-not ($r.stderr|ConvertFrom-Json).error) {throw 'Invalid sibling did not report JSON error'}
    $evidence.checks+=@{name='missing_and_invalid_sibling_gui_return_errors_without_system_dialogs';passed=$true}

    # Deterministic broken stdout: the owned server waits until the client has
    # submitted its request, then closes its stdout reader before replying.
    foreach ($file in @($gui,$console,$control)) {
        $name='flowmux-'+$PID+'-'+[guid]::NewGuid()
        $server=[IO.Pipes.NamedPipeServerStream]::new($name,[IO.Pipes.PipeDirection]::InOut,1,[IO.Pipes.PipeTransmissionMode]::Byte,[IO.Pipes.PipeOptions]::Asynchronous)
        $p=$null
        try {
            $accepted=$server.WaitForConnectionAsync()
            $p=[CliProbe]::Start($file,@('--pipe',('\\.\pipe\'+$name),'read-screen'),$cwd,$directory)
            $err=$p.StandardError.ReadToEndAsync()
            if (-not $accepted.Wait(5000)) {throw 'Owned diagnostic server not connected'}
            $reader=[IO.StreamReader]::new($server,(New-Object Text.UTF8Encoding($false)),$false,4096,$true)
            $read=$reader.ReadLineAsync();if (-not $read.Wait(5000)) {throw 'CLI request not received'}
            $p.StandardOutput.Dispose()
            $bytes=[Text.Encoding]::UTF8.GetBytes('{"text":"한글 output"}'+"`n")
            $server.Write($bytes,0,$bytes.Length);$server.Flush()
            if (-not $p.WaitForExit(10000)) {throw 'Broken stdout did not terminate'}
            if ($p.ExitCode -ne 1 -or -not $err.Result.Contains('writing CLI stdout') -or $err.Result.Contains('panicked')) {throw ('Broken stdout result: '+$err.Result)}
        } finally {
            $server.Dispose()
            if ($p) {if (-not $p.HasExited) {$p.Kill();$p.WaitForExit()};$p.Dispose()}
        }
    }
    $evidence.checks+=@{name='closed_stdout_returns_error_without_panic_or_retransmission_for_all_entrypoints';passed=$true}
    foreach ($owned in @($a,$b,$c)) {Tree $owned|Out-Null;Request $owned @('quit','--discard-state')|Out-Null;if (-not $owned.process.WaitForExit(10000)) {throw 'Owned host did not quit'}}
    $evidence.status='passed_entrypoint_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
    foreach ($owned in $hosts) {if (-not $owned.process.HasExited) {$owned.process.Kill();$owned.process.WaitForExit()};$owned.process.Dispose()}
    $evidence.finished=(Get-Date).ToString('o')
    $evidence|ConvertTo-Json -Depth 20|Set-Content -Encoding UTF8 (Join-Path $directory 'entrypoints.json')
}
$evidence|ConvertTo-Json -Depth 20
