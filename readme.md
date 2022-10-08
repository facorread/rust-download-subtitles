# rust-download-subtitles
Download subtitles for TV shows from OpenSubtitles

Copyright © 2022 Fabio A. Correa Duran facorread@gmail.com

This is a client for the new OpenSubtitles.com REST API. The software focuses on downloading subtitles for TV shows, while respecting the OpenSubtitles download limits per account and per session. The client is published in source code form, so that anyone can customize the output file names or folders.

The config.ron file specifies the TV show, season, and episodes. It also requires an API key and a valid account. You can obtain an API key at

https://www.opensubtitles.com/en/consumers

## Development notes

`rust-download-subtitles` uses `tokio` to process episodes asynchronously. One task is spawned per episode. The main task maintains the HTTP connection to OpenSubtitles.com, authenticates the user, and enforces the download and request limits.

This client does not aim to be a complete solution. It does not implement any command-line or environment-variable parsing. Also, it does not offer an API.

