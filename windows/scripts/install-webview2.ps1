# SPDX-License-Identifier: GPL-3.0-or-later
# Microsoft's documented Evergreen bootstrapper. Runs only when doctor reports a missing/old runtime.
$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$directory = Join-Path ([IO.Path]::GetTempPath()) ('flowmux-webview2-' + [Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $directory | Out-Null
try {
    $bootstrapper = Join-Path $directory 'MicrosoftEdgeWebview2Setup.exe'
    Invoke-WebRequest -UseBasicParsing -Uri 'https://go.microsoft.com/fwlink/p/?LinkId=2124703' -OutFile $bootstrapper
    $signature = Get-AuthenticodeSignature -FilePath $bootstrapper
    if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation(?:,|$)') {
        throw 'The WebView2 bootstrapper does not have a valid Microsoft signature.'
    }
    $process = Start-Process -FilePath $bootstrapper -ArgumentList @('/silent', '/install') -PassThru -Wait
    if ($process.ExitCode -ne 0) { throw "WebView2 installation failed with exit code $($process.ExitCode)" }
} catch {
    Write-Error $_
    exit 1
} finally {
    Remove-Item -LiteralPath $directory -Recurse -Force -ErrorAction SilentlyContinue
}
