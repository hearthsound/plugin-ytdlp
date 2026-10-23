# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Candidate naming: a search result's candidate now carries an empty path and a
  file name built from the video title, prefixed with the uploader when the
  title does not name the channel, so the server derives the artist and title
  it matches against instead of scoring a bare video id and rejecting every
  candidate.

## [0.1.1] - 2026-10-23

### Fixed

- Health reporting: `health` no longer starts yt-dlp outside an acquisition,
  where the host denies the spawn and the setup summary showed the instance as
  an invalid configuration. It validates the configuration and says that
  yt-dlp is checked when an acquisition runs.
- Tool failures: every `host-process` error now reaches the server with its
  detail, a denied spawn or a failed start as `unavailable` and an empty
  `tools.yt-dlp.path` as `invalid-config`, and a search output line that is
  not a result object fails the search with that line's text instead of an
  internal error.
- Search and download state: a search kept its whole metadata dump between
  polls, which exceeded the host's value cap on real output; it now asks yt-dlp
  to project only the fields a candidate needs, parses each complete line as
  it arrives and stores the reduced candidates with the unfinished line. The
  download's progress template and `--no-quiet` make progress and the printed
  output path parse from current yt-dlp output.

## [0.1.0] - 2026-10-23

### Added

- The repository scaffolding: the crate manifest, the toolchain pin, the dual
  MIT/Apache-2.0 license files, this changelog and the manifest that declares
  `yt-dlp` as the plugin's process tool.
- Track search through yt-dlp: the search text, the metadata-only argument
  vector, the parsing of `--dump-json` output into one candidate per result
  with its best audio-only format, the codec and source-key mapping, and the
  candidate token that carries a download's values.
- Download monitoring: the audio-extraction argument vector, the progress
  template, the line-buffered parsing of progress and of the printed output
  path, the staging check that confirms the file exists, and the exit handling
  that turns a failure into yt-dlp's last error line.
- Configuration validation and the health probe: `searchCount`,
  `searchPrefix` and `downloadTimeoutSecs`, with `yt-dlp --version` behind
  `health`.
- A CI workflow that formats, lints, tests, builds, checks and packs the
  plugin, and uploads the archive.

[Unreleased]: https://github.com/hearthsound/plugin-ytdlp/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/hearthsound/plugin-ytdlp/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/hearthsound/plugin-ytdlp/releases/tag/v0.1.0
