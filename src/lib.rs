//! A Hearthsound acquisition plugin that downloads tracks with yt-dlp.
//!
//! The plugin implements the `acquisition` provider interface at track
//! granularity and runs yt-dlp through the host's `host-process` import: an
//! administrator installs yt-dlp and points `tools.yt-dlp.path` at it, and the
//! plugin never sees a filesystem path for the tool, only its name.
//!
//! A search asks yt-dlp to select each result's best audio-only format and to
//! print one compact JSON object per result, which the plugin parses into a
//! candidate as the output arrives. A download fetches the chosen format,
//! extracts the audio without re-encoding, embeds the site's metadata and
//! reports progress from yt-dlp's own progress lines. The produced file is
//! reported under the staging root, where the server imports it like any other
//! downloading provider's output.
//!
//! All network access happens inside yt-dlp; the component requests no HTTP
//! permission.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod args;
mod config;
mod progress;
mod search;
mod token;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use hearthsound_plugin::host::files;
use hearthsound_plugin::host::kv as host_kv;
use hearthsound_plugin::host::process::{
    self, ProcessError, ProcessHandle, ProcessState, SpawnOptions, StdoutMode,
};
use hearthsound_plugin::kv;
use hearthsound_plugin::process_acquisition_provider::exports::hearthsound::plugin::acquisition::{
    self, Candidate, Capabilities, DownloadState, Granularity, OutputFile, Progress, Query,
    SearchState,
};
use hearthsound_plugin::process_acquisition_provider::exports::hearthsound::plugin::lifecycle;
use hearthsound_plugin::types::ProviderError;

use crate::config::Config;

/// The manifest tool name every call passes to `host-process`.
const TOOL: &str = "yt-dlp";

/// The staging root every reported file lives under.
const STAGING_ROOT: &str = "staging";

/// How long a stored search may stay readable before it is dropped.
const SEARCH_TTL_MS: u64 = 10 * 60 * 1000;

/// How long a stored download may stay readable before it is dropped.
const DOWNLOAD_TTL_MS: u64 = 2 * 60 * 60 * 1000;

/// Most bytes of an unfinished output line kept between polls.
///
/// A stored record has to fit the host's 256 KiB per-value cap, so a line that
/// grows past this is dropped instead of kept. Search lines are a few hundred
/// bytes and download lines shorter still; the limit only stops a tool that
/// never writes a newline from filling the store.
const MAX_PENDING_BYTES: usize = 64 * 1024;

/// Most characters of one output line kept for a failure message.
const MAX_TAIL_CHARS: usize = 512;

/// The plugin implementation.
struct YtDlpAcquisition;

/// A search kept between polls.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchRecord {
    /// The process running the search.
    handle: ProcessHandle,
    /// The candidates parsed from the complete lines read so far.
    candidates: Vec<search::StoredCandidate>,
    /// The bytes of the last, incomplete output line.
    pending: Vec<u8>,
    /// Whether the rest of a dropped line is still to be skipped.
    discard_line: bool,
    /// The last complete output line, kept for a failure without stderr.
    stdout_tail: String,
    /// The first line that was not a result object, for the server log.
    unparsable: Option<String>,
}

/// The bytes reported by the last progress line.
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredProgress {
    /// Bytes downloaded so far.
    bytes: u64,
    /// Bytes expected, when yt-dlp reported a total.
    total_bytes: Option<u64>,
}

impl StoredProgress {
    /// Renders the stored counters as the interface's progress record.
    fn to_wit(self) -> Progress {
        Progress {
            bytes: self.bytes,
            total_bytes: self.total_bytes,
            speed_bps: None,
        }
    }
}

/// A download kept between polls.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DownloadRecord {
    /// The process running the download.
    handle: ProcessHandle,
    /// The key the download's files land under, relative to staging.
    key: String,
    /// The bytes of the last, incomplete output line.
    pending: Vec<u8>,
    /// Whether the rest of a dropped line is still to be skipped.
    discard_line: bool,
    /// The last progress yt-dlp reported.
    progress: StoredProgress,
    /// The last file path yt-dlp printed.
    printed: Option<String>,
    /// The last complete output line, kept for a failure without stderr.
    stdout_tail: String,
    /// The tail of standard error, kept for a failure message.
    stderr_tail: String,
    /// The configured timeout, kept for the timeout message.
    timeout_secs: u32,
    /// What the download reported once it reached a terminal state.
    done: Option<StoredDone>,
}

impl DownloadRecord {
    /// Builds the record of a just-started download.
    fn new(handle: ProcessHandle, key: String, timeout_secs: u32) -> DownloadRecord {
        DownloadRecord {
            handle,
            key,
            pending: Vec::new(),
            discard_line: false,
            progress: StoredProgress::default(),
            printed: None,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            timeout_secs,
            done: None,
        }
    }

    /// Renders a terminal state, or a running state from the stored progress.
    fn state(&self) -> DownloadState {
        match &self.done {
            Some(done) => done.to_wit(),
            None => DownloadState::Running(self.progress.to_wit()),
        }
    }
}

/// What a download reported when it finished.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum StoredDone {
    /// The file at this staging-relative path is ready.
    Complete(String),
    /// The download failed with this message.
    Failed(String),
}

impl StoredDone {
    /// Renders the terminal state.
    fn to_wit(&self) -> DownloadState {
        match self {
            Self::Complete(path) => DownloadState::Complete(vec![OutputFile {
                root: STAGING_ROOT.to_string(),
                path: path.clone(),
                track_index: None,
            }]),
            Self::Failed(message) => DownloadState::Failed(message.clone()),
        }
    }
}

/// Returns the key a search's record is stored under.
fn search_key(token: &str) -> String {
    format!("search:{token}")
}

/// Returns the key a download's record is stored under.
fn download_key(token: &str) -> String {
    format!("download:{token}")
}

/// Builds the spawn options of a captured process.
fn capture_options(timeout_ms: u32) -> SpawnOptions {
    SpawnOptions {
        cwd: None,
        env: Vec::new(),
        stdout: StdoutMode::Capture,
        timeout_ms,
    }
}

/// Maps a host process error to a provider error.
///
/// Every variant carries its detail into the provider error, so the server log
/// names the cause. A tool the administrator has not configured is a
/// configuration problem; a spawn the host denied, a start the operating system
/// refused and a handle whose process is gone are transient unavailability.
fn process_error(error: ProcessError) -> ProviderError {
    match error {
        ProcessError::Denied(message) => ProviderError::Unavailable(message),
        ProcessError::NotConfigured(message) => ProviderError::InvalidConfig(message),
        ProcessError::SpawnFailed(message) => ProviderError::Unavailable(message),
        ProcessError::UnknownHandle => {
            ProviderError::Unavailable("the yt-dlp process is no longer running".to_string())
        }
    }
}

/// Starts yt-dlp with the given arguments.
fn spawn(arguments: Vec<String>, timeout_ms: u32) -> Result<ProcessHandle, ProviderError> {
    process::spawn(TOOL, &arguments, &capture_options(timeout_ms)).map_err(process_error)
}

/// Reads a process's standard output.
fn read_stdout(handle: ProcessHandle) -> Result<Vec<u8>, ProviderError> {
    process::read_stdout(handle).map_err(process_error)
}

/// Reads a process's standard error, treating a released handle as no output.
fn read_stderr(handle: ProcessHandle) -> Vec<u8> {
    process::read_stderr(handle).unwrap_or_default()
}

impl SearchRecord {
    /// Builds the record of a just-started search.
    fn new(handle: ProcessHandle) -> SearchRecord {
        SearchRecord {
            handle,
            candidates: Vec::new(),
            pending: Vec::new(),
            discard_line: false,
            stdout_tail: String::new(),
            unparsable: None,
        }
    }

    /// Keeps the detail of the first output line that was not a result.
    fn remember_unparsable(&mut self, detail: String) {
        if self.unparsable.is_none() {
            self.unparsable = Some(detail);
        }
    }

    /// Renders the candidates parsed so far.
    fn to_wit(&self) -> Vec<Candidate> {
        self.candidates
            .iter()
            .map(search::StoredCandidate::to_wit)
            .collect()
    }
}

/// Appends a search output chunk to its record.
///
/// Only complete lines are parsed, so a line split across reads is never
/// mistaken for bad output. Parsed candidates replace the raw line in the
/// record, which keeps a stored search far below the host's value cap.
fn absorb_search(record: &mut SearchRecord, chunk: &[u8]) {
    for line in progress::append_lines(&mut record.pending, chunk) {
        if std::mem::take(&mut record.discard_line) {
            continue;
        }
        if !line.trim().is_empty() {
            record.stdout_tail = clip(line.trim());
        }
        match search::parse_line(&line) {
            Ok(Some(candidate)) => record.candidates.push(candidate),
            Ok(None) => {}
            Err(detail) => record.remember_unparsable(detail),
        }
    }
    if record.pending.len() > MAX_PENDING_BYTES {
        record.pending.clear();
        record.discard_line = true;
        record.remember_unparsable(format!(
            "a search output line exceeded {} KiB",
            MAX_PENDING_BYTES / 1024
        ));
    }
}

/// Parses the last, unterminated line and returns the search's result.
///
/// # Returns
///
/// The candidates of every result line.
///
/// # Errors
///
/// Returns the detail of the first output line that was not a result object,
/// so a search that printed something else fails with that text.
fn finish_search(record: &mut SearchRecord) -> Result<Vec<Candidate>, String> {
    if !record.discard_line && !record.pending.is_empty() {
        let pending = std::mem::take(&mut record.pending);
        match search::parse_line(&String::from_utf8_lossy(&pending)) {
            Ok(Some(candidate)) => record.candidates.push(candidate),
            Ok(None) => {}
            Err(detail) => record.remember_unparsable(detail),
        }
    }
    match record.unparsable.take() {
        Some(detail) => Err(detail),
        None => Ok(record.to_wit()),
    }
}

/// Appends a download output chunk to its record.
///
/// Only complete lines are parsed; the bytes of an unfinished line stay in the
/// record for the next read.
fn absorb_download(record: &mut DownloadRecord, chunk: &[u8]) {
    let mut lines = Vec::new();
    for line in progress::append_lines(&mut record.pending, chunk) {
        if std::mem::take(&mut record.discard_line) {
            continue;
        }
        lines.push(line);
    }
    for line in &lines {
        if let Some(reported) = progress::parse_progress(line) {
            record.progress = StoredProgress {
                bytes: reported.bytes,
                total_bytes: reported.total_bytes,
            };
        }
    }
    if let Some(path) = progress::printed_path(&lines) {
        record.printed = Some(path);
    }
    if let Some(line) = lines
        .iter()
        .rev()
        .map(|line| line.trim())
        .find(|line| !line.is_empty())
    {
        record.stdout_tail = clip(line);
    }
    if record.pending.len() > MAX_PENDING_BYTES {
        record.pending.clear();
        record.discard_line = true;
    }
}

/// Keeps the start of a line for a failure message.
fn clip(line: &str) -> String {
    line.chars().take(MAX_TAIL_CHARS).collect()
}

impl acquisition::Guest for YtDlpAcquisition {
    fn get_capabilities() -> Capabilities {
        Capabilities {
            granularities: vec![Granularity::Track],
            progressive: false,
        }
    }

    fn start_search(query: Query) -> Result<String, ProviderError> {
        let config = Config::load()?;
        if query.granularity != Granularity::Track {
            return Err(ProviderError::InvalidConfig(
                "yt-dlp acquires one track at a time".to_string(),
            ));
        }
        let wanted = query.tracks.first().ok_or_else(|| {
            ProviderError::InvalidConfig("the search query carries no track".to_string())
        })?;
        let artist = wanted.artists.first().map(String::as_str);
        let target = args::search_target(&config, artist, &wanted.title);
        let handle = spawn(args::search_arguments(&target), args::SEARCH_TIMEOUT_MS)?;
        let token = Uuid::new_v4().to_string();
        let record = SearchRecord::new(handle);
        kv::set_json(&search_key(&token), &record, Some(SEARCH_TTL_MS))?;
        hearthsound_plugin::debug!("started a search for `{target}` as {token}");
        Ok(token)
    }

    fn poll_search(search_token: String) -> Result<SearchState, ProviderError> {
        let key = search_key(&search_token);
        let mut record: SearchRecord = kv::get_json(&key)?.ok_or(ProviderError::NotFound)?;
        let status = process::poll(record.handle).map_err(process_error)?;
        let handle = record.handle;
        absorb_search(&mut record, &read_stdout(handle)?);
        if !matches!(status, ProcessState::Running) {
            // A finished process may have written its last line between the
            // read above and the poll, so take one more read before parsing.
            absorb_search(&mut record, &read_stdout(handle)?);
        }
        match status {
            ProcessState::Running => {
                kv::set_json(&key, &record, Some(SEARCH_TTL_MS))?;
                Ok(SearchState::Running(record.to_wit()))
            }
            ProcessState::Exited(0) => {
                process::kill(record.handle);
                host_kv::delete(&key);
                finish_search(&mut record)
                    .map(SearchState::Complete)
                    .map_err(ProviderError::Unavailable)
            }
            ProcessState::Exited(code) => {
                let stderr = String::from_utf8_lossy(&read_stderr(record.handle)).to_string();
                process::kill(record.handle);
                host_kv::delete(&key);
                Err(ProviderError::Unavailable(progress::failure_detail(
                    code,
                    &stderr,
                    &record.stdout_tail,
                )))
            }
            ProcessState::Killed => {
                process::kill(record.handle);
                host_kv::delete(&key);
                Err(ProviderError::Unavailable(
                    "the yt-dlp search was cancelled".to_string(),
                ))
            }
            ProcessState::TimedOut => {
                process::kill(record.handle);
                host_kv::delete(&key);
                Err(ProviderError::Unavailable(format!(
                    "yt-dlp did not finish the search within {} seconds",
                    args::SEARCH_TIMEOUT_MS / 1000
                )))
            }
        }
    }

    fn cancel_search(search_token: String) {
        let key = search_key(&search_token);
        if let Ok(Some(record)) = kv::get_json::<SearchRecord>(&key) {
            process::kill(record.handle);
        }
        host_kv::delete(&key);
    }

    fn start_download(candidate_token: String, files: Vec<u32>) -> Result<String, ProviderError> {
        let _ = files;
        let candidate = token::DownloadToken::decode(&candidate_token)?;
        let config = Config::load()?;
        let key = Uuid::new_v4().simple().to_string();
        let handle = spawn(
            args::download_arguments(&key, &candidate),
            config.download_timeout_ms(),
        )?;
        let token = Uuid::new_v4().to_string();
        let record = DownloadRecord::new(handle, key, config.download_timeout_secs);
        kv::set_json(&download_key(&token), &record, Some(DOWNLOAD_TTL_MS))?;
        hearthsound_plugin::debug!("started a download of {} as {token}", candidate.url);
        Ok(token)
    }

    fn poll_download(download_token: String) -> Result<DownloadState, ProviderError> {
        let key = download_key(&download_token);
        let mut record: DownloadRecord = kv::get_json(&key)?.ok_or(ProviderError::NotFound)?;
        if record.done.is_some() {
            return Ok(record.state());
        }
        let status = process::poll(record.handle).map_err(process_error)?;
        let handle = record.handle;
        absorb_download(&mut record, &read_stdout(handle)?);
        if !matches!(status, ProcessState::Running) {
            // A finished process may have written its last line between the
            // read above and the poll, so take one more read before parsing.
            absorb_download(&mut record, &read_stdout(handle)?);
            let stderr = read_stderr(record.handle);
            record.stderr_tail = progress::append_stderr(&record.stderr_tail, &stderr);
        }
        let state = match status {
            ProcessState::Running => None,
            ProcessState::Exited(0) => Some(
                match progress::exit_outcome(
                    0,
                    &record.key,
                    record.printed.as_deref(),
                    &record.stderr_tail,
                    &record.stdout_tail,
                ) {
                    Ok(path) => match files::stat(STAGING_ROOT, &path) {
                        Some(stat) if !stat.is_dir => StoredDone::Complete(path),
                        _ => StoredDone::Failed(format!(
                            "yt-dlp reported `{path}` but the file is not in staging"
                        )),
                    },
                    Err(message) => StoredDone::Failed(message),
                },
            ),
            ProcessState::Exited(code) => Some(StoredDone::Failed(progress::failure_detail(
                code,
                &record.stderr_tail,
                &record.stdout_tail,
            ))),
            ProcessState::Killed => Some(StoredDone::Failed(
                "the yt-dlp download was cancelled".to_string(),
            )),
            ProcessState::TimedOut => Some(StoredDone::Failed(format!(
                "yt-dlp did not finish the download within {} seconds",
                record.timeout_secs
            ))),
        };
        if let Some(done) = state {
            record.done = Some(done);
            process::kill(record.handle);
            kv::set_json(&key, &record, Some(DOWNLOAD_TTL_MS))?;
        } else {
            kv::set_json(&key, &record, Some(DOWNLOAD_TTL_MS))?;
        }
        Ok(record.state())
    }

    fn cancel_download(download_token: String) {
        let key = download_key(&download_token);
        if let Ok(Some(record)) = kv::get_json::<DownloadRecord>(&key) {
            process::kill(record.handle);
        }
        host_kv::delete(&key);
    }
}

impl lifecycle::Guest for YtDlpAcquisition {
    fn validate_config(server_config: String) -> Result<(), Vec<String>> {
        match serde_json::from_str::<Config>(&server_config) {
            Ok(config) => {
                let errors = config.errors();
                if errors.is_empty() {
                    Ok(())
                } else {
                    Err(errors)
                }
            }
            Err(error) => Err(vec![format!(
                "the configuration is not valid JSON for this plugin: {error}"
            )]),
        }
    }

    /// Reports readiness without starting the tool.
    ///
    /// The host allows `host-process.spawn` only while an acquisition has a
    /// staging directory, so a health probe that started `yt-dlp` would always
    /// be denied and the instance would look misconfigured. The probe
    /// validates the configuration and tells the administrator where the
    /// executable is checked instead.
    ///
    /// # Returns
    ///
    /// A status line saying the instance is ready and that `yt-dlp` is checked
    /// when an acquisition runs.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::InvalidConfig`] when the server configuration
    /// does not load or is out of range.
    fn health() -> Result<String, ProviderError> {
        Config::load()?;
        Ok("configuration is valid; yt-dlp is checked when an acquisition runs".to_string())
    }
}

hearthsound_plugin::export_process_acquisition_provider!(YtDlpAcquisition);

#[cfg(test)]
mod tests {
    use super::*;

    /// A captured search output the size and partial-line tests run on.
    const SEARCH: &str = include_str!("fixtures/search.json");

    /// A captured download report.
    const DOWNLOAD: &str = include_str!("fixtures/download.txt");

    /// Feeds a whole output through a record in small chunks, as a process
    /// read would.
    fn absorb(search: &mut SearchRecord, output: &str) {
        for chunk in output.as_bytes().chunks(8192) {
            absorb_search(search, chunk);
        }
    }

    #[test]
    fn keeps_process_error_details() {
        match process_error(ProcessError::Denied("too many processes".to_string())) {
            ProviderError::Unavailable(detail) => assert_eq!(detail, "too many processes"),
            other => panic!("expected unavailable, got {other:?}"),
        }
        match process_error(ProcessError::NotConfigured(
            "tools.yt-dlp.path is empty".to_string(),
        )) {
            ProviderError::InvalidConfig(detail) => {
                assert!(detail.contains("tools.yt-dlp.path"));
            }
            other => panic!("expected invalid config, got {other:?}"),
        }
        assert!(matches!(
            process_error(ProcessError::SpawnFailed("permission denied".to_string())),
            ProviderError::Unavailable(detail) if detail.contains("permission denied")
        ));
        assert!(matches!(
            process_error(ProcessError::UnknownHandle),
            ProviderError::Unavailable(_)
        ));
    }

    #[test]
    fn keeps_an_unfinished_line_between_reads() {
        let line = SEARCH.lines().next().expect("a fixture line");
        let split = line.len() / 2;
        let mut record = SearchRecord::new(0);
        absorb_search(&mut record, &line.as_bytes()[..split]);
        assert!(record.candidates.is_empty());
        assert!(!record.pending.is_empty());
        absorb_search(&mut record, &line.as_bytes()[split..]);
        absorb_search(&mut record, b"\n");
        assert_eq!(record.candidates.len(), 1);
        assert!(record.pending.is_empty());
        assert_eq!(
            finish_search(&mut record).expect("search completes").len(),
            1
        );
    }

    #[test]
    fn a_large_dump_stays_reduced() {
        let padding = "x".repeat(60 * 1024);
        let mut dump = String::new();
        for result in 0..10 {
            for line in SEARCH.lines() {
                let body = line.trim_end().trim_end_matches('}');
                dump.push_str(&body.replacen("\"id\": \"", &format!("\"id\": \"{result}-"), 1));
                dump.push_str(&format!(", \"padding\": \"{padding}\"}}\n"));
            }
        }
        assert!(dump.len() > 1024 * 1024, "dump is {} bytes", dump.len());

        let mut record = SearchRecord::new(0);
        absorb(&mut record, &dump);
        assert_eq!(record.candidates.len(), 30);
        let stored = serde_json::to_vec(&record).expect("the record serializes");
        assert!(
            stored.len() < 256 * 1024,
            "the stored record grew to {} bytes",
            stored.len()
        );
        assert_eq!(
            finish_search(&mut record).expect("search completes").len(),
            30
        );
    }

    #[test]
    fn reports_an_unparsable_search_line() {
        let mut record = SearchRecord::new(0);
        absorb(&mut record, "WARNING: something went wrong\n");
        let detail = finish_search(&mut record).expect_err("the line is not a result");
        assert!(detail.contains("WARNING: something went wrong"));
    }

    #[test]
    fn drops_a_line_beyond_the_pending_cap() {
        let mut record = SearchRecord::new(0);
        absorb_search(&mut record, &vec![b'x'; MAX_PENDING_BYTES + 1]);
        assert!(record.pending.is_empty());
        assert!(record.discard_line);
        let stored = serde_json::to_vec(&record).expect("the record serializes");
        assert!(stored.len() < 256 * 1024);
        assert!(finish_search(&mut record).is_err());
    }

    #[test]
    fn reads_a_real_download_report() {
        let mut record = DownloadRecord::new(0, "key".to_string(), 900);
        absorb_download(&mut record, DOWNLOAD.as_bytes());
        assert!(record.pending.is_empty());
        assert_eq!(record.progress.bytes, 4_628_757);
        assert_eq!(record.progress.total_bytes, Some(4_628_757));
        assert_eq!(
            record.printed.as_deref(),
            Some("/tmp/dl-test/ZHGw78IJryE.opus")
        );
        assert_eq!(
            progress::output_relative("key", record.printed.as_deref().unwrap_or_default())
                .as_deref(),
            Some("key/ZHGw78IJryE.opus")
        );
    }

    #[test]
    fn a_download_record_stays_small() {
        let mut record = DownloadRecord::new(0, "key".to_string(), 900);
        absorb_download(&mut record, &vec![b'y'; MAX_PENDING_BYTES * 2]);
        absorb_download(&mut record, b"\ndownload:5/10\n");
        let stored = serde_json::to_vec(&record).expect("the record serializes");
        assert!(
            stored.len() < 256 * 1024,
            "the stored record grew to {} bytes",
            stored.len()
        );
        assert_eq!(record.progress.bytes, 5);
        assert_eq!(record.progress.total_bytes, Some(10));
    }
}
