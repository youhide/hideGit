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

/// The App's slug, which is what its public URLs are built from.
///
/// **Not derivable from the name.** GitHub generates a slug at registration and
/// it need not resemble the App — this one is `hidegit-github` rather than
/// `hidegit`, because the shorter name was taken. It sits next to the client ID
/// because it is the same kind of thing: a fact about the registered App that
/// only its owner knows, and that no API will hand back without the private key
/// hideGit deliberately does not have.
pub const APP_SLUG: &str = "hidegit-github";

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
/// Untagged because the two shapes are not distinguished by status: a pending
/// authorisation is a `200` with an `error` field, and a refused *request* is a
/// `400` whose body is the only thing that says why. Both are read from the
/// body, which is why every call here goes through [`ask`] rather than through
/// octocrab's `post` — that one turns a non-2xx into an error and discards the
/// explanation GitHub took the trouble to write.
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

/// GitHub's answer to a device-code request.
///
/// Untagged for the same reason as [`TokenReply`], and it is the one that bit:
/// an app without Device Flow enabled answers `400` with
/// `{"error":"device_flow_disabled"}`, which is a complete and actionable
/// explanation — and which reading only the status throws away.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DeviceCodeReply {
    Issued {
        device_code: String,
        user_code: String,
        verification_uri: String,
        expires_in: u64,
        interval: u64,
    },
    Refused {
        error: String,
        #[serde(default)]
        error_description: Option<String>,
    },
}

/// Posts to an OAuth endpoint and reads the body, whatever the status.
///
/// Both endpoints answer `400` with a JSON body that is the useful part, so a
/// helper that errors on non-2xx would hide exactly what the user needs to be
/// told. The URI is absolute because these live on the website rather than on
/// the API host.
async fn ask<T: serde::de::DeserializeOwned>(
    oauth: &Octocrab,
    url: String,
    body: &serde_json::Value,
) -> Result<T, ForgeError> {
    let response = oauth
        ._post(url.clone(), Some(body))
        .await
        .map_err(transport)?;
    let status = response.status();
    let text = oauth.body_to_string(response).await.map_err(transport)?;

    serde_json::from_str(&text).map_err(|error| ForgeError::Malformed {
        host: url,
        // The body is GitHub's own words about a request that carried no
        // credential, so quoting it costs nothing and is usually the answer.
        detail: format!("{status}: {text} ({error})"),
    })
}

/// Turns a refusal into the error that names it.
fn refused(error: &str, description: Option<&str>) -> ForgeError {
    match error {
        "expired_token" => DeviceFlowError::Expired.into(),
        "access_denied" => DeviceFlowError::Denied.into(),
        "device_flow_disabled" => DeviceFlowError::Disabled.into(),
        // Anything else — a wrong client id, a grant type GitHub stopped
        // accepting — keeps GitHub's own description rather than being forced
        // into a diagnosis hideGit cannot support.
        other => ForgeError::Api {
            status: 400,
            message: description.unwrap_or(other).to_owned(),
        },
    }
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
    base: &str,
    client_id: &str,
    announce: impl FnOnce(DeviceCode) + Send,
) -> Result<Issued, ForgeError> {
    if client_id.is_empty() {
        return Err(DeviceFlowError::NotConfigured.into());
    }

    let reply: DeviceCodeReply = ask(
        oauth,
        format!("{base}/login/device/code"),
        &json!({ "client_id": client_id, "scope": SCOPE }),
    )
    .await?;

    let DeviceCodeReply::Issued {
        device_code,
        user_code,
        verification_uri,
        expires_in,
        interval: first_interval,
    } = reply
    else {
        let DeviceCodeReply::Refused {
            error,
            error_description,
        } = reply
        else {
            unreachable!("the other variant was just matched")
        };
        return Err(refused(&error, error_description.as_deref()));
    };

    announce(DeviceCode {
        user_code,
        verification_uri,
        expires_in: Duration::from_secs(expires_in),
    });

    // GitHub's own deadline, honoured rather than polled past. Without it the
    // loop would keep asking about a code that can no longer be approved, and
    // the user would watch a spinner instead of being told to start again.
    let deadline = OffsetDateTime::now_utc() + time::Duration::seconds(expires_in as i64);
    let mut interval = Duration::from_secs(first_interval.max(1));

    loop {
        tokio::time::sleep(interval).await;

        if OffsetDateTime::now_utc() >= deadline {
            return Err(DeviceFlowError::Expired.into());
        }

        let reply: TokenReply = ask(
            oauth,
            format!("{base}/login/oauth/access_token"),
            &json!({
                "client_id": client_id,
                "device_code": device_code,
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            }),
        )
        .await?;

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
                other => return Err(refused(other, error_description.as_deref())),
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
    base: &str,
    client_id: &str,
    refresh_token: &SecretString,
) -> Result<Issued, ForgeError> {
    let reply: TokenReply = ask(
        oauth,
        format!("{base}/login/oauth/access_token"),
        &json!({
            "client_id": client_id,
            "grant_type": "refresh_token",
            "refresh_token": refresh_token.expose(),
        }),
    )
    .await?;

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

    #[test]
    fn a_refusal_keeps_the_name_github_gave_it() {
        // The case that actually bit: an app without Device Flow enabled
        // answers 400 with a complete explanation, and reading only the status
        // turns that into "could not reach the authorisation endpoint".
        assert!(matches!(
            refused(
                "device_flow_disabled",
                Some("Device Flow must be explicitly enabled")
            ),
            ForgeError::DeviceFlow(DeviceFlowError::Disabled)
        ));
        assert!(matches!(
            refused("expired_token", None),
            ForgeError::DeviceFlow(DeviceFlowError::Expired)
        ));
        assert!(matches!(
            refused("access_denied", None),
            ForgeError::DeviceFlow(DeviceFlowError::Denied)
        ));
    }

    #[test]
    fn an_unrecognised_refusal_shows_githubs_description_rather_than_its_code() {
        match refused(
            "incorrect_client_credentials",
            Some("The client_id is not valid"),
        ) {
            ForgeError::Api { message, .. } => assert_eq!(message, "The client_id is not valid"),
            other => panic!("expected an Api error, got {other:?}"),
        }
    }

    #[test]
    fn a_device_code_refusal_parses_as_a_refusal_rather_than_failing_to_parse() {
        // Verbatim from GitHub, for client ID Iv23ctri74KmT4Js2gDM before
        // Device Flow was enabled on the app.
        let body = r#"{"error":"device_flow_disabled","error_description":"Device Flow must be explicitly enabled for this App","error_uri":"https://docs.github.com"}"#;

        match serde_json::from_str::<DeviceCodeReply>(body).expect("it parses") {
            DeviceCodeReply::Refused { error, .. } => assert_eq!(error, "device_flow_disabled"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_device_code_grant_still_parses_as_one() {
        let body = r#"{"device_code":"d","user_code":"WDJB-MJHT","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#;

        assert!(matches!(
            serde_json::from_str::<DeviceCodeReply>(body).expect("it parses"),
            DeviceCodeReply::Issued { .. }
        ));
    }

    #[tokio::test]
    async fn a_build_with_no_client_id_says_so_instead_of_asking_github() {
        // A source build against an unregistered app. Personal access tokens
        // still work, so this is a sentence to show rather than a dead end.
        let crab = Octocrab::builder().build().unwrap();

        let outcome = device_flow(&crab, "https://github.com", "", |_| {
            panic!("nothing to announce")
        })
        .await;
        assert!(matches!(
            outcome,
            Err(ForgeError::DeviceFlow(DeviceFlowError::NotConfigured))
        ));
    }
}
