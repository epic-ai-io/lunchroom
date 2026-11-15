//! Lunchroom 1.1, unit U5: the store's second file (D1.2), the transcript read (T1.7), the paths a
//! HELD session wrote (T1.8) and the sessions it started (T1.9). Each test is derived from the
//! acceptance criterion of the node it names and from the failure classes of the clause: an old
//! layout, a field empty or corrupt, a lock held, two programs at once.

// A test of a list that is empty says so by `is_empty`, as the clause does.
#![allow(clippy::assert_is_empty)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::*;
use crate::identity::Harness;
use crate::loops::Conversations;
use crate::store::Store;

fn claude(s: &str) -> Identity {
    Identity::new(Harness::ClaudeCode, s)
}

fn codex(s: &str) -> Identity {
    Identity::new(Harness::Codex, s)
}

/// A folder of this test, empty.
fn scratch(name: &str) -> PathBuf {
    let dir = crate::test_target()
        .join("store-tests-u5")
        .join(format!("{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

/// Claude (session c1) and Codex (session x1), each seen by a hook and named by the user.
fn pair() -> Mail {
    let mut m = Mail::default();
    for (name, who) in [("claude", claude("c1")), ("codex", codex("x1"))] {
        m.observe(&who);
        assert_eq!(m.register(name, &who, Origin::User, None), Ok(None));
    }
    m
}

/// Codex's message to Claude, delivered: Claude is HELD by it. Returns the message's id.
fn hold_claude(m: &mut Mail) -> String {
    let id = m
        .send(&codex("x1"), "codex", "claude", "hello", 0)
        .map(|a| a.id)
        .unwrap_or_default();
    assert_eq!(m.confirm_at(&claude("c1"), &id, 1), Confirmation::Show);
    assert!(m.is_held(&claude("c1")));
    id
}

fn held_by(m: &Mail, who: &Identity) -> Vec<String> {
    let mut ids: Vec<String> = m.holding(who).iter().map(|h| h.message.clone()).collect();
    ids.sort();
    ids
}

// ---------------------------------------------------------------------------------------------
// D1.2: the fields 1.1 adds, in a file of their own.
// ---------------------------------------------------------------------------------------------

/// The program of 1.0.1 as far as the store goes: its `Mail`, with the fields it names and no
/// field it does not (`c59a74c`, `rust/crates/lunchroom-core/src/mail.rs`), read and written whole.
pub(super) mod v101 {
    use super::*;

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Message {
        pub id: String,
        pub from: Identity,
        pub from_name: String,
        pub to: Identity,
        pub to_name: String,
        pub text: String,
        pub sent_at: u64,
        pub status: Status,
        pub failed_attempts: u32,
        #[serde(default)]
        pub presented_at: Option<u64>,
        #[serde(default)]
        pub handed_over: bool,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Notice {
        pub message: String,
        pub text: String,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct SessionRecord {
        pub process: String,
        pub cwd: PathBuf,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Approval {
        pub id: String,
        pub session: Identity,
        pub tool: String,
        pub input_hash: String,
        pub holding: Vec<String>,
        pub given_at: u64,
        pub used: bool,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Request {
        pub session: Identity,
        pub agent: String,
        pub tool: String,
        pub input: Value,
        #[serde(default)]
        pub rule: String,
        pub at: u64,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Mail {
        next_message: u64,
        seen: BTreeSet<String>,
        #[serde(default)]
        processes: BTreeMap<String, Identity>,
        #[serde(default)]
        sessions: BTreeMap<String, SessionRecord>,
        names: BTreeMap<String, Identity>,
        receivers: BTreeSet<String>,
        pub messages: BTreeMap<String, Message>,
        inboxes: BTreeMap<Harness, BTreeMap<String, Vec<String>>>,
        holds: BTreeMap<String, Vec<Hold>>,
        pub notices: BTreeMap<String, Vec<Notice>>,
        conversations: Conversations,
        next_approval: u64,
        pub approvals: Vec<Approval>,
        #[serde(default)]
        pub requests: Vec<Request>,
        #[serde(default)]
        asked: Vec<(String, String, String)>,
        #[serde(default)]
        yolo: BTreeMap<String, u64>,
        #[serde(default)]
        harness_yolo: BTreeSet<String>,
        #[serde(default)]
        unguarded_told: BTreeSet<String>,
        #[serde(default)]
        transcripts_read: BTreeSet<String>,
        #[serde(default)]
        guard_fingerprints: BTreeMap<String, String>,
        #[serde(default)]
        process_members: BTreeMap<String, BTreeSet<String>>,
        #[serde(default)]
        hold_mode: Option<crate::policy::HoldMode>,
    }

    /// What a 1.0.1 program does to the store: it takes the lock, reads the state file, changes
    /// it with `change` and writes it whole, as the store writes (a temporary file renamed over).
    pub fn rewrite(dir: &std::path::Path, change: impl FnOnce(&mut Mail)) {
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("state.flock"));
        assert!(lock.is_ok());
        let Ok(lock) = lock else { return };
        assert!(lock.lock().is_ok());
        let text = fs::read_to_string(dir.join("state.json")).unwrap_or_default();
        let parsed: Result<Mail, _> = serde_json::from_str(&text);
        assert!(parsed.is_ok(), "{parsed:?}");
        let Ok(mut state) = parsed else { return };
        change(&mut state);
        let bytes = serde_json::to_vec(&state).unwrap_or_default();
        let mut temp = fs::File::create(dir.join("state.json.tmp"));
        assert!(temp.is_ok());
        if let Ok(file) = &mut temp {
            assert!(file.write_all(&bytes).is_ok());
            assert!(file.sync_all().is_ok());
        }
        assert!(fs::rename(dir.join("state.json.tmp"), dir.join("state.json")).is_ok());
        assert!(lock.unlock().is_ok());
    }
}

/// A store holding a message from Codex to Claude with a failed attempt, a notice for its sender
/// and a notice for the user, a session record with a script, a request and an approval with their
/// folders, and a transcript record.
fn full_store(dir: &std::path::Path) -> (Store<Mail>, String) {
    let store = Store::<Mail>::open(dir);
    assert!(store.is_ok());
    let Ok(store) = store else {
        unreachable!("the store could not be opened");
    };
    let id = store
        .update(|m| {
            *m = pair();
            let id = m
                .send(&codex("x1"), "codex", "claude", "hi", 0)
                .map(|a| a.id)
                .unwrap_or_default();
            assert!(m.attempt_failed(&id, "codex queue exited 1", 100).is_some());
            m.note_unreachable(&id, Some("claude is STOPPED.".to_owned()));
            m.tell_waiting(WAITING_TOLD_SECS + 1000);
            m.observe_process("123:456", std::path::Path::new("/work"), &claude("c1"));
            m.observe_entry(&claude("c1"), std::path::Path::new("/opt/entry.mjs"));
            m.request(Request {
                session: claude("c1"),
                agent: "claude".to_owned(),
                tool: "Bash".to_owned(),
                input: json!({ "command": "git push" }),
                cwd: "/work".to_owned(),
                rule: "r".to_owned(),
                at: 10,
            });
            m.approve(
                &claude("c1"),
                "Bash",
                &json!({ "command": "ls" }),
                "/work",
                20,
            );
            m.transcript_due(&claude("c1"), "/t.jsonl", 10);
            id
        })
        .unwrap_or_default();
    assert_ne!(id, "");
    (store, id)
}

/// The two files of the store in `dir`, parsed.
fn files(dir: &std::path::Path) -> (Value, Value) {
    let read = |name: &str| {
        serde_json::from_str::<Value>(&fs::read_to_string(dir.join(name)).unwrap_or_default())
            .unwrap_or(Value::Null)
    };
    (read("state.json"), read("state.side.json"))
}

// §10::42 (D1.2): "A test makes a failed attempt and a notice, then shows the time of
// the failed attempt and the notice are in a second file beside the store file and are not in the
// store file."
#[test]
fn the_failed_attempt_time_and_the_notices_are_in_the_second_file_only() {
    let dir = scratch("second-file");
    let (_store, id) = full_store(&dir);
    let (main, side) = files(&dir);
    // The time of the message's last failed attempt (`retry_after`), and what was told of it.
    assert!(side["messages"][id.as_str()]["told"]["retry_after"].is_u64());
    assert_eq!(
        side["messages"][id.as_str()]["told"]["last_failure"],
        "codex queue exited 1"
    );
    assert!(main["messages"][id.as_str()].get("told").is_none());
    // The notices: the user's list, and the time and kind of the sender's.
    assert!(
        side["top"]["user_notices"]
            .as_array()
            .is_some_and(|l| !l.is_empty())
    );
    assert!(main.get("user_notices").is_none());
    let sender_notice = &main["notices"]["codex:x1"][0];
    assert!(sender_notice.get("at").is_none() && sender_notice.get("reason").is_none());
    assert!(side["notices"]["codex:x1"][0]["fields"]["at"].is_u64());
    // The other fields 1.0.1 does not know: the script a session runs, the folder an approval and a
    // request are bound to, the transcript record.
    assert_eq!(
        side["sessions"]["claude-code:c1"]["entry"],
        "/opt/entry.mjs"
    );
    assert!(main["sessions"]["claude-code:c1"].get("entry").is_none());
    assert_eq!(side["requests"][0]["fields"]["cwd"], "/work");
    assert!(main["requests"][0].get("cwd").is_none());
    assert_eq!(side["approvals"][0]["fields"]["cwd"], "/work");
    assert!(main["approvals"][0].get("cwd").is_none());
    assert!(side["top"]["transcripts"]["claude-code:c1"].is_object());
    assert!(main.get("transcripts").is_none());
    // The state file carries the number of the program layout (2 in 1.1) and the fields 1.0.1 has.
    assert_eq!(main["schema"], json!(SCHEMA));
    assert_eq!(SCHEMA, 2);
    assert!(main["messages"][id.as_str()]["failed_attempts"].is_u64());
    // Read back whole.
    let back = Store::<Mail>::read_without_change(&dir).unwrap_or_default();
    assert_eq!(back.message(&id).map(|m| m.failed_attempts), Some(1));
    assert!(back.message(&id).and_then(|m| m.told.retry_after).is_some());
    assert!(!back.user_notices().is_empty());
    let _ = fs::remove_dir_all(&dir);
}

// §10::44 (D1.2): "A test has a 1.0.1 program write the store and shows the fields in
// the second file are unchanged." The program of 1.0.1 reads the state file into types that keep
// no field they do not name and writes it whole: the second file stays byte for byte as it was,
// and a 1.1 program reads every field again, with what 1.0.1 changed.
#[test]
fn a_1_0_1_program_writing_the_store_leaves_the_second_file_as_it_was() {
    let dir = scratch("v101");
    let (store, id) = full_store(&dir);
    let side_before = fs::read(dir.join("state.side.json")).unwrap_or_default();
    assert!(!side_before.is_empty());
    v101::rewrite(&dir, |m| {
        // 1.0.1 changes what it knows: a message it fails, and a notice it adds for the sender.
        if let Some(message) = m.messages.get_mut(&id) {
            message.failed_attempts = 2;
        }
        m.notices
            .entry("codex:x1".to_owned())
            .or_default()
            .push(v101::Notice {
                message: id.clone(),
                text: "written by 1.0.1".to_owned(),
            });
    });
    assert_eq!(
        fs::read(dir.join("state.side.json")).unwrap_or_default(),
        side_before,
        "the second file is byte for byte as it was"
    );
    // The state file as 1.0.1 left it has none of the 1.1 fields, and no layout number.
    let (main, _) = files(&dir);
    assert!(main.get("schema").is_none() && main.get("user_notices").is_none());
    assert!(main["messages"][id.as_str()].get("told").is_none());
    // A 1.1 program reads it: its own fields are all there, 1.0.1's change too, and it sees that an
    // older program wrote last.
    let back = store.read().unwrap_or_default();
    assert!(back.written_by_older_program());
    assert_eq!(back.message(&id).map(|m| m.failed_attempts), Some(2));
    assert!(back.message(&id).and_then(|m| m.told.retry_after).is_some());
    assert_eq!(
        back.message(&id).and_then(|m| m.told.last_failure.clone()),
        Some("codex queue exited 1".to_owned())
    );
    assert!(!back.user_notices().is_empty());
    let notices = back.notices(&codex("x1"));
    // The notice 1.1 made keeps its time and kind; the one 1.0.1 added has none (it read as 0).
    assert!(notices.iter().any(|n| n.at > 0));
    assert!(
        notices
            .iter()
            .any(|n| n.text == "written by 1.0.1" && n.at == 0 && !n.reason)
    );
    assert_eq!(
        back.requests().first().map(|r| r.cwd.as_str()),
        Some("/work")
    );
    assert!(back.transcript_record(&claude("c1")).is_some());
    // The next 1.1 write stamps the layout again and rewrites nothing in the second file that
    // 1.0.1 left alone.
    assert!(store.update(|m| m.observe(&claude("c9"))).is_ok());
    let (main, side) = files(&dir);
    assert_eq!(main["schema"], json!(SCHEMA));
    assert_eq!(side["requests"][0]["fields"]["cwd"], "/work");
    let _ = fs::remove_dir_all(&dir);
}

// D1.2, old layout read by a 1.0.1 program and back: a store 1.0.1 wrote (no layout number, no
// second file, none of the 1.1 fields) reads in 1.1 as written by an older program and gains the
// second file at its first write; a request 1.0.1 consumed leaves no orphan in the second file.
#[test]
fn a_store_of_the_old_layout_reads_and_is_upgraded_at_its_first_write() {
    let dir = scratch("old-layout");
    let (store, _) = full_store(&dir);
    // 1.0.1 consumes the request and the approval, and rewrites the file.
    v101::rewrite(&dir, |m| {
        m.requests.clear();
        m.approvals.clear();
    });
    let back = store.read().unwrap_or_default();
    assert!(back.requests().is_empty());
    assert!(store.update(|m| m.observe(&claude("c9"))).is_ok());
    let (_, side) = files(&dir);
    assert!(
        side.get("requests").is_none() && side.get("approvals").is_none(),
        "{side}"
    );
    // A store with no second file at all, as 1.0.1 left it, reads and gains one.
    let old = scratch("no-side");
    assert!(fs::create_dir_all(&old).is_ok());
    let text = json!({
        "next_message": 0, "seen": [], "names": {}, "receivers": [], "messages": {},
        "inboxes": {}, "holds": {}, "notices": {},
        "conversations": serde_json::to_value(Conversations::default()).unwrap_or(Value::Null),
        "next_approval": 0, "approvals": [],
    });
    assert!(fs::write(old.join("state.json"), text.to_string()).is_ok());
    let opened = Store::<Mail>::open(&old);
    assert!(opened.is_ok());
    let Ok(opened) = opened else { return };
    let read = opened.read();
    assert!(
        read.as_ref().is_ok_and(Mail::written_by_older_program),
        "{read:?}"
    );
    assert!(!old.join("state.side.json").exists());
    assert!(
        opened
            .update(|m| m.transcript_due(&claude("c1"), "/t", 1))
            .is_ok()
    );
    assert!(old.join("state.side.json").exists());
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&old);
}

// D1.2, empty or corrupt: a second file that is empty or damaged is an error, never an empty
// state (nothing fails open: holds kept there would vanish); the repair puts the previous second
// file back and keeps the damaged one beside it. A second file missing beside a previous one is a
// lost state, and repaired the same way.
#[test]
fn an_empty_or_corrupt_second_file_is_an_error_and_is_repaired_from_its_previous_state() {
    for (case, damage) in [
        ("empty", Some("")),
        ("corrupt", Some("{ broken")),
        ("not-an-object", Some("[1,2]")),
        ("wrong-type", Some("{\"top\":{\"transcripts\":7}}")),
        ("missing", None),
    ] {
        let dir = scratch(case);
        let (store, id) = full_store(&dir);
        // A second write, so the second file has a previous state.
        assert!(
            store
                .update(|m| m.transcript_due(&claude("c2"), "/t2", 1))
                .is_ok()
        );
        let side = dir.join("state.side.json");
        match damage {
            Some(text) => assert!(fs::write(&side, text).is_ok()),
            None => assert!(fs::remove_file(&side).is_ok()),
        }
        assert!(store.read().is_err(), "{case}: reads as an error");
        assert!(
            store.update(|m| m.observe(&claude("c9"))).is_err(),
            "{case}: no write over it"
        );
        assert_eq!(store.restore_previous().ok(), Some(true), "{case}");
        let back = store.read();
        assert!(back.is_ok(), "{case}: {back:?}");
        let back = back.unwrap_or_default();
        assert!(back.message(&id).is_some(), "{case}");
        if damage.is_some() {
            assert!(dir.join("state.side.json.unreadable-1").exists(), "{case}");
        }
        let _ = fs::remove_dir_all(&dir);
    }
}

// D1.2, empty or corrupt in the state file: still an error, with the second file beside it; the
// repair restores the state file only.
#[test]
fn a_corrupt_state_file_is_repaired_and_the_second_file_stays() {
    let dir = scratch("corrupt-main");
    let (store, id) = full_store(&dir);
    assert!(store.update(|m| m.observe(&claude("c2"))).is_ok());
    let side_before = fs::read(dir.join("state.side.json")).unwrap_or_default();
    assert!(fs::write(dir.join("state.json"), "").is_ok());
    assert!(store.read().is_err());
    assert_eq!(store.restore_previous().ok(), Some(true));
    assert!(store.read().is_ok_and(|m| m.message(&id).is_some()));
    assert_eq!(
        fs::read(dir.join("state.side.json")).unwrap_or_default(),
        side_before
    );
    let _ = fs::remove_dir_all(&dir);
}

// D1.2, two programs at once: ten writers each add a session, a notice of their own and a field of
// the second file, from threads that each open the store as a process would; nothing is lost in
// either file, and the two files agree at the end.
#[test]
fn two_programs_at_once_lose_nothing_in_either_file() {
    let dir = scratch("concurrent");
    let (_store, _) = full_store(&dir);
    let threads: Vec<_> = (0..10)
        .map(|n| {
            let dir = dir.clone();
            std::thread::spawn(move || {
                let store = Store::<Mail>::open(&dir);
                assert!(store.is_ok());
                let Ok(store) = store else { return };
                for k in 0..3 {
                    let who = claude(&format!("w{n}-{k}"));
                    let done = store.update(|m| {
                        m.observe(&who);
                        m.transcript_due(&who, "/t", 5);
                    });
                    assert!(done.is_ok());
                }
            })
        })
        .collect();
    for t in threads {
        assert!(t.join().is_ok());
    }
    let back = Store::<Mail>::read_without_change(&dir).unwrap_or_default();
    for n in 0..10 {
        for k in 0..3 {
            let who = claude(&format!("w{n}-{k}"));
            assert!(back.has_seen(&who), "{who}");
            assert!(back.transcript_record(&who).is_some(), "{who}");
        }
    }
    let _ = fs::remove_dir_all(&dir);
}

// D1.2 `§10::41`: a field no program knows survives a read and a write, at the store, message,
// session, notice, approval and request levels.
#[test]
fn a_field_no_program_knows_survives_a_read_and_a_write() {
    let dir = scratch("unknown");
    let (store, id) = full_store(&dir);
    // Another program writes fields at every level into the second file.
    let side_path = dir.join("state.side.json");
    let mut side: Value = serde_json::from_str(&fs::read_to_string(&side_path).unwrap_or_default())
        .unwrap_or(Value::Null);
    side["top"]["future_top"] = json!(1);
    side["messages"][id.as_str()]["future_message"] = json!(2);
    side["sessions"]["claude-code:c1"]["future_session"] = json!(3);
    side["requests"][0]["fields"]["future_request"] = json!(4);
    side["approvals"][0]["fields"]["future_approval"] = json!(5);
    side["notices"]["codex:x1"][0]["fields"]["future_notice"] = json!(6);
    assert!(fs::write(&side_path, side.to_string()).is_ok());
    assert!(store.update(|m| m.observe(&claude("c9"))).is_ok());
    let (_, side) = files(&dir);
    assert_eq!(side["top"]["future_top"], 1);
    assert_eq!(side["messages"][id.as_str()]["future_message"], 2);
    assert_eq!(side["sessions"]["claude-code:c1"]["future_session"], 3);
    assert_eq!(side["requests"][0]["fields"]["future_request"], 4);
    assert_eq!(side["approvals"][0]["fields"]["future_approval"], 5);
    assert_eq!(side["notices"]["codex:x1"][0]["fields"]["future_notice"], 6);
    let _ = fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------------------------
// T1.7: the transcript, read over several hooks.
// ---------------------------------------------------------------------------------------------

fn read(path: &str, from: u64, reached: u64, ids: &[&str], at_end: bool) -> TranscriptSearch {
    TranscriptSearch {
        path: path.to_owned(),
        from,
        reached,
        ids: ids.iter().map(|s| (*s).to_owned()).collect(),
        at_end,
    }
}

// §5::38-41: a transcript not yet read to its end holds the session, also with no label
// in it; the read that reaches the end ends that hold, and holds by exactly the messages found.
#[test]
fn an_unread_transcript_holds_until_the_read_reaches_the_end_and_then_by_the_ids_found() {
    let mut m = pair();
    let me = claude("c1");
    let id = m
        .send(&codex("x1"), "codex", "claude", "hello", 0)
        .map(|a| a.id)
        .unwrap_or_default();
    let other = m
        .send(&codex("x1"), "codex", "claude", "again", 0)
        .map(|a| a.id)
        .unwrap_or_default();
    assert_eq!(m.transcript_due(&me, "/t", 100), Some(0));
    assert_eq!(
        held_by(&m, &me),
        vec![format!("unjudged:{UNREAD_TRANSCRIPT}")]
    );
    // The first read stops before the end and finds one label.
    assert!(m.transcript_read(&me, &read("/t", 0, 60, &[id.as_str()], false)));
    assert_eq!(m.transcript_record(&me).map(|r| r.offset), Some(60));
    assert_eq!(
        held_by(&m, &me),
        vec![format!("unjudged:{UNREAD_TRANSCRIPT}")]
    );
    // The next hook goes on from the offset kept.
    assert_eq!(m.transcript_due(&me, "/t", 100), Some(60));
    // The read that reaches the end: the unread hold ends; the session is HELD by exactly the one
    // message found, and by no other.
    assert!(m.transcript_read(&me, &read("/t", 60, 100, &[], true)));
    assert_eq!(held_by(&m, &me), vec![id.clone()]);
    assert!(!held_by(&m, &me).contains(&other));
    assert_eq!(m.transcript_due(&me, "/t", 100), None);
    // The hold carries the sender, so the session may reply to it (I1.4).
    assert!(m.holding(&me).iter().all(|h| h.from.is_some()));
}

// §5::40: with no label, HELD only until the read reaches the end.
#[test]
fn a_transcript_with_no_label_holds_only_until_the_read_reaches_the_end() {
    let mut m = pair();
    let me = claude("c1");
    assert_eq!(m.transcript_due(&me, "/t", 100), Some(0));
    assert!(m.is_held(&me));
    assert!(!m.transcript_read(&me, &read("/t", 0, 100, &[], true)));
    assert!(!m.is_held(&me));
}

// §5::31: a label quoted with no such message id puts no session in HELD.
#[test]
fn a_label_naming_no_message_holds_nothing() {
    let mut m = pair();
    let me = claude("c1");
    assert_eq!(m.transcript_due(&me, "/t", 100), Some(0));
    assert!(!m.transcript_read(&me, &read("/t", 0, 100, &["lr-99999999"], true)));
    assert!(!m.is_held(&me));
}

// §5::37: a transcript shorter than the kept offset is read again from its start; so is
// one that is another file.
#[test]
fn a_transcript_shorter_than_the_kept_offset_is_read_again_from_its_start() {
    let mut m = pair();
    let me = claude("c1");
    assert_eq!(m.transcript_due(&me, "/t", 100), Some(0));
    assert!(m.transcript_read(&me, &read("/t", 0, 80, &[], false)));
    assert_eq!(m.transcript_due(&me, "/t", 100), Some(80));
    // The file was replaced by a shorter one.
    assert_eq!(m.transcript_due(&me, "/t", 50), Some(0));
    assert_eq!(m.transcript_record(&me).map(|r| r.offset), Some(0));
    // Another file at the same session: from its start.
    // Read to its end with no label: the unread hold ends, and nothing else holds the session.
    assert!(!m.transcript_read(&me, &read("/t", 0, 50, &[], true)));
    assert_eq!(m.transcript_due(&me, "/other", 500), Some(0));
    // A read that began before the record was started again is dropped.
    assert!(m.transcript_read(&me, &read("/other", 0, 100, &[], false)));
    assert_eq!(m.transcript_due(&me, "/other", 50), Some(0));
    assert!(m.transcript_read(&me, &read("/other", 100, 500, &[], true)));
    assert_eq!(
        m.transcript_record(&me).map(|r| (r.offset, r.done)),
        Some((0, false))
    );
}

// §5::39: after the user types, a session held only for an unread transcript is no
// longer HELD, and is not held again by it at the next hook; what the read finds after that holds.
#[test]
fn the_user_typing_ends_the_hold_of_an_unread_transcript() {
    let mut m = pair();
    let me = claude("c1");
    let id = m
        .send(&codex("x1"), "codex", "claude", "hello", 0)
        .map(|a| a.id)
        .unwrap_or_default();
    assert_eq!(m.transcript_due(&me, "/t", 100), Some(0));
    assert!(m.transcript_read(&me, &read("/t", 0, 60, &[], false)));
    m.user_typed(&me);
    assert!(!m.is_held(&me));
    assert_eq!(m.transcript_due(&me, "/t", 100), Some(60));
    assert!(!m.is_held(&me), "the unread rest does not hold it again");
    // A label found in the rest still holds, at the end.
    assert!(m.transcript_read(&me, &read("/t", 60, 100, &[id.as_str()], true)));
    assert_eq!(held_by(&m, &me), vec![id]);
}

// §5::35: the store holds the offset and the ids and none of the transcript's text.
#[test]
fn the_record_of_a_transcript_holds_no_text() {
    let mut m = pair();
    let me = claude("c1");
    assert_eq!(m.transcript_due(&me, "/t", 100), Some(0));
    assert!(m.transcript_read(&me, &read("/t", 0, 40, &["lr-00000001"], false)));
    let text = serde_json::to_string(&m).unwrap_or_default();
    let record = serde_json::to_value(m.transcript_record(&me)).unwrap_or(Value::Null);
    let mut keys: Vec<&str> = record
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    keys.sort_unstable();
    assert_eq!(keys, ["done", "ids", "offset", "path", "reading", "typed"]);
    assert!(text.contains("lr-00000001"));
}

// ---------------------------------------------------------------------------------------------
// T1.8: the paths a HELD session wrote.
// ---------------------------------------------------------------------------------------------

fn paths(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

// §5::43: a HELD session's call that wrote a file leaves a record of the path with the
// ids of the messages that held it at that moment; a call of a session that is not HELD, one that
// was blocked or cancelled (no after-call hook), and a repeat record nothing more.
#[test]
fn a_held_sessions_write_is_recorded_with_the_messages_that_held_it() {
    let mut m = pair();
    let me = claude("c1");
    // Not HELD: nothing is recorded.
    m.note_writing(&me, "call-0", paths(&["/w/free.txt"]), 5);
    let kept = m.take_writing(&me, "call-0");
    assert_eq!(kept, Some(paths(&["/w/free.txt"])));
    assert!(!m.record_written(&me, kept.unwrap_or_default(), "call-0", 6));
    assert!(m.marked().is_empty());
    // HELD: the call's paths were kept before it, and are recorded after it.
    let id = hold_claude(&mut m);
    m.note_writing(&me, "call-1", paths(&["/w/a.txt", "/real/a.txt"]), 10);
    m.note_writing(&me, "call-2", paths(&["/w/never.txt"]), 11);
    let kept = m.take_writing(&me, "call-1");
    assert_eq!(kept, Some(paths(&["/w/a.txt", "/real/a.txt"])));
    assert!(m.record_written(&me, kept.unwrap_or_default(), "call-1", 12));
    assert_eq!(m.marked().len(), 1);
    let marked = &m.marked()[0];
    assert_eq!(marked.paths, paths(&["/w/a.txt", "/real/a.txt"]));
    assert_eq!(marked.writer, me);
    assert_eq!(marked.writer_name, "claude");
    assert_eq!(
        marked
            .holders
            .iter()
            .map(|h| h.message.clone())
            .collect::<Vec<_>>(),
        vec![id.clone()]
    );
    assert_eq!((marked.since, marked.call.as_str()), (12, "call-1"));
    // The call that was cancelled has no after-call hook: nothing recorded for it, and its paths
    // are taken once only.
    assert_eq!(m.take_writing(&me, "call-1"), None);
    assert_eq!(m.marked().len(), 1);
    // A repeated after-call hook records nothing twice.
    assert!(m.record_written(&me, paths(&["/w/a.txt", "/real/a.txt"]), "call-1", 13));
    assert_eq!(m.marked().len(), 1);
}

// §5::44: a session that is not HELD and then reads the recorded file, or searches its
// folder, is HELD by the same message ids; a path that is not recorded holds nothing.
#[test]
fn reading_a_recorded_path_or_searching_its_folder_holds_by_the_recorded_ids() {
    let mut m = pair();
    let writer = claude("c1");
    let id = hold_claude(&mut m);
    assert!(m.record_written(&writer, paths(&["/w/dir/f.txt"]), "c", 5));
    let reader = codex("x1");
    // A read of another path, and a search of another folder, hold nothing.
    assert!(!m.hold_by_marked(&reader, &paths(&["/w/dir/g.txt"]), &paths(&["/w/other"])));
    assert!(!m.is_held(&reader));
    // A search of the file's folder, or of a folder above it, holds; so does a read of the file.
    for (reads, searches) in [
        (paths(&["/w/dir/f.txt"]), paths(&[])),
        (paths(&[]), paths(&["/w/dir"])),
        (paths(&[]), paths(&["/w"])),
        (paths(&[]), paths(&["/"])),
    ] {
        let mut copy = m.clone();
        assert!(
            copy.hold_by_marked(&reader, &reads, &searches),
            "{reads:?} {searches:?}"
        );
        assert_eq!(
            held_by(&copy, &reader),
            vec![id.clone()],
            "{reads:?} {searches:?}"
        );
        // The hold names the writer's sender, so the reader may reply to it (I1.4).
        assert!(copy.holding(&reader).iter().all(|h| h.from.is_some()));
    }
    // A folder that only shares a name prefix with the file's folder does not hold.
    assert!(!m.hold_by_marked(&reader, &paths(&[]), &paths(&["/w/di"])));
}

// §5::45: a recorded path never expires by itself: it holds a reader after the records
// of its writer's holding messages are gone, and after Lunchroom prunes; only the user clears it.
#[test]
fn a_recorded_path_outlives_the_messages_that_held_the_writer() {
    let mut m = pair();
    let writer = claude("c1");
    let id = hold_claude(&mut m);
    assert!(m.record_written(&writer, paths(&["/w/f.txt"]), "c", 5));
    // The messages' records are gone: removed from the state as a later program may drop them.
    let mut value = serde_json::to_value(&m).unwrap_or(Value::Null);
    value["messages"] = json!({});
    value["holds"] = json!({});
    let mut gone: Mail = serde_json::from_value(value).unwrap_or_default();
    assert!(gone.message(&id).is_none());
    // Time passes, and everything that can expire does.
    gone.prune(5 + 100 * KEEP_SECS, |_| false);
    let reader = codex("x1");
    assert!(gone.hold_by_marked(&reader, &paths(&["/w/f.txt"]), &[]));
    assert_eq!(held_by(&gone, &reader), vec![id.clone()]);
    // Only `lunchroom clear <path>` ends it; a path that is not recorded clears nothing.
    assert_eq!(gone.clear_marked(&paths(&["/w/other.txt"])), 0);
    assert_eq!(gone.marked().len(), 1);
    assert_eq!(gone.clear_marked(&paths(&["/w/f.txt"])), 1);
    assert!(gone.marked().is_empty());
    let late = claude("c2");
    assert!(!gone.hold_by_marked(&late, &paths(&["/w/f.txt"]), &[]));
}

// T1.8: the paths a call was judged to write are kept between its two hooks, and a call that never
// reaches the after-call hook does not stay for ever: by age, and by count.
#[test]
fn the_paths_kept_for_an_after_call_hook_are_bounded() {
    let mut m = pair();
    let me = claude("c1");
    m.note_writing(&me, "old", paths(&["/w/old"]), 0);
    m.note_writing(&me, "new", paths(&["/w/new"]), 100_000);
    m.prune(100_000, |_| false);
    assert_eq!(m.take_writing(&me, "old"), None);
    assert_eq!(m.take_writing(&me, "new"), Some(paths(&["/w/new"])));
    for n in 0..600 {
        m.note_writing(&me, &format!("c{n}"), paths(&["/w/x"]), n);
    }
    assert_eq!(m.take_writing(&me, "c0"), None);
    assert_eq!(m.take_writing(&me, "c599"), Some(paths(&["/w/x"])));
    // The same call reported twice (a harness that gives one id to a turn's calls) keeps both.
    m.note_writing(&me, "turn", paths(&["/w/1"]), 1);
    m.note_writing(&me, "turn", paths(&["/w/1", "/w/2"]), 2);
    assert_eq!(m.take_writing(&me, "turn"), Some(paths(&["/w/1", "/w/2"])));
    // A call with no paths keeps nothing.
    m.note_writing(&me, "none", Vec::new(), 3);
    assert_eq!(m.take_writing(&me, "none"), None);
}

// ---------------------------------------------------------------------------------------------
// T1.9: a session started under a HELD session.
// ---------------------------------------------------------------------------------------------

// §5::47, §5::48: a session first seen with a HELD session above it, or naming one by an
// inherited identity variable, is HELD by every message holding that session at that moment.
#[test]
fn a_session_started_by_a_held_session_is_held_by_its_messages() {
    let mut m = pair();
    let parent = claude("c1");
    let id = hold_claude(&mut m);
    // A second message arrives; the child takes both.
    let second = m
        .send(&codex("x1"), "codex", "claude", "more", 0)
        .map(|a| a.id)
        .unwrap_or_default();
    assert_eq!(m.confirm_at(&parent, &second, 2), Confirmation::Show);
    // The process the parent runs in is known to the store; the child is unknown.
    m.observe_process("100:7", std::path::Path::new("/w"), &parent);
    assert_eq!(m.sessions_seen_in("100:7"), vec![parent.clone()]);
    let child = claude("child");
    assert!(!m.has_seen(&child));
    assert!(m.inherit_holds(&child, &parent));
    let mut want = vec![id.clone(), second.clone()];
    want.sort();
    assert_eq!(held_by(&m, &child), want);
    // An unrelated session is not held, and a session is never held by itself.
    assert!(!m.inherit_holds(&claude("stranger"), &claude("not-held")));
    assert!(!m.is_held(&claude("stranger")));
    assert!(!m.inherit_holds(&parent, &parent));
    // Inheriting twice adds nothing twice.
    assert!(!m.inherit_holds(&child, &parent));
    // The holds are copies: the user typing in the parent leaves the child held.
    m.user_typed(&parent);
    assert_eq!(held_by(&m, &child), want);
}

// A1.3 "keeps the answer", D1.2: the answer `sudo -n -l` gave is kept in the second file, read
// back whole, replaced by the next, survives a 1.0.1 program's write of the state file, and a
// value that is not an answer reads as none kept without making the store unreadable.
#[test]
fn the_kept_sudo_answer_is_in_the_second_file_and_survives_a_1_0_1_write() {
    use crate::sudoers::Kept;
    let dir = scratch("sudo-answer");
    let store = Store::<Mail>::open(&dir);
    assert!(store.is_ok());
    let Ok(store) = store else { return };
    assert!(store.read().is_ok_and(|m| m.sudo_answer().is_none()));
    let first = Kept {
        read_at: 5,
        own: "User x may run:\n    (root) NOPASSWD: ALL\n".to_owned(),
        others: BTreeMap::from([("svc".to_owned(), "(root) ALL".to_owned())]),
    };
    assert!(store.update(|m| m.keep_sudo_answer(&first)).is_ok());
    let (main, side) = files(&dir);
    assert!(main.get("sudo_answer").is_none());
    assert_eq!(side["top"]["sudo_answer"]["own"], json!(first.own));
    assert_eq!(
        store.read().ok().and_then(|m| m.sudo_answer()),
        Some(first.clone())
    );
    // A 1.0.1 program rewrites the state file: the answer is still there.
    v101::rewrite(&dir, |m| m.approvals.clear());
    assert_eq!(store.read().ok().and_then(|m| m.sudo_answer()), Some(first));
    // The next answer replaces it.
    let second = Kept {
        read_at: 9,
        own: "none".to_owned(),
        others: BTreeMap::new(),
    };
    assert!(store.update(|m| m.keep_sudo_answer(&second)).is_ok());
    assert_eq!(
        store.read().ok().and_then(|m| m.sudo_answer()),
        Some(second)
    );
    // A field that is not an answer (an old or damaged value): none kept, the store still reads.
    let mut side: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("state.side.json")).unwrap_or_default())
            .unwrap_or(Value::Null);
    side["top"]["sudo_answer"] = json!(7);
    assert!(fs::write(dir.join("state.side.json"), side.to_string()).is_ok());
    let back = store.read();
    assert!(back.is_ok());
    assert_eq!(back.ok().and_then(|m| m.sudo_answer()), None);
    let _ = fs::remove_dir_all(&dir);
}
