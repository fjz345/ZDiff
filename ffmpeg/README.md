`cargo xtask fetch-ffmpeg` downloads a pinned LGPL ffmpeg build here (`ffmpeg.exe`, `ffprobe.exe`, `ffmpeg-LICENSE.txt`, `ffmpeg-SOURCE.txt`). The zdiff-gui build copies them next to its exe. Everything but this file is gitignored.

Windows only: on Linux and macOS `fetch-ffmpeg` exits with an error, the build copies nothing and `cargo dist` leaves ffmpeg out. Install ffmpeg with the package manager (`apt install ffmpeg`, `dnf install ffmpeg`, `brew install ffmpeg`); zdiff-gui finds ffmpeg and ffprobe on PATH.
