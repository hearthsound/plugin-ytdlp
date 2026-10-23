# Hearthsound yt-dlp acquisition plugin

A [Hearthsound](https://github.com/hearthsound) acquisition plugin that
downloads tracks with [yt-dlp](https://github.com/yt-dlp/yt-dlp). The server
hands the plugin a wanted track, the plugin asks yt-dlp for search results,
offers one candidate per result with its best audio-only format, and downloads
the candidate the server picked. The imported file is reported under the
staging root, so the server handles it like the output of any other
downloading provider.

The plugin runs yt-dlp through the host's `host-process` import. It never sees
a path to the tool and requests no HTTP permission: all network access happens
inside yt-dlp, with the arguments the plugin builds.

The plugin provides only the `track` granularity. Album acquisition stays with
providers that can fetch a whole album from one source.

## Requirements

- Rust 1.99 or newer, with the `wasm32-wasip2` target. The included
  `rust-toolchain.toml` pins both; `rustup toolchain install` fetches them.
- yt-dlp installed on the machine that runs the server.

An administrator must set `tools.yt-dlp.path` to the absolute path of the
yt-dlp executable. The plugin's manifest declares `yt-dlp` as a process tool,
and the host starts only that name through the configured path; while the
setting is empty every spawn fails as `not-configured` and the acquisition log
names the setting. The container image's `runtime-ytdlp` variant installs
yt-dlp and Deno and points the setting at them, so the variant needs no extra
configuration.

yt-dlp needs a JavaScript runtime for YouTube. It looks for Deno on `PATH`;
the plugin passes `PATH` and `HOME` to the process, which is what yt-dlp needs
to find Deno and ffmpeg and to keep its cache. Every other server environment
variable is cleared.

## Build

```console
cargo build --release --locked --target wasm32-wasip2
```

The component lands at
`target/wasm32-wasip2/release/hearthsound_plugin_ytdlp.wasm`.

## Check and pack

Install the plugin CLI from the SDK repository:

```console
cargo install --locked --git https://github.com/hearthsound/plugin-sdk --tag v0.3.0 hearthsound-plugin-cli
```

Then validate the manifest and the component:

```console
hsplugin check
```

It prints `ok: app.hearthsound.ytdlp 0.1.0 (provides acquisition)` on success.

Finally, build the archive you install on a server:

```console
hsplugin pack --out-dir dist
```

`dist/app.hearthsound.ytdlp-0.1.0.hsplugin` contains the manifest and the
component.

## Tests

The argument vectors, the search output parsing, the candidate token, the
progress and path parsing and the exit handling run on the host target, so
their unit tests need no WebAssembly toolchain, no yt-dlp and no network:

```console
cargo test
```

## Configuration

| Instance setting | Default | Meaning |
|---|---|---|
| `searchCount` | `5` | How many search results one query asks yt-dlp for, 1 to 10. |
| `searchPrefix` | `ytsearch` | The yt-dlp search prefix: `ytsearch` for YouTube, `scsearch` for SoundCloud. |
| `downloadTimeoutSecs` | `900` | How long one download may run before the host kills yt-dlp, 60 to 3600. |

The instance requests no host-side rate limit: a search is one process and a
download is one process, and the server already bounds how many acquisitions
run at once.

## Notes

- A search runs
  `yt-dlp --ignore-config --no-warnings --no-playlist --skip-download --dump-json`
  with `<prefix><count>:<artist> <title>` as the search text, where `<artist>`
  is the track's primary artist. Standard output is one JSON object per
  result; the plugin keeps the results that carry both a page URL and an
  audio-only format.
- The chosen format is the audio-only format (`vcodec` equal to `none`) with
  the highest average bitrate. Formats without a bitrate rank below every
  format that has one.
- The file's `format` comes from the format's audio codec: `opus` stays
  `opus`, an `mp4a` codec becomes `aac`, and `vorbis` and `mp3` keep their
  names. A codec the server does not name leaves the field empty. The bitrate
  is yt-dlp's average in kbps, rounded; the sample rate is its audio sample
  rate; the size is the exact or estimated file size yt-dlp reports.
- A candidate's source key is `<extractor>:<channel id>`, falling back to the
  uploader id and then to the extractor alone, so every result from one
  channel shares a key.
- A candidate token is URL-safe base64 without padding over a small JSON
  object holding the page URL, the format id, the container extension and the
  video id. The token carries everything a download needs, so it survives the
  server recycling a plugin instance; there is no server-side state a token
  points at.
- A download runs
  `yt-dlp --ignore-config --no-playlist -f <format id> -x --audio-format best --embed-metadata --newline --progress-template "download:%(progress.downloaded_bytes)s/%(progress.total_bytes)s" -o "<key>/%(id)s.%(ext)s" --print after_move:filepath <page URL>`.
  `--audio-format best` keeps the chosen audio codec, so the file is not
  re-encoded, and `--embed-metadata` writes the site's tags.
- Progress comes from the `download:<downloaded>/<total>` lines; a `NA` total
  leaves the total unknown. A read can split a line, so the download keeps the
  unfinished line's bytes between polls.
- When yt-dlp exits with status 0, the plugin takes the last path line it
  printed, keeps only its file name, and reports `<key>/<file name>` under the
  staging root after checking through `host-files` that the file is there. A
  non-zero exit reports the last standard error line, falling back to the last
  standard output line and then to the exit status.
- `cancel-search` and `cancel-download` kill the process and forget the
  token's record. A tool the manifest does not declare, a tool without a
  configured path, and an option the host rejects are reported as
  configuration errors; a process the operating system refuses to start is
  reported as unavailable.
- `health` runs `yt-dlp --version` and answers with the version line, so an
  administrator sees at a glance whether the configured executable works.

## Layout

| Path | Contents |
|---|---|
| `Cargo.toml` | Crate metadata and the SDK dependency. |
| `src/lib.rs` | The plugin: lifecycle, search and download state, and the acquisition interface. |
| `src/args.rs` | The yt-dlp argument vectors and the search text. |
| `src/config.rs` | The instance configuration and its schema-level checks. |
| `src/progress.rs` | Progress lines, printed paths and exit handling. |
| `src/search.rs` | `--dump-json` parsing: format choice, codec names and source keys. |
| `src/token.rs` | The candidate token and the stored byte encoding. |
| `manifest.json` | The plugin manifest the server reads. |
| `src/fixtures/search.json` | A captured search output the parsing tests run on. |

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
