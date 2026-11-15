//! The fields 1.1 adds, kept in a file of their own (spec §10 D1.2, `§10::40-44`).
//!
//! "The fields 1.1 adds (the notices, the time of a message's last failed attempt, and the other
//! fields 1.0.1 does not know) are kept in a file of their own beside the store file, read and
//! written under the same `state.flock` lock, which a 1.0.1 program takes and never rewrites the
//! second file under, so a 1.0.1 program still running loses none of them."
//!
//! 1.0.1 reads the state file into its own types, which keep no field they do not name, and writes
//! it whole. So the state file holds exactly the fields 1.0.1's types name (plus the layout number
//! `schema`, which 1.0.1 drops and so shows that it wrote last), and everything else is taken out of
//! it at every write and put back at every read: each top-level field 1.0.1 has no name for, and,
//! inside the lists and maps 1.0.1 has, each field of a message, a session record, a notice, an
//! approval and a request that it has no name for. A field of a kind later than this one lands in
//! the second file the same way, because the lists below name what is *known*, not what is moved.
//!
//! Lists have no ids to key the extra fields by (a notice, a request), and a 1.0.1 program drops
//! entries it consumes; the second file therefore carries, for each such entry, enough of the entry
//! itself to find it again (a notice's message and text; a request's session, tool, time and the
//! hash of its input), and an entry the state file no longer has is dropped at the next write.

use serde_json::{Map, Value};

use crate::jcs::input_hash;

/// The top-level fields of 1.0.1's `Mail`, plus the layout number. Read from the released 1.0.1
/// (tag `v1.0.1`, `aaa504c1`, `mail.rs`); the earlier list was `c59a74c`'s, which lacked
/// `codex_setting`, `codex_setting_open` and `titles`, so a 1.1 write took those out of the store
/// file and a 1.0.1 still running lost them (2026-11-14 real-world kit `mixed`).
const TOP: [&str; 26] = [
    "schema",
    "next_message",
    "seen",
    "processes",
    "sessions",
    "names",
    "receivers",
    "messages",
    "inboxes",
    "holds",
    "notices",
    "conversations",
    "next_approval",
    "approvals",
    "requests",
    "asked",
    "yolo",
    "harness_yolo",
    "unguarded_told",
    "transcripts_read",
    "guard_fingerprints",
    "process_members",
    "hold_mode",
    "codex_setting",
    "codex_setting_open",
    "titles",
];

/// A message's fields in 1.0.1.
const MESSAGE: [&str; 11] = [
    "id",
    "from",
    "from_name",
    "to",
    "to_name",
    "text",
    "sent_at",
    "status",
    "failed_attempts",
    "presented_at",
    "handed_over",
];

/// A session record's fields in 1.0.1.
const SESSION: [&str; 2] = ["process", "cwd"];

/// A notice's fields in 1.0.1.
const NOTICE: [&str; 2] = ["message", "text"];

/// An approval's fields in 1.0.1.
const APPROVAL: [&str; 7] = [
    "id",
    "session",
    "tool",
    "input_hash",
    "holding",
    "given_at",
    "used",
];

/// A request's fields in 1.0.1.
const REQUEST: [&str; 6] = ["session", "agent", "tool", "input", "rule", "at"];

/// The name under which [`join`] hands the second file's fields of the kinds the typed state does
/// not keep unknown fields for (a session record, a notice, an approval, a request) to the state, in
/// its own `extra` map, and [`split`] takes them back out: a field that no program knows survives a
/// read and a write (D1.2, `§10::41`). It never reaches either file under this name.
const CARRIED: &str = "__carried_side_fields";

/// The kinds [`CARRIED`] holds.
const CARRIED_KINDS: [&str; 4] = ["sessions", "notices", "approvals", "requests"];

/// Removes from `object` every field not in `known` and returns them.
fn take_unknown(object: &mut Map<String, Value>, known: &[&str]) -> Map<String, Value> {
    let names: Vec<String> = object
        .keys()
        .filter(|k| !known.contains(&k.as_str()))
        .cloned()
        .collect();
    let mut taken = Map::new();
    for name in names {
        if let Some(value) = object.remove(&name) {
            taken.insert(name, value);
        }
    }
    taken
}

/// Puts every field of `extra` into `object`.
fn put_back(object: &mut Map<String, Value>, extra: &Map<String, Value>) {
    for (name, value) in extra {
        object.insert(name.clone(), value.clone());
    }
}

/// Moves the fields of each entry of the map `main[field]` that 1.0.1 has no name for into
/// `side[field][<key>]`.
fn split_map(
    main: &mut Map<String, Value>,
    side: &mut Map<String, Value>,
    field: &str,
    known: &[&str],
) {
    let mut moved = Map::new();
    if let Some(Value::Object(entries)) = main.get_mut(field) {
        for (key, entry) in entries.iter_mut() {
            if let Value::Object(object) = entry {
                let taken = take_unknown(object, known);
                if !taken.is_empty() {
                    moved.insert(key.clone(), Value::Object(taken));
                }
            }
        }
    }
    if !moved.is_empty() {
        side.insert(field.to_owned(), Value::Object(moved));
    }
}

/// The reverse of [`split_map`].
fn join_map(main: &mut Map<String, Value>, side: &Map<String, Value>, field: &str) {
    let (Some(Value::Object(extra)), Some(Value::Object(entries))) =
        (side.get(field), main.get_mut(field))
    else {
        return;
    };
    for (key, extra_fields) in extra {
        if let (Some(Value::Object(entry)), Value::Object(fields)) =
            (entries.get_mut(key), extra_fields)
        {
            put_back(entry, fields);
        }
    }
}

/// How an entry of a list is found again: the fields of the entry that 1.0.1 has, as they stand.
type Finder = fn(&Map<String, Value>) -> Value;

/// A notice is found by its message and its text.
fn notice_find(object: &Map<String, Value>) -> Value {
    Value::Array(vec![
        object.get("message").cloned().unwrap_or(Value::Null),
        object.get("text").cloned().unwrap_or(Value::Null),
    ])
}

/// A request is found by its session, tool, time and the hash of its input.
fn request_find(object: &Map<String, Value>) -> Value {
    Value::Array(vec![
        object.get("session").cloned().unwrap_or(Value::Null),
        object.get("tool").cloned().unwrap_or(Value::Null),
        object.get("at").cloned().unwrap_or(Value::Null),
        Value::String(input_hash(object.get("input").unwrap_or(&Value::Null))),
    ])
}

/// An approval is found by its id.
fn approval_find(object: &Map<String, Value>) -> Value {
    object.get("id").cloned().unwrap_or(Value::Null)
}

/// Moves the fields 1.0.1 has no name for out of each entry of the list `list` into a list of
/// `{find, fields}` records, one for each entry that had any.
fn split_list(list: &mut Value, known: &[&str], find: Finder) -> Vec<Value> {
    let mut moved = Vec::new();
    if let Value::Array(entries) = list {
        for entry in entries.iter_mut() {
            if let Value::Object(object) = entry {
                let taken = take_unknown(object, known);
                if !taken.is_empty() {
                    let mut record = Map::new();
                    record.insert("find".to_owned(), find(object));
                    record.insert("fields".to_owned(), Value::Object(taken));
                    moved.push(Value::Object(record));
                }
            }
        }
    }
    moved
}

/// The reverse of [`split_list`]: each record's fields go to the first entry of `list` it finds
/// that no earlier record has taken.
fn join_list(list: &mut Value, records: &[Value], find: Finder) {
    let Value::Array(entries) = list else { return };
    let mut taken = vec![false; entries.len()];
    for record in records {
        let (Some(wanted), Some(Value::Object(fields))) =
            (record.get("find"), record.get("fields"))
        else {
            continue;
        };
        for (k, entry) in entries.iter_mut().enumerate() {
            if taken[k] {
                continue;
            }
            if let Value::Object(object) = entry
                && find(object) == *wanted
            {
                put_back(object, fields);
                taken[k] = true;
                break;
            }
        }
    }
}

/// Takes the fields 1.1 adds out of `main` and returns them (`Null` when there are none).
pub(super) fn split(main: &mut Value) -> Value {
    let Value::Object(top) = main else {
        return Value::Null;
    };
    let mut side = Map::new();
    let carried = match top.remove(CARRIED) {
        Some(Value::Object(carried)) => carried,
        _ => Map::new(),
    };
    let moved = take_unknown(top, &TOP);
    if !moved.is_empty() {
        side.insert("top".to_owned(), Value::Object(moved));
    }
    split_map(top, &mut side, "messages", &MESSAGE);
    split_map(top, &mut side, "sessions", &SESSION);
    // Lists of notices by session, and the approvals and requests.
    if let Some(Value::Object(by_session)) = top.get_mut("notices") {
        let mut moved = Map::new();
        for (key, list) in by_session.iter_mut() {
            let records = split_list(list, &NOTICE, notice_find);
            if !records.is_empty() {
                moved.insert(key.clone(), Value::Array(records));
            }
        }
        if !moved.is_empty() {
            side.insert("notices".to_owned(), Value::Object(moved));
        }
    }
    for (field, known, find) in [
        ("approvals", &APPROVAL[..], approval_find as Finder),
        ("requests", &REQUEST[..], request_find as Finder),
    ] {
        if let Some(list) = top.get_mut(field) {
            let records = split_list(list, known, find);
            if !records.is_empty() {
                side.insert(field.to_owned(), Value::Array(records));
            }
        }
    }
    carry_over(top, &mut side, &carried);
    if side.is_empty() {
        Value::Null
    } else {
        Value::Object(side)
    }
}

/// Merges what [`join`] carried into the second file being written: the fields of an entry that the
/// typed state does not know stay, those it does are the typed state's (it wins), and an entry the
/// state no longer has is dropped.
fn carry_over(
    top: &Map<String, Value>,
    side: &mut Map<String, Value>,
    carried: &Map<String, Value>,
) {
    for kind in CARRIED_KINDS {
        let Some(old) = carried.get(kind) else {
            continue;
        };
        let merged = match (kind, old) {
            ("sessions", Value::Object(old)) => {
                let current = top.get(kind).and_then(Value::as_object);
                let mut out = side
                    .get(kind)
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                for (key, fields) in old {
                    if current.is_some_and(|c| c.contains_key(key))
                        && let Value::Object(fields) = fields
                    {
                        let entry = out
                            .entry(key.clone())
                            .or_insert_with(|| Value::Object(Map::new()));
                        if let Value::Object(entry) = entry {
                            for (name, value) in fields {
                                entry.entry(name.clone()).or_insert_with(|| value.clone());
                            }
                        }
                    }
                }
                Value::Object(out)
            }
            ("notices", Value::Object(old)) => {
                let current = top.get(kind).and_then(Value::as_object);
                let mut out = side
                    .get(kind)
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                for (key, records) in old {
                    let (Some(list), Value::Array(records)) =
                        (current.and_then(|c| c.get(key)), records)
                    else {
                        continue;
                    };
                    let mut now = out
                        .get(key)
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    merge_records(&mut now, records, list, notice_find);
                    out.insert(key.clone(), Value::Array(now));
                }
                Value::Object(out)
            }
            ("approvals" | "requests", Value::Array(old)) => {
                let find: Finder = if kind == "approvals" {
                    approval_find
                } else {
                    request_find
                };
                let mut now = side
                    .get(kind)
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if let Some(list) = top.get(kind) {
                    merge_records(&mut now, old, list, find);
                }
                Value::Array(now)
            }
            _ => continue,
        };
        let empty = match &merged {
            Value::Object(o) => o.is_empty(),
            Value::Array(a) => a.is_empty(),
            _ => true,
        };
        if empty {
            side.remove(kind);
        } else {
            side.insert(kind.to_owned(), merged);
        }
    }
}

/// Adds to `now` (records of fields the typed state wrote) the fields of the `carried` records
/// whose entry is still in `list`, without replacing a field `now` has.
fn merge_records(now: &mut Vec<Value>, carried: &[Value], list: &Value, find: Finder) {
    let Value::Array(entries) = list else { return };
    let finds: Vec<Value> = entries
        .iter()
        .filter_map(|e| e.as_object().map(find))
        .collect();
    for record in carried {
        let (Some(wanted), Some(Value::Object(fields))) =
            (record.get("find"), record.get("fields"))
        else {
            continue;
        };
        if !finds.contains(wanted) {
            continue;
        }
        let slot = now.iter_mut().find(|r| r.get("find") == Some(wanted));
        if let Some(Value::Object(r)) = slot {
            if let Some(Value::Object(have)) = r.get_mut("fields") {
                for (name, value) in fields {
                    have.entry(name.clone()).or_insert_with(|| value.clone());
                }
            }
        } else {
            let mut r = Map::new();
            r.insert("find".to_owned(), wanted.clone());
            r.insert("fields".to_owned(), Value::Object(fields.clone()));
            now.push(Value::Object(r));
        }
    }
}

/// Puts the fields of the second file back into `main`. A field the state file still holds (a store
/// written before the second file existed) is replaced by the second file's.
pub(super) fn join(main: &mut Value, side: &Value) {
    let (Value::Object(top), Value::Object(side)) = (main, side) else {
        return;
    };
    if let Some(Value::Object(extra)) = side.get("top") {
        for (name, value) in extra {
            if name != "schema" {
                top.insert(name.clone(), value.clone());
            }
        }
    }
    join_map(top, side, "messages");
    join_map(top, side, "sessions");
    if let (Some(Value::Object(extra)), Some(Value::Object(by_session))) =
        (side.get("notices"), top.get_mut("notices"))
    {
        for (key, records) in extra {
            if let (Some(list), Value::Array(records)) = (by_session.get_mut(key), records) {
                join_list(list, records, notice_find);
            }
        }
    }
    for (field, find) in [
        ("approvals", approval_find as Finder),
        ("requests", request_find as Finder),
    ] {
        if let (Some(Value::Array(records)), Some(list)) = (side.get(field), top.get_mut(field)) {
            join_list(list, records, find);
        }
    }
    let mut carried = Map::new();
    for kind in CARRIED_KINDS {
        if let Some(fields) = side.get(kind) {
            carried.insert(kind.to_owned(), fields.clone());
        }
    }
    if !carried.is_empty() {
        top.insert(CARRIED.to_owned(), Value::Object(carried));
    }
}
