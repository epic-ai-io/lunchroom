//! Approvals for the Agent Execution Controller (§17 C2 item 9).
//!
//! The controller approves each phase of a run from a record bound to one run, one phase and the
//! run's frozen plan hash. This file reads the approval request the controller wrote, checks it,
//! builds the record the controller's `make_record` builds, signs its canonical JSON with the
//! console's Ed25519 key and writes the approval under a temporary name, then renames it into
//! place. Asking the user (`y/N`, then the passphrase, for every phase) is the console's screen;
//! nothing here writes a byte until it is called with a request the user was shown.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signer as _, SigningKey};
use rustix::fs::{Mode, OFlags};
use serde_json::{Map, Value};

use crate::console::model::{Listing, ReqView};
use crate::install::console_keys;

/// The most bytes of a request file that are read: a request is three short fields.
const REQUEST_LIMIT: u64 = 4_096;

/// A request older than this many seconds is not approved: the controller refuses a record for a
/// request that old (§17 item 9: "or one older than 10 minutes"). The request holds no time of its
/// own (exactly three fields), so its age is the time its file was last written.
pub const REQUEST_MAX_AGE_SECS: u64 = 600;

/// The file in the delivery folder that keeps the nonces used, so that none is used twice.
pub const NONCES_FILE: &str = "used-nonces";

/// A nonce is kept for a day, far past the 10 minutes in which the controller takes a record.
const NONCE_KEPT_SECS: u64 = 86_400;

/// `run-` followed by 32 lowercase hexadecimal digits.
#[must_use]
pub fn valid_run_id(text: &str) -> bool {
    text.strip_prefix("run-")
        .is_some_and(|hex| hex.len() == 32 && lower_hex(hex))
}

/// `^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$`: a final line feed does not match (as Python's `$` would
/// let it).
#[must_use]
pub fn valid_phase(text: &str) -> bool {
    let bytes = text.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// 64 lowercase hexadecimal digits.
#[must_use]
pub fn valid_hash(text: &str) -> bool {
    text.len() == 64 && lower_hex(text)
}

fn lower_hex(text: &str) -> bool {
    text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The file of a request or an approval: `<run-id>.<phase>.json`.
#[must_use]
pub fn file_name(run_id: &str, phase: &str) -> String {
    format!("{run_id}.{phase}.json")
}

/// Whether `run_id` and `phase` are of the form the key takes.
///
/// # Errors
/// What is wrong with the value.
pub fn check_key(run_id: &str, phase: &str) -> Result<(), String> {
    if !valid_run_id(run_id) {
        return Err("a run id is `run-` followed by 32 lowercase hexadecimal digits".to_owned());
    }
    if !valid_phase(phase) {
        return Err(
            "a phase is 1 to 64 letters, digits, `_`, `.` or `-`, starting with a \
                    letter or digit"
                .to_owned(),
        );
    }
    Ok(())
}

/// The text of the file `name` in `dir`: a regular file, not a link, at most [`REQUEST_LIMIT`]
/// bytes.
fn read_plain(dir: &Path, name: &str) -> Result<(String, u64), String> {
    let path = dir.join(name);
    let fd = rustix::fs::open(
        &path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|e| format!("{} cannot be read: {e}", path.display()))?;
    let file = std::fs::File::from(fd);
    let meta = file
        .metadata()
        .map_err(|e| format!("{} cannot be read: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let mut text = Vec::new();
    file.take(REQUEST_LIMIT + 1)
        .read_to_end(&mut text)
        .map_err(|e| format!("{} cannot be read: {e}", path.display()))?;
    if text.len() as u64 > REQUEST_LIMIT {
        return Err(format!("{} is too long for a request", path.display()));
    }
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let text =
        String::from_utf8(text).map_err(|_| format!("{} is not UTF-8 text", path.display()))?;
    Ok((text, modified))
}

/// The request in `text`: a JSON object with exactly `run_id`, `phase` and `manifest_hash`, each a
/// string, the last 64 lowercase hexadecimal digits, the first two of the form the key takes.
///
/// # Errors
/// Why it is not a request.
pub fn parse_request(text: &str) -> Result<ReqView, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("it is not JSON: {e}"))?;
    let Value::Object(map) = value else {
        return Err("it is not a JSON object".to_owned());
    };
    if map.len() != 3
        || ["run_id", "phase", "manifest_hash"]
            .iter()
            .any(|k| !map.contains_key(*k))
    {
        return Err(
            "it must hold exactly the fields `run_id`, `phase` and `manifest_hash`".to_owned(),
        );
    }
    let text_of = |key: &str| -> Result<String, String> {
        map.get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("`{key}` is not a string"))
    };
    let (run_id, phase, manifest_hash) = (
        text_of("run_id")?,
        text_of("phase")?,
        text_of("manifest_hash")?,
    );
    check_key(&run_id, &phase)?;
    if !valid_hash(&manifest_hash) {
        return Err("`manifest_hash` is not 64 lowercase hexadecimal digits".to_owned());
    }
    Ok(ReqView {
        run_id,
        phase,
        manifest_hash,
    })
}

/// The approval request for `run_id` and `phase` in `requests`, read and checked: it is refused
/// when the key is malformed, the file is missing or unreadable, holds any other field or form, or
/// names another run or phase than the one typed.
///
/// # Errors
/// Why the approval is refused.
pub fn read_request(requests: &Path, run_id: &str, phase: &str) -> Result<ReqView, String> {
    load(requests, run_id, phase).map(|(request, _)| request)
}

/// [`read_request`] that also refuses a request older than [`REQUEST_MAX_AGE_SECS`] at `now`.
///
/// # Errors
/// Why the approval is refused.
pub fn read_fresh(requests: &Path, run_id: &str, phase: &str, now: u64) -> Result<ReqView, String> {
    let (request, modified) = load(requests, run_id, phase)?;
    too_old(modified, now)?;
    Ok(request)
}

/// Refuses a request written more than [`REQUEST_MAX_AGE_SECS`] before `now` (one written at
/// exactly that age is the last that is taken).
fn too_old(modified: u64, now: u64) -> Result<(), String> {
    let age = now.saturating_sub(modified);
    if age > REQUEST_MAX_AGE_SECS {
        return Err(format!(
            "the approval request is {age} seconds old, older than 10 minutes: the controller \
             refuses a record for it, so ask it for a new request"
        ));
    }
    Ok(())
}

/// The request for `run_id` and `phase`, and when its file was last written.
fn load(requests: &Path, run_id: &str, phase: &str) -> Result<(ReqView, u64), String> {
    check_key(run_id, phase)?;
    let name = file_name(run_id, phase);
    let (text, modified) = read_plain(requests, &name)
        .map_err(|why| format!("there is no usable approval request: {why}"))?;
    let request = parse_request(&text)
        .map_err(|why| format!("the approval request {name} is not accepted: {why}"))?;
    if request.run_id != run_id || request.phase != phase {
        return Err(format!(
            "the approval request {name} names run {} and phase {}, not the ones typed",
            request.run_id, request.phase
        ));
    }
    Ok((request, modified))
}

/// The requests in `requests`, each with its run id, phase and plan hash; the files that are not
/// a valid request are counted, not listed. In the order of their names.
#[must_use]
pub fn list_requests(requests: &Path) -> Listing {
    let entries = match std::fs::read_dir(requests) {
        Ok(entries) => entries,
        Err(e) => {
            return Listing {
                error: Some(format!(
                    "requests/ cannot be listed: {e} ({})",
                    requests.display()
                )),
                ..Listing::default()
            };
        }
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    let mut listing = Listing::default();
    for name in names {
        let shown = name.strip_suffix(".json").and_then(|stem| {
            let (run_id, phase) = stem.split_at_checked(36)?;
            Some((run_id.to_owned(), phase.strip_prefix('.')?.to_owned()))
        });
        match shown.map(|(run_id, phase)| read_request(requests, &run_id, &phase)) {
            Some(Ok(request)) => listing.requests.push(request),
            _ => listing.ignored += 1,
        }
    }
    listing
}

/// `now` (seconds since the Unix epoch) in UTC as `YYYY-MM-DDTHH:MM:SSZ`.
#[must_use]
pub fn issued_at(now: u64) -> String {
    let days = i64::try_from(now / 86_400).unwrap_or(0);
    let secs = now % 86_400;
    // Days since 1970-01-01 to a civil date (proleptic Gregorian).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year += 1;
    }
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3_600,
        secs % 3_600 / 60,
        secs % 60
    )
}

/// The record the controller's `make_record` builds: `run_id`, `phase`, `manifest_hash`,
/// `operator`, `source` (`"console"`), `nonce` and `issued_at`.
#[must_use]
pub fn make_record(
    request: &ReqView,
    operator: &str,
    nonce: [u8; 16],
    now: u64,
) -> BTreeMap<String, String> {
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(32);
    for b in nonce {
        let _ = write!(hex, "{b:02x}");
    }
    BTreeMap::from([
        ("run_id".to_owned(), request.run_id.clone()),
        ("phase".to_owned(), request.phase.clone()),
        ("manifest_hash".to_owned(), request.manifest_hash.clone()),
        ("operator".to_owned(), operator.to_owned()),
        ("source".to_owned(), "console".to_owned()),
        ("nonce".to_owned(), hex),
        ("issued_at".to_owned(), issued_at(now)),
    ])
}

/// The record's canonical JSON, the bytes of the controller's `canonical`:
/// `json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")`.
/// Keys are sorted by code point (a `BTreeMap` of `String` sorts by UTF-8 bytes, which is the same
/// order), no space follows `,` or `:`, and a character outside ASCII is written as it is.
#[must_use]
pub fn canonical(record: &BTreeMap<String, String>) -> Vec<u8> {
    // `serde_json`'s compact writer escapes `"`, `\`, the C0 controls (`\b \f \n \r \t`, the others
    // as `\u00xx` in lowercase) and nothing else: the escapes Python's `json.dumps` makes with
    // `ensure_ascii=False`. The test holds the bytes of a record with every such character.
    serde_json::to_vec(record).unwrap_or_default()
}

/// The approval file's bytes: `{"record": ..., "signature": <64 bytes, standard base64>}`.
fn approval_bytes(record: &BTreeMap<String, String>, signature: &[u8; 64]) -> Vec<u8> {
    let mut map = Map::new();
    map.insert(
        "record".to_owned(),
        Value::Object(
            record
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect(),
        ),
    );
    map.insert(
        "signature".to_owned(),
        Value::String(console_keys::b64_encode(signature)),
    );
    let mut bytes = serde_json::to_vec(&Value::Object(map)).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

/// What approving needs besides the request.
pub struct Ctl<'a> {
    /// `/var/lib/lunchroom-approvals/<uid>/requests`.
    pub requests: PathBuf,
    /// `/var/lib/lunchroom-approvals/<uid>`.
    pub approvals: PathBuf,
    /// `/etc/lunchroom/<uid>.console.key`.
    pub key: PathBuf,
    /// The developer's login name.
    pub operator: String,
    /// 16 bytes of the system's random source.
    pub random: &'a mut dyn FnMut() -> Result<[u8; 16], String>,
    /// The user id of the system user `lunchroom`, which owns each approval file (§17.9).
    pub owner: u32,
    /// Makes the file at the path owned by the user id; the system's `chown` ([`system_chown`]),
    /// or a test's own.
    pub chown: fn(&std::fs::File, u32) -> std::io::Result<()>,
}

/// Makes the open file owned by user `uid` (`fchown`: by the descriptor, never by a path another
/// process could have swapped), leaving its group (the delivery folder's setgid gives it the
/// developer's group).
///
/// # Errors
/// Why the owner could not be set.
pub fn system_chown(file: &std::fs::File, uid: u32) -> std::io::Result<()> {
    rustix::fs::fchown(file, Some(rustix::fs::Uid::from_raw(uid)), None).map_err(Into::into)
}

/// 16 bytes from `/dev/urandom`.
///
/// # Errors
/// Why the system's random source could not be read.
pub fn system_random() -> Result<[u8; 16], String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| format!("random bytes cannot be read: {e}"))?;
    Ok(bytes)
}

/// The key's seed in the file at `path`.
fn read_seed(path: &Path) -> Result<[u8; 32], String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("the console's key {} cannot be read: {e}", path.display()))?;
    console_keys::seed_from(&text).ok_or_else(|| {
        format!(
            "the console's key {} is not one line of base64 holding 32 bytes",
            path.display()
        )
    })
}

/// Draws a nonce from the system's random source that has not been used, and keeps it as used
/// (§17 item 9: the controller refuses a reused nonce, so none is made twice). The check and the
/// record are one step under an exclusive lock on [`NONCES_FILE`], so two approvals at once cannot
/// both take one. A nonce kept for more than a day is forgotten: no record that old is taken.
///
/// # Errors
/// Why no unused nonce was had: the random source failed, the file could not be used, or the
/// source gave a used nonce three times.
fn fresh_nonce(ctl: &mut Ctl<'_>, now: u64) -> Result<[u8; 16], String> {
    for _ in 0..3 {
        let nonce = (ctl.random)()?;
        if claim_nonce(&ctl.approvals, &hex_of(&nonce), now)? {
            return Ok(nonce);
        }
    }
    Err("the system's random source gave a nonce that was already used".to_owned())
}

fn hex_of(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// Records `nonce` as used at `now`, unless it already is (then `false`).
fn claim_nonce(dir: &Path, nonce: &str, now: u64) -> Result<bool, String> {
    use std::io::{Seek as _, SeekFrom};
    let path = dir.join(NONCES_FILE);
    let unusable = |e: &dyn std::fmt::Display| format!("{} cannot be used: {e}", path.display());
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o640)
        .open(&path)
        .map_err(|e| unusable(&e))?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
        .map_err(|e| unusable(&e))?;
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(|e| unusable(&e))?;
    let mut kept: Vec<&str> = Vec::new();
    for line in text.lines() {
        let Some((at, used)) = line.split_once(' ') else {
            continue;
        };
        let Ok(at) = at.parse::<u64>() else {
            continue;
        };
        if used == nonce && now.saturating_sub(at) <= NONCE_KEPT_SECS {
            return Ok(false);
        }
        if now.saturating_sub(at) <= NONCE_KEPT_SECS {
            kept.push(line);
        }
    }
    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&now.to_string());
    out.push(' ');
    out.push_str(nonce);
    out.push('\n');
    file.set_len(0).map_err(|e| unusable(&e))?;
    file.seek(SeekFrom::Start(0)).map_err(|e| unusable(&e))?;
    file.write_all(out.as_bytes()).map_err(|e| unusable(&e))?;
    file.sync_all().map_err(|e| unusable(&e))?;
    // The lock is released when the file is closed, here.
    Ok(true)
}

/// Approves `shown`, which the user was shown and confirmed: the request is read again and must
/// still be the one shown (an agent runs as the developer and can rewrite it), the record is
/// built and signed, and the approval is written under a temporary name in the delivery folder
/// and renamed into place, replacing an earlier approval of the same run and phase. Nothing is
/// written when anything before the write fails.
///
/// # Errors
/// Why nothing was approved.
pub fn approve(ctl: &mut Ctl<'_>, shown: &ReqView, now: u64) -> Result<PathBuf, String> {
    let (current, modified) = load(&ctl.requests, &shown.run_id, &shown.phase)?;
    if &current != shown {
        return Err("the approval request changed after it was shown: look at it again".to_owned());
    }
    too_old(modified, now)?;
    let seed = read_seed(&ctl.key)?;
    let nonce = fresh_nonce(ctl, now)?;
    let record = make_record(shown, &ctl.operator, nonce, now);
    let signature = SigningKey::from_bytes(&seed)
        .sign(&canonical(&record))
        .to_bytes();
    let bytes = approval_bytes(&record, &signature);
    let name = file_name(&shown.run_id, &shown.phase);
    let target = ctl.approvals.join(&name);
    let nonce_hex = record.get("nonce").cloned().unwrap_or_default();
    let temp = ctl.approvals.join(format!("{name}.tmp-{nonce_hex}"));
    // The temporary file is removed only when this call made it: a name that was already there
    // belongs to someone else.
    let mut made = false;
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o640)
            .open(&temp)?;
        made = true;
        // Everything below is by the open descriptor, not by the path.
        file.set_permissions(std::fs::Permissions::from_mode(0o640))?;
        // Owned by user `lunchroom` before it holds a byte, and before it has its name.
        (ctl.chown)(&file, ctl.owner)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, &target)
    })();
    if let Err(e) = written {
        if made {
            let _ = std::fs::remove_file(&temp);
        }
        return Err(format!("the approval could not be written: {e}"));
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        clippy::assert_is_empty,
        clippy::too_many_lines,
        clippy::many_single_char_names,
        clippy::similar_names
    )] // test code
    use std::fmt::Write as _;
    use std::os::unix::fs::MetadataExt as _;

    use ed25519_dalek::{Signature, Verifier as _};

    use super::*;

    const RUN: &str = "run-0123456789abcdef0123456789abcdef";
    const HASH: &str = "4b1e9c07aa11bb22cc33dd44ee55ff66778899aabbccddeeff00112233445566";

    fn scratch(name: &str) -> PathBuf {
        let dir = crate::test_target()
            .join("controller-unit")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("requests")).unwrap();
        dir
    }

    fn request_json(run: &str, phase: &str, hash: &str) -> String {
        format!(r#"{{"run_id":"{run}","phase":"{phase}","manifest_hash":"{hash}"}}"#)
    }

    fn put_request(dir: &Path, run: &str, phase: &str, text: &str) {
        std::fs::write(dir.join("requests").join(file_name(run, phase)), text).unwrap();
    }

    fn view() -> ReqView {
        ReqView {
            run_id: RUN.to_owned(),
            phase: "deploy-prod".to_owned(),
            manifest_hash: HASH.to_owned(),
        }
    }

    const SEED: [u8; 32] = [9; 32];

    fn ctl<'a>(dir: &Path, random: &'a mut dyn FnMut() -> Result<[u8; 16], String>) -> Ctl<'a> {
        let key = dir.join("key");
        std::fs::write(&key, console_keys::private_text(&SEED)).unwrap();
        Ctl {
            requests: dir.join("requests"),
            approvals: dir.to_path_buf(),
            key,
            operator: "alice".to_owned(),
            random,
            owner: 998,
            chown: |_, _| Ok(()),
        }
    }

    thread_local! {
        /// The approval's final name, and what each chown saw: the user id, whether the final name
        /// existed, how many bytes the file held, and its mode.
        static TARGET: std::cell::RefCell<PathBuf> = const { std::cell::RefCell::new(PathBuf::new()) };
        static CHOWNED: std::cell::RefCell<Vec<(u32, bool, u64, u32)>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    /// A `chown` that records the user id and what the file and its final name were then.
    #[allow(clippy::unnecessary_wraps)] // the signature of a chown
    fn recording_chown(file: &std::fs::File, uid: u32) -> std::io::Result<()> {
        let meta = file.metadata()?;
        let named = TARGET.with(|t| t.borrow().exists());
        CHOWNED.with(|c| {
            c.borrow_mut()
                .push((uid, named, meta.len(), meta.mode() & 0o7777));
        });
        Ok(())
    }

    fn failing_chown(_: &std::fs::File, _: u32) -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
    }

    // §17::157: the approval file is given owner `lunchroom` (the system's chown, through the
    // injected one), before it holds a byte and before it has its name; the mode is 0640.
    #[test]
    fn the_approval_file_is_given_to_lunchroom_before_it_has_its_name() {
        let dir = scratch("owner");
        put_request(
            &dir,
            RUN,
            "deploy-prod",
            &request_json(RUN, "deploy-prod", HASH),
        );
        CHOWNED.with(|c| c.borrow_mut().clear());
        TARGET.with(|t| *t.borrow_mut() = dir.join(file_name(RUN, "deploy-prod")));
        let mut rnd = || Ok([4; 16]);
        let mut c = ctl(&dir, &mut rnd);
        c.owner = 4242;
        c.chown = recording_chown;
        let path = approve(&mut c, &view(), 7).unwrap();
        let calls = CHOWNED.with(|c| c.borrow().clone());
        // One call: with the user id, before the file had its name, empty, with the mode 0640.
        assert_eq!(calls, [(4242, false, 0, 0o640)]);
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o7777, 0o640);
    }

    // §17::157: where the owner cannot be set nothing is approved: no file, no temporary file.
    #[test]
    fn an_approval_that_cannot_be_given_to_lunchroom_writes_nothing() {
        let dir = scratch("owner-fails");
        put_request(
            &dir,
            RUN,
            "deploy-prod",
            &request_json(RUN, "deploy-prod", HASH),
        );
        let mut rnd = || Ok([4; 16]);
        let mut c = ctl(&dir, &mut rnd);
        c.chown = failing_chown;
        assert!(
            approve(&mut c, &view(), 7)
                .unwrap_err()
                .contains("could not be written")
        );
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.starts_with("run-"))
            .collect();
        assert!(names.is_empty(), "{names:?}");
    }

    fn set_age(path: &Path, modified: u64) {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(modified))
            .unwrap();
    }

    // §17::9, "or one older than 10 minutes": a request written 600 seconds before the approval is
    // the last that is taken; one second more and it is refused, with nothing written, at the
    // approval and at the screen's load.
    #[test]
    fn a_request_older_than_ten_minutes_is_refused_at_the_boundary() {
        let now = 1_800_000_000;
        for (age, taken) in [
            (0, true),
            (599, true),
            (600, true),
            (601, false),
            (86_400, false),
        ] {
            let dir = scratch(&format!("age-{age}"));
            put_request(
                &dir,
                RUN,
                "deploy-prod",
                &request_json(RUN, "deploy-prod", HASH),
            );
            set_age(
                &dir.join("requests").join(file_name(RUN, "deploy-prod")),
                now - age,
            );
            let mut rnd = || Ok([6; 16]);
            let mut c = ctl(&dir, &mut rnd);
            let fresh = read_fresh(&dir.join("requests"), RUN, "deploy-prod", now);
            let done = approve(&mut c, &view(), now);
            assert_eq!(fresh.is_ok(), taken, "load at {age} seconds: {fresh:?}");
            assert_eq!(done.is_ok(), taken, "approval at {age} seconds: {done:?}");
            if !taken {
                assert!(done.unwrap_err().contains("older than 10 minutes"));
                assert!(!dir.join(file_name(RUN, "deploy-prod")).exists());
                assert!(!dir.join(NONCES_FILE).exists(), "no nonce was used up");
            }
        }
        // A request stamped in the future (a clock stepped back) is not old.
        let dir = scratch("age-future");
        put_request(
            &dir,
            RUN,
            "deploy-prod",
            &request_json(RUN, "deploy-prod", HASH),
        );
        set_age(
            &dir.join("requests").join(file_name(RUN, "deploy-prod")),
            now + 5_000,
        );
        let mut rnd = || Ok([6; 16]);
        assert!(approve(&mut ctl(&dir, &mut rnd), &view(), now).is_ok());
    }

    // §17::9, "a reused nonce": a nonce the random source gives twice is used once; the second
    // approval draws again, and a source that only repeats approves nothing the second time.
    #[test]
    fn a_nonce_is_not_used_twice() {
        let dir = scratch("nonce-reuse");
        put_request(
            &dir,
            RUN,
            "deploy-prod",
            &request_json(RUN, "deploy-prod", HASH),
        );
        let mut same = || Ok([7; 16]);
        let first = approve(&mut ctl(&dir, &mut same), &view(), 100).unwrap();
        let kept = std::fs::read(&first).unwrap();
        let again = approve(&mut ctl(&dir, &mut same), &view(), 160).unwrap_err();
        assert!(again.contains("nonce"), "{again}");
        assert_eq!(
            std::fs::read(&first).unwrap(),
            kept,
            "the first approval stands as it was"
        );
        // A source that repeats once and then gives a new one is drawn from again.
        let calls = std::cell::Cell::new(0u8);
        let mut repeats = || {
            calls.set(calls.get() + 1);
            Ok(if calls.get() == 1 { [7; 16] } else { [8; 16] })
        };
        let later = approve(&mut ctl(&dir, &mut repeats), &view(), 220).unwrap();
        let file: Value = serde_json::from_slice(&std::fs::read(later).unwrap()).unwrap();
        assert_eq!(file["record"]["nonce"], "08".repeat(16));
        assert_eq!(calls.get(), 2);
        // The file holds each nonce once, with the time.
        let text = std::fs::read_to_string(dir.join(NONCES_FILE)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines,
            [
                format!("100 {}", "07".repeat(16)),
                format!("220 {}", "08".repeat(16))
            ]
        );
        assert_eq!(
            std::fs::metadata(dir.join(NONCES_FILE)).unwrap().mode() & 0o7777,
            0o640
        );
    }

    // Many approvals at once with one nonce: exactly one is made.
    #[test]
    fn a_nonce_used_by_two_approvals_at_once_is_used_by_one() {
        let dir = scratch("nonce-race");
        put_request(
            &dir,
            RUN,
            "deploy-prod",
            &request_json(RUN, "deploy-prod", HASH),
        );
        let key = dir.join("key");
        std::fs::write(&key, console_keys::private_text(&SEED)).unwrap();
        let wins = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        let mut rnd = || Ok([9; 16]);
                        let mut c = Ctl {
                            requests: dir.join("requests"),
                            approvals: dir.clone(),
                            key: key.clone(),
                            operator: "alice".to_owned(),
                            random: &mut rnd,
                            owner: 998,
                            chown: |_, _| Ok(()),
                        };
                        approve(&mut c, &view(), 100).is_ok()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .filter(|ok| *ok)
                .count()
        });
        assert_eq!(wins, 1);
        let text = std::fs::read_to_string(dir.join(NONCES_FILE)).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
    }

    // A nonce kept for more than a day is forgotten: no record that old is taken; the file stays
    // small.
    #[test]
    fn old_nonces_are_forgotten() {
        let dir = scratch("nonce-old");
        put_request(
            &dir,
            RUN,
            "deploy-prod",
            &request_json(RUN, "deploy-prod", HASH),
        );
        let now = 1_000_000;
        std::fs::write(
            dir.join(NONCES_FILE),
            format!(
                "1 {}\n{} {}\nnot a line\n",
                "05".repeat(16),
                now - 10,
                "04".repeat(16)
            ),
        )
        .unwrap();
        let mut rnd = || Ok([5; 16]);
        approve(&mut ctl(&dir, &mut rnd), &view(), now).unwrap();
        let text = std::fs::read_to_string(dir.join(NONCES_FILE)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines,
            [
                format!("{} {}", now - 10, "04".repeat(16)),
                format!("{now} {}", "05".repeat(16))
            ]
        );
    }

    // A name that was already there is not this call's to remove: the approval fails and the other
    // file stays.
    #[test]
    fn an_existing_temporary_name_is_left_alone() {
        let dir = scratch("temp-exists");
        put_request(
            &dir,
            RUN,
            "deploy-prod",
            &request_json(RUN, "deploy-prod", HASH),
        );
        let temp = dir.join(format!(
            "{}.tmp-{}",
            file_name(RUN, "deploy-prod"),
            "05".repeat(16)
        ));
        std::fs::write(&temp, "someone else's").unwrap();
        let mut rnd = || Ok([5; 16]);
        let mut c = ctl(&dir, &mut rnd);
        assert!(approve(&mut c, &view(), 7).is_err());
        assert_eq!(std::fs::read_to_string(&temp).unwrap(), "someone else's");
        assert!(!dir.join(file_name(RUN, "deploy-prod")).exists());
    }

    // The system's chown really changes an owner: to our own id (any process may), and fails on a
    // file that is not there.
    #[test]
    fn the_system_chown_sets_an_owner_and_fails_on_a_missing_file() {
        let dir = scratch("sys-chown");
        let path = dir.join("f");
        std::fs::write(&path, "x").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let mine = file.metadata().unwrap().uid();
        system_chown(&file, mine).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().uid(), mine);
        // Another owner is refused to a process that is not root (and the call is really made).
        if mine != 0 {
            assert!(system_chown(&file, mine + 1).is_err());
        }
    }

    // §17::123: the run id form.
    #[test]
    fn a_run_id_is_run_and_32_lowercase_hex_digits() {
        assert!(valid_run_id(RUN));
        for bad in [
            "",
            "run-",
            "Run-0123456789abcdef0123456789abcdef",
            "rn-0123456789abcdef0123456789abcdef",
            "run-0123456789abcdef0123456789abcde",
            "run-0123456789abcdef0123456789abcdef0",
            "run-0123456789ABCDEF0123456789abcdef",
            "run-0123456789abcdef0123456789abcdeg",
            "run-0123456789abcdef0123456789abcdef\n",
            " run-0123456789abcdef0123456789abcdef",
        ] {
            assert!(!valid_run_id(bad), "{bad:?}");
        }
    }

    // §17::124: the phase form.
    #[test]
    fn a_phase_matches_the_controllers_pattern() {
        for good in ["a", "0", "deploy-prod", "a.b_c-d", &"a".repeat(64), "Z9"] {
            assert!(valid_phase(good), "{good:?}");
        }
        for bad in [
            "",
            ".a",
            "-a",
            "_a",
            &"a".repeat(65),
            "a b",
            "a/b",
            "a\n",
            "é",
            "a\u{0}",
            "..",
        ] {
            assert!(!valid_phase(bad), "{bad:?}");
        }
    }

    // §17::125, 126: a malformed key is refused and nothing is written.
    #[test]
    fn a_malformed_key_is_refused_and_writes_nothing() {
        let dir = scratch("malformed");
        let mut rnd = || Ok([1u8; 16]);
        let mut c = ctl(&dir, &mut rnd);
        for (run, phase) in [("run-x", "p"), (RUN, ".p"), (RUN, ""), ("", "p")] {
            assert!(read_request(&c.requests, run, phase).is_err());
            let shown = ReqView {
                run_id: run.to_owned(),
                phase: phase.to_owned(),
                manifest_hash: HASH.to_owned(),
            };
            assert!(approve(&mut c, &shown, 0).is_err());
        }
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            2,
            "key and requests only"
        );
    }

    // §17::127, 128: the plan hash is the request's; only the three fields are accepted.
    #[test]
    fn the_request_is_accepted_with_exactly_three_fields() {
        let dir = scratch("accepts");
        put_request(
            &dir,
            RUN,
            "deploy-prod",
            &request_json(RUN, "deploy-prod", HASH),
        );
        assert_eq!(
            read_request(&dir.join("requests"), RUN, "deploy-prod"),
            Ok(view())
        );
    }

    // §17::128, 129, 130: each way a request is wrong is refused, and nothing is written.
    #[test]
    fn a_wrong_request_is_refused_and_nothing_is_written() {
        let dir = scratch("wrong");
        let phase = "deploy-prod";
        let good = request_json(RUN, phase, HASH);
        let other_run = "run-ffffffffffffffffffffffffffffffff";
        let cases: Vec<(&str, String)> = vec![
            ("another field", good.replace('}', r#","extra":"x"}"#)),
            (
                "a missing field",
                format!(r#"{{"run_id":"{RUN}","phase":"{phase}"}}"#),
            ),
            ("a short hash", request_json(RUN, phase, &HASH[..63])),
            (
                "an upper-case hash",
                request_json(RUN, phase, &HASH.to_uppercase()),
            ),
            ("another run", request_json(other_run, phase, HASH)),
            ("another phase", request_json(RUN, "other", HASH)),
            ("not json", "run".to_owned()),
            ("an array", "[]".to_owned()),
            (
                "a number hash",
                format!(r#"{{"run_id":"{RUN}","phase":"{phase}","manifest_hash":1}}"#),
            ),
            (
                "a null run",
                format!(r#"{{"run_id":null,"phase":"{phase}","manifest_hash":"{HASH}"}}"#),
            ),
            ("empty", String::new()),
        ];
        let mut rnd = || Ok([1u8; 16]);
        for (what, text) in cases {
            put_request(&dir, RUN, phase, &text);
            let mut c = ctl(&dir, &mut rnd);
            assert!(read_request(&c.requests, RUN, phase).is_err(), "{what}");
            assert!(approve(&mut c, &view(), 0).is_err(), "{what}");
            assert!(!dir.join(file_name(RUN, phase)).exists(), "{what}");
        }
        // Missing.
        std::fs::remove_file(dir.join("requests").join(file_name(RUN, phase))).unwrap();
        let mut c = ctl(&dir, &mut rnd);
        assert!(read_request(&c.requests, RUN, phase).is_err());
        assert!(approve(&mut c, &view(), 0).is_err());
        // A link is not read, nor is a folder.
        std::os::unix::fs::symlink(
            dir.join("key"),
            dir.join("requests").join(file_name(RUN, phase)),
        )
        .unwrap();
        assert!(read_request(&c.requests, RUN, phase).is_err());
        std::fs::remove_file(dir.join("requests").join(file_name(RUN, phase))).unwrap();
        std::fs::create_dir(dir.join("requests").join(file_name(RUN, phase))).unwrap();
        assert!(read_request(&c.requests, RUN, phase).is_err());
        // A request over the limit is not read.
        std::fs::remove_dir(dir.join("requests").join(file_name(RUN, phase))).unwrap();
        put_request(&dir, RUN, phase, &" ".repeat(5_000));
        assert!(
            read_request(&c.requests, RUN, phase)
                .unwrap_err()
                .contains("too long")
        );
        assert!(!dir.join(file_name(RUN, phase)).exists());
    }

    // The request changing after it was shown: nothing is signed.
    #[test]
    fn a_request_rewritten_after_it_was_shown_is_refused() {
        let dir = scratch("rewritten");
        put_request(
            &dir,
            RUN,
            "deploy-prod",
            &request_json(RUN, "deploy-prod", HASH),
        );
        let shown = read_request(&dir.join("requests"), RUN, "deploy-prod").unwrap();
        let other = "f".repeat(64);
        put_request(
            &dir,
            RUN,
            "deploy-prod",
            &request_json(RUN, "deploy-prod", &other),
        );
        let mut rnd = || Ok([1u8; 16]);
        let mut c = ctl(&dir, &mut rnd);
        assert!(approve(&mut c, &shown, 0).unwrap_err().contains("changed"));
        assert!(!dir.join(file_name(RUN, "deploy-prod")).exists());
    }

    // §17::224: the listing shows each valid request and counts the others.
    #[test]
    fn the_listing_shows_run_phase_and_hash() {
        let dir = scratch("listing");
        put_request(&dir, RUN, "b-phase", &request_json(RUN, "b-phase", HASH));
        put_request(&dir, RUN, "a-phase", &request_json(RUN, "a-phase", HASH));
        std::fs::write(dir.join("requests/notes.txt"), "x").unwrap();
        put_request(&dir, RUN, "bad", &request_json(RUN, "other", HASH));
        let got = list_requests(&dir.join("requests"));
        assert_eq!(got.ignored, 2);
        assert_eq!(
            got.requests
                .iter()
                .map(|r| r.phase.as_str())
                .collect::<Vec<_>>(),
            ["a-phase", "b-phase"]
        );
        assert!(
            got.requests
                .iter()
                .all(|r| r.run_id == RUN && r.manifest_hash == HASH)
        );
        assert!(list_requests(&dir.join("missing")).error.is_some());
        assert_eq!(
            list_requests(&scratch("empty-listing").join("requests")),
            Listing::default()
        );
    }

    // §17::143: the time form, at the edges of the calendar.
    #[test]
    fn issued_at_is_utc_in_the_form() {
        assert_eq!(issued_at(0), "1970-01-01T00:00:00Z");
        assert_eq!(issued_at(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(issued_at(1_709_251_199), "2024-02-29T23:59:59Z");
        assert_eq!(issued_at(1_709_251_200), "2024-03-01T00:00:00Z");
        assert_eq!(issued_at(4_102_444_799), "2099-12-31T23:59:59Z");
        assert_eq!(issued_at(1_793_956_447), "2026-11-06T09:14:07Z");
    }

    fn rich_record() -> BTreeMap<String, String> {
        let mut record = make_record(
            &ReqView {
                run_id: RUN.to_owned(),
                phase: "deploy-prod".to_owned(),
                manifest_hash: "ab".repeat(32),
            },
            "Zoë ☃ \"q\" \\ \n\t\u{7f}\u{1} \u{2028} 😀",
            [
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff,
            ],
            1_793_956_447,
        );
        record.insert("issued_at".to_owned(), "2026-11-06T09:14:07Z".to_owned());
        record
    }

    // §17::145, 146: the canonical bytes are Python's `json.dumps(sort_keys=True,
    // separators=(",", ":"), ensure_ascii=False).encode()` for a record with a quote, a backslash,
    // line feed, tab, DEL, a control character, U+2028, a non-ASCII letter and an astral character
    // (the expected bytes were produced by `python3` for exactly this record).
    #[test]
    fn the_canonical_bytes_are_pythons() {
        const PYTHON: &str = "7b226973737565645f6174223a22323032362d31312d30365430393a31343a30375a222c226d616e69666573745f68617368223a2261626162616261626162616261626162616261626162616261626162616261626162616261626162616261626162616261626162616261626162616261626162222c226e6f6e6365223a223030313132323333343435353636373738383939616162626363646465656666222c226f70657261746f72223a225a6fc3ab20e29883205c22715c22205c5c205c6e5c747f5c753030303120e280a820f09f9880222c227068617365223a226465706c6f792d70726f64222c2272756e5f6964223a2272756e2d3031323334353637383961626364656630313233343536373839616263646566222c22736f75726365223a22636f6e736f6c65227d";
        let hex = canonical(&rich_record())
            .iter()
            .fold(String::new(), |mut hex, b| {
                let _ = write!(hex, "{b:02x}");
                hex
            });
        assert_eq!(hex, PYTHON);
        let text = String::from_utf8(canonical(&rich_record())).unwrap();
        assert!(!text.contains(": ") && !text.contains(", "));
        assert!(text.contains('ë') && text.contains('☃') && text.contains('😀'));
    }

    fn read_approval(dir: &Path, phase: &str) -> Value {
        serde_json::from_slice(&std::fs::read(dir.join(file_name(RUN, phase))).unwrap()).unwrap()
    }

    // §17::137-144, 157, 158, 150: the file holds exactly `record` and `signature`; the record the
    // seven fields; its values; the file's mode.
    #[test]
    fn the_approval_file_holds_the_record_and_its_signature() {
        let dir = scratch("approval");
        put_request(
            &dir,
            RUN,
            "deploy-prod",
            &request_json(RUN, "deploy-prod", HASH),
        );
        let mut rnd = || Ok([0xAB; 16]);
        let mut c = ctl(&dir, &mut rnd);
        let path = approve(&mut c, &view(), 1_793_956_447).unwrap();
        assert_eq!(path, dir.join(file_name(RUN, "deploy-prod")));
        let file = read_approval(&dir, "deploy-prod");
        let keys: Vec<&str> = file
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["record", "signature"]);
        let record = file["record"].as_object().unwrap();
        let mut fields: Vec<&str> = record.keys().map(String::as_str).collect();
        fields.sort_unstable();
        assert_eq!(
            fields,
            [
                "issued_at",
                "manifest_hash",
                "nonce",
                "operator",
                "phase",
                "run_id",
                "source"
            ]
        );
        assert_eq!(record["run_id"], RUN);
        assert_eq!(record["phase"], "deploy-prod");
        assert_eq!(record["manifest_hash"], HASH);
        assert_eq!(record["operator"], "alice");
        assert_eq!(record["source"], "console");
        assert_eq!(record["nonce"], "ab".repeat(16));
        assert_eq!(record["issued_at"], "2026-11-06T09:14:07Z");
        let sig = console_keys::b64_decode(file["signature"].as_str().unwrap()).unwrap();
        assert_eq!(sig.len(), 64);
        // The signature verifies against the public key over the canonical bytes.
        let public = ed25519_dalek::VerifyingKey::from_bytes(
            &console_keys::b64_decode(console_keys::public_text(&SEED).trim_end())
                .unwrap()
                .try_into()
                .unwrap(),
        )
        .unwrap();
        let map: BTreeMap<String, String> = record
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
            .collect();
        let signature = Signature::from_slice(&sig).unwrap();
        assert!(public.verify(&canonical(&map), &signature).is_ok());
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.mode() & 0o7777, 0o640);
        // No temporary file is left.
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(names.iter().all(|n| !n.contains(".tmp-")), "{names:?}");
    }

    // §17::144: a verifier that is not Lunchroom's (`openssl pkeyutl -verify -rawin`) accepts the
    // signature over the canonical bytes, and refuses it over another byte.
    #[test]
    fn openssl_verifies_the_signature() {
        let dir = scratch("openssl");
        put_request(
            &dir,
            RUN,
            "deploy-prod",
            &request_json(RUN, "deploy-prod", HASH),
        );
        let mut rnd = || Ok([7; 16]);
        let mut c = ctl(&dir, &mut rnd);
        approve(&mut c, &view(), 1_793_956_447).unwrap();
        let file = read_approval(&dir, "deploy-prod");
        let map: BTreeMap<String, String> = file["record"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
            .collect();
        let message = dir.join("message");
        std::fs::write(&message, canonical(&map)).unwrap();
        let sig = dir.join("sig");
        std::fs::write(
            &sig,
            console_keys::b64_decode(file["signature"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        // The public key as an X.509 SubjectPublicKeyInfo in PEM.
        let raw = console_keys::b64_decode(console_keys::public_text(&SEED).trim_end()).unwrap();
        let mut der = vec![
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
        ];
        der.extend_from_slice(&raw);
        std::fs::write(
            dir.join("pub.pem"),
            format!(
                "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n",
                console_keys::b64_encode(&der)
            ),
        )
        .unwrap();
        let verify = |message: &Path| {
            std::process::Command::new("openssl")
                .args(["pkeyutl", "-verify", "-rawin", "-pubin", "-inkey"])
                .arg(dir.join("pub.pem"))
                .arg("-in")
                .arg(message)
                .arg("-sigfile")
                .arg(&sig)
                .output()
                .unwrap()
        };
        let ok = verify(&message);
        assert!(
            ok.status.success(),
            "{}",
            String::from_utf8_lossy(&ok.stderr)
        );
        let mut changed = canonical(&map);
        changed[5] ^= 1;
        let altered = dir.join("altered");
        std::fs::write(&altered, changed).unwrap();
        assert!(!verify(&altered).status.success());
    }

    // §17::142, 160: the nonce is the 16 random bytes as 32 lowercase hex digits; two approvals
    // carry different nonces; a later approval of the same run and phase replaces the earlier.
    #[test]
    fn two_approvals_carry_different_nonces_and_the_later_replaces_the_earlier() {
        let dir = scratch("two");
        put_request(
            &dir,
            RUN,
            "deploy-prod",
            &request_json(RUN, "deploy-prod", HASH),
        );
        let calls = std::cell::Cell::new(0u8);
        let mut rnd = || {
            calls.set(calls.get() + 1);
            Ok([calls.get(); 16])
        };
        let mut c = ctl(&dir, &mut rnd);
        approve(&mut c, &view(), 100).unwrap();
        let first = read_approval(&dir, "deploy-prod");
        approve(&mut c, &view(), 160).unwrap();
        let second = read_approval(&dir, "deploy-prod");
        assert_eq!(first["record"]["nonce"], "01".repeat(16));
        assert_eq!(second["record"]["nonce"], "02".repeat(16));
        assert_ne!(first["signature"], second["signature"]);
        let approvals = std::fs::read_dir(&dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".json")
            })
            .count();
        assert_eq!(approvals, 1);
    }

    // §17::159: the file reaches its name by a rename, so a reader polling the folder never sees
    // part of it: while the approval is being written, the name does not exist, and the temporary
    // name never matches `<run-id>.<phase>.json`.
    #[test]
    fn the_temporary_name_is_not_an_approval_name() {
        let dir = scratch("tempname");
        put_request(
            &dir,
            RUN,
            "deploy-prod",
            &request_json(RUN, "deploy-prod", HASH),
        );
        let seen = std::cell::RefCell::new(Vec::<bool>::new());
        let dir2 = dir.clone();
        let mut rnd = || {
            // The random source is read before the write starts: the name is not there yet.
            seen.borrow_mut()
                .push(dir2.join(file_name(RUN, "deploy-prod")).exists());
            Ok([3; 16])
        };
        let mut c = ctl(&dir, &mut rnd);
        approve(&mut c, &view(), 5).unwrap();
        assert_eq!(*seen.borrow(), [false]);
        let temp = format!("{}.tmp-{}", file_name(RUN, "deploy-prod"), "03".repeat(16));
        assert_ne!(temp.rsplit('.').next(), Some("json"));
    }

    // A failing random source, or a key that is not there, writes nothing.
    #[test]
    fn no_random_bytes_or_no_key_writes_nothing() {
        let dir = scratch("nokey");
        put_request(
            &dir,
            RUN,
            "deploy-prod",
            &request_json(RUN, "deploy-prod", HASH),
        );
        let mut bad = || Err("no random".to_owned());
        let mut c = ctl(&dir, &mut bad);
        assert!(approve(&mut c, &view(), 5).unwrap_err().contains("random"));
        let mut ok = || Ok([1; 16]);
        let mut c = ctl(&dir, &mut ok);
        std::fs::write(&c.key, "not a key\n").unwrap();
        assert!(approve(&mut c, &view(), 5).is_err());
        std::fs::remove_file(&c.key).unwrap();
        assert!(approve(&mut c, &view(), 5).is_err());
        assert!(!dir.join(file_name(RUN, "deploy-prod")).exists());
        // An approvals folder that cannot be written leaves no temporary file.
        let mut c = ctl(&dir, &mut ok);
        c.approvals = dir.join("not-there");
        // Nothing in the folder can be used: the nonces' file is the first thing it cannot make.
        assert!(
            approve(&mut c, &view(), 5)
                .unwrap_err()
                .contains("not-there")
        );
    }
}
