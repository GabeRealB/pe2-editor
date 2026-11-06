# Installing ffmpeg

Nothing: pe2.ffmpeg fetches `ffmpeg` and `ffprobe` 9.0.2, built with libx264, once you trust it - for Linux on x86_64
from BtbN's static FFmpeg-Builds (glibc 2.28 or newer), for Windows from gyan.dev's builds, for macOS on Apple silicon
(macOS 12 or newer) from Martin Riedl's builds.
Each is held to its hashes, and kept for every project on this computer.

Elsewhere - an Intel Mac, Linux on ARM - nothing is fetched, and the system's are run, which have to be on the search
path and have libx264 for the lossless encoding pe2.ffmpeg writes by default:

- Debian and Ubuntu: `sudo apt install ffmpeg`
- Fedora: `sudo dnf install ffmpeg` (from RPM Fusion: Fedora's own `ffmpeg-free` has no libx264)
- macOS, with Homebrew: `brew install ffmpeg`
