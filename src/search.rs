//! Turning a yt-dlp search's output into candidates.
//!
//! A search asks yt-dlp to select each result's best audio-only format and to
//! print one compact JSON object per result. The objects are a few hundred
//! bytes, so the plugin parses them as they arrive and keeps only the reduced
//! candidates between polls, never the raw output.

use hearthsound_plugin::process_acquisition_provider::exports::hearthsound::plugin::acquisition::{
    Candidate, CandidateFile, SourceInfo,
};
use serde::{Deserialize, Deserializer, Serialize};

use crate::token::DownloadToken;

/// The default extractor name when a result does not name one.
const DEFAULT_EXTRACTOR: &str = "youtube";

/// Most characters of an unreadable line kept in an error message.
const EXCERPT_CHARS: usize = 160;

/// One result line yt-dlp printed.
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
    /// The id of the format yt-dlp selected.
    #[serde(default)]
    format_id: String,
    /// The container extension of the selected format.
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
    #[serde(default, deserialize_with = "optional_u64")]
    filesize: Option<u64>,
    /// The estimated file size in bytes.
    #[serde(default, deserialize_with = "optional_u64")]
    filesize_approx: Option<u64>,
}

/// Deserializes a byte count yt-dlp may report as an integer, a float or null.
///
/// # Parameters
///
/// - `deserializer`: the field's value.
///
/// # Returns
///
/// The count rounded to a whole number of bytes, or `None` for null and for a
/// value that is not a non-negative finite number.
fn optional_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<f64>::deserialize(deserializer)?;
    Ok(value.and_then(|bytes| {
        if bytes.is_finite() && bytes >= 0.0 && bytes <= u64::MAX as f64 {
            Some(bytes.round() as u64)
        } else {
            None
        }
    }))
}

/// A candidate reduced to the values the server needs.
///
/// The plugin stores one of these between polls, so it carries no format list
/// and serializes to a few hundred bytes.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredCandidate {
    /// The opaque token a download passes back.
    pub token: String,
    /// The result's title.
    pub title: String,
    /// The page the result plays on.
    pub path: String,
    /// The file name the download will produce.
    pub name: String,
    /// The selected format's size in bytes, when yt-dlp knows it.
    pub size: Option<u64>,
    /// The selected format's audio codec as a server format name.
    pub format: Option<String>,
    /// The selected format's average bitrate in kbps.
    pub bitrate_kbps: Option<u32>,
    /// The selected format's sample rate in Hz.
    pub sample_rate: Option<u32>,
    /// The result's duration in milliseconds.
    pub duration_ms: Option<u32>,
    /// The source key shared by results from one channel.
    pub source_key: String,
}

impl StoredCandidate {
    /// Renders the interface's candidate record.
    ///
    /// # Returns
    ///
    /// One candidate with one file and no peer-queue details.
    pub fn to_wit(&self) -> Candidate {
        Candidate {
            token: self.token.clone(),
            title: self.title.clone(),
            path: self.path.clone(),
            files: vec![CandidateFile {
                name: self.name.clone(),
                size: self.size,
                format: self.format.clone(),
                bitrate_kbps: self.bitrate_kbps,
                sample_rate: self.sample_rate,
                bit_depth: None,
                duration_ms: self.duration_ms,
                track_index: None,
            }],
            source: SourceInfo {
                key: self.source_key.clone(),
                speed_bps: None,
                queue_length: None,
                free_slot: None,
            },
        }
    }
}

/// Parses one complete output line.
///
/// # Parameters
///
/// - `line`: one line of `yt-dlp --print` output.
///
/// # Returns
///
/// `Ok(Some(candidate))` for a result the server can use, `Ok(None)` for a
/// blank line and for a result without a page or without an audio-only
/// selected format.
///
/// # Errors
///
/// Returns a message naming the line when it is not a JSON result object, so
/// the server log says what yt-dlp printed instead of the search silently
/// returning nothing.
pub fn parse_line(line: &str) -> Result<Option<StoredCandidate>, String> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }
    let video: Video = serde_json::from_str(line).map_err(|error| {
        format!(
            "a search output line is not a result object ({error}): {}",
            excerpt(line)
        )
    })?;
    Ok(candidate_from(video))
}

/// Builds a candidate from one parsed result.
fn candidate_from(video: Video) -> Option<StoredCandidate> {
    if video.id.is_empty() || video.title.is_empty() || video.format_id.is_empty() {
        return None;
    }
    let url = video.webpage_url.clone()?;
    if !is_audio_only(&video) {
        return None;
    }
    let ext = video.ext.clone();
    let name = if ext.is_empty() {
        video.id.clone()
    } else {
        format!("{}.{}", video.id, ext)
    };
    let token = DownloadToken {
        url: url.clone(),
        format_id: video.format_id.clone(),
        ext: ext.clone(),
        id: video.id.clone(),
    }
    .encode()
    .ok()?;
    Some(StoredCandidate {
        token,
        title: video.title,
        path: url,
        name,
        size: video.filesize.or(video.filesize_approx),
        format: video
            .acodec
            .as_deref()
            .and_then(codec_name)
            .map(str::to_string),
        bitrate_kbps: video.abr.and_then(round_u32),
        sample_rate: video.asr.and_then(round_u32),
        duration_ms: video.duration.and_then(round_ms),
        source_key: source_key(
            video.extractor.as_deref(),
            video.channel_id.as_deref(),
            video.uploader_id.as_deref(),
        ),
    })
}

/// Returns whether a result's selected format carries audio and no video.
fn is_audio_only(video: &Video) -> bool {
    video.vcodec.as_deref() == Some("none")
        && video
            .acodec
            .as_deref()
            .is_some_and(|codec| !codec.is_empty() && codec != "none")
}

/// Returns the start of a line for an error message.
fn excerpt(line: &str) -> String {
    let mut excerpt: String = line.chars().take(EXCERPT_CHARS).collect();
    if line.chars().count() > EXCERPT_CHARS {
        excerpt.push_str("...");
    }
    excerpt
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

    /// Parses every line of a captured output.
    fn parsed(text: &str) -> Vec<StoredCandidate> {
        text.lines()
            .filter_map(|line| parse_line(line).expect("every line parses"))
            .collect()
    }

    #[test]
    fn parses_every_result() {
        let candidates = parsed(SEARCH);
        assert_eq!(candidates.len(), 3);
        assert_eq!(
            candidates[0].title,
            "Weyes Blood - Andromeda (Official Video)"
        );
        assert_eq!(
            candidates[1].title,
            "Weyes Blood - Andromeda (HD) [Official Music Video]"
        );
        assert_eq!(candidates[2].title, "Weyes Blood - Andromeda");
    }

    #[test]
    fn reads_the_selected_audio_format() {
        let candidates = parsed(SEARCH);
        let candidate = &candidates[0];
        assert_eq!(candidate.name, "ZHGw78IJryE.webm");
        assert_eq!(candidate.format.as_deref(), Some("opus"));
        assert_eq!(candidate.bitrate_kbps, Some(132));
        assert_eq!(candidate.sample_rate, Some(48_000));
        assert_eq!(candidate.size, Some(4_628_757));
        assert_eq!(candidate.duration_ms, Some(280_000));

        let token = DownloadToken::decode(&candidate.token).expect("token decodes");
        assert_eq!(token.format_id, "251");
        assert_eq!(token.ext, "webm");
        assert_eq!(token.id, "ZHGw78IJryE");
        assert_eq!(token.url, "https://www.youtube.com/watch?v=ZHGw78IJryE");
    }

    #[test]
    fn names_the_channel() {
        let candidates = parsed(SEARCH);
        assert_eq!(candidates[0].source_key, "youtube:UCngjw6cGfzm6bUIDuhGMntg");
        assert_eq!(candidates[2].source_key, "youtube:UCsgEkEWaXKQwrhlLHFbcQFw");
    }

    #[test]
    fn accepts_integer_float_and_null_numbers() {
        let line = r#"{"id":"v1","title":"Numbers","webpage_url":"https://example.test/v1","duration":280.5,"format_id":"251","ext":"webm","vcodec":"none","acodec":"opus","abr":132.145,"asr":48000,"filesize":null,"filesize_approx":1710107.6}"#;
        let candidate = parse_line(line)
            .expect("line parses")
            .expect("candidate exists");
        assert_eq!(candidate.size, Some(1_710_108));
        assert_eq!(candidate.duration_ms, Some(280_500));
        assert_eq!(candidate.bitrate_kbps, Some(132));
        assert_eq!(candidate.sample_rate, Some(48_000));
    }

    #[test]
    fn reports_a_line_that_is_not_a_result() {
        let error = parse_line("Py\nnot a result").expect_err("line is not a result");
        assert!(error.contains("not a result"), "unexpected detail: {error}");

        let error = parse_line(r#"{"id":1,"title":"Wrong type"}"#).expect_err("types do not match");
        assert!(!error.is_empty());
    }

    #[test]
    fn skips_results_without_audio() {
        let line = r#"{"id":"v1","title":"Muxed","webpage_url":"https://example.test/v1","format_id":"18","ext":"mp4","vcodec":"avc1","acodec":"mp4a.40.2","abr":96}"#;
        assert!(parse_line(line).expect("line parses").is_none());
    }

    #[test]
    fn skips_results_without_a_page() {
        let line = r#"{"id":"v1","title":"No page","format_id":"251","ext":"webm","vcodec":"none","acodec":"opus","abr":160}"#;
        assert!(parse_line(line).expect("line parses").is_none());
    }

    #[test]
    fn skips_results_without_a_format() {
        let line = r#"{"id":"v1","title":"No format","webpage_url":"https://example.test/v1","vcodec":"none","acodec":"opus"}"#;
        assert!(parse_line(line).expect("line parses").is_none());
    }

    #[test]
    fn ignores_blank_lines() {
        assert!(parse_line("").expect("blank line").is_none());
        assert!(parse_line("  \t ").expect("blank line").is_none());
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
}
