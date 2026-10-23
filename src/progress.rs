//! Reading yt-dlp's progress and exit reports.
//!
//! With `--newline` yt-dlp prints one progress line per update and, because the
//! download passes `--print after_move:filepath`, one final line holding the
//! path of the file it produced. Reads from a process stream can split a line
//! anywhere, so the download keeps the bytes of an unfinished line and only
//! parses complete ones.

use std::path::Path;

use hearthsound_plugin::process_acquisition_provider::exports::hearthsound::plugin::acquisition::Progress;

/// The prefix the progress template puts in front of every progress line.
const PROGRESS_PREFIX: &str = "download:";

/// The prefix yt-dlp puts in front of a progress line without a template.
const DEFAULT_PROGRESS_PREFIX: &str = "[download] ";

/// The marker yt-dlp puts in front of its own diagnostics.
const DIAGNOSTIC_PREFIX: char = '[';

/// Most bytes of standard error the download keeps for a failure message.
const STDERR_TAIL_BYTES: usize = 4096;

/// Splits the complete lines out of a stream chunk.
///
/// # Parameters
///
/// - `pending`: the bytes of a line that was not finished by an earlier chunk.
///   The function appends `chunk`, keeps the bytes after the last newline, and
///   leaves them there for the next call.
/// - `chunk`: the bytes a read returned.
///
/// # Returns
///
/// Every complete line, decoded as UTF-8 with replacement characters for bytes
/// that are not, so one odd byte cannot discard the rest of a download's
/// output.
pub fn append_lines(pending: &mut Vec<u8>, chunk: &[u8]) -> Vec<String> {
    pending.extend_from_slice(chunk);
    let split = pending
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    if split == 0 {
        return Vec::new();
    }
    let complete: Vec<u8> = pending.drain(..split).collect();
    String::from_utf8_lossy(&complete)
        .lines()
        .map(str::to_string)
        .collect()
}

/// Parses one progress line.
///
/// # Parameters
///
/// - `line`: one output line, for example `download:1048576/4194304`.
///
/// # Returns
///
/// The bytes reported as downloaded and, when the total is known, the bytes
/// expected; `None` for any other line, including a progress line whose
/// counters are not numbers (yt-dlp prints `NA` while the total is unknown).
pub fn parse_progress(line: &str) -> Option<Progress> {
    let line = line.trim();
    let line = line.strip_prefix(DEFAULT_PROGRESS_PREFIX).unwrap_or(line);
    let rest = line.strip_prefix(PROGRESS_PREFIX)?;
    let (downloaded, total) = rest.split_once('/')?;
    let bytes = downloaded.trim().parse::<u64>().ok()?;
    let total_bytes = total.trim().parse::<u64>().ok();
    Some(Progress {
        bytes,
        total_bytes,
        speed_bps: None,
    })
}

/// Returns the last output line that is not progress or a diagnostic.
///
/// # Parameters
///
/// - `lines`: the complete lines read so far, in order.
///
/// # Returns
///
/// The last path yt-dlp printed, which is the `after_move:filepath` report
/// once the process exits.
pub fn printed_path(lines: &[String]) -> Option<String> {
    lines
        .iter()
        .rev()
        .map(|line| line.trim())
        .find(|line| {
            !line.is_empty()
                && !line.starts_with(PROGRESS_PREFIX)
                && !line.starts_with(DIAGNOSTIC_PREFIX)
        })
        .map(str::to_string)
}

/// Appends standard error and keeps only its tail.
///
/// # Parameters
///
/// - `tail`: what earlier reads kept.
/// - `chunk`: the bytes this read returned.
///
/// # Returns
///
/// The concatenation, cut to its last
/// [`STDERR_TAIL_BYTES`](self::STDERR_TAIL_BYTES) characters, so a chatty
/// failure cannot grow the stored download record without bound.
pub fn append_stderr(tail: &str, chunk: &[u8]) -> String {
    let mut combined = String::with_capacity(tail.len() + chunk.len());
    combined.push_str(tail);
    combined.push_str(&String::from_utf8_lossy(chunk));
    if combined.len() <= STDERR_TAIL_BYTES {
        return combined;
    }
    let split = combined.len() - STDERR_TAIL_BYTES;
    match combined.get(split..) {
        Some(tail) => tail.to_string(),
        None => combined,
    }
}

/// Returns the filesystem name of a printed path.
///
/// # Parameters
///
/// - `printed`: a path yt-dlp printed, absolute or relative to the staging
///   directory.
///
/// # Returns
///
/// The last path component, or `None` when the printed value has none.
pub fn output_name(printed: &str) -> Option<&str> {
    Path::new(printed.trim())
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
}

/// Rebuilds the staging-relative path of a downloaded file.
///
/// # Parameters
///
/// - `key`: the download's staging key.
/// - `printed`: the path yt-dlp printed after moving the file.
///
/// # Returns
///
/// `<key>/<file name>`, the path the download reports back to the server, or
/// `None` when the printed value has no file name.
pub fn output_relative(key: &str, printed: &str) -> Option<String> {
    output_name(printed).map(|name| format!("{key}/{name}"))
}

/// Decides what a finished download reports.
///
/// # Parameters
///
/// - `code`: the process exit status.
/// - `key`: the download's staging key.
/// - `printed`: the last path line yt-dlp printed, when it printed one.
/// - `stderr_tail`: the standard error kept for a failure.
/// - `stdout_tail`: the standard output kept for a failure without stderr.
///
/// # Returns
///
/// The staging-relative path of the produced file on success, and a failure
/// message when the exit status is not zero or no path was reported.
pub fn exit_outcome(
    code: i32,
    key: &str,
    printed: Option<&str>,
    stderr_tail: &str,
    stdout_tail: &str,
) -> Result<String, String> {
    if code != 0 {
        return Err(failure_detail(code, stderr_tail, stdout_tail));
    }
    printed
        .and_then(|printed| output_relative(key, printed))
        .ok_or_else(|| "yt-dlp finished without reporting an output file".to_string())
}

/// Builds the failure message of a download.
///
/// # Parameters
///
/// - `code`: the non-zero exit status.
/// - `stderr_tail`: standard error the process wrote.
/// - `stdout_tail`: standard output the process wrote.
///
/// # Returns
///
/// The last non-empty standard error line, else the last non-empty standard
/// output line, else a message naming the exit status.
pub fn failure_detail(code: i32, stderr_tail: &str, stdout_tail: &str) -> String {
    last_line(stderr_tail)
        .or_else(|| last_line(stdout_tail))
        .unwrap_or_else(|| format!("yt-dlp exited with status {code}"))
}

/// Returns the last non-empty line of `text`.
fn last_line(text: &str) -> Option<String> {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_progress_line() {
        let progress = parse_progress("download:1048576/4194304").expect("progress");
        assert_eq!(progress.bytes, 1_048_576);
        assert_eq!(progress.total_bytes, Some(4_194_304));
        assert_eq!(progress.speed_bps, None);
    }

    #[test]
    fn parses_a_progress_line_without_a_total() {
        let progress = parse_progress("download:1024/NA").expect("progress");
        assert_eq!(progress.bytes, 1024);
        assert_eq!(progress.total_bytes, None);
    }

    #[test]
    fn tolerates_the_default_prefix_and_whitespace() {
        let progress = parse_progress("  [download] download:10/20  ").expect("progress");
        assert_eq!(progress.bytes, 10);
        assert_eq!(progress.total_bytes, Some(20));
    }

    #[test]
    fn ignores_lines_that_are_not_progress() {
        assert!(parse_progress("").is_none());
        assert!(parse_progress("download:NA/NA").is_none());
        assert!(parse_progress("download:10").is_none());
        assert!(parse_progress("[info] Downloading").is_none());
        assert!(parse_progress("/tmp/staging/key/out.opus").is_none());
    }

    #[test]
    fn splits_chunks_into_complete_lines() {
        let mut pending = Vec::new();
        assert_eq!(
            append_lines(&mut pending, b"download:1/2\ndown"),
            ["download:1/2"]
        );
        assert_eq!(pending, b"down");
        assert_eq!(
            append_lines(&mut pending, b"load:3/4\npath/out.opus"),
            ["download:3/4"]
        );
        assert_eq!(pending, b"path/out.opus");
    }

    #[test]
    fn keeps_the_last_printed_path() {
        let lines = [
            "download:1/2".to_string(),
            "/tmp/staging/key/out.webm.part".to_string(),
            "[download] Destination: /tmp/staging/key/out.webm".to_string(),
            "download:2/2".to_string(),
            "/tmp/staging/key/out.webm".to_string(),
        ];
        assert_eq!(
            printed_path(&lines).as_deref(),
            Some("/tmp/staging/key/out.webm")
        );
        assert_eq!(printed_path(&[]), None);
        assert_eq!(printed_path(&["download:1/2".to_string()]), None);
    }

    #[test]
    fn bounds_the_stderr_tail() {
        let tail = append_stderr("", b"first\nsecond\n");
        assert_eq!(tail, "first\nsecond\n");
        let tail = append_stderr(&tail, &vec![b'x'; STDERR_TAIL_BYTES + 100]);
        assert_eq!(tail.len(), STDERR_TAIL_BYTES);
        assert!(tail.chars().all(|character| character == 'x'));
    }

    #[test]
    fn rebuilds_the_staging_path() {
        assert_eq!(
            output_relative("key", "/tmp/staging/key/out.opus").as_deref(),
            Some("key/out.opus")
        );
        assert_eq!(
            output_relative("key", "key/out.opus").as_deref(),
            Some("key/out.opus")
        );
        assert_eq!(output_relative("key", ""), None);
    }

    #[test]
    fn reports_success_with_the_output_path() {
        assert_eq!(
            exit_outcome(0, "key", Some("key/out.opus"), "", ""),
            Ok("key/out.opus".to_string())
        );
    }

    #[test]
    fn reports_a_missing_print() {
        let failure = exit_outcome(0, "key", None, "", "").expect_err("no path");
        assert!(failure.contains("without reporting"));
    }

    #[test]
    fn reports_the_last_error_line() {
        assert_eq!(
            exit_outcome(1, "key", None, "line one\nERROR: unavailable\n", ""),
            Err("ERROR: unavailable".to_string())
        );
        assert_eq!(
            exit_outcome(2, "key", None, "", "last stdout line\n"),
            Err("last stdout line".to_string())
        );
        assert_eq!(
            exit_outcome(3, "key", None, "", ""),
            Err("yt-dlp exited with status 3".to_string())
        );
    }
}
