# Installing ffmpeg

Nothing: pe2.ffmpeg fetches `ffmpeg` and `ffprobe` 9.0.2 once you trust it, built by the editor's project from FFmpeg's
sources with the codec, format, subtitle and graphics card libraries common builds of it have - libx264 among them -
and published with the editor: for Linux on
x86_64 (glibc 2.28 or newer), Windows on x86_64, and macOS on Apple silicon (macOS 15 or newer). Each is held to its
hashes, and kept for every project on this computer. Each release holds the licenses of everything built into it and
every source it was built from.

Elsewhere - an Intel Mac, Linux on ARM - nothing is fetched, and the system's are run, which have to be on the search
path and have libx264 for the lossless encoding pe2.ffmpeg writes by default:

- Debian and Ubuntu: `sudo apt install ffmpeg`
- Fedora: `sudo dnf install ffmpeg` (from RPM Fusion: Fedora's own `ffmpeg-free` has no libx264)
- macOS, with Homebrew: `brew install ffmpeg`
