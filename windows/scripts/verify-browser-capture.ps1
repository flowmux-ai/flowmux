# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden owned WebView2 host + loopback fixture only. No foreground, input, clipboard or external sites.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path
$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor=& $cli doctor|ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) {throw 'Working debug build required; no host launched'}
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs')
Add-Type -Path (Join-Path $PSScriptRoot 'BrowserFixture.cs')
$directory=Join-Path $PSScriptRoot ('..\dist\evidence\browser-capture-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$fixture=New-Object BrowserFixture((Join-Path $PSScriptRoot 'browser-capture.html'));$origin=$fixture.Origin
$pipeName=$null;$process=$null;$hosts=@();$shells=@()
foreach($file in @('flowmux.exe','flowmuxctl.exe','flowmux-command.exe')) {
    $probe=[CliProbe]::Start((Join-Path $BuildDirectory $file),@('doctor'),$directory,$directory)
    try {
        $out=$probe.StandardOutput.ReadToEndAsync();$err=$probe.StandardError.ReadToEndAsync()
        if(-not $probe.WaitForExit(15000)){$probe.Kill();[CliProbe]::WaitAfterKill($probe);throw 'Owned entry-point doctor timed out'}
        if(-not $out.Wait(3000) -or -not $err.Wait(3000) -or $probe.ExitCode -ne 0){throw ('Entry-point parser failed: '+$file+' '+([CliProbe]::Output($err)))}
        if(-not ($out.Result|ConvertFrom-Json).background_testing){throw 'Wrong entry-point build'}
    } finally {$probe.Dispose()}
}

$evidence=[ordered]@{started=(Get-Date).ToString('o');mode='background';checks=@();clipboardAccess=$false;desktopInput=$false;externalSites=$false;unicodeComparison='ordinal'}
function Request([string[]]$Arguments,[int]$Exit=0) {
    if (-not $script:pipeName) {throw 'Owned pipe required'}
    $p=[CliProbe]::Start($cli,(@('--pipe',$script:pipeName,'--json')+$Arguments),$directory,$directory)
    try {
        $out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
        if (-not $p.WaitForExit(30000)) {$p.Kill();[CliProbe]::WaitAfterKill($p);throw 'Owned CLI timed out; not retried'}
        if (-not $out.Wait(3000) -or -not $err.Wait(3000)) {throw 'Owned output did not close'}
        if ($p.ExitCode -ne $Exit) {throw "CLI $Arguments exit $($p.ExitCode): $(([CliProbe]::Output($err))) $($out.Result)"}
        if ($Exit -eq 0) {return ($out.Result|ConvertFrom-Json)}
        return (([CliProbe]::Output($err))|ConvertFrom-Json)
    } finally {$p.Dispose()}
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
            $read=$reader.ReadLineAsync();if (-not $read.Wait(20000)) {throw 'Owned browser fixture request timed out; not retried'}
            return ($read.Result|ConvertFrom-Json)
        } finally {$writer.Dispose();$reader.Dispose()}
    } finally {$stream.Dispose()}
}
function Tree {
    $tree=Request @('tree');$window=[IntPtr]([long]$tree.window_handle)
    if (-not $tree.background_testing -or [CliProbe]::IsWindowVisible($window) -or [CliProbe]::GetForegroundWindow() -eq $window) {throw 'Owned host became visible or foreground'}
    foreach ($browser in $tree.browsers) {
        if ([CliProbe]::IsWindowVisible([IntPtr]([long]$browser.view_handle)) -or [CliProbe]::IsWindowVisible([IntPtr]([long]$browser.chrome_handle))) {throw 'Owned browser became visible'}
    }
    return $tree
}
function Start-Owned([string[]]$Launch) {
    $script:pipeName=$null;$utc=[DateTime]::UtcNow
    $script:process=[CliProbe]::Start($gui,$Launch,$directory,$directory)
    $script:hosts+= $process.Id
    $script:stdout=$process.StandardOutput.ReadToEndAsync();$script:stderr=$process.StandardError.ReadToEndAsync()
    $file=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($process.Id).json";$deadline=(Get-Date).AddSeconds(40)
    do {
        if ($process.HasExited -or (Get-Date) -gt $deadline) {throw ('Owned startup failed: '+([CliProbe]::Output($stderr)))}
        if ((Test-Path $file) -and (Get-Item $file).LastWriteTimeUtc -ge $utc) {
            $record=Get-Content -Raw $file|ConvertFrom-Json
            if ($record.pid -ne $process.Id) {throw 'Wrong discovery owner'}
            $script:pipeName=$record.pipe;break
        }
        Start-Sleep -Milliseconds 50
    } while ($true)
    if ((Request @('identify')).pid -ne $process.Id) {throw 'Wrong pipe owner'}
    do {
        $tree=Tree
        if (@($tree.surfaces|Where-Object {-not $_.ready}).Count -eq 0) {return $tree}
        if ((Get-Date) -gt $deadline) {throw 'Terminal readiness timed out'}
        Start-Sleep -Milliseconds 50
    } while ($true)
}
function Same-Text([string]$Left,[string]$Right) {return [string]::Equals($Left,$Right,[StringComparison]::Ordinal)}
function Eval-Page([string]$Pane,[string]$Source) {return (Request @('browser','eval',('pane:'+$Pane),$Source)).result}
function Wait-Page([string]$Pane,[string]$Suffix,[string]$Title) {
    $deadline=(Get-Date).AddSeconds(20)
    do {
        $status=Request @('browser','status',('pane:'+$Pane))
        if (-not $status.loading -and $status.url.Contains($Suffix) -and (Same-Text $status.title $Title)) {
            if ((Eval-Page $Pane 'document.readyState') -eq 'complete') {return $status}
        }
        if ((Get-Date) -gt $deadline) {throw ('Page did not load: '+($status|ConvertTo-Json -Compress))}
        Start-Sleep -Milliseconds 50
    } while ($true)
}
function Plain([string[]]$Arguments) {
    $p=[CliProbe]::Start($cli,(@('--pipe',$script:pipeName)+$Arguments),$directory,$directory)
    try {
        $out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
        if (-not $p.WaitForExit(30000)) {$p.Kill();[CliProbe]::WaitAfterKill($p);throw 'Owned plain CLI timed out'}
        if (-not $out.Wait(3000) -or -not $err.Wait(3000) -or $p.ExitCode -ne 0) {throw ('Plain CLI failed: '+([CliProbe]::Output($err)))}
        return $out.Result.TrimEnd("`r","`n")
    } finally {$p.Dispose()}
}


$clients=@()
function Begin-Command([string[]]$Arguments) {
    $p=[CliProbe]::Start($cli,(@('--pipe',$script:pipeName,'--json')+$Arguments),$directory,$directory)
    $job=[pscustomobject]@{process=$p;pid=$p.Id;output=$p.StandardOutput.ReadToEndAsync();error=$p.StandardError.ReadToEndAsync();completed=$false}
    $script:clients+=$job;return $job
}
function End-Command($Job,[int]$Exit=0) {
    try {
        if(-not $Job.process.WaitForExit(20000)) {$Job.process.Kill();[CliProbe]::WaitAfterKill($Job.process);throw 'Owned concurrent CLI timed out; not retried'}
        if(-not $Job.output.Wait(3000) -or -not $Job.error.Wait(3000) -or $Job.process.ExitCode -ne $Exit) {throw ('Concurrent CLI failed: '+([CliProbe]::Output($Job.error))+' '+([CliProbe]::Output($Job.output)))}
        if($Exit -eq 0) {return ($Job.output.Result|ConvertFrom-Json)}
        return (([CliProbe]::Output($Job.error))|ConvertFrom-Json)
    } finally {$Job.completed=$true;$Job.process.Dispose()}
}
Add-Type -AssemblyName System.Drawing
function Pixel([string]$Path,[int]$Red,[int]$Green,[int]$Blue) {
    $bmp=[Drawing.Bitmap]::new($Path)
    try {
        $pixel=$bmp.GetPixel(10,10)
        if ($pixel.R -ne $Red -or $pixel.G -ne $Green -or $pixel.B -ne $Blue -or $pixel.A -ne 255) {throw ('Wrong rendered pixel: '+$pixel.ToString())}
        return @{width=$bmp.Width;height=$bmp.Height;color=$pixel.ToArgb()}
    } finally {$bmp.Dispose()}
}
function Capture([string]$Path) {return Request @('browser','screenshot',$script:domPane,$Path)}
try {
    $tree=Start-Owned @('--temporary','--shell=cmd','--cwd',$directory)
    $source=Request @('identify');$terminal=$tree.surfaces[0];$script:shells+=$terminal.pid
    $opened=(Request @('browser','open',($origin+'/dom'),'--pane',$source.pane)).browser_pane_opened
    $script:domPane=$opened.pane
    $before=Wait-Page $domPane '/dom' 'Capture 한글 한 é 😀'
    $name='캡처 한 é 😀.png';$path=Join-Path $directory $name
    $r=Capture $name
    if (-not (Same-Text $r.path $path) -or $r.surface -ne $opened.surface -or $r.generation -ne $before.generation -or $r.bytes -ne (Get-Item -LiteralPath $path).Length) {throw 'Capture identity/path/size differs'}
    $pixel=Pixel $path 12 34 56
    if ($pixel.width -ne $r.width -or $pixel.height -ne $r.height -or $r.width -lt 50 -or $r.height -lt 50 -or $r.height -ge 2000) {throw 'Capture is not the viewport size'}
    $evidence.first=$r;$evidence.firstPixel=$pixel
    $evidence.checks+=@{name='hidden_viewport_png_decoded_color_and_size_exact_unicode_relative_cli_path';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)

    Eval-Page $domPane 'window.scrollTo({top:2000,behavior:"instant"});window.scrollY'|Out-Null
    $scrolled=Join-Path $directory 'scrolled.png';$r=Capture $scrolled
    $evidence.scrolledPixel=Pixel $scrolled 90 160 210
    if ((Eval-Page $domPane 'window.scrollY') -ne 2000 -or (Eval-Page $domPane 'focusEvents') -ne 0) {throw 'Capture changed scroll or requested DOM focus'}
    $plain=Plain @('browser','screenshot',$domPane,'plain.png')
    if (-not (Same-Text $plain (Join-Path $directory 'plain.png'))) {throw 'Plain capture output is not the absolute path'}
    Pixel $plain 90 160 210|Out-Null
    $r=Capture $path;Pixel $path 90 160 210|Out-Null
    $evidence.checks+=@{name='current_scroll_plain_output_atomic_overwrite_without_focus_or_scroll_change';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)

    $original=[IO.File]::ReadAllBytes($path)
    $lock=[IO.File]::Open($path,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::None)
    try {$err=Request @('browser','screenshot',$domPane,$path) 1;if(-not $err.error.Contains('atomic replacement')) {throw 'Locked destination did not fail at replacement'}} finally {$lock.Dispose()}
    if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($path)) -cne [Convert]::ToBase64String($original)) {throw 'Failed save changed the existing destination'}
    if (@(Get-ChildItem -LiteralPath $directory -Filter '.flowmux-capture-*.tmp').Count -ne 0) {throw 'Failed save left an owned temp'}
    $missing=Join-Path $directory 'missing\capture.png'
    Request @('browser','screenshot',$domPane,$missing) 1|Out-Null
    if(Test-Path -LiteralPath $missing) {throw 'Missing parent was created'}
    $evidence.checks+=@{name='locked_destination_preserved_temporary_file_removed_missing_parent_rejected';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)

    foreach($bad in @('relative.png',(Join-Path $directory 'capture.jpg'),(Join-Path $directory 'capture.png:stream'),'\\.\pipe\capture.png')) {
        $r=Raw-Request @{method='browser';op=@{kind='screenshot';pane=$domPane;path=$bad}}
        if(-not $r.error) {throw ('Invalid raw capture path accepted: '+$bad)}
    }
    Request @('browser','screenshot',$source.pane,(Join-Path $directory 'terminal.png')) 1|Out-Null
    if(Test-Path -LiteralPath (Join-Path $directory 'terminal.png')) {throw 'Terminal was captured through browser command'}
    Request @('browser','navigate',$domPane,($origin+'/slow'))|Out-Null
    $r=Request @('browser','screenshot',$domPane,(Join-Path $directory 'loading.png')) 1
    if(-not $r.error.Contains('navigation')) {throw 'Loading document capture was accepted'}
    Request @('browser','wait',$domPane,'--ready-state','complete','--timeout-ms','10000')|Out-Null
    if(Test-Path -LiteralPath (Join-Path $directory 'loading.png')) {throw 'Loading request wrote a file'}
    $evidence.checks+=@{name='invalid_paths_raw_relative_terminal_target_and_loading_capture_rejected';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)

    Request @('browser','navigate',$domPane,($origin+'/dom'))|Out-Null
    Wait-Page $domPane '/dom' 'Capture 한글 한 é 😀'|Out-Null
    Request @('browser','zoom',$domPane,'1.5')|Out-Null
    $zoom=Capture (Join-Path $directory 'zoom.png');Pixel $zoom.path 12 34 56|Out-Null
    if((Request @('browser','status',$domPane)).zoom -ne 1.5) {throw 'Capture reset browser zoom'}
    $second=(Request @('browser','open',($origin+'/dom'),'--pane',$source.pane)).browser_pane_opened
    Wait-Page $domPane '/dom' 'Capture 한글 한 é 😀'|Out-Null
    $r=Capture (Join-Path $directory 'second-tab.png')
    if ($r.surface -ne $second.surface) {throw 'Screenshot captured the previous browser tab'}
    Pixel $r.path 12 34 56|Out-Null
    Request @('close-tab',$second.surface)|Out-Null
    $r=Capture (Join-Path $directory 'restored-tab.png')
    if ($r.surface -ne $opened.surface) {throw 'Screenshot switched away from restored tab'}
    Pixel $r.path 12 34 56|Out-Null
    $after=Request @('browser','status',$domPane)
    if ($after.zoom -ne 1.5 -or (Eval-Page $domPane 'focusEvents') -ne 0) {throw 'Capture changed view state'}
    $current=(Tree).surfaces|Where-Object {$_.id -eq $terminal.id}
    if($current.pid -ne $terminal.pid -or -not $current.running) {throw 'Source terminal identity changed'}
    $evidence.checks+=@{name='zoom_active_browser_tab_restoration_and_source_terminal_identity';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)
    # A busy loop is confined to our owned renderer; native status remains responsive.
    $busy=Begin-Command @('browser','eval',$domPane,'(()=>{const until=performance.now()+4000;while(performance.now()<until){};return "done";})()')
    Start-Sleep -Milliseconds 150
    $onePath=Join-Path $directory 'changed-one.png';$twoPath=Join-Path $directory 'changed-two.png'
    $one=Begin-Command @('browser','screenshot',$domPane,$onePath)
    $two=Begin-Command @('browser','screenshot',$domPane,$twoPath)
    $deadline=(Get-Date).AddSeconds(2)
    do {
        $status=Request @('browser','status',$domPane)
        if($status.captures_pending -eq 2) {break}
        if($one.process.HasExited -or $two.process.HasExited -or (Get-Date) -gt $deadline) {throw 'Could not observe two pending captures behind owned renderer work'}
        Start-Sleep -Milliseconds 10
    } while($true)
    $err=Request @('browser','screenshot',$domPane,(Join-Path $directory 'third.png')) 1
    if(-not $err.error.Contains('two browser captures')) {throw 'Capture admission limit not enforced'}
    Request @('browser','zoom',$domPane,'1.25')|Out-Null
    End-Command $busy|Out-Null
    foreach($job in @($one,$two)) {$err=End-Command $job 1;if(-not $err.error.Contains('viewport changed')) {throw 'Changed viewport capture was not rejected'}}
    foreach($name in @('changed-one.png','changed-two.png','third.png')) {if(Test-Path -LiteralPath (Join-Path $directory $name)) {throw 'Rejected capture created a file'}}
    if((Request @('browser','status',$domPane)).captures_pending -ne 0) {throw 'Capture completion did not release slots'}
    $r=Capture (Join-Path $directory 'recovered.png');Pixel $r.path 12 34 56|Out-Null
    $evidence.checks+=@{name='two_native_captures_bound_viewport_change_discards_images_and_capacity_recovers';passed=$true}
    Write-Host ("[check] passed "+$evidence.checks[-1].name)
    Tree|Out-Null
    $evidence.status='passed_background_browser_capture_subset'
} catch {$evidence.status='failed';$evidence.error=$_.Exception.Message;throw}
finally {
    foreach($job in $clients) {if(-not $job.completed) {if(-not $job.process.HasExited) {$job.process.Kill();[CliProbe]::WaitAfterKill($job.process)};$job.process.Dispose()}}
    if ($pipeName -and $process -and -not $process.HasExited) {try {Request @('quit','--discard-state')|Out-Null} catch {}}
    if ($process) {if (-not $process.HasExited -and -not $process.WaitForExit(10000)) {$process.Kill();[CliProbe]::WaitAfterKill($process)};if ($stderr -and $stderr.Wait(3000)) {$evidence.hostStderr=([CliProbe]::Output($stderr))};$evidence.hostExitCode=$process.ExitCode;$process.Dispose()}
    $fixture.Dispose();$evidence.clientPids=@($clients|ForEach-Object {$_.pid});$evidence.hosts=$hosts;$evidence.shells=$shells;$evidence.finished=(Get-Date).ToString('o')
    $evidence|ConvertTo-Json -Depth 12|Set-Content -Encoding UTF8 (Join-Path $directory 'native-browser-capture-background.json')
    Write-Output ('Evidence: '+$directory)
}
$evidence|ConvertTo-Json -Depth 12
