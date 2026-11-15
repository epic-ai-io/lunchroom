//! What `lunchroom read <id>` keeps: that the recipient read a message, and the hold that reading
//! it puts on the session (`§10::27-30`, D1.1, T1.7).
//!
//! A message handed to a Codex or OpenClaw session is handed over as one line naming its id and
//! `lunchroom read <id>`, never the envelope (`§10::24`); the agent reads the envelope with that
//! command, and `lunchroom status` shows whether it did. The time of the reading is a field of
//! 1.1 on the message (`read_at`), so it lives in the store's second file with the other fields
//! 1.0.1 does not know (D1.2, [`super::side`]).

use serde_json::Value;

use super::{Hold, Mail, Message, Status};
use crate::identity::{Harness, Identity};

/// The field of a message that holds the time its recipient read it.
const READ_AT: &str = "read_at";

impl Mail {
    /// D1.1: the recipient read message `id` at `now`. The first reading is the one kept.
    /// Returns whether the message exists.
    pub fn mark_read(&mut self, id: &str, now: u64) -> bool {
        match self.messages.get_mut(id) {
            Some(message) => {
                message
                    .extra
                    .entry(READ_AT.to_owned())
                    .or_insert_with(|| Value::from(now));
                true
            }
            None => false,
        }
    }

    /// D1.1: when the recipient read message `id`; `None` when it has not, or there is no such
    /// message.
    #[must_use]
    pub fn read_at(&self, id: &str) -> Option<u64> {
        self.messages
            .get(id)?
            .extra
            .get(READ_AT)
            .and_then(Value::as_u64)
    }

    /// T1.7: `to` reads message `id`, so its text is in the session: the message holds it, as a
    /// delivered one does, also where the user typed since the delivery and ended that hold.
    /// `false` when the message is not addressed to `to` or is FAILED (never delivered).
    pub fn hold_by_reading(&mut self, to: &Identity, id: &str) -> bool {
        let Some(message) = self
            .messages
            .get(id)
            .filter(|m| m.to == *to && !matches!(m.status, Status::Failed { .. }))
        else {
            return false;
        };
        let hold = Hold {
            message: id.to_owned(),
            from: Some(message.from.clone()),
            from_name: message.from_name.clone(),
        };
        let holds = self.holds.entry(to.key()).or_default();
        if !holds.iter().any(|h| h.message == id) {
            holds.push(hold);
        }
        true
    }

    /// D1.1: the messages handed over by the one line that names the id and `lunchroom read <id>`
    /// (to a Codex or OpenClaw session), as far as they were handed over or delivered, oldest
    /// first; `lunchroom status` shows for each whether its recipient has read it.
    #[must_use]
    pub fn handed_by_id(&self) -> Vec<&Message> {
        let mut found: Vec<&Message> = self
            .messages
            .values()
            .filter(|m| matches!(m.to.harness, Harness::Codex | Harness::OpenClaw))
            .filter(|m| match m.status {
                Status::Delivered => true,
                Status::Queued => m.handed_over,
                Status::Failed { .. } => false,
            })
            .collect();
        found.sort_by(|a, b| (a.sent_at, &a.id).cmp(&(b.sent_at, &b.id)));
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail::{Confirmation, Origin};
    use crate::store::Store;

    fn claude(s: &str) -> Identity {
        Identity::new(Harness::ClaudeCode, s)
    }

    fn codex(s: &str) -> Identity {
        Identity::new(Harness::Codex, s)
    }

    /// Claude (c1) and Codex (x1), each seen and named; Claude's message to Codex, QUEUED.
    fn sent() -> (Mail, String) {
        let mut m = Mail::default();
        for (name, who) in [("claude", claude("c1")), ("codex", codex("x1"))] {
            m.observe(&who);
            assert_eq!(m.register(name, &who, Origin::User, None), Ok(None));
        }
        let id = m
            .send(&claude("c1"), "claude", "codex", "hello", 0)
            .map(|a| a.id)
            .unwrap_or_default();
        assert_ne!(id, "");
        (m, id)
    }

    // §10::30 (D1.1): the time of the reading is kept once, the first one; a message that
    // does not exist is not read.
    #[test]
    fn the_first_reading_is_the_one_kept() {
        let (mut m, id) = sent();
        assert_eq!(m.read_at(&id), None);
        assert!(m.mark_read(&id, 40));
        assert!(m.mark_read(&id, 90));
        assert_eq!(m.read_at(&id), Some(40));
        assert!(!m.mark_read("m-none", 5));
        assert_eq!(m.read_at("m-none"), None);
    }

    // §10::30 and §10::41 (D1.2): the reading is a field of 1.1, so it is kept in the
    // store's second file and comes back from the store whole.
    #[test]
    fn the_reading_survives_the_store() {
        let dir = crate::test_target()
            .join("store-tests-u10")
            .join(std::process::id().to_string());
        let _ = std::fs::remove_dir_all(&dir);
        let (m, id) = sent();
        let store = Store::<Mail>::open(&dir);
        assert!(store.is_ok(), "{store:?}");
        let Ok(store) = store else { return };
        assert!(store.update(|w| *w = m).is_ok());
        assert!(store.update(|w| w.mark_read(&id, 77)).is_ok());
        let back = store.read();
        assert_eq!(back.ok().and_then(|b| b.read_at(&id)), Some(77));
        // The state file carries no `read_at`: a 1.0.1 program drops what it does not know, so
        // the field is in the second file.
        let state = std::fs::read_to_string(dir.join("state.json")).unwrap_or_default();
        assert!(!state.contains("read_at"), "{state}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // §10::29 (T1.7): reading holds the reader by the message, also after the user typed
    // in the session and ended an earlier hold; another session, an unknown id and a FAILED
    // message hold nobody.
    #[test]
    fn reading_holds_the_reader_by_the_message() {
        let (mut m, id) = sent();
        assert!(!m.hold_by_reading(&claude("c1"), &id), "not its recipient");
        assert!(!m.hold_by_reading(&codex("x1"), "m-none"));
        assert_eq!(m.confirm_at(&codex("x1"), &id, 1), Confirmation::Show);
        m.user_typed(&codex("x1"));
        assert!(!m.is_held(&codex("x1")));
        assert!(m.hold_by_reading(&codex("x1"), &id));
        assert!(m.is_held(&codex("x1")));
        assert!(m.hold_by_reading(&codex("x1"), &id));
        assert_eq!(m.holding(&codex("x1")).len(), 1);
        assert!(!m.is_held(&claude("c1")));
    }

    // §10::30: only a message to a Codex or OpenClaw session that was handed over or
    // delivered is listed, oldest first.
    #[test]
    fn the_messages_handed_by_id_are_those_to_codex_and_openclaw() {
        let (mut m, id) = sent();
        assert!(m.handed_by_id().is_empty(), "QUEUED, not handed over");
        // Put in front of Codex at time 1 and handed over: it waits for its confirmation.
        assert!(m.present(&codex("x1"), 1).is_some());
        m.handed_over(&id);
        assert_eq!(m.handed_by_id().len(), 1);
        // A message to Claude Code is delivered by the prompt, not by the line.
        let to_claude = m
            .send(&codex("x1"), "codex", "claude", "back", 5)
            .map(|a| a.id)
            .unwrap_or_default();
        assert_eq!(
            m.confirm_at(&claude("c1"), &to_claude, 6),
            Confirmation::Show
        );
        let ids: Vec<&str> = m.handed_by_id().iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, [id.as_str()]);
    }
}
