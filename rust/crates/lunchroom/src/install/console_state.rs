//! Why the console was not set up (§17.8), which `lunchroom doctor` reads (C1.4).
//!
//! The record is `/etc/lunchroom/<uid>.console.state`, written by the step that runs as root, so no
//! process of the user's can change or remove it; it holds one JSON object with `kind` (`declined`
//! or `not-in-force`), `reason` and `at` (seconds since 1970). A successful `lunchroom install
//! console` removes it.

use serde_json::{Value, json};

/// Why the console was not set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The user answered anything but `yes` to the question about making `sudo` ask for a password.
    Declined,
    /// The rule was written but is not in force, or could not be written.
    NotInForce,
}

impl Kind {
    const fn word(self) -> &'static str {
        match self {
            Self::Declined => "declined",
            Self::NotInForce => "not-in-force",
        }
    }
}

/// The record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub kind: Kind,
    /// The reason, as the FAIL line gives it.
    pub reason: String,
    /// Seconds since 1970.
    pub at: u64,
}

impl State {
    /// The file's text.
    #[must_use]
    pub fn to_json(&self) -> String {
        format!(
            "{}\n",
            json!({ "kind": self.kind.word(), "reason": self.reason, "at": self.at })
        )
    }

    /// The record in a file's text; `None` where the text is not one.
    #[must_use]
    pub fn from_json(text: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(text).ok()?;
        let kind = match v.get("kind")?.as_str()? {
            "declined" => Kind::Declined,
            "not-in-force" => Kind::NotInForce,
            _ => return None,
        };
        Some(Self {
            kind,
            reason: v.get("reason")?.as_str()?.to_owned(),
            at: v.get("at")?.as_u64()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The record reads back as it was written, and a text that is not one reads as none.
    #[test]
    fn a_record_reads_back() {
        for kind in [Kind::Declined, Kind::NotInForce] {
            let s = State {
                kind,
                reason: "a later rule wins: \"dev ALL=(ALL) NOPASSWD: ALL\"".to_owned(),
                at: 1_762_300_000,
            };
            assert_eq!(State::from_json(&s.to_json()), Some(s));
        }
        for bad in [
            "",
            "{}",
            "[]",
            "{\"kind\":\"x\",\"reason\":\"r\",\"at\":1}",
            "{\"kind\":\"declined\",\"reason\":1,\"at\":1}",
            "{\"kind\":\"declined\",\"reason\":\"r\"}",
        ] {
            assert_eq!(State::from_json(bad), None, "{bad:?}");
        }
    }
}
