//! The yt-dlp argument vectors.
//!
//! The host starts a tool with an argument vector and never a shell, so every
//! argument is built here as one list element. Keeping the vectors in one place
//! makes them testable on the host target: the unit tests assert the exact
//! arguments a search and a download pass.

use crate::config::{Config, SearchPrefix};
use crate::token::DownloadToken;

/// How long one search may run, in milliseconds.
///
/// A search only prints metadata for at most ten results, but the site lookup
/// can be slow, so the limit is generous.
pub const SEARCH_TIMEOUT_MS: u32 = 120_000;

/// The object yt-dlp prints for one search result.
///
/// The projection keeps the fields the plugin's candidate needs and nothing
/// else, so a result line is a few hundred bytes instead of the full metadata
/// dump. `j` encodes the object as JSON.
const SEARCH_FIELDS: &str = "%(.{id,title,webpage_url,extractor,channel_id,uploader_id,duration,format_id,ext,vcodec,acodec,abr,asr,filesize,filesize_approx})j";

/// Builds the search text, prefix included.
///
/// # Parameters
///
/// - `config`: the instance configuration, for the result count and the search
///   site.
/// - `artist`: the primary artist of the wanted track, when the query has one.
/// - `title`: the title of the wanted track.
///
/// # Returns
///
/// `<prefix><count>:<artist> <title>`, with whitespace collapsed and empty
/// parts left out, for example `ytsearch5:Radiohead Creep`.
pub fn search_target(config: &Config, artist: Option<&str>, title: &str) -> String {
    let mut query = String::new();
    if let Some(artist) = artist.map(str::trim).filter(|artist| !artist.is_empty()) {
        query.push_str(artist);
    }
    let title = title.trim();
    if !title.is_empty() {
        if !query.is_empty() {
            query.push(' ');
        }
        query.push_str(title);
    }
    format!(
        "{}{}:{}",
        prefix(config.search_prefix),
        config.search_count,
        query
    )
}

/// Returns the yt-dlp prefix for a configured search site.
fn prefix(search: SearchPrefix) -> &'static str {
    search.as_str()
}

/// Builds the arguments of a search.
///
/// # Parameters
///
/// - `target`: the search text from [`search_target`].
///
/// # Returns
///
/// The argument vector that prints one compact JSON object per result to
/// standard output without downloading anything. Format selection is left to
/// yt-dlp: it picks the audio-only format with the highest average bitrate,
/// falling back to a muxed format the parser then skips.
pub fn search_arguments(target: &str) -> Vec<String> {
    [
        "--ignore-config",
        "--no-warnings",
        "--no-playlist",
        "--skip-download",
        "-f",
        "bestaudio/best",
        "-S",
        "abr",
        "--print",
        SEARCH_FIELDS,
        target,
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// Builds the arguments of a download.
///
/// # Parameters
///
/// - `key`: the filesystem-safe key every output of this download lands under,
///   relative to the staging directory.
/// - `token`: the candidate the server asked to download.
///
/// # Returns
///
/// The argument vector that downloads the chosen format, extracts its audio
/// without re-encoding, embeds metadata, reports progress and prints the final
/// file path. The template repeats the `download:` prefix because yt-dlp uses
/// the first one as the progress type, and `--no-quiet` restores progress,
/// which `--print` would otherwise suppress.
pub fn download_arguments(key: &str, token: &DownloadToken) -> Vec<String> {
    [
        "--ignore-config".to_string(),
        "--no-playlist".to_string(),
        "-f".to_string(),
        token.format_id.clone(),
        "-x".to_string(),
        "--audio-format".to_string(),
        "best".to_string(),
        "--embed-metadata".to_string(),
        "--newline".to_string(),
        "--no-quiet".to_string(),
        "--progress-template".to_string(),
        "download:download:%(progress.downloaded_bytes)s/%(progress.total_bytes)s".to_string(),
        "-o".to_string(),
        format!("{key}/%(id)s.%(ext)s"),
        "--print".to_string(),
        "after_move:filepath".to_string(),
        token.url.clone(),
    ]
    .to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_youtube_search() {
        let config = Config::default();
        let target = search_target(&config, Some("Radiohead"), "Creep");
        assert_eq!(target, "ytsearch5:Radiohead Creep");
        assert_eq!(
            search_arguments(&target),
            [
                "--ignore-config",
                "--no-warnings",
                "--no-playlist",
                "--skip-download",
                "-f",
                "bestaudio/best",
                "-S",
                "abr",
                "--print",
                SEARCH_FIELDS,
                "ytsearch5:Radiohead Creep",
            ]
        );
    }

    #[test]
    fn keeps_the_search_output_small() {
        let fields = SEARCH_FIELDS;
        assert!(fields.contains("format_id"));
        assert!(fields.contains("filesize_approx"));
        assert!(fields.ends_with(")j"));
    }

    #[test]
    fn builds_a_soundcloud_search() {
        let config = Config {
            search_count: 10,
            search_prefix: SearchPrefix::ScSearch,
            ..Config::default()
        };
        assert_eq!(
            search_target(&config, Some(" Portishead "), " Glory Box "),
            "scsearch10:Portishead Glory Box"
        );
    }

    #[test]
    fn leaves_missing_parts_out() {
        let config = Config::default();
        assert_eq!(search_target(&config, None, "Creep"), "ytsearch5:Creep");
        assert_eq!(
            search_target(&config, Some("  "), "Creep"),
            "ytsearch5:Creep"
        );
    }

    #[test]
    fn builds_a_download() {
        let token = DownloadToken {
            url: "https://www.youtube.com/watch?v=abc123".to_string(),
            format_id: "251".to_string(),
            ext: "webm".to_string(),
            id: "abc123".to_string(),
        };
        assert_eq!(
            download_arguments("3f1c2a", &token),
            [
                "--ignore-config",
                "--no-playlist",
                "-f",
                "251",
                "-x",
                "--audio-format",
                "best",
                "--embed-metadata",
                "--newline",
                "--no-quiet",
                "--progress-template",
                "download:download:%(progress.downloaded_bytes)s/%(progress.total_bytes)s",
                "-o",
                "3f1c2a/%(id)s.%(ext)s",
                "--print",
                "after_move:filepath",
                "https://www.youtube.com/watch?v=abc123",
            ]
        );
    }
}
