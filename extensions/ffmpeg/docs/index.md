# pe2.ffmpeg

Video files written and read through ffmpeg, offered as the `video-container` service: what makes a movie into a video
file, or reads one back, uses whichever extension provides it.

It runs `ffmpeg` and `ffprobe` - fetched for Linux, Windows and Apple silicon, the system's elsewhere - so it runs
outside the sandbox: you trust it before anything of it runs, and trusting it fetches them. Where they come from is in
[installing](install.md).
