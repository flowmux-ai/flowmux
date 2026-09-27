# SPDX-License-Identifier: GPL-3.0-or-later
# Hidden owned hosts only. Read parsed screens through IPC; never touch desktop input or clipboard.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$BuildDirectory=(Resolve-Path $BuildDirectory).Path
$gui=Join-Path $BuildDirectory 'flowmux.exe';$cli=Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor=& $cli doctor|ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) {throw 'A working debug build is required; no host was launched.'}
Add-Type -Path (Join-Path $PSScriptRoot 'CliProbe.cs')
$directory=Join-Path $PSScriptRoot ('..\dist\evidence\notifications-'+[guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory)|Out-Null;$directory=(Resolve-Path $directory).Path
$probe=Join-Path $directory 'notifications-probe.exe';$control=Join-Path $directory 'control.txt'
Add-Type -Path (Join-Path $PSScriptRoot 'NotificationProbe.cs') -OutputAssembly $probe -OutputType ConsoleApplication
$pipeName=$null;$process=$null;$otherProcess=$null;$otherPipe=$null
$evidence=[ordered]@{started=(Get-Date).ToString('o');mode='background';checks=@();clipboardAccess=$false;desktopInput=$false}
function Request([string[]]$Arguments,[int]$Exit=0) {
    if (-not $script:pipeName) {throw 'Owned pipe required; refusing discovery fallback'}
    $p=[CliProbe]::Start($cli,(@('--pipe',$script:pipeName,'--json')+$Arguments),$directory,$directory)
    try {
        $out=$p.StandardOutput.ReadToEndAsync();$err=$p.StandardError.ReadToEndAsync()
        if (-not $p.WaitForExit(30000)) {$p.Kill();$p.WaitForExit();throw 'Owned CLI timed out; not retried'}
        if (-not $out.Wait(3000) -or -not $err.Wait(3000)) {throw 'Owned output pipes did not close'}
        if ($p.ExitCode -ne $Exit) {throw "CLI exit $($p.ExitCode): $($err.Result)"}
        if ($Exit -eq 0) {return ($out.Result|ConvertFrom-Json)}
        return ($err.Result|ConvertFrom-Json)
    } finally {$p.Dispose()}
}
function Tree {
    $tree=Request @('tree');$window=[IntPtr]([long]$tree.window_handle)
    if (-not $tree.background_testing -or [CliProbe]::IsWindowVisible($window) -or [CliProbe]::GetForegroundWindow() -eq $window) {throw 'Owned host became visible or foreground'}
    return $tree
}
function Wait-Screen([string]$Text) {
    $deadline=(Get-Date).AddSeconds(20)
    do {
        $screen=Request @('read-screen','--surface',$script:surface,'--recent')
        if ($screen.text.Contains($Text)) {return $screen}
        if ((Get-Date) -gt $deadline) {throw ('Missing probe output: '+$Text)}
        Start-Sleep -Milliseconds 30
    } while ($true)
}
function Control([string]$Action) {
    $id=[guid]::NewGuid().ToString('N')
    [IO.File]::WriteAllText(($control+'.tmp'),($id+':'+$Action),[Text.Encoding]::UTF8)
    if (-not [CliProbe]::MoveFileEx(($control+'.tmp'),$control,1)) {throw ('Owned control replacement failed: '+[Runtime.InteropServices.Marshal]::GetLastWin32Error())}
    if ($Action -ne 'exit') {return (Wait-Screen ('NOTIFICATION_CONTROL_'+$id)).sequence}
}


Add-Type @'
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class NotificationInspect {
    delegate bool EnumCallback(IntPtr h,IntPtr data);
    [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr parent,EnumCallback callback,IntPtr data);
    public static string[] Captions(IntPtr parent) {
        var list=new List<string>(); EnumChildWindows(parent,delegate(IntPtr h,IntPtr data) {list.Add(Text(h));return true;},IntPtr.Zero);return list.ToArray();
    }
    [DllImport("user32.dll")] public static extern IntPtr GetDlgItem(IntPtr h,int id);
    [DllImport("user32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern IntPtr SendMessageTimeout(IntPtr h,uint msg,IntPtr w,StringBuilder text,uint flags,uint timeout,out IntPtr result);
    public static string Text(IntPtr h) {
        var b=new StringBuilder(16384);IntPtr result;
        if(SendMessageTimeout(h,0x000D,(IntPtr)b.Capacity,b,2,1000,out result)==IntPtr.Zero) throw new Exception("Owned control text read failed");
        return b.ToString();
    }
}
'@
try {
    $process=[CliProbe]::Start($gui,@('--temporary',('--shell='+$probe),('--shell-arg='+$control)),$directory,$directory)
    $stdout=$process.StandardOutput.ReadToEndAsync();$stderr=$process.StandardError.ReadToEndAsync()
    $discovery=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($process.Id).json";$deadline=(Get-Date).AddSeconds(25)
    do {
        if ($process.HasExited -or (Get-Date) -gt $deadline) {throw 'Owned host startup failed'}
        if (Test-Path $discovery) {
            $record=Get-Content -Raw $discovery|ConvertFrom-Json
            if ($record.pid -ne $process.Id) {throw 'Wrong discovery owner'}
            $script:pipeName=$record.pipe;break
        }
        Start-Sleep -Milliseconds 50
    } while ($true)
    if ((Request @('identify')).pid -ne $process.Id) {throw 'Wrong pipe target'}
    do {
        $tree=Tree
        if ($tree.surfaces[0].ready) {break}
        if ((Get-Date) -gt $deadline) {throw 'Owned terminal not ready'}
        Start-Sleep -Milliseconds 50
    } while ($true)
    $script:surface=$tree.surfaces[0].id;$probePid=$tree.surfaces[0].pid
    $evidence.host=@{pid=$process.Id;probePid=$probePid;surface=$surface}



    Wait-Screen 'NOTIFICATION_PROBE_READY'|Out-Null
    $before=(Tree).surfaces[0]
    Control 'osc'|Out-Null
    $osc=Request @('notifications','list')
    $evidence.osc=$osc
    $after=(Tree).surfaces|Where-Object {$_.id -eq $surface}
    if ($after.observed_output_bytes -le $before.observed_output_bytes -or -not $after.last_output_ms) {throw 'Output activity was not observed'}
    if ($osc.entries.Count -ne 3 -or $osc.entries[0].body -cne 'OSC9_한글_한_😀' -or $osc.entries[1].level -ne 'needs_input' -or $osc.entries[2].body -cne 'error_한_😀') {throw 'ConPTY did not preserve the OSC 9/99/777 notification sequence'}
    $evidence.checks+=@{name='live_conpty_osc_9_99_777_unicode_and_metadata_filter';passed=$true;activity=$after.observed_output_bytes}
    Request @('notifications','clear')|Out-Null
    Control 'burst'|Out-Null
    $burst=Request @('notifications','list')
    if ($burst.entries.Count -ne 2 -or $burst.entries[1].body -ne 'error after burst' -or $burst.entries[1].level -ne 'error') {throw 'Lower-priority burst hid a later error'}
    $evidence.burst=$burst
    Request @('notifications','clear')|Out-Null
    $title='알림_한_😀';$body="완료 한글 é`n두 번째 줄"
    $first=Request @('notify','--surface',$surface,'--title',$title,$body)
    if (-not $first.accepted) {throw 'Explicit notice rejected'}
    $duplicate=Request @('notify','--surface',$surface,'different body')
    if ($duplicate.accepted -or $duplicate.reason -ne 'duplicate') {throw 'Duplicate source not suppressed'}
    $attention=Request @('notify','--surface',$surface,'--level','attention','needs input')
    $errorNotice=Request @('notify','--surface',$surface,'--level','error','failed')
    if (-not $attention.accepted -or -not $errorNotice.accepted) {throw 'Priority escalation suppressed'}
    $notes=Request @('notifications','list')
    if ($notes.entries.Count -ne 3 -or $notes.unread_count -ne 3 -or $notes.entries[0].title -cne $title -or $notes.entries[0].body -cne $body) {throw 'Unicode/list/read contract differs'}
    if ((Request @('notifications','list','--unread')).entries.Count -ne 3) {throw 'Read-only list changed unread state'}
    $captions=[NotificationInspect]::Captions([IntPtr]([long](Tree).window_handle))
    if (-not ($captions -contains 'Notifications (3)') -or @($captions|Where-Object {$_ -like '[[]3] *'}).Count -lt 2) {throw 'Native notification/workspace/tab badge captions differ'}
    $evidence.checks+=@{name='explicit_unicode_dedup_escalation_read_only_list_and_native_badges';passed=$true;captions=$captions}

    Request @('notifications','mark-read',$first.id)|Out-Null
    if ((Request @('notifications','list')).unread_count -ne 2) {throw 'Mark read failed'}
    $shown=Request @('notifications','show')
    $panel=[IntPtr]([long]$shown.panel_handle)
    if ($shown.unread_count -ne 0 -or $shown.panel_rows -ne 3 -or [CliProbe]::IsWindowVisible($panel) -or [CliProbe]::GetForegroundWindow() -eq $panel) {throw 'Panel was visible or failed to mark existing entries read'}
    if ([NotificationInspect]::Text([NotificationInspect]::GetDlgItem($panel,12)) -cne ($title+"`r`n`r`n"+$body.Replace("`n","`r`n"))) {throw 'Native Unicode detail differs'}
    $global=Request @('notify','--global','global')
    $shown=Request @('notifications','list')
    if ($shown.panel_rows -ne 4 -or $shown.unread_count -ne 1 -or $shown.button_text -ne 'Notifications (1)') {throw 'Panel live update did not preserve new unread notice'}
    $evidence.checks+=@{name='native_hidden_panel_unicode_detail_and_live_rows';passed=$true;panel=$shown}
    Request @('notifications','delete',$global.id)|Out-Null
    if ((Request @('notifications','list')).entries.Count -ne 3) {throw 'Delete failed'}
    Request @('notifications','clear')|Out-Null
    Request @('new-workspace','--shell=cmd')|Out-Null;$target=Request @('identify')
    $newNotice=Request @('notify-complete','--surface',$surface,'--agent','에이전트','--message','완료_😀')
    if (-not $newNotice.accepted -or (Request @('identify')).surface -ne $target.surface) {throw 'Notice activated its source'}
    Request @('move-tab',$surface,'--to-pane',$target.pane)|Out-Null
    $moved=(Request @('notifications','list')).entries[0]
    if ($moved.surface -ne $surface -or $moved.workspace -ne $target.workspace -or $moved.pane -ne $target.pane -or $moved.level -ne 'turn_completed') {throw 'Move did not resolve stable notification source'}
    Request @('focus-tab',$target.surface)|Out-Null
    Request @('notifications','open',$newNotice.id)|Out-Null
    if ((Request @('identify')).surface -ne $surface -or ((Tree).surfaces|Where-Object {$_.id -eq $surface}).pid -ne $probePid -or (Request @('notifications','list')).unread_count -ne 0) {throw 'Notification did not reopen same source/process'}
    $evidence.checks+=@{name='completion_stays_inactive_and_moves_resolve_same_surface_process';passed=$true}
    Control 'exit';$deadline=(Get-Date).AddSeconds(15)
    do {
        $stopped=(Tree).surfaces|Where-Object {$_.id -eq $surface}
        if ($stopped.resources_released -and $stopped.exit_code -eq 7) {break}
        if ((Get-Date) -gt $deadline) {throw 'Owned renderer did not exit'}
        Start-Sleep -Milliseconds 30
    } while ($true)
    Request @('focus-tab',$target.surface)|Out-Null
    Request @('notifications','open',$newNotice.id)|Out-Null
    if (-not (Request @('read-screen','--surface',$surface,'--recent')).text.Contains('NOTIFICATION_EXITED')) {throw 'Exited terminal notification lost final screen'}
    Request @('notifications','clear')|Out-Null
    $closed=Request @('notify','--surface',$surface,'closed source')
    Request @('close-tab',$surface)|Out-Null
    $active=(Request @('identify')).surface
    $rejected=Request @('notifications','open',$closed.id) 1
    $row=(Request @('notifications','list')).entries[0]
    if (-not $rejected.error.Contains('source was closed') -or -not $row.closed -or $row.read -or (Request @('identify')).surface -ne $active) {throw 'Closed source changed read/focus state'}
    $evidence.checks+=@{name='natural_exit_keeps_source_readable_and_closed_source_is_atomic_error';passed=$true}
    Request @('notifications','clear')|Out-Null
    0..54|ForEach-Object {Request @('notify','--global',('retention_'+$_))|Out-Null}
    $retained=Request @('notifications','list')
    if ($retained.entries.Count -ne 50 -or $retained.entries[0].body -ne 'retention_5' -or $retained.entries[49].body -ne 'retention_54' -or $retained.panel_rows -ne 50) {throw 'Retention cap/order differs'}
    Request @('notifications','jump-to-unread')|Out-Null
    if ((Request @('notifications','list')).unread_count -ne 49) {throw 'Jump did not acknowledge oldest unread'}
    Request @('notify','--global','--title',('한'*350),'oversize') 1|Out-Null
    if ((Request @('notifications','list')).entries.Count -ne 50) {throw 'Invalid notice mutated store'}
    Request @('notifications','clear')|Out-Null
    $empty=Request @('notifications','list')
    if ($empty.entries.Count -ne 0 -or $empty.unread_count -ne 0 -or $empty.panel_rows -ne 0 -or (Request @('notifications','jump-to-unread')).opened) {throw 'Clear/empty jump failed'}
    Tree|Out-Null
    $evidence.checks+=@{name='bounded_retention_delete_clear_jump_and_invalid_title';passed=$true}
    $otherDirectory=Join-Path $directory 'other';[IO.Directory]::CreateDirectory($otherDirectory)|Out-Null
    $otherProcess=[CliProbe]::Start($gui,@('--temporary','--shell=cmd'),$otherDirectory,$otherDirectory)
    $otherOut=$otherProcess.StandardOutput.ReadToEndAsync();$otherErr=$otherProcess.StandardError.ReadToEndAsync()
    $discovery=Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($otherProcess.Id).json";$deadline=(Get-Date).AddSeconds(25)
    do {
        if ($otherProcess.HasExited -or (Get-Date) -gt $deadline) {throw 'Second owned host startup failed'}
        if (Test-Path $discovery) {
            $record=Get-Content -Raw $discovery|ConvertFrom-Json
            if ($record.pid -ne $otherProcess.Id) {throw 'Wrong second discovery owner'}
            $otherPipe=$record.pipe;break
        }
        Start-Sleep -Milliseconds 50
    } while ($true)
    $primaryPipe=$script:pipeName;$script:pipeName=$otherPipe
    try {
        $otherTree=Tree
        if ((Request @('identify')).pid -ne $otherProcess.Id -or (Request @('notifications','list')).entries.Count -ne 0) {throw 'Wrong second window notification store'}
        Request @('notify','--global','other-window')|Out-Null
        if ((Request @('notifications','list')).entries.Count -ne 1) {throw 'Second window rejected notice'}
        $evidence.otherHost=@{pid=$otherProcess.Id;surfaces=$otherTree.surfaces}
    } finally {$script:pipeName=$primaryPipe}
    if ((Request @('notifications','list')).entries.Count -ne 0) {throw 'Other window changed primary store'}
    $evidence.checks+=@{name='two_hidden_windows_keep_explicit_pipe_notification_stores_isolated';passed=$true}
    $evidence.status='passed_background_notifications_subset'
} catch {
    $evidence.status='failed';$evidence.error=$_.Exception.Message
    throw
} finally {
    if ($otherProcess) {
        $savedPipe=$script:pipeName
        if ($otherPipe) {try {$script:pipeName=$otherPipe;Request @('quit','--discard-state')|Out-Null} catch {} finally {$script:pipeName=$savedPipe}}
        if (-not $otherProcess.HasExited -and -not $otherProcess.WaitForExit(10000)) {$otherProcess.Kill();$otherProcess.WaitForExit()}
        $otherProcess.Dispose()
    }
    if ($pipeName) {try {Request @('quit','--discard-state')|Out-Null} catch {}}
    if ($process) {
        if (-not $process.HasExited -and -not $process.WaitForExit(10000)) {$process.Kill();$process.WaitForExit()}
        $process.Dispose()
    }
    $evidence.finished=(Get-Date).ToString('o')
    $evidence|ConvertTo-Json -Depth 12|Set-Content -Encoding UTF8 (Join-Path $directory 'native-notifications-background.json')
    Write-Output ('Evidence: '+$directory)
}
$evidence|ConvertTo-Json -Depth 12
