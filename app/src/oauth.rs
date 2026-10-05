//! The OAuth client Sealwax signs in with (ADR-008, OAUTH-SETUP.md): Microsoft's is public, so
//! only its client ID is needed. `[oauth.microsoft] client_id` in `config.toml` overrides the
//! one the build was given. The ID is no secret, but it is never logged either.

use sealwax_domain::config::Config;

/// `SEALWAX_MICROSOFT_CLIENT_ID` as it was when Sealwax was built, if it was set.
pub const BUILD_MICROSOFT_CLIENT_ID: Option<&str> = option_env!("SEALWAX_MICROSOFT_CLIENT_ID");

/// The Microsoft client ID in use: `config.toml`'s, else `built_in` (the build's). A blank one
/// counts as none, so Microsoft sign-in is unavailable.
pub fn microsoft_client_id(config: &Config, built_in: Option<&str>) -> Option<String> {
    let usable = |id: &str| {
        let id = id.trim();
        (!id.is_empty()).then(|| id.to_owned())
    };
    config
        .oauth
        .microsoft
        .client_id
        .as_deref()
        .and_then(usable)
        .or_else(|| built_in.and_then(usable))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_toml_wins_over_the_build() {
        let none = Config::default();
        assert_eq!(microsoft_client_id(&none, None), None);
        assert_eq!(microsoft_client_id(&none, Some("  ")), None);
        assert_eq!(
            microsoft_client_id(&none, Some(" built-in-id ")).as_deref(),
            Some("built-in-id")
        );
        let configured = Config::parse("[oauth.microsoft]\nclient_id = \"configured-id\"\n")
            .expect("a valid config.toml");
        assert_eq!(
            microsoft_client_id(&configured, Some("built-in-id")).as_deref(),
            Some("configured-id")
        );
        assert_eq!(
            microsoft_client_id(&configured, None).as_deref(),
            Some("configured-id")
        );
    }
}
