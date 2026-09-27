# SPDX-License-Identifier: GPL-3.0-or-later
# All hosts and children are owned/hidden. No OS input, clipboard, or foreground mutation.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug",[switch]$Baseline)
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path
$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor=& $cli doctor | ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) {throw 'A working debug build is required; no host was launched.'}
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs')
$directory=Join-Path $PSScriptRoot ('..\dist\evidence\keys-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$probe=Join-Path $directory 'key-probe.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'KeyProbe.cs') -OutputAssembly $probe -OutputType ConsoleApplication
$raw=Join-Path $directory 'input.bin';$control=Join-Path $directory 'mode.txt'
$trace=Join-Path $directory 'input.jsonl';$pipeName=$null;$process=$null
$traceLinesBefore=0
$expected=New-Object 'System.Collections.Generic.List[byte]'
$evidence=[ordered]@{started=(Get-Date).ToString('o');mode='background';checks=@();clipboardAccess=$false;desktopInput=$false}
function Program([string[]]$Arguments,[int]$Exit=0) {
    $p=[CliProbe]::Start($cli,$Arguments,$directory,$directory)
    try {
        $out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
        if (-not $p.WaitForExit(30000)) {$p.Kill();$p.WaitForExit();throw 'Owned CLI timed out; not retried'}
        if (-not $out.Wait(3000) -or -not $err.Wait(3000)) {throw 'Owned CLI pipes did not close'}
        if ($p.ExitCode -ne $Exit) {throw "CLI exit $($p.ExitCode): $($err.Result)"}
        if ($Exit -eq 0) {return ($out.Result|ConvertFrom-Json)}
        return ($err.Result|ConvertFrom-Json)
    } finally {$p.Dispose()}
}
function Request([string[]]$Arguments,[int]$Exit=0) {
    if (-not $script:pipeName) {throw 'Owned pipe required; refusing discovery fallback'}
    return Program (@('--pipe',$script:pipeName,'--json')+$Arguments) $Exit
}
function Raw-Request($Body) {
    if (-not $script:pipeName) {throw 'Owned pipe required'}
    $stream=[IO.Pipes.NamedPipeClientStream]::new('.',$script:pipeName.Substring(9),[IO.Pipes.PipeDirection]::InOut,[IO.Pipes.PipeOptions]::Asynchronous,[Security.Principal.TokenImpersonationLevel]::Identification)
    try {
        $stream.Connect(3000)
        $writer=[IO.StreamWriter]::new($stream,(New-Object Text.UTF8Encoding($false)),4096,$true)
        $reader=[IO.StreamReader]::new($stream,(New-Object Text.UTF8Encoding($false)),$false,4096,$true)
        try {
            $writer.WriteLine(($Body|ConvertTo-Json -Depth 10 -Compress));$writer.Flush()
            $read=$reader.ReadLineAsync();if (-not $read.Wait(20000)) {throw 'Owned paste request timed out; not retried'}
            return ($read.Result|ConvertFrom-Json)
        } finally {$writer.Dispose();$reader.Dispose()}
    } finally {$stream.Dispose()}
}
function Tree {
    $tree=Request @('tree');$window=[IntPtr]([long]$tree.window_handle)
    if (-not $tree.background_testing -or [CliProbe]::IsWindowVisible($window) -or [CliProbe]::GetForegroundWindow() -eq $window) {throw 'Test host became visible or foreground'}
    return $tree
}
function Bytes([string]$Path) {
    if (-not (Test-Path $Path)) {return ,([byte[]]@())}
    $stream=[IO.File]::Open($Path,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::ReadWrite)
    $memory=New-Object IO.MemoryStream
    try {$stream.CopyTo($memory);return ,$memory.ToArray()} finally {$stream.Dispose();$memory.Dispose()}
}
function Hex([byte[]]$Bytes) {return [BitConverter]::ToString($Bytes)}
function Hash([byte[]]$Bytes) {
    $sha=[Security.Cryptography.SHA256]::Create()
    try {return ([BitConverter]::ToString($sha.ComputeHash($Bytes))).Replace('-','').ToLowerInvariant()} finally {$sha.Dispose()}
}
function Wait-Screen([string]$Surface,[string]$Text) {
    $deadline=(Get-Date).AddSeconds(20)
    do {
        $screen=Request @('read-screen','--surface',$Surface)
        if ($screen.text.Contains($Text)) {return $screen}
        if ((Get-Date) -gt $deadline) {throw "Owned probe did not print $Text"}
        Start-Sleep -Milliseconds 30
    } while ($true)
}
function Set-Mode([string]$Surface,[bool]$Enabled) {
    $id=[guid]::NewGuid().ToString('N');$value=if ($Enabled) {'on'} else {'off'}
    $temp=$control+'.tmp';[IO.File]::WriteAllText($temp,($id+':'+$value),[Text.Encoding]::UTF8)
    if (-not [CliProbe]::MoveFileEx($temp,$control,1)) {throw 'Owned key control replacement failed'}
    return (Wait-Screen $Surface ('KEY_MODE_'+$id)).sequence
}
function Check-Bytes {
    $deadline=(Get-Date).AddSeconds(20)
    do {
        $actual=Bytes $script:raw
        if ($actual.Length -ge $script:expected.Count) {break}
        if ((Get-Date) -gt $deadline) {throw "Console input stopped at $($actual.Length), expected $($script:expected.Count) bytes"}
        Start-Sleep -Milliseconds 20
    } while ($true)
    if ((Hex $actual) -cne (Hex $script:expected.ToArray())) {throw "Console bytes differ: actual $($actual.Length), expected $($script:expected.Count)"}
    $wire=New-Object 'System.Collections.Generic.List[byte]'
    if (Test-Path $script:trace) {
        foreach ($line in ([IO.File]::ReadAllLines($script:trace,[Text.Encoding]::UTF8) | Select-Object -Skip $script:traceLinesBefore)) {
            $item=$line|ConvertFrom-Json
            if ($item.pid -eq $script:probePid) {$wire.AddRange([byte[]]$item.bytes)}
        }
    }
    if ((Hex $wire.ToArray()) -cne (Hex $script:expected.ToArray())) {throw 'Pre-ConPTY bytes differ or paste was duplicated'}
    return @{count=$actual.Length;sha256=(Hash $actual);beforeConptyMatches=$true;readConsoleMatches=$true}
}
try {
    $launchedUtc=[DateTime]::UtcNow
    $previous=$env:FLOWMUX_TEST_INPUT_TRACE;$env:FLOWMUX_TEST_INPUT_TRACE=$trace
    try {$process=[CliProbe]::Start($gui,@('--temporary',('--shell='+$probe),('--shell-arg='+$raw),('--shell-arg='+$control)),$directory,$directory)}
    finally {$env:FLOWMUX_TEST_INPUT_TRACE=$previous}
    $evidence.launched=@{pid=$process.Id;started=$launchedUtc.ToString('o')}
    $stdout=$process.StandardOutput.ReadToEndAsync();$stderr=$process.StandardError.ReadToEndAsync()
    $discovery=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($process.Id).json";$deadline=(Get-Date).AddSeconds(25)
    do {
        if ($process.HasExited -or (Get-Date) -gt $deadline) {throw 'Owned host startup failed'}
        if ((Test-Path $discovery) -and (Get-Item $discovery).LastWriteTimeUtc -ge $launchedUtc) {
            $record=Get-Content -Raw $discovery|ConvertFrom-Json
            if ($record.pid -ne $process.Id) {throw 'Wrong discovery PID'}
            $script:pipeName=$record.pipe;break
        }
        Start-Sleep -Milliseconds 50
    } while ($true)
    if ((Request @('identify')).pid -ne $process.Id) {throw 'Wrong owned pipe'}
    do {
        $tree=Tree
        if ($tree.surfaces[0].ready) {break}
        if ((Get-Date) -gt $deadline) {throw 'Owned view did not become ready'}
        Start-Sleep -Milliseconds 50
    } while ($true)
    $surface=$tree.surfaces[0].id;$script:probePid=$tree.surfaces[0].pid
    $evidence.host=@{pid=$process.Id;probePid=$probePid;surface=$surface}
    Wait-Screen $surface 'KEY_PROBE_READY'|Out-Null
    if ((Bytes $raw).Length -ne 0) {throw 'Probe has unexpected input before the first paste'}
    # ConPTY startup device/focus replies precede the probe's ready marker.
    # Record that boundary once; all subsequent target bytes must match exactly.
    $startup=@(if (Test-Path $trace) {[IO.File]::ReadAllLines($trace,[Text.Encoding]::UTF8)})
    $script:traceLinesBefore=$startup.Count
    $evidence.startupInputBeforeReady=@($startup|ForEach-Object {($_|ConvertFrom-Json).bytes})

    $pane=(Request @('identify')).pane
    $appSequence=Set-Mode $surface $true
    if ($Baseline) {
        $result=Request @('send-key','Up','--pane',$pane)
        $script:expected.AddRange([byte[]](27,91,65))
        $bytes=Check-Bytes
        $evidence.checks+=@{name='baseline_sends_normal_cursor_bytes_after_application_mode_output';passed=$true;bytes=$bytes;actual='1B-5B-41';desired='1B-4F-41';modeSequence=$appSequence}
        $evidence.status='reproduced_fixed_normal_cursor_encoding'
    } else {
        $cases=(Get-Content -Raw -Encoding UTF8 (Join-Path $PSScriptRoot '..\terminal\src\named-key.cases.json')|ConvertFrom-Json).cases
        foreach ($application in @($true,$false)) {
            $after=Set-Mode $surface $application
            foreach ($case in $cases) {
                $receipt=Request @('send-key',$case.name,'--surface',$surface)
                $text=if ($application) {$case.application} else {$case.normal}
                $bytes=[Text.Encoding]::UTF8.GetBytes($text)
                if (-not $receipt.ok -or $receipt.surface -ne $surface -or $receipt.application_cursor -ne $application -or $receipt.sequence -lt $after -or $receipt.accepted_bytes -ne $bytes.Length -or $receipt.delivery -ne 'queued') {throw ('Wrong key receipt: '+$case.name)}
                $script:expected.AddRange($bytes)
            }
            $evidence.checks+=@{name=('reference_vectors_mode_'+$application);passed=$true;keys=$cases.Count;bytes=(Check-Bytes)}
        }
        $application=$true;$after=Set-Mode $surface $true
        $legacy=Raw-Request @{method='send_key';key='Home';pane=$pane}
        if (-not $legacy.ok -or -not $legacy.application_cursor) {throw 'Legacy wire request lost mode-aware behavior'}
        $script:expected.AddRange([byte[]](27,79,72))
        $evidence.checks+=@{name='legacy_send_key_wire_keeps_pane_target_and_mode';passed=$true;bytes=(Check-Bytes)}
        Request @('new-tab','--shell=cmd')|Out-Null
        $focused=(Request @('identify')).surface
        $receipt=Request @('send-key','Up','--surface',$surface)
        $script:expected.AddRange([byte[]](27,79,65))
        if (-not $receipt.application_cursor -or (Request @('identify')).surface -ne $focused) {throw 'Inactive key target/mode/focus differs'}
        $evidence.checks+=@{name='inactive_surface_uses_its_own_mode_without_focus';passed=$true;bytes=(Check-Bytes)}
        foreach ($case in @(
            @{body=@{method='send_key_mode';key='한';surface=$surface};error='unsupported named key'},
            @{body=@{method='send_key_mode';key='Shift+Insert';surface=$surface};error='clipboard'},
            @{body=@{method='send_key_mode';key='Ctrl+Ctrl+C';surface=$surface};error='duplicate'},
            @{body=@{method='send_key_mode';key='Up';surface=$surface;pane=$pane};error='either pane or surface'},
            @{body=@{method='send_key_mode';key='Up';surface=[guid]::NewGuid().ToString()};error='no longer exists'}
        )) {
            $result=Raw-Request $case.body
            if (-not $result.error -or -not $result.error.Contains($case.error)) {throw 'Invalid key request did not fail correctly'}
        }
        $evidence.checks+=@{name='invalid_keys_ui_actions_and_targets_produce_no_input';passed=$true;bytes=(Check-Bytes)}
        Request @('new-workspace','--shell=cmd')|Out-Null;$target=Request @('identify')
        Request @('move-tab',$surface,'--to-pane',$target.pane)|Out-Null
        if (((Tree).surfaces|Where-Object {$_.id -eq $surface}).pid -ne $probePid) {throw 'Key target move replaced process'}
        Request @('focus-tab',$target.surface)|Out-Null
        Request @('send-key','End','--surface',$surface)|Out-Null
        $script:expected.AddRange([byte[]](27,79,70))
        if ((Request @('identify')).surface -ne $target.surface) {throw 'Moved target key changed active tab'}
        $evidence.checks+=@{name='moved_source_retains_mode_and_process';passed=$true;bytes=(Check-Bytes)}
        # Private control asks our probe to exit; Ctrl+Q remains ordinary test data.
        [IO.File]::WriteAllText(($control+'.tmp'),([guid]::NewGuid().ToString('N')+':exit'),[Text.Encoding]::UTF8)
        if (-not [CliProbe]::MoveFileEx(($control+'.tmp'),$control,1)) {throw 'Owned exit control replacement failed'}
        $deadline=(Get-Date).AddSeconds(10)
        do {
            $stopped=(Tree).surfaces|Where-Object {$_.id -eq $surface}
            if ($stopped.resources_released -and $stopped.exit_code -eq 7) {break}
            if ((Get-Date) -gt $deadline) {throw 'Owned key probe did not exit'}
            Start-Sleep -Milliseconds 30
        } while ($true)
        $errorResult=Request @('send-key','Up','--surface',$surface) 1
        if (-not $errorResult.error.Contains('has exited')) {throw 'Exited surface accepted key'}
        Request @('close-tab',$surface)|Out-Null
        $errorResult=Request @('send-key','Up','--surface',$surface) 1
        if (-not $errorResult.error.Contains('no longer exists')) {throw 'Closed surface accepted key'}
        $evidence.checks+=@{name='exited_and_closed_targets_fail_without_restarting';passed=$true}
        Tree|Out-Null
        $evidence.status='passed_background_named_keys_subset'
    }
} catch {
    $evidence.status='failed';$evidence.error=$_.Exception.Message
    throw
} finally {
    if ($pipeName) {try {Request @('quit','--discard-state')|Out-Null} catch {}}
    if ($process) {
        if (-not $process.HasExited -and -not $process.WaitForExit(10000)) {$process.Kill();$process.WaitForExit()}
        $process.Dispose()
    }
    $evidence.finished=(Get-Date).ToString('o')
    $evidence|ConvertTo-Json -Depth 10|Set-Content -Encoding UTF8 (Join-Path $directory 'native-keys-background.json')
    Write-Output ('Evidence: '+$directory)
}
$evidence|ConvertTo-Json -Depth 10
