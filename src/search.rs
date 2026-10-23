//! Turning `yt-dlp --dump-json` output into candidates.
//!
//! A search asks yt-dlp for metadata only, so standard output is one JSON
//! object per result. Each object lists every format the site offers; the
//! plugin keeps the best audio-only one, which is what a download then asks
//! yt-dlp to fetch and extract.

use hearthsound_plugin::process_acquisition_provider::exports::hearthsound::plugin::acquisition::{
    Candidate, CandidateFile, SourceInfo,
};
use serde::Deserialize;

use crate::token::DownloadToken;

/// The default extractor name when a result does not name one.
const DEFAULT_EXTRACTOR: &str = "youtube";

/// One result of `yt-dlp --dump-json`.
#[derive(Debug, Deserialize)]
struct Video {
    /// The site's id for the video.
    #[serde(default)]
    id: String,
    /// The video's title.
    #[serde(default)]
    title: String,
    /// The page the video plays on.
    #[serde(default)]
    webpage_url: Option<String>,
    /// The site's name, for example `youtube`.
    #[serde(default)]
    extractor: Option<String>,
    /// The channel the video belongs to.
    #[serde(default)]
    channel_id: Option<String>,
    /// The uploader, used when the result has no channel id.
    #[serde(default)]
    uploader_id: Option<String>,
    /// The duration in seconds.
    #[serde(default)]
    duration: Option<f64>,
    /// Every format the site offers.
    #[serde(default)]
    formats: Vec<Format>,
}

/// One format of a result.
#[derive(Debug, Deserialize)]
struct Format {
    /// The id a download passes back to yt-dlp.
    #[serde(default)]
    format_id: String,
    /// The container extension.
    #[serde(default)]
    ext: String,
    /// The video codec; `none` marks an audio-only format.
    #[serde(default)]
    vcodec: Option<String>,
    /// The audio codec.
    #[serde(default)]
    acodec: Option<String>,
    /// The average audio bitrate in kbps.
    #[serde(default)]
    abr: Option<f64>,
    /// The audio sample rate in Hz.
    #[serde(default)]
    asr: Option<f64>,
    /// The exact file size in bytes.
    #[serde(default)]
    filesize: Option<u64>,
    /// The estimated file size in bytes.
    #[serde(default)]
    filesize_approx: Option<u64>,
}

/// Parses the standard output of a search into candidates.
///
/// # Parameters
///
/// - `stdout`: the complete output of `yt-dlp --dump-json`, one JSON object per
///   line. Blank lines and lines that are not a result object are skipped, so
///   a stray line does not discard the rest of a search.
///
/// # Returns
///
/// One candidate per result that carries an audio-only format and a page URL,
/// in the order the results were printed.
pub fn candidates(stdout: &str) -> Vec<Candidate> {
    stdout
        .lines()
        .filter_map(|line| parse_video(line.trim()))
        .collect()
}

/// Parses one output line into a candidate.
fn parse_video(line: &str) -> Option<Candidate> {
    if line.is_empty() {
        return None;
    }
    let video: Video = serde_json::from_str(line).ok()?;
    if video.id.is_empty() || video.title.is_empty() {
        return None;
    }
    let url = video.webpage_url.clone()?;
    let format = best_audio_format(&video.formats)?;
    let ext = format.ext.clone();
    let name = if ext.is_empty() {
        video.id.clone()
    } else {
        format!("{}.{}", video.id, ext)
    };
    let token = DownloadToken {
        url: url.clone(),
        format_id: format.format_id.clone(),
        ext: ext.clone(),
        id: video.id.clone(),
    }
    .encode()
    .ok()?;
    let file = CandidateFile {
        name,
        size: format.filesize.or(format.filesize_approx),
        format: format
            .acodec
            .as_deref()
            .and_then(codec_name)
            .map(str::to_string),
        bitrate_kbps: format.abr.and_then(round_u32),
        sample_rate: format.asr.and_then(round_u32),
        bit_depth: None,
        duration_ms: video.duration.and_then(round_ms),
        track_index: None,
    };
    Some(Candidate {
        token,
        title: video.title,
        path: url,
        files: vec![file],
        source: SourceInfo {
            key: source_key(
                video.extractor.as_deref(),
                video.channel_id.as_deref(),
                video.uploader_id.as_deref(),
            ),
            speed_bps: None,
            queue_length: None,
            free_slot: None,
        },
    })
}

/// Chooses the best audio-only format of a result.
///
/// # Parameters
///
/// - `formats`: every format the site offers.
///
/// # Returns
///
/// The audio-only format with the highest average bitrate, or `None` when the
/// site offers none. A format without a bitrate ranks below every format that
/// has one.
fn best_audio_format(formats: &[Format]) -> Option<&Format> {
    formats
        .iter()
        .filter(|format| is_audio_only(format))
        .max_by(|left, right| {
            let left = left.abr.unwrap_or(f64::NEG_INFINITY);
            let right = right.abr.unwrap_or(f64::NEG_INFINITY);
            left.total_cmp(&right)
        })
}

/// Returns whether a format carries audio and no video.
fn is_audio_only(format: &Format) -> bool {
    if format.format_id.is_empty() {
        return false;
    }
    format.vcodec.as_deref() == Some("none")
        && format
            .acodec
            .as_deref()
            .is_some_and(|codec| !codec.is_empty() && codec != "none")
}

/// Maps a yt-dlp audio codec name to the format name the server stores.
///
/// # Parameters
///
/// - `acodec`: the `acodec` field of a format, for example `opus` or
///   `mp4a.40.2`.
///
/// # Returns
///
/// `opus`, `aac`, `vorbis` or `mp3`, or `None` for a codec the server does not
/// name. The match ignores case and accepts the profile suffix `mp4a` codecs
/// carry.
pub fn codec_name(acodec: &str) -> Option<&'static str> {
    let codec = acodec.trim().to_ascii_lowercase();
    if codec.starts_with("mp4a") {
        Some("aac")
    } else if codec.contains("opus") {
        Some("opus")
    } else if codec.contains("vorbis") {
        Some("vorbis")
    } else if codec.contains("mp3") {
        Some("mp3")
    } else {
        None
    }
}

/// Builds the source key of a result.
///
/// # Parameters
///
/// - `extractor`: the site's name.
/// - `channel_id`: the channel the result belongs to.
/// - `uploader_id`: the uploader, used when the result has no channel id.
///
/// # Returns
///
/// `<extractor>:<channel or uploader id>`, or the extractor alone when the
/// result names neither, so every result from one channel shares a key.
pub fn source_key(
    extractor: Option<&str>,
    channel_id: Option<&str>,
    uploader_id: Option<&str>,
) -> String {
    let extractor = extractor
        .map(str::trim)
        .filter(|extractor| !extractor.is_empty())
        .unwrap_or(DEFAULT_EXTRACTOR);
    let id = channel_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .or_else(|| uploader_id.map(str::trim).filter(|id| !id.is_empty()));
    match id {
        Some(id) => format!("{extractor}:{id}"),
        None => extractor.to_string(),
    }
}

/// Rounds a non-negative, finite number to the nearest `u32`.
fn round_u32(value: f64) -> Option<u32> {
    if !value.is_finite() || value < 0.0 || value > f64::from(u32::MAX) {
        None
    } else {
        Some(value.round() as u32)
    }
}

/// Converts a duration in seconds to whole milliseconds.
fn round_ms(seconds: f64) -> Option<u32> {
    round_u32(seconds * 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The captured search output the parsing tests run on.
    const SEARCH: &str = include_str!("fixtures/search.json");

    #[test]
    fn parses_every_result() {
        let candidates = candidates(SEARCH);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].title, "Example Track One");
        assert_eq!(candidates[1].title, "Example Track Two");
    }

    #[test]
    fn picks_the_best_audio_only_format() {
        let candidates = candidates(SEARCH);
        let file = &candidates[0].files[0];
        assert_eq!(file.name, "vid0000001.webm");
        assert_eq!(file.format.as_deref(), Some("opus"));
        assert_eq!(file.bitrate_kbps, Some(160));
        assert_eq!(file.sample_rate, Some(48_000));
        assert_eq!(file.size, Some(4_000_000));
        assert_eq!(file.duration_ms, Some(213_000));

        let token = DownloadToken::decode(&candidates[0].token).expect("token decodes");
        assert_eq!(token.format_id, "251");
        assert_eq!(token.ext, "webm");
        assert_eq!(token.id, "vid0000001");
        assert_eq!(token.url, "https://www.youtube.com/watch?v=vid0000001");
    }

    #[test]
    fn maps_the_codec_of_each_result() {
        let candidates = candidates(SEARCH);
        assert_eq!(candidates[0].files[0].format.as_deref(), Some("opus"));
        assert_eq!(candidates[1].files[0].format.as_deref(), Some("mp3"));
    }

    #[test]
    fn names_the_channel_or_the_uploader() {
        let candidates = candidates(SEARCH);
        assert_eq!(candidates[0].source.key, "youtube:chan0000001");
        assert_eq!(candidates[1].source.key, "soundcloud:uploader01");
    }

    #[test]
    fn skips_lines_without_audio() {
        let line = r#"{"id":"v1","title":"Silent","webpage_url":"https://example.test/v1","formats":[{"format_id":"18","ext":"mp4","vcodec":"avc1","acodec":"mp4a.40.2","abr":96}]}"#;
        assert!(candidates(line).is_empty());
    }

    #[test]
    fn skips_lines_that_are_not_results() {
        let output = format!("not json\n\n{SEARCH}\n[download] oops\n");
        assert_eq!(candidates(&output).len(), 2);
    }

    #[test]
    fn skips_results_without_a_page() {
        let line = r#"{"id":"v1","title":"No page","formats":[{"format_id":"251","ext":"webm","vcodec":"none","acodec":"opus","abr":160}]}"#;
        assert!(candidates(line).is_empty());
    }

    #[test]
    fn maps_known_codecs() {
        assert_eq!(codec_name("opus"), Some("opus"));
        assert_eq!(codec_name("OPUS"), Some("opus"));
        assert_eq!(codec_name("mp4a.40.2"), Some("aac"));
        assert_eq!(codec_name("vorbis"), Some("vorbis"));
        assert_eq!(codec_name("mp3"), Some("mp3"));
        assert_eq!(codec_name("flac"), None);
        assert_eq!(codec_name("none"), None);
    }

    #[test]
    fn falls_back_to_the_extractor_alone() {
        assert_eq!(source_key(Some("vimeo"), None, None), "vimeo");
        assert_eq!(source_key(None, None, None), "youtube");
        assert_eq!(
            source_key(Some("youtube"), Some(""), Some("@handle")),
            "youtube:@handle"
        );
    }

    #[test]
    fn prefers_a_format_with_a_bitrate() {
        let formats: Vec<Format> = serde_json::from_str(
            r#"[{"format_id":"1","vcodec":"none","acodec":"opus"},{"format_id":"2","vcodec":"none","acodec":"opus","abr":64.4}]"#,
        )
        .expect("formats parse");
        let best = best_audio_format(&formats).expect("one format");
        assert_eq!(best.format_id, "2");
        assert_eq!(best.abr.and_then(round_u32), Some(64));
    }
}
