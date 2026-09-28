# SPDX-License-Identifier: GPL-3.0-or-later
# Compatibility entry point for the current native keybindings UI verification.
param([string]$BuildDirectory="$PSScriptRoot\..\target\x86_64-pc-windows-msvc\debug")
$ErrorActionPreference='Stop'
& (Join-Path $PSScriptRoot 'verify-keybindings-dialog.ps1') -BuildDirectory $BuildDirectory
