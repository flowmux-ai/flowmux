# SPDX-License-Identifier: GPL-3.0-or-later
param([string]$Installer = "$PSScriptRoot\..\dist\flowmux-windows-0.10.1-dev-x64-setup.exe")
$ErrorActionPreference = 'Stop'
$Installer = (Resolve-Path $Installer).Path
$registry = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\flowmux-windows'
if ((Test-Path $registry) -or (Test-Path 'HKCU:\Software\flowmux\Windows')) {
    throw 'An existing Windows flowmux installation is present; refusing to replace it in this test.'
}
$installDirectory = Join-Path $env:LOCALAPPDATA ('Programs\flowmux-native-test-' + [Guid]::NewGuid().ToString())
$environment = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment')
$oldPath = $environment.GetValue('Path', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
$oldKind = if ($environment.GetValueNames() -contains 'Path') { $environment.GetValueKind('Path').ToString() } else { 'Absent' }
$environment.Dispose()
$evidence = [ordered]@{ started = (Get-Date).ToString('o'); installDirectory = $installDirectory; checks = @() }
try {
    $process = Start-Process -FilePath $Installer -ArgumentList ('/S /D=' + $installDirectory) -PassThru -Wait
    if ($process.ExitCode -ne 0) { throw "Installer failed: $($process.ExitCode)" }
    $cli = Join-Path $installDirectory 'flowmuxctl.exe'
    $doctor = & $cli doctor
    if ($LASTEXITCODE -ne 0) { throw 'Installed CLI doctor failed' }
    $consoleDoctor = & (Join-Path $installDirectory 'flowmux.com') --json doctor
    if ($LASTEXITCODE -ne 0 -or ($consoleDoctor | ConvertFrom-Json).status -ne 'ok') { throw 'Installed console entry point failed' }
    $evidence.checks += @{ name = 'installed_native_binaries_and_runtime'; passed = $true; doctor = ($doctor -join "`n") }
    if (-not (Test-Path $registry)) { throw 'Uninstall registration missing' }
    if (-not (Test-Path (Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\flowmux (Windows)\flowmux.lnk'))) { throw 'Start menu entry missing' }
    $newPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not ($newPath -split ';' -contains $installDirectory)) { throw 'User PATH entry missing' }
    $evidence.checks += @{ name = 'start_menu_uninstall_registration_user_path'; passed = $true }
    # Upgrade the same test installation. Existing sessions are not terminated by the installer.
    $process = Start-Process -FilePath $Installer -ArgumentList ('/S /D=' + $installDirectory) -PassThru -Wait
    if ($process.ExitCode -ne 0) { throw 'Upgrade failed' }
    $count = @([Environment]::GetEnvironmentVariable('Path', 'User') -split ';' | Where-Object { $_ -eq $installDirectory }).Count
    if ($count -ne 1) { throw 'Upgrade duplicated the PATH entry' }
    $evidence.checks += @{ name = 'repeat_install_is_idempotent'; passed = $true }
    $evidence.status = 'passed_install_subset'
} catch {
    $evidence.status = 'failed'; $evidence.error = $_.Exception.Message
    throw
} finally {
    $uninstaller = Join-Path $installDirectory 'Uninstall.exe'
    if (Test-Path $uninstaller) {
        # _?= runs in-place, allowing WaitForExit to wait for the real uninstaller.
        $process = Start-Process -FilePath $uninstaller -ArgumentList ('/S _?=' + $installDirectory) -PassThru -Wait
        $evidence.uninstallExitCode = $process.ExitCode
        if (Test-Path $uninstaller) { Remove-Item -LiteralPath $uninstaller }
        if ((Test-Path $installDirectory) -and @(Get-ChildItem $installDirectory -Force).Count -eq 0) { Remove-Item $installDirectory }
    }
    $environment = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment')
    $newPath = $environment.GetValue('Path', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    $newKind = if ($environment.GetValueNames() -contains 'Path') { $environment.GetValueKind('Path').ToString() } else { 'Absent' }
    $environment.Dispose()
    $restored = ($oldPath -ceq $newPath -and $oldKind -eq $newKind -and -not (Test-Path $registry))
    $evidence.checks += @{ name = 'uninstall_restores_exact_user_path_and_value_type'; passed = $restored }
    if (-not $restored) { $evidence.status = 'failed'; $evidence.cleanupError = 'Uninstall did not restore PATH/registration' }
    $evidence.finished = (Get-Date).ToString('o')
    $directory = Join-Path $PSScriptRoot '..\dist\evidence'
    New-Item -ItemType Directory -Path $directory -Force | Out-Null
    $evidence | ConvertTo-Json -Depth 10 | Set-Content -Encoding UTF8 (Join-Path $directory 'installer-smoke.json')
}
$evidence | ConvertTo-Json -Depth 10
if ($evidence.status -ne 'passed_install_subset' -or $evidence.uninstallExitCode -ne 0) { throw 'Installer checks failed; see installer-smoke.json.' }
