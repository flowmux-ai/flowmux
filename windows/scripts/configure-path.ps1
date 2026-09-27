# SPDX-License-Identifier: GPL-3.0-or-later
param([ValidateSet('Add', 'Remove')][string]$Action, [Parameter(Mandatory=$true)][string]$Directory)
$ErrorActionPreference = 'Stop'
$directoryKey = $Directory.TrimEnd('\')
$environment = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
$metadata = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Software\flowmux\Windows')
try {
    $present = $environment.GetValueNames() -contains 'Path'
    $kind = if ($present) { $environment.GetValueKind('Path') } else { [Microsoft.Win32.RegistryValueKind]::ExpandString }
    $previous = $environment.GetValue('Path', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    $parts = if ($null -eq $previous) { @() } else { @($previous -split ';') }
    $filtered = @($parts | Where-Object {
        -not [string]::Equals($_.Trim('"').TrimEnd('\'), $directoryKey, [StringComparison]::OrdinalIgnoreCase)
    })
    if ($Action -eq 'Add') {
        if ($null -eq $metadata.GetValue('PathOriginallyPresent')) {
            $metadata.SetValue('PathOriginallyPresent', [int]$present, [Microsoft.Win32.RegistryValueKind]::DWord)
        }
        if ($filtered.Count -eq $parts.Count) { $parts += $Directory }
        $next = $parts -join ';'
    } else { $next = $filtered -join ';' }
    if ($Action -eq 'Remove' -and $filtered.Count -eq 0 -and $metadata.GetValue('PathOriginallyPresent', 1) -eq 0) {
        $environment.DeleteValue('Path', $false)
    } elseif ($next -cne $previous) {
        # Keep REG_EXPAND_SZ and literal percent variables; do not expand unrelated entries.
        $environment.SetValue('Path', $next, $kind)
    }
} finally { $environment.Dispose(); $metadata.Dispose() }
