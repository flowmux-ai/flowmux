# SPDX-License-Identifier: GPL-3.0-or-later
# Always hidden. No SendInput, visible windows, clipboard or foreground changes.
param([string]$BuildDirectory = "$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference = 'Stop'
$OutputEncoding = [Console]::OutputEncoding = New-Object Text.UTF8Encoding($false)
$BuildDirectory = (Resolve-Path $BuildDirectory).Path
$cli = Join-Path $BuildDirectory 'flowmuxctl.exe'
$doctor = (& $cli doctor | ConvertFrom-Json)
if ($LASTEXITCODE -ne 0 -or -not $doctor.background_testing) { throw 'A working debug build is required; no window was launched.' }
Add-Type -Path (Join-Path $PSScriptRoot 'NativeInput.cs')
$directory = Join-Path $(if ($env:FLOWMUX_TEST_ARTIFACT_ROOT) { $env:FLOWMUX_TEST_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\dist\evidence' }) ('all-search-' + [guid]::NewGuid())
[IO.Directory]::CreateDirectory($directory) | Out-Null
$directory = (Resolve-Path $directory).Path
$evidence = [ordered]@{ started=(Get-Date).ToString('o'); mode='background'; checks=@() }
$process = $null
function Invoke-Flowmux([string[]]$Arguments) {
    $value = & $cli --pipe $script:pipeName --json @Arguments
    if ($LASTEXITCODE -ne 0) { throw "flowmuxctl failed: $Arguments" }
    return (($value -join "`n") | ConvertFrom-Json)
}
function Wait-Ready {
    $deadline = (Get-Date).AddSeconds(25)
    do {
        $tree = Invoke-Flowmux @('tree')
        if (@($tree.surfaces | Where-Object { -not $_.ready -or -not $_.cwd_reported }).Count -eq 0) {
            $window = [IntPtr]([long]$tree.window_handle)
            if (-not $tree.background_testing -or [NativeInput]::IsWindowVisible($window) -or [NativeInput]::GetForegroundWindow() -eq $window) { throw 'Host became visible or foreground' }
            return $tree
        }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    throw 'Terminal did not become ready'
}
function Send-Script([string]$Pane, [string]$Source) {
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Source))
    Invoke-Flowmux @('send-keys', $Pane, "Invoke-Expression ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$encoded')))") | Out-Null
    Invoke-Flowmux @('send-key', 'Enter', '--pane', $Pane) | Out-Null
}
function Find([string]$Surface, [string]$Query, [string[]]$Options=@()) {
    $reply = Invoke-Flowmux (@('find',$Query,'--surface',$Surface) + $Options)
    if ($reply.surface -ne $Surface) { throw 'Find returned the wrong surface' }
    return $reply.result
}
function Wait-Find([string]$Surface, [string]$Query) {
    $deadline = (Get-Date).AddSeconds(12)
    do {
        $found = Find $Surface $Query @('--match-case')
        if ($found.found) { return $found }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    throw "Missing output: $Query"
}
function Require-Match($Result, [string]$Expected) {
    if (-not $Result.found -or $Result.error -or -not [string]::Equals($Result.selection,$Expected,[StringComparison]::Ordinal)) {
        $Result | ConvertTo-Json -Depth 12 | Set-Content -Encoding UTF8 (Join-Path $directory 'failed-match.json')
        throw "Selection mismatch: $Expected"
    }
}
function Wait-File([string]$Path) {
    $deadline = (Get-Date).AddSeconds(12)
    while (-not [IO.File]::Exists($Path)) {
        if ((Get-Date) -gt $deadline) { throw "Missing child marker: $Path" }
        Start-Sleep -Milliseconds 50
    }
}
function Search-All([string]$Query, [string[]]$Options=@()) {
    $result = Invoke-Flowmux (@('search-all',$Query) + $Options)
    $deadline = (Get-Date).AddSeconds(30)
    while ($result.pending) {
        if ((Get-Date) -gt $deadline) { throw 'Search did not finish' }
        Start-Sleep -Milliseconds 50
        $result = Invoke-Flowmux @('search-results',$result.search)
    }
    if ($result.cancelled -or $result.unavailable.Count -ne 0) { $result | ConvertTo-Json -Depth 20; throw 'Search was cancelled or unavailable' }
    if ($result.panel_rows -ne $result.hits.Count) { throw 'Native result list disagrees with the search response' }
    $window=[IntPtr]([long]$result.panel_handle)
    if ([NativeInput]::IsWindowVisible($window) -or [NativeInput]::GetForegroundWindow() -eq $window) { throw 'Search window appeared on the desktop' }
    return $result
}
function Reject([string[]]$Arguments, [string]$Expected) {
    $previousPreference=$ErrorActionPreference
    try { $ErrorActionPreference='Continue'; $answer=& $cli --pipe $script:pipeName --json @Arguments 2>&1; $status=$LASTEXITCODE }
    finally { $ErrorActionPreference=$previousPreference }
    if ($status -eq 0 -or ($answer -join "`n") -notlike "*$Expected*") { throw "Expected rejection $Expected : $answer" }
}
try {
    $previous = $env:FLOWMUX_TEST_BACKGROUND
    try {
        $env:FLOWMUX_TEST_BACKGROUND = '1'
        $process = Start-Process -FilePath (Join-Path $BuildDirectory 'flowmux.exe') -ArgumentList @('--temporary','--cwd',('"'+$directory+'"')) -PassThru
    } finally { $env:FLOWMUX_TEST_BACKGROUND = $previous }
    $discovery = Join-Path $env:LOCALAPPDATA "flowmux\windows\instances\$($process.Id).json"
    $deadline = (Get-Date).AddSeconds(25)
    while (-not (Test-Path $discovery)) {
        if ($process.HasExited -or (Get-Date) -gt $deadline) { throw 'Host did not start' }
        Start-Sleep -Milliseconds 100
    }
    $script:pipeName = (Get-Content -Raw $discovery | ConvertFrom-Json).pipe
    $tree=Wait-Ready
    $origin=Invoke-Flowmux @('identify'); $surface=$origin.surface; $pane=$origin.pane
    $korean=([string][char]0xD55C)+[char]0xAE00
    $needle='ALL_'+$korean
    $unicode=([string][char]0x1112)+[char]0x1161+[char]0x11AB+' e'+[char]0x301+' '+[char]::ConvertFromUtf32(0x1F642)
    $cols=($tree.surfaces | Where-Object { $_.id -eq $surface }).cols
    $text="first $needle`r`n"+('x'*($cols-2))+$needle+"_WRAP`r`n"+$unicode+"`r`n[literal].*`r`n"+[char]0x130+"Z_Case`r`nINITIAL_DONE"
    $encoded=[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($text))
    Send-Script $pane ("[Console]::OutputEncoding=[Text.Encoding]::UTF8; [Console]::WriteLine([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$encoded')))")
    Wait-Find $surface 'INITIAL_DONE' | Out-Null
    Invoke-Flowmux @('new-tab') | Out-Null; Wait-Ready | Out-Null
    $second=Invoke-Flowmux @('identify')
    Send-Script $second.pane ("Write-Output ('"+$needle+" second')")
    Wait-Find $second.surface $needle | Out-Null
    Invoke-Flowmux @('new-workspace','--cwd',$directory) | Out-Null; Wait-Ready | Out-Null
    $third=Invoke-Flowmux @('identify')
    Send-Script $third.pane ("Write-Output ('"+$needle+" third')")
    Wait-Find $third.surface $needle | Out-Null
    $result=Search-All $needle @('--match-case')
    if ($result.total -ne 4 -or $result.hits.Count -ne 4 -or $result.searched -ne 3) { $result | ConvertTo-Json -Depth 20; throw 'Cross-workspace count differs' }
    if ((Invoke-Flowmux @('identify')).surface -ne $third.surface) { throw 'Search changed active focus' }
    $evidence.checks+='Search covers three visible/hidden terminals across two workspaces; four logical matches and native list agree without activating a tab'
    $wrappedIndex=0
    for($i=0;$i -lt $result.hits.Count;$i++){if($result.hits[$i].preview.Contains('_WRAP')){$wrappedIndex=$i}}
    $opened=Invoke-Flowmux @('search-open',$result.search,"$wrappedIndex")
    if ($opened.surface -ne $surface -or $opened.selection -cne $needle) { throw 'Soft-wrap result selected the wrong cells' }
    Send-Script $pane 'Write-Output ("UNRELATED_"+"APPEND")'
    Wait-Find $surface 'UNRELATED_APPEND' | Out-Null
    $opened=Invoke-Flowmux @('search-open',$result.search,"$wrappedIndex")
    if($opened.selection -cne $needle){throw 'Append invalidated retained result'}
    Invoke-Flowmux @('move-tab',$surface,'--to-pane',$third.pane) | Out-Null
    $opened=Invoke-Flowmux @('search-open',$result.search,"$wrappedIndex")
    if($opened.surface -ne $surface -or $opened.selection -cne $needle){throw 'Moved hit lost stable surface identity'}
    $moved=Invoke-Flowmux @('identify')
    $evidence.checks+='Opening selects the exact Korean cells across soft wraps; appended output and same-size workspace move preserve the retained result'
    Invoke-Flowmux @('split','vertical') | Out-Null; Wait-Ready | Out-Null
    $split=Invoke-Flowmux @('identify')
    Reject @('search-open',$result.search,"$wrappedIndex") 'changed'
    Invoke-Flowmux @('close-tab',$split.surface) | Out-Null
    Invoke-Flowmux @('focus-tab',$surface) | Out-Null; Wait-Ready | Out-Null
    $moved=Invoke-Flowmux @('identify')
    $evidence.checks+='A result captured before pane resize is rejected after column reflow; closing the test split preserves the original surface'
    $result=Search-All $unicode @('--match-case')
    if($result.total -ne 1){throw 'Unicode logical line missing'}
    $opened=Invoke-Flowmux @('search-open',$result.search,'0')
    if(-not [string]::Equals($opened.selection,$unicode,[StringComparison]::Ordinal)){throw 'Unicode selected text changed'}
    $result=Search-All '[literal].*'
    if($result.total -ne 1){throw 'Literal query was treated as regex'}
    $result=Search-All 'z_case'
    $opened=Invoke-Flowmux @('search-open',$result.search,'0')
    if($opened.selection -cne 'Z_Case'){throw 'Case-fold expansion corrupted cell offsets'}
    if((Search-All 'z_case' @('--match-case')).total -ne 0){throw 'Match case was ignored'}
    $evidence.checks+='Literal/case matching and selected text preserve NFD Jamo, combining accent and emoji; lowercasing U+0130 does not shift following cell coordinates'
    Send-Script $moved.pane ("[Console]::WriteLine(('w' * " + ($cols*260) + ") + 'LONG_" + $korean + "'); Write-Output ('LONG_'+'DONE')")
    Wait-Find $surface 'LONG_DONE' | Out-Null
    $long=Search-All ('LONG_'+$korean) @('--match-case')
    if($long.total -ne 1){throw 'Long soft-wrap line was split into search records'}
    $opened=Invoke-Flowmux @('search-open',$long.search,'0')
    if($opened.selection -cne ('LONG_'+$korean)){throw 'Long wrapped match selected wrong cells'}
    $evidence.checks+='A logical line spanning more than 260 physical rows is searched and activated across asynchronous scan chunks'
    Send-Script $moved.pane 'Write-Output ((0..502 | ForEach-Object { "PAGED_MATCH_{0:D4}" -f $_ }) -join "`r`n"); Write-Output ("PAGING_"+"DONE")'
    Wait-Find $surface 'PAGING_DONE' | Out-Null
    $page=Search-All 'PAGED_MATCH_' @('--match-case')
    if($page.total -ne 503 -or $page.hits.Count -ne 500){throw 'First page size/count differs'}
    $page2=Search-All 'PAGED_MATCH_' @('--match-case','--offset','500')
    if($page2.total -ne 503 -or $page2.hits.Count -ne 3 -or -not $page2.hits[0].preview.Contains('0500')){throw 'Second page did not continue in logical order'}
    Reject @('search-open',$page.search,'0') 'expired'
    $cancelled=Invoke-Flowmux @('search-cancel',$page2.search)
    if(-not $cancelled.cancelled -or $cancelled.hits.Count -ne 0 -or $cancelled.panel_rows -ne 0){throw 'Cancel retained result references'}
    Reject @('search-open',$page2.search,'0') 'expired'
    $evidence.checks+='503 logical matches page as 500 plus 3; new searches expire old references and cancellation clears native results'
    $rewrite=Join-Path $directory 'rewrite'; $rewritten=Join-Path $directory 'rewritten'; $release=Join-Path $directory 'release'
    Send-Script $moved.pane ('[Console]::Write("STALE_OLD"); while (-not [IO.File]::Exists(' + "'$rewrite'" + ')) { Start-Sleep -Milliseconds 20 }; [Console]::Write(([string][char]27)+"[9DSTALE_NEW"); [IO.File]::WriteAllText(' + "'$rewritten', 'done'" + '); while (-not [IO.File]::Exists(' + "'$release'" + ')) { Start-Sleep -Milliseconds 20 }')
    Wait-Find $surface 'STALE_OLD' | Out-Null
    $old=Search-All 'STALE_OLD' @('--match-case')
    [IO.File]::WriteAllText($rewrite,'go'); Wait-File $rewritten
    Wait-Find $surface 'STALE_NEW' | Out-Null
    Invoke-Flowmux @('focus-tab',$third.surface) | Out-Null
    Reject @('search-open',$old.search,'0') 'changed'
    if((Invoke-Flowmux @('identify')).surface -ne $third.surface){throw 'Stale result changed active focus before validation'}
    Start-Sleep -Milliseconds 1100
    $messages=[NativeInput]::ChildTitles([IntPtr]([long]$old.panel_handle),'STATIC')
    if(-not ($messages -join ' ').Contains('Output changed')){throw 'Native stale-result error was overwritten by idle timers'}
    [IO.File]::WriteAllText($release,'go')
    Invoke-Flowmux @('focus-tab',$surface) | Out-Null
    $evidence.checks+='Opening a rewritten result is rejected after parser-complete same-cursor output, even when its surface remains alive'
    $enter=Join-Path $directory 'alternate-enter';$leave=Join-Path $directory 'alternate-leave'
    Send-Script $moved.pane ('[Console]::Write(([string][char]27)+"[?1049h"+([string][char]27)+"[H"+"ALL_ALT_ONLY"); [IO.File]::WriteAllText(' + "'$enter', 'done'" + '); while (-not [IO.File]::Exists(' + "'$leave'" + ')) { Start-Sleep -Milliseconds 20 }; [Console]::Write(([string][char]27)+"[?1049l"); Write-Output ("NORMAL_"+"RETURNED")')
    Wait-File $enter; Wait-Find $surface 'ALL_ALT_ONLY' | Out-Null
    $alt=Search-All 'ALL_ALT_ONLY' @('--match-case')
    if($alt.total -ne 1){throw 'Alternate search missing'}
    [IO.File]::WriteAllText($leave,'go'); Wait-Find $surface 'NORMAL_RETURNED' | Out-Null
    Reject @('search-open',$alt.search,'0') 'changed'
    if((Search-All 'ALL_ALT_ONLY').total -ne 0){throw 'Alternate text leaked into normal search'}
    $evidence.checks+='Alternate-screen results are rejected after buffer switch; normal history does not include alternate text'
    $evict=Search-All 'STALE_NEW' @('--match-case')
    Send-Script $moved.pane '[Console]::WriteLine(((1..10100 | ForEach-Object { "discard {0}" -f $_ }) -join "`r`n")); Write-Output ("TRIM_"+"DONE")'
    Wait-Find $surface 'TRIM_DONE' | Out-Null
    Reject @('search-open',$evict.search,'0') 'scrollback'
    $evidence.checks+='Scrollback eviction disposes the original marker and rejects activation instead of selecting another row'
    $tree=Wait-Ready; $evidence.host=$process.Id; $evidence.surfaces=$tree.surfaces.Count
    Invoke-Flowmux @('quit','--discard-state') | Out-Null
    if(-not $process.WaitForExit(10000)){throw 'Host did not close'}
    $evidence.finished=(Get-Date).ToString('o')
    $evidence.interactive='Native search window stayed hidden. Mouse, keyboard, Microsoft IME, visual layout and DPI remain pending.'
} catch {
    $evidence.status='failed';$evidence.error=$_.Exception.Message
    $evidence | ConvertTo-Json -Depth 20 | Set-Content -Encoding UTF8 (Join-Path $directory 'native-output-search-background.json')
    throw
} finally { if($process -and -not $process.HasExited){$process.Kill();$process.WaitForExit()} }
[ordered]@{status='passed';checks=$evidence.checks.Count}|ConvertTo-Json -Compress
