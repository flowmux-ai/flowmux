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
$directory = Join-Path $(if ($env:FLOWMUX_TEST_ARTIFACT_ROOT) { $env:FLOWMUX_TEST_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\dist\evidence' }) ('find-' + [guid]::NewGuid())
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
    $tree = Wait-Ready
    $origin = Invoke-Flowmux @('identify'); $surface=$origin.surface; $pane=$origin.pane
    $korean = ([string][char]0xD55C) + [char]0xAE00
    $jamo = ([string][char]0x1112) + [char]0x1161 + [char]0x11AB
    $needle = 'FIND_' + $korean
    $unicode = $jamo + ' e' + [char]0x301 + ' ' + [char]::ConvertFromUtf32(0x1F642)
    $columns = ($tree.surfaces | Where-Object { $_.id -eq $surface }).cols
    $text = 'first ' + $needle + "`r`nCaseToken_AbC`r`n" + $unicode + "`r`n" + ('x' * ($columns-2)) + $korean + '_SOFT_WRAP' + "`r`n" + 'second ' + $needle + "`r`nthird " + $needle + "`r`n" + ((1..100 | ForEach-Object { "filler $_`r`n" }) -join '') + 'FIND_OUTPUT_DONE'
    $payload = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($text))
    Send-Script $pane ("[Console]::OutputEncoding=[Text.Encoding]::UTF8; [Console]::WriteLine([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$payload')))")
    Wait-Find $surface 'FIND_OUTPUT_DONE' | Out-Null
    $positions = @()
    for ($i=0; $i -lt 4; $i++) {
        $found = Find $surface $needle @('--match-case'); Require-Match $found $needle
        $positions += $found.position.start.y
    }
    if ($positions[0] -ne $positions[3] -or @($positions[0..2] | Select-Object -Unique).Count -ne 3) { throw 'Forward search did not wrap across the three retained matches' }
    $previousMatch = Find $surface $needle @('--match-case','--previous')
    if ($previousMatch.position.start.y -ne $positions[2]) { throw 'Previous search did not wrap' }
    $evidence.checks += 'Retained history searches next/previous and wraps across three Korean matches after 100 filler lines'
    Require-Match (Find $surface 'casetoken_abc') 'CaseToken_AbC'
    if ((Find $surface 'casetoken_abc' @('--match-case')).found) { throw 'Match-case ignored case' }
    Require-Match (Find $surface 'CaseToken_[A-Z][a-z][A-Z]' @('--regex','--match-case')) 'CaseToken_AbC'
    $invalid = Find $surface '[' @('--regex')
    if ($invalid.error -ne 'Invalid regular expression' -or $invalid.selection) { throw 'Invalid regex retained a stale selection or lacked an error' }
    Require-Match (Find $surface 'CaseToken_AbC') 'CaseToken_AbC'
    $evidence.checks += 'Case-sensitive/insensitive and regular-expression searches work; invalid regex clears selection and valid search recovers'
    Require-Match (Find $surface $unicode @('--match-case')) $unicode
    Require-Match (Find $surface ($korean+'_SOFT_WRAP') @('--match-case')) ($korean+'_SOFT_WRAP')
    $evidence.checks += 'Selected text preserves decomposed Jamo, combining accent and emoji; Korean query crosses a soft-wrap boundary'
    $closed = Invoke-Flowmux @('find','--surface',$surface,'--close')
    if ($closed.result.visible -or $closed.result.selection) { throw 'Close did not hide find bar and clear selection' }
    Invoke-Flowmux @('new-tab') | Out-Null
    $tree = Wait-Ready
    $other = Invoke-Flowmux @('identify')
    Require-Match (Find $surface $needle) $needle
    if ((Invoke-Flowmux @('identify')).surface -ne $other.surface) { throw 'Inactive find changed active tab' }
    if ((Find $other.surface $needle).found) { throw 'Results leaked between surfaces' }
    Invoke-Flowmux @('new-workspace','--cwd',$directory) | Out-Null
    $tree = Wait-Ready
    $destination = Invoke-Flowmux @('identify')
    Invoke-Flowmux @('move-tab',$surface,'--to-pane',$destination.pane) | Out-Null
    Require-Match (Find $surface $needle) $needle
    $moved = Invoke-Flowmux @('identify')
    Send-Script $moved.pane 'Write-Output ("FRESH_" + "FIND_OUTPUT")'
    Require-Match (Wait-Find $surface 'FRESH_FIND_OUTPUT') 'FRESH_FIND_OUTPUT'
    $evidence.checks += 'Hidden surface lookup leaves active focus intact, new tabs have independent search state, moved surface and fresh output stay searchable'
    $rewrite = Join-Path $directory 'rewrite'; $rewritten = Join-Path $directory 'rewritten'; $release = Join-Path $directory 'release'
    $source = '[Console]::Write("CACHE_OLD"); while (-not [IO.File]::Exists(' + "'$rewrite'" + ')) { Start-Sleep -Milliseconds 20 }; [Console]::Write(([string][char]27)+"[9DCACHE_NEW"); [IO.File]::WriteAllText(' + "'$rewritten', 'done'" + '); while (-not [IO.File]::Exists(' + "'$release'" + ')) { Start-Sleep -Milliseconds 20 }'
    Send-Script $moved.pane $source
    Wait-Find $surface 'CACHE_OLD' | Out-Null
    [IO.File]::WriteAllText($rewrite, 'go'); Wait-File $rewritten
    # Query the completed grid first so the next find is after the rewrite's parser ACK.
    $deadline = (Get-Date).AddSeconds(4)
    do {
        $screen = Invoke-Flowmux @('read-screen','--surface',$surface)
        if ($screen.text.Contains('CACHE_NEW')) { break }
        Start-Sleep -Milliseconds 50
    } while ((Get-Date) -lt $deadline)
    if (-not $screen.text.Contains('CACHE_NEW')) { throw 'Rewrite did not reach the terminal grid' }
    if ((Find $surface 'CACHE_OLD').found) { throw 'Search cache retained overwritten text' }
    Require-Match (Find $surface 'CACHE_NEW') 'CACHE_NEW'
    [IO.File]::WriteAllText($release, 'go')
    $evidence.checks += 'In-place rewrite ending at the same cursor position invalidates stale search text immediately'
    $enter = Join-Path $directory 'alternate-enter'; $leave = Join-Path $directory 'alternate-leave'
    $source = '[Console]::Write(([string][char]27)+"[?1049h"+([string][char]27)+"[H"+"ALTERNATE_FIND_ONLY"); [IO.File]::WriteAllText(' + "'$enter', 'done'" + '); while (-not [IO.File]::Exists(' + "'$leave'" + ')) { Start-Sleep -Milliseconds 20 }; [Console]::Write(([string][char]27)+"[?1049l"); Write-Output ("NORMAL_"+"RETURNED")'
    Send-Script $moved.pane $source
    Wait-File $enter
    $alt = Wait-Find $surface 'ALTERNATE_FIND_ONLY'; Require-Match $alt 'ALTERNATE_FIND_ONLY'
    if ($alt.buffer -ne 'alternate' -or (Find $surface $needle).found) { throw 'Alternate search leaked normal-buffer history' }
    [IO.File]::WriteAllText($leave, 'go')
    Wait-Find $surface 'NORMAL_RETURNED' | Out-Null
    Require-Match (Find $surface $needle) $needle
    if ((Find $surface 'ALTERNATE_FIND_ONLY').found) { throw 'Normal search retained alternate-buffer text' }
    $evidence.checks += 'Alternate-screen search is isolated from normal history; normal history remains searchable after returning'
    Send-Script $moved.pane 'Write-Output ("FIND_EXIT_" + "RETAINED"); exit 7'
    $deadline = (Get-Date).AddSeconds(12)
    do {
        $tree = Invoke-Flowmux @('tree')
        $exited = $tree.surfaces | Where-Object { $_.id -eq $surface }
        if ($exited.resources_released) { break }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    if (-not $exited.resources_released -or $exited.exit_code -ne 7) { throw 'Shell did not exit cleanly' }
    Require-Match (Find $surface 'FIND_EXIT_RETAINED') 'FIND_EXIT_RETAINED'
    $evidence.checks += 'Exited terminal remains searchable after native process resources are released'
    $tree = Wait-Ready
    $evidence.origin=$surface; $evidence.positions=$positions; $evidence.host=$process.Id
    Invoke-Flowmux @('quit','--discard-state') | Out-Null
    if (-not $process.WaitForExit(10000)) { throw 'Host did not close' }
    $evidence.finished=(Get-Date).ToString('o')
    $evidence.interactive='Real search-field IME, buttons, keyboard navigation and DPI remain pending; no desktop input was injected.'
    $evidence | ConvertTo-Json -Depth 20 | Set-Content -Encoding UTF8 (Join-Path $directory 'native-find-background.json')
    $evidence | ConvertTo-Json -Depth 20
} finally {
    if ($process -and -not $process.HasExited) { $process.Kill(); $process.WaitForExit() }
}
