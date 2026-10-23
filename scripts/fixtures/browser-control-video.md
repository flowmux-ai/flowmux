<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

`browser-control.mp4` is a four-second synthetic silent test pattern, generated
for the browser media regression test. No external footage is included.
The matrix, primaries, transfer function and range are explicit so pixel
comparisons do not depend on decoder defaults for untagged video.

Regenerate with:

```sh
ffmpeg -f lavfi -i testsrc2=size=96x64:rate=10 -t 4 \
  -vf colorspace=iall=bt601-6-625:all=bt709:trc=srgb:range=tv -c:v libx264 \
  -pix_fmt yuv420p -movflags +faststart -an scripts/fixtures/browser-control.mp4
```
