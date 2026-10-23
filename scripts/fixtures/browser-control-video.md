<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

`browser-control.mp4` is a four-second synthetic silent test pattern, generated
for the browser media regression test. No external footage is included.

Regenerate with:

```sh
ffmpeg -f lavfi -i testsrc2=size=96x64:rate=10 -t 4 -c:v libx264 \
  -pix_fmt yuv420p -movflags +faststart -an scripts/fixtures/browser-control.mp4
```
