//! A Hearthsound acquisition plugin that downloads tracks with yt-dlp.
//!
//! The plugin implements the `acquisition` provider interface at track
//! granularity and runs yt-dlp through the host's `host-process` import: an
//! administrator installs yt-dlp and points `tools.yt-dlp.path` at it, and the
//! plugin never sees a filesystem path for the tool, only its name.
//!
//! A search asks yt-dlp for metadata only, with `--dump-json`, and turns each
//! result into a candidate carrying the best audio-only format. A download
//! fetches that format, extracts the audio without re-encoding, embeds the
//! site's metadata and reports progress from yt-dlp's own progress lines. The
//! produced file is reported under the staging root, where the server imports
//! it like any other downloading provider's output.
//!
//! All network access happens inside yt-dlp; the component requests no HTTP
//! permission.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod args;
mod config;
mod progress;
mod search;
mod token;

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use hearthsound_plugin::host::files;
use hearthsound_plugin::host::kv as host_kv;
use hearthsound_plugin::host::process::{
    self, ProcessError, ProcessHandle, ProcessState, SpawnOptions, StdoutMode,
};
use hearthsound_plugin::kv;
use hearthsound_plugin::process_acquisition_provider::exports::hearthsound::plugin::acquisition::{
    self, Capabilities, DownloadState, Granularity, OutputFile, Progress, Query, SearchState,
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

/// How long the health probe waits for `yt-dlp --version`.
const HEALTH_DEADLINE: Duration = Duration::from_secs(8);

/// How often the health probe asks whether the version process finished.
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The plugin implementation.
struct YtDlpAcquisition;

/// A search kept between polls.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchRecord {
    /// The process running the search.
    handle: ProcessHandle,
    /// Everything the process wrote, encoded by [`token::encode_bytes`].
    output: String,
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
/// A tool the manifest does not declare, or one the administrator has not
/// configured, is a configuration problem; a process the operating system
/// refused to start is a transient unavailability; an unknown handle means the
/// record a token names no longer has a process behind it.
fn process_error(error: ProcessError) -> ProviderError {
    match error {
        ProcessError::Denied(message) => ProviderError::InvalidConfig(message),
        ProcessError::NotConfigured(message) => ProviderError::InvalidConfig(message),
        ProcessError::SpawnFailed(message) => ProviderError::Unavailable(message),
        ProcessError::UnknownHandle => ProviderError::NotFound,
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

/// Appends a search output chunk to its record.
///
/// # Returns
///
/// Everything the search wrote so far, decoded with replacement characters for
/// bytes that are not UTF-8.
fn absorb_search(record: &mut SearchRecord, chunk: &[u8]) -> String {
    let mut output = token::decode_bytes(&record.output);
    output.extend_from_slice(chunk);
    record.output = token::encode_bytes(&output);
    String::from_utf8_lossy(&output).to_string()
}

/// Appends a download output chunk to its record.
///
/// Only complete lines are parsed; the bytes of an unfinished line stay in the
/// record for the next read.
fn absorb_download(record: &mut DownloadRecord, chunk: &[u8]) {
    let lines = progress::append_lines(&mut record.pending, chunk);
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
        record.stdout_tail = line.to_string();
    }
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
        let record = SearchRecord {
            handle,
            output: String::new(),
        };
        kv::set_json(&search_key(&token), &record, Some(SEARCH_TTL_MS))?;
        hearthsound_plugin::debug!("started a search for `{target}` as {token}");
        Ok(token)
    }

    fn poll_search(search_token: String) -> Result<SearchState, ProviderError> {
        let key = search_key(&search_token);
        let mut record: SearchRecord = kv::get_json(&key)?.ok_or(ProviderError::NotFound)?;
        let status = process::poll(record.handle).map_err(process_error)?;
        let handle = record.handle;
        let mut text = absorb_search(&mut record, &read_stdout(handle)?);
        if !matches!(status, ProcessState::Running) {
            // A finished process may have written its last line between the
            // read above and the poll, so take one more read before parsing.
            text = absorb_search(&mut record, &read_stdout(handle)?);
        }
        match status {
            ProcessState::Running => {
                kv::set_json(&key, &record, Some(SEARCH_TTL_MS))?;
                Ok(SearchState::Running(search::candidates(&text)))
            }
            ProcessState::Exited(0) => {
                process::kill(record.handle);
                host_kv::delete(&key);
                Ok(SearchState::Complete(search::candidates(&text)))
            }
            ProcessState::Exited(code) => {
                let stderr = String::from_utf8_lossy(&read_stderr(record.handle)).to_string();
                process::kill(record.handle);
                host_kv::delete(&key);
                Err(ProviderError::Unavailable(progress::failure_detail(
                    code, &stderr, &text,
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

    fn health() -> Result<String, ProviderError> {
        let handle = spawn(args::version_arguments(), args::VERSION_TIMEOUT_MS)?;
        let deadline = Instant::now() + HEALTH_DEADLINE;
        loop {
            match process::poll(handle).map_err(process_error)? {
                ProcessState::Running => {
                    if Instant::now() >= deadline {
                        process::kill(handle);
                        return Err(ProviderError::Unavailable(
                            "yt-dlp did not report its version in time".to_string(),
                        ));
                    }
                    std::thread::sleep(HEALTH_POLL_INTERVAL);
                }
                ProcessState::Exited(0) => {
                    let output = read_stdout(handle)?;
                    process::kill(handle);
                    let version = String::from_utf8_lossy(&output)
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    return Ok(if version.is_empty() {
                        TOOL.to_string()
                    } else {
                        version
                    });
                }
                ProcessState::Exited(code) => {
                    let stderr = String::from_utf8_lossy(&read_stderr(handle)).to_string();
                    process::kill(handle);
                    return Err(ProviderError::Unavailable(progress::failure_detail(
                        code, &stderr, "",
                    )));
                }
                ProcessState::Killed | ProcessState::TimedOut => {
                    process::kill(handle);
                    return Err(ProviderError::Unavailable(
                        "yt-dlp --version did not finish".to_string(),
                    ));
                }
            }
        }
    }
}

hearthsound_plugin::export_process_acquisition_provider!(YtDlpAcquisition);
