# SPDX-License-Identifier: GPL-3.0-or-later
param([switch]$Installer)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path "$PSScriptRoot\..").Path
$previousFlags = $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS
try {
    # Installer payloads must not require a separately installed VC++ Redistributable.
    $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = '-C target-feature=+crt-static'
    & cargo build --manifest-path "$root\Cargo.toml" --target x86_64-pc-windows-msvc --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'Windows release build failed' }
    & python "$PSScriptRoot\fetch-conpty.py" --output "$root\target\x86_64-pc-windows-msvc\release"
    if ($LASTEXITCODE -ne 0) { throw 'ConPTY support files could not be verified' }
    if ($Installer) {
        & python "$PSScriptRoot\rust-notices.py"
        if ($LASTEXITCODE -ne 0) { throw 'Dependency notices could not be generated' }
        New-Item -ItemType Directory -Path "$root\dist" -Force | Out-Null
        & makensis "$root\installer.nsi"
        if ($LASTEXITCODE -ne 0) { throw 'Installer build failed' }
    }
} finally { $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = $previousFlags }
