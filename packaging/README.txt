Squeeze: shrink NVIDIA ShadowPlay clips to share on Discord
============================================================

  https://github.com/kayex/squeeze


WHAT'S IN HERE
--------------

  squeeze.exe       The app. Double-click it, then drag video files onto the
                    window. Start here.

  squeeze-cli.exe   Command-line version, for scripting:
                        squeeze-cli.exe --max-mb 10 "C:\path\to\clip.mp4"

Both are self-contained. Nothing to install, no FFmpeg, no DLLs.


HOW IT WORKS
------------

Drop one or more clips on the window and pick a size limit (10 MB is Discord's
free-tier upload cap; 50 and 500 MB are the Nitro tiers). Each clip is
re-encoded to fit just under that limit and saved next to the original with a
"_discord" suffix. Your originals are never modified.

Encoding uses your NVIDIA GPU (NVENC) when one is available, which is fast and
barely touches the CPU. Without a suitable GPU or driver it falls back to
software encoding automatically, which is slower but gives the same result.


"WINDOWS PROTECTED YOUR PC"
---------------------------

You will probably see a blue warning box the first time you run this, saying
the publisher is unknown.

That is expected. It appears because this build is not code-signed. A
certificate costs money and has to be renewed, and this is a free hobby
project. It is not a statement that anything was found wrong with the file.

To run it anyway:  click "More info", then "Run anyway".

If you would rather verify the download first, every release publishes SHA-256
checksums, and the binaries carry a GitHub build attestation proving they were
produced by the public build workflow in the repository above. See the release
notes for how to check both.


LICENCE
-------

Squeeze is MIT licensed. It links the FFmpeg libraries under the LGPL v2.1
(built without GPL or non-free components). See LICENSE for details.
