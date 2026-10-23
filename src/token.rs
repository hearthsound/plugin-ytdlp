//! Byte and token encoding.
//!
//! A candidate token has to survive the server recycling a plugin instance, so
//! it carries the values rather than naming a row in the key-value store. It is
//! URL-safe base64 over a small JSON object, because the host treats a token as
//! an opaque string and the plugin prints it in logs.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use hearthsound_plugin::types::ProviderError;

/// The values a download of one candidate needs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadToken {
    /// The page yt-dlp downloads from.
    pub url: String,
    /// The yt-dlp format id the candidate chose.
    pub format_id: String,
    /// The container extension that format produces.
    pub ext: String,
    /// The site's id for the video.
    pub id: String,
}

impl DownloadToken {
    /// Encodes the token as URL-safe base64 without padding.
    ///
    /// # Returns
    ///
    /// The token string handed to the server as a candidate token.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Internal`] when the JSON cannot be serialized,
    /// which cannot happen for this struct.
    pub fn encode(&self) -> Result<String, ProviderError> {
        let json = serde_json::to_vec(self)
            .map_err(|error| ProviderError::Internal(format!("candidate token: {error}")))?;
        Ok(URL_SAFE_NO_PAD.encode(json))
    }

    /// Decodes a candidate token.
    ///
    /// # Parameters
    ///
    /// - `token`: the string a previous [`DownloadToken::encode`] produced.
    ///
    /// # Returns
    ///
    /// The values the token carries.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::NotFound`] when the token is not base64 of a
    /// JSON object with the expected fields, so a corrupt or foreign token is
    /// answered as an unknown candidate.
    pub fn decode(token: &str) -> Result<DownloadToken, ProviderError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(token.trim())
            .map_err(|_| ProviderError::NotFound)?;
        serde_json::from_slice(&bytes).map_err(|_| ProviderError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> DownloadToken {
        DownloadToken {
            url: "https://www.youtube.com/watch?v=abc123".to_string(),
            format_id: "251".to_string(),
            ext: "webm".to_string(),
            id: "abc123".to_string(),
        }
    }

    #[test]
    fn round_trips() {
        let token = example();
        let encoded = token.encode().expect("encodes");
        assert_eq!(DownloadToken::decode(&encoded).expect("decodes"), token);
    }

    #[test]
    fn uses_url_safe_base64_without_padding() {
        let encoded = example().encode().expect("encodes");
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains('='));
    }

    #[test]
    fn rejects_foreign_tokens() {
        assert!(matches!(
            DownloadToken::decode("not base64!"),
            Err(ProviderError::NotFound)
        ));
        assert!(matches!(
            DownloadToken::decode(&URL_SAFE_NO_PAD.encode(b"{\"url\":1}")),
            Err(ProviderError::NotFound)
        ));
        assert!(matches!(
            DownloadToken::decode(""),
            Err(ProviderError::NotFound)
        ));
    }
}
