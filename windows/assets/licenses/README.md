<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

The webview2-com crates omit the repository license from their crate archives.
`webview2-rs-MIT.txt` is copied from their published source revision:
https://raw.githubusercontent.com/wravery/webview2-rs/edc2caf886175ccaebe86078c9cfe1ae2a187328/LICENSE

The Microsoft SDK license and notice are copied from the NuGet
package matching webview2-com-sys 0.39.1's SDK version, 1.0.3650.58:
https://api.nuget.org/v3-flatcontainer/microsoft.web.webview2/1.0.3650.58/microsoft.web.webview2.1.0.3650.58.nupkg

`scripts/rust-notices.py` combines these with the other locked dependency
licenses into the installed THIRD_PARTY_RUST.txt. The WebView2 runtime is
installed separately using Microsoft's signed Evergreen bootstrapper.

`Microsoft-ConPTY-LICENSE.txt` is the Microsoft Terminal project's MIT license:
https://raw.githubusercontent.com/microsoft/terminal/main/LICENSE
The official Microsoft.Windows.Console.ConPTY 1.24.260710001 package declares
that MIT license. Its exact package URL, SHA-256, and native file layout are
pinned in `conpty.lock.json`; package binaries are fetched into build outputs.
License wording is retained; line endings and trailing whitespace are normalized.
