//! The instance's server-scoped configuration.
//!
//! The host hands the plugin the JSON document the administrator saved, with
//! the manifest schema's defaults filled in. Every field is optional here so a
//! document from an older server still loads with the documented defaults.

use serde::Deserialize;

use hearthsound_plugin::types::ProviderError;

/// The search prefix the plugin puts in front of the query.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchPrefix {
    /// Search YouTube through yt-dlp's `ytsearch`.
    #[default]
    YtSearch,
    /// Search SoundCloud through yt-dlp's `scsearch`.
    ScSearch,
}

impl SearchPrefix {
    /// Returns the prefix yt-dlp expects.
    ///
    /// # Returns
    ///
    /// `"ytsearch"` or `"scsearch"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::YtSearch => "ytsearch",
            Self::ScSearch => "scsearch",
        }
    }
}

/// Server-scoped configuration of the plugin.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// How many search results one query asks yt-dlp for.
    pub search_count: u32,
    /// The search site the query is sent to.
    pub search_prefix: SearchPrefix,
    /// How long one download may run, in seconds.
    pub download_timeout_secs: u32,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            search_count: 5,
            search_prefix: SearchPrefix::YtSearch,
            download_timeout_secs: 900,
        }
    }
}

impl Config {
    /// Reads the instance configuration from the host.
    ///
    /// # Returns
    ///
    /// The configuration with every default applied.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::InvalidConfig`] when the host's document does
    /// not deserialize or a value is outside its documented range.
    pub fn load() -> Result<Config, ProviderError> {
        let config: Config = hearthsound_plugin::config::server_config()?;
        config.validate()?;
        Ok(config)
    }

    /// Checks the values the schema cannot express on its own.
    ///
    /// # Returns
    ///
    /// `Ok(())` when every value is in range.
    ///
    /// # Errors
    ///
    /// Returns one message per out-of-range value.
    pub fn validate(&self) -> Result<(), ProviderError> {
        let errors = self.errors();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ProviderError::InvalidConfig(errors.join("; ")))
        }
    }

    /// Returns every configuration problem, for `validate-config`.
    ///
    /// # Returns
    ///
    /// One message per out-of-range value, in field order.
    pub fn errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if !(1..=10).contains(&self.search_count) {
            errors.push(format!(
                "searchCount must be between 1 and 10, got {}",
                self.search_count
            ));
        }
        if !(60..=3600).contains(&self.download_timeout_secs) {
            errors.push(format!(
                "downloadTimeoutSecs must be between 60 and 3600, got {}",
                self.download_timeout_secs
            ));
        }
        errors
    }

    /// Returns the download timeout in whole milliseconds, capped at the host's
    /// limit.
    ///
    /// # Returns
    ///
    /// `downloadTimeoutSecs * 1000`, never above 3,600,000.
    pub fn download_timeout_ms(&self) -> u32 {
        (self.download_timeout_secs.saturating_mul(1000)).min(3_600_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_manifest() {
        let config = Config::default();
        assert_eq!(config.search_count, 5);
        assert_eq!(config.search_prefix, SearchPrefix::YtSearch);
        assert_eq!(config.download_timeout_secs, 900);
        assert!(config.errors().is_empty());
    }

    #[test]
    fn reads_camel_case_fields() {
        let config: Config = serde_json::from_str(
            r#"{"searchCount":3,"searchPrefix":"scsearch","downloadTimeoutSecs":120}"#,
        )
        .expect("valid config");
        assert_eq!(config.search_count, 3);
        assert_eq!(config.search_prefix, SearchPrefix::ScSearch);
        assert_eq!(config.download_timeout_ms(), 120_000);
    }

    #[test]
    fn reports_out_of_range_values() {
        let config = Config {
            search_count: 0,
            download_timeout_secs: 10,
            ..Config::default()
        };
        let errors = config.errors();
        assert_eq!(errors.len(), 2);
        assert!(errors[0].contains("searchCount"));
        assert!(errors[1].contains("downloadTimeoutSecs"));
    }

    #[test]
    fn caps_the_download_timeout() {
        let config = Config {
            download_timeout_secs: 10_000,
            ..Config::default()
        };
        assert_eq!(config.download_timeout_ms(), 3_600_000);
    }
}
