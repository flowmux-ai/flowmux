# SPDX-License-Identifier: GPL-3.0-or-later
# All hosts and children are owned/hidden. No OS input, clipboard, or foreground mutation.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path
$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor=& $cli doctor | ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) {throw 'A working debug build is required; no host was launched.'}
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs')
$directory=Join-Path $PSScriptRoot ('..\dist\evidence\paste-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$probe=Join-Path $directory 'paste-probe.exe'
Add-Type -Path (Join-Path $PSScriptRoot 'PasteProbe.cs') -OutputAssembly $probe -OutputType ConsoleApplication
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
    Move-Item -Force $temp $control
    return (Wait-Screen $Surface ('PASTE_MODE_'+$id)).sequence
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
function Paste-Case([string]$Name,[string]$Text,[bool]$Bracketed,[string]$Surface,[bool]$Raw=$false) {
    $result=if ($Raw) {Raw-Request @{method='paste';surface=$Surface;text=$Text}} else {Request @('paste','--surface',$Surface,'--',$Text)}
    if ($result.error) {throw $result.error}
    $data=$Text.Replace("`r`n","`r").Replace("`n","`r")
    if ($Bracketed -and $data.Length -gt 0) {$data=[string][char]27+'[200~'+$data+[char]27+'[201~'}
    $bytes=[Text.Encoding]::UTF8.GetBytes($data)
    $isBracketed=$Bracketed -and $Text.Length -gt 0
    if ($result.surface -ne $Surface -or $result.bracketed -ne $isBracketed -or $result.delivery -ne 'queued' -or $result.accepted_bytes -ne $bytes.Length) {throw 'Paste receipt differs'}
    $script:expected.AddRange($bytes)
    $bytesEvidence=Check-Bytes
    $script:evidence.checks+=@{name=$Name;passed=$true;receipt=$result;inputUtf8Bytes=[Text.Encoding]::UTF8.GetByteCount($Text);bytes=$bytesEvidence}
    Tree|Out-Null
}
try {
    $previous=$env:FLOWMUX_TEST_INPUT_TRACE;$env:FLOWMUX_TEST_INPUT_TRACE=$trace
    try {$process=[CliProbe]::Start($gui,@('--temporary',('--shell='+$probe),('--shell-arg='+$raw),('--shell-arg='+$control)),$directory,$directory)}
    finally {$env:FLOWMUX_TEST_INPUT_TRACE=$previous}
    $stdout=$process.StandardOutput.ReadToEndAsync();$stderr=$process.StandardError.ReadToEndAsync()
    $discovery=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($process.Id).json";$deadline=(Get-Date).AddSeconds(25)
    do {
        if ($process.HasExited -or (Get-Date) -gt $deadline) {throw 'Owned host startup failed'}
        if (Test-Path $discovery) {
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
    Wait-Screen $surface 'PASTE_PROBE_READY'|Out-Null
    if ((Bytes $raw).Length -ne 0) {throw 'Probe has unexpected input before the first paste'}
    # ConPTY startup device/focus replies precede the probe's ready marker.
    # Record that boundary once; all subsequent target bytes must match exactly.
    $startup=@(if (Test-Path $trace) {[IO.File]::ReadAllLines($trace,[Text.Encoding]::UTF8)})
    $script:traceLinesBefore=$startup.Count
    $evidence.startupInputBeforeReady=@($startup|ForEach-Object {($_|ConvertFrom-Json).bytes})
    Paste-Case 'plain_korean_decomposed_emoji_and_mixed_newlines' "한글 한 😀`r`n둘째`n셋째`r끝`t!" $false $surface
    $after=Set-Mode $surface $true
    Paste-Case 'bracketed_korean_multiline' "한글 한 😀`nsecond`r`nthird" $true $surface
    if ($evidence.checks[-1].receipt.sequence -lt $after) {throw 'Paste preceded mode output'}
    Paste-Case 'empty_paste_has_no_brackets_or_input' '' $true $surface $true
    Paste-Case 'control_sequences_are_preserved_not_claimed_sanitized' ([string][char]27+'[201~한'+[char]27+'[200~') $true $surface
    Set-Mode $surface $false|Out-Null
    Paste-Case 'disabling_bracketed_mode_takes_effect' "한`n글" $false $surface
    Set-Mode $surface $true|Out-Null
    Paste-Case '128k_utf8_boundary_without_truncation' (('가'*43690)+'ab') $true $surface $true
    # A second active tab must not redirect input or steal the selected tab.
    Request @('new-tab','--shell=cmd')|Out-Null
    $focused=(Request @('identify')).surface
    Paste-Case 'inactive_surface_keeps_its_mode_and_target' '숨김 한 😀' $true $surface
    if ((Request @('identify')).surface -ne $focused) {throw 'Paste changed the active tab'}
    $pane=(Request @('identify')).pane
    foreach ($case in @(
        @{name='oversize';body=@{method='paste';surface=$surface;text=('x'*131073)};error='128 KiB'},
        @{name='nul';body=@{method='paste';surface=$surface;text=("한"+[char]0+'글')};error='NUL'},
        @{name='conflicting_targets';body=@{method='paste';surface=$surface;pane=$pane;text='rejected'};error='either pane or surface'},
        @{name='closed_or_missing_target';body=@{method='paste';surface=[guid]::NewGuid().ToString();text='rejected'};error='no longer exists'}
    )) {
        $result=Raw-Request $case.body
        if (-not $result.error -or -not $result.error.Contains($case.error)) {throw "Wrong rejection for $($case.name): $($result|ConvertTo-Json -Compress)"}
        $evidence.checks+=@{name=$case.name;passed=$true;error=$result.error;bytes=(Check-Bytes)}
    }
    # Stop only our probe through its explicit input contract; no desktop event.
    Request @('focus-tab',$surface)|Out-Null
    $targetPane=(Request @('identify')).pane
    Request @('send-keys',$targetPane,([string][char]0x11))|Out-Null
    $deadline=(Get-Date).AddSeconds(10)
    do {
        $stopped=(Tree).surfaces|Where-Object {$_.id -eq $surface}
        if ($null -ne $stopped.exit_code) {break}
        if ((Get-Date) -gt $deadline) {throw 'Owned probe did not exit'}
        Start-Sleep -Milliseconds 30
    } while ($true)
    $result=Request @('paste','--surface',$surface,'rejected') 1
    if (-not $result.error.Contains('has exited')) {throw 'Exited surface accepted paste'}
    $evidence.checks+=@{name='exited_surface_rejects_without_restarting';passed=$true;error=$result.error}
    $evidence.status='passed_background_paste_subset'
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
    $evidence|ConvertTo-Json -Depth 10|Set-Content -Encoding UTF8 (Join-Path $directory 'native-paste-background.json')
    Write-Output ('Evidence: '+$directory)
}
$evidence|ConvertTo-Json -Depth 10
