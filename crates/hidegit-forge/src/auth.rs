//! OAuth 2.0 Device Authorization Flow ([RFC 8628]), and token refresh.
//!
//! **No client secret is embedded.** hideGit is open source, so anything
//! compiled in is public; the device flow exists precisely for public clients
//! that cannot hold a secret. A client *identifier* is not a secret — it names
//! the application, it does not authorise anything — so [`CLIENT_ID`] below is
//! a constant rather than a configuration value somebody has to find.
//!
//! [RFC 8628]: https://datatracker.ietf.org/doc/html/rfc8628

use std::time::Duration;

use octocrab::Octocrab;
use serde::Deserialize;
use serde_json::json;
use time::OffsetDateTime;

use crate::error::{DeviceFlowError, ForgeError};
use crate::secret::SecretString;
use crate::token::{StoredToken, to_unix};
use crate::{DeviceCode, model::ForgeId};

/// hideGit's registered GitHub App.
///
/// Public by design: a client ID identifies the application to GitHub and
/// authorises nothing on its own, which is what makes the device flow usable by
/// an open-source client at all. Compare `SECURITY.md`, which says the same
/// thing about what must *not* be compiled in.
pub const CLIENT_ID: &str = "Iv23ctri74KmT4Js2gDM";

/// A GitHub App requests no scopes.
///
/// Permissions come from the App's own definition and from what the user grants
/// when installing it, not from the authorisation request. Sending a scope here
/// would be an OAuth App's habit applied to something that ignores it.
const SCOPE: &str = "";

/// What GitHub issued.
#[derive(Debug)]
pub struct Issued {
    pub access: SecretString,
    pub expires_at: Option<OffsetDateTime>,
    pub refresh: Option<SecretString>,
    pub refresh_expires_at: Option<OffsetDateTime>,
}

impl Issued {
    /// Attaches the login the token turned out to belong to.
    pub fn into_stored(self, login: impl Into<String>) -> StoredToken {
        StoredToken {
            login: login.into(),
            access: self.access,
            expires_at: self.expires_at.map(to_unix),
            refresh: self.refresh,
            refresh_expires_at: self.refresh_expires_at.map(to_unix),
        }
    }
}

/// GitHub's answer to a token request.
///
/// Untagged because both shapes come back with `200 OK`: a pending
/// authorisation is not an HTTP error, it is a body with an `error` field.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum TokenReply {
    Issued {
        access_token: String,
        #[serde(default)]
        expires_in: Option<i64>,
        #[serde(default)]
        refresh_token: Option<String>,
        #[serde(default)]
        refresh_token_expires_in: Option<i64>,
    },
    Refused {
        error: String,
        #[serde(default)]
        error_description: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
struct DeviceCodeReply {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
}

/// Runs the device flow to completion.
///
/// `announce` is called once, as soon as there is a code to show, and polling
/// starts after it returns — the flow's whole premise is that the user goes and
/// approves it somewhere else, so nothing can proceed until the code is on
/// screen.
///
/// Every way this can end is its own error, because each is a different
/// sentence to show somebody staring at a code they just typed. Collapsing them
/// into "authorisation failed" would throw away the only useful part.
pub async fn device_flow(
    oauth: &Octocrab,
    client_id: &str,
    announce: impl FnOnce(DeviceCode) + Send,
) -> Result<Issued, ForgeError> {
    if client_id.is_empty() {
        return Err(DeviceFlowError::NotConfigured.into());
    }

    let codes: DeviceCodeReply = oauth
        .post(
            "/login/device/code",
            Some(&json!({ "client_id": client_id, "scope": SCOPE })),
        )
        .await
        .map_err(transport)?;

    announce(DeviceCode {
        user_code: codes.user_code.clone(),
        verification_uri: codes.verification_uri.clone(),
        expires_in: Duration::from_secs(codes.expires_in),
    });

    // GitHub's own deadline, honoured rather than polled past. Without it the
    // loop would keep asking about a code that can no longer be approved, and
    // the user would watch a spinner instead of being told to start again.
    let deadline = OffsetDateTime::now_utc() + time::Duration::seconds(codes.expires_in as i64);
    let mut interval = Duration::from_secs(codes.interval.max(1));

    loop {
        tokio::time::sleep(interval).await;

        if OffsetDateTime::now_utc() >= deadline {
            return Err(DeviceFlowError::Expired.into());
        }

        let reply: TokenReply = oauth
            .post(
                "/login/oauth/access_token",
                Some(&json!({
                    "client_id": client_id,
                    "device_code": codes.device_code,
                    "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
                })),
            )
            .await
            .map_err(transport)?;

        match reply {
            TokenReply::Issued { .. } => return Ok(issued(reply)),
            TokenReply::Refused {
                ref error,
                ref error_description,
            } => match error.as_str() {
                // Nobody has clicked approve yet. This is the ordinary answer
                // for most of the flow.
                "authorization_pending" => {}
                // Asked too often. Five seconds is what GitHub's own
                // documentation says to add.
                "slow_down" => interval += Duration::from_secs(5),
                "expired_token" => return Err(DeviceFlowError::Expired.into()),
                "access_denied" => return Err(DeviceFlowError::Denied.into()),
                "device_flow_disabled" => return Err(DeviceFlowError::Disabled.into()),
                // Anything else — a wrong client id, a grant type GitHub
                // stopped accepting — keeps GitHub's own description rather
                // than being forced into a diagnosis hideGit cannot support.
                other => {
                    return Err(ForgeError::Api {
                        status: 200,
                        message: error_description
                            .clone()
                            .unwrap_or_else(|| other.to_owned()),
                    });
                }
            },
        }
    }
}

/// Exchanges a refresh token for a new access token.
///
/// Only reached for a GitHub App with token expiry left on. When it is turned
/// off GitHub issues no refresh token, nothing calls this, and the difference
/// needs no configuration on hideGit's side.
pub async fn refresh(
    oauth: &Octocrab,
    client_id: &str,
    refresh_token: &SecretString,
) -> Result<Issued, ForgeError> {
    let reply: TokenReply = oauth
        .post(
            "/login/oauth/access_token",
            Some(&json!({
                "client_id": client_id,
                "grant_type": "refresh_token",
                "refresh_token": refresh_token.expose(),
            })),
        )
        .await
        .map_err(transport)?;

    match reply {
        TokenReply::Issued { .. } => Ok(issued(reply)),
        // A refresh token that GitHub will not honour means the session is
        // over — the user revoked it, or it outlived its own expiry. Reported
        // as "not signed in", which is the state it leaves them in and the one
        // with an obvious next action.
        TokenReply::Refused { .. } => Err(ForgeError::NotAuthenticated(ForgeId::GitHub)),
    }
}

/// Turns an issued reply into absolute expiry times.
///
/// GitHub sends durations; storing a duration would mean the token appears to
/// last another eight hours every time hideGit restarts.
fn issued(reply: TokenReply) -> Issued {
    let TokenReply::Issued {
        access_token,
        expires_in,
        refresh_token,
        refresh_token_expires_in,
    } = reply
    else {
        unreachable!("only called on an issued reply")
    };

    let now = OffsetDateTime::now_utc();
    let at = |seconds: Option<i64>| seconds.map(|s| now + time::Duration::seconds(s));

    Issued {
        access: SecretString::new(access_token),
        expires_at: at(expires_in),
        refresh: refresh_token.map(SecretString::new),
        refresh_expires_at: at(refresh_token_expires_in),
    }
}

fn transport(error: octocrab::Error) -> ForgeError {
    ForgeError::Network {
        host: "the authorisation endpoint".to_owned(),
        source: Box::new(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> TokenReply {
        serde_json::from_str(json).expect("GitHub's shapes deserialise")
    }

    #[test]
    fn both_shapes_of_a_token_response_arrive_with_the_same_status() {
        // The reason `TokenReply` is untagged: a pending authorisation is a
        // 200 with an `error` field, not an HTTP error.
        assert!(matches!(
            parse(r#"{"access_token":"ghu_x","token_type":"bearer"}"#),
            TokenReply::Issued { .. }
        ));
        assert!(matches!(
            parse(r#"{"error":"authorization_pending","error_description":"Pending"}"#),
            TokenReply::Refused { .. }
        ));
    }

    #[test]
    fn a_duration_is_stored_as_a_moment_rather_than_a_length() {
        // Keeping the duration would make the token look eight hours fresh
        // again on every restart.
        let before = OffsetDateTime::now_utc();
        let issued = issued(parse(
            r#"{"access_token":"ghu_x","expires_in":28800,"refresh_token":"ghr_x","refresh_token_expires_in":15811200}"#,
        ));

        let expires_at = issued.expires_at.expect("an expiring token");
        assert!(expires_at > before + time::Duration::hours(7));
        assert!(expires_at < before + time::Duration::hours(9));
        assert!(issued.refresh.is_some());
    }

    #[test]
    fn a_token_with_no_expiry_stores_none_rather_than_a_guess() {
        let issued = issued(parse(r#"{"access_token":"ghp_x"}"#));

        assert_eq!(issued.expires_at, None);
        assert!(issued.refresh.is_none());

        let stored = issued.into_stored("youhide");
        assert!(!stored.needs_refresh(OffsetDateTime::now_utc()));
    }

    #[tokio::test]
    async fn a_build_with_no_client_id_says_so_instead_of_asking_github() {
        // A source build against an unregistered app. Personal access tokens
        // still work, so this is a sentence to show rather than a dead end.
        let crab = Octocrab::builder().build().unwrap();

        let outcome = device_flow(&crab, "", |_| panic!("nothing to announce")).await;
        assert!(matches!(
            outcome,
            Err(ForgeError::DeviceFlow(DeviceFlowError::NotConfigured))
        ));
    }
}
