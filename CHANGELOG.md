# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Health reporting: `health` no longer starts yt-dlp outside an acquisition,
  where the host denies the spawn and the setup summary showed the instance as
  an invalid configuration. It validates the configuration and says that
  yt-dlp is checked when an acquisition runs.

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

[Unreleased]: https://github.com/hearthsound/plugin-ytdlp/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/hearthsound/plugin-ytdlp/releases/tag/v0.1.0
