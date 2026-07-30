//! A string that does not print itself.
//!
//! Tokens are stored in the OS keychain, never in config and never in logs.
//! Keeping that promise by discipline alone does not survive a `#[derive(Debug)]`
//! somebody adds later, so the type carries it instead: there is no way to see
//! the value except by asking for it by name.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A token, kept out of every diagnostic.
///
/// `Debug` and `Display` both redact. [`SecretString::expose`] is the only way
/// to read it, and it is named to be conspicuous at the call site — a grep for
/// `expose` finds every place a token can escape.
///
/// Serde is derived because a token has exactly one destination that is allowed
/// to hold the real value: the OS keychain, which stores one JSON string. It is
/// `transparent`, so the keychain entry holds the token rather than a wrapper
/// around it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(redacted)")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("redacted")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neither_debug_nor_display_leaks_the_value() {
        let secret = SecretString::new("ghu_averyrealtokenindeed");

        assert_eq!(format!("{secret:?}"), "SecretString(redacted)");
        assert_eq!(format!("{secret}"), "redacted");
        assert!(!format!("{secret:?} {secret}").contains("ghu_"));
    }

    #[test]
    fn a_struct_deriving_debug_around_one_does_not_leak_it_either() {
        // The case the type exists for: nobody has to remember to write a
        // manual Debug impl for every struct that holds a token.
        #[derive(Debug)]
        struct Stored {
            login: String,
            token: SecretString,
        }

        let stored = Stored {
            login: "youhide".to_owned(),
            token: SecretString::new("ghu_averyrealtokenindeed"),
        };

        let printed = format!("{stored:?}");
        assert!(printed.contains(&stored.login));
        assert!(!printed.contains(stored.token.expose()), "{printed}");
    }

    #[test]
    fn the_value_is_readable_when_it_is_asked_for_by_name() {
        assert_eq!(SecretString::new("abc").expose(), "abc");
    }
}
