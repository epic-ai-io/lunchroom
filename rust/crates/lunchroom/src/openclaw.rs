//! Delivery into an OpenClaw session (spec §10 D1, §13 H1.1 item 4): `openclaw agent --session-key
//! <key> --message <envelope>` runs one turn in that session through its gateway, and Lunchroom's
//! plugin sees the turn's prompt in `before_agent_run` before the model does, where the guard
//! confirms the message (DELIVERED) and holds the session (D1.1, T1). `openclaw agent` waits for the
//! whole turn, so Lunchroom starts it and does not wait for it to end; it refuses to run inside an
//! agent's own `exec` shell (`OPENCLAW_SHELL=exec`), which Lunchroom's own delivery is not.
//!
//! Host facts: `docs/host-facts/openclaw-and-hermes.md`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use lunchroom_core::identity::Identity;
use lunchroom_core::mail::{Mail, Status};
use lunchroom_core::store::Store;

use crate::codex::{
    CHILD_ENV, Delivered, UNREADABLE_ENV, failed, fits_one_argument, guarded, message_is,
    recipient_env, recipient_program, update,
};
use crate::delivery::envelope;
use crate::home::Env;

/// How long Lunchroom watches a started `openclaw agent` for an early failure.
const START_CHECK: Duration = Duration::from_secs(5);
/// How long `lunchroom send` waits for the recipient's plugin to confirm the message.
const CONFIRM_WAIT: Duration = Duration::from_secs(10);

/// The variables of the gateway process `process` that tell `openclaw agent` which gateway and
/// state to use, with the few the program needs to run ([`CHILD_ENV`]); nothing else of any
/// environment reaches it, the sender's least of all.
fn gateway_env(process: &str) -> Option<Vec<(String, String)>> {
    let mut keys = vec![
        "OPENCLAW_STATE_DIR",
        "OPENCLAW_CONFIG_PATH",
        "OPENCLAW_GATEWAY_URL",
        "OPENCLAW_PROFILE",
    ];
    keys.extend(CHILD_ENV);
    recipient_env(process, &keys)
}

/// The command that runs `openclaw` for the gateway process `process`: the gateway's own program
/// by its absolute path, as the process runs it ([`recipient_program`]), and for a runtime (`node`)
/// the `openclaw.mjs` at the root of the `openclaw` package that holds `entry`, the script the
/// plugin read from inside the gateway, which is the package's bin (host facts, OpenClaw 2026.10.7
/// `package.json:23-25`). The system cannot show that script: the gateway rewrites its command
/// line to `openclaw-gateway`. Never an `openclaw` found on a `PATH`.
///
/// # Errors
/// Why the program cannot be established.
fn openclaw_command(
    process: &str,
    entry: Option<&Path>,
) -> Result<(PathBuf, Option<PathBuf>), String> {
    let program = recipient_program(process)?;
    if program.file_name().is_some_and(|n| n == "openclaw") {
        return Ok((program, None));
    }
    let entry = entry.ok_or("the recipient's openclaw script is not recorded")?;
    gateway_bin(entry).map(|bin| (program, Some(bin)))
}

/// The `openclaw.mjs` of the package holding the gateway's script. `npm install -g` links
/// `<prefix>/bin/openclaw` to the package's `openclaw.mjs`, so the script is resolved first.
fn gateway_bin(entry: &Path) -> Result<PathBuf, String> {
    let script = std::fs::canonicalize(entry)
        .map_err(|_| "the recipient's openclaw script cannot be read".to_owned())?;
    package_bin(&script)
}

/// The `openclaw.mjs` at the root of the `openclaw` package that holds `script`, found by walking
/// up from it.
fn package_bin(script: &Path) -> Result<PathBuf, String> {
    let mut dir = script.parent();
    for _ in 0..8 {
        let Some(d) = dir else { break };
        let named = std::fs::read(d.join("package.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .is_some_and(|p| p["name"].as_str() == Some("openclaw"));
        if named {
            let bin = d.join("openclaw.mjs");
            return if bin.is_file() {
                Ok(bin)
            } else {
                Err("the recipient's openclaw package has no openclaw.mjs".to_owned())
            };
        }
        dir = d.parent();
    }
    Err("the recipient's openclaw package cannot be found".to_owned())
}

/// Offers the next waiting message for the OpenClaw session `to`, if any.
///
/// # Errors
/// When the store cannot be used.
/// `by`: the sender whose `lunchroom send` makes this hand-over. It hands over only that sender's
/// own message, never another sender's, and waits for the plugin's confirmation for a while. `None`
/// from the plugin's `agent_end` ask: the next message whoever sent it, with no wait.
pub fn deliver(
    env: &Env,
    store: &Store<Mail>,
    to: &Identity,
    by: Option<&Identity>,
) -> Result<Delivered, String> {
    let wait = by.is_some();
    // The hand-over mark is kept before `openclaw agent` starts: a crash after it can leave the
    // message not confirmed, never handed over twice (D1.2). No hold is needed first: the plugin
    // holds the session when the turn's prompt reaches it, before the model sees it.
    let next = update(store, |m| {
        if !guarded(m, env, to) {
            return None;
        }
        let record = m.session(to).cloned()?;
        let message = m.present_from(to, by, env.now)?;
        m.handed_over(&message.id);
        Some((
            message.id.clone(),
            envelope(&message, m.hold_mode()),
            record.process,
            record.entry,
        ))
    })?;
    let Some((id, text, process, entry)) = next else {
        return Ok(Delivered::Nothing);
    };
    // The envelope is one command argument: one the system would refuse is a failed attempt.
    if !fits_one_argument(&text) {
        return failed(
            store,
            id,
            "the envelope does not fit one command argument".to_owned(),
        );
    }
    // A recipient's `openclaw` program that cannot be established says nothing about the
    // recipient: the message stays QUEUED, uncounted and not handed over (D1.3).
    let (program, bin) = match openclaw_command(&process, entry.as_deref()) {
        Ok(command) => command,
        Err(reason) => {
            update(store, |m| m.hand_over_undone(&id))?;
            return Ok(Delivered::Failed(id, reason, false));
        }
    };
    let Some(variables) = gateway_env(&process) else {
        update(store, |m| m.hand_over_undone(&id))?;
        return Ok(Delivered::Failed(id, UNREADABLE_ENV.to_owned(), false));
    };
    let started = Command::new(&program)
        .args(bin)
        .args(["agent", "--session-key", &to.session, "--message", &text])
        .env_clear()
        .env("HOME", &env.home)
        .envs(variables)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match started {
        Ok(child) => child,
        Err(e) => return failed(store, id, format!("openclaw agent could not start: {e}")),
    };
    let confirmed = || message_is(store, &id, |d| d.status == Status::Delivered);
    let start = Instant::now();
    let limit = if wait { CONFIRM_WAIT } else { START_CHECK };
    loop {
        if confirmed() {
            return Ok(Delivered::Confirmed(id));
        }
        match child.try_wait() {
            // The turn ran to its end; the plugin confirmed it if the prompt reached it.
            Ok(Some(status)) if status.success() => {
                return Ok(if confirmed() {
                    Delivered::Confirmed(id)
                } else {
                    Delivered::Unconfirmed(id)
                });
            }
            // An error before the plugin saw the prompt: the gateway did not take the message.
            Ok(Some(status)) if !confirmed() => {
                let reason = format!("openclaw agent exited {}", status.code().unwrap_or(-1));
                return failed(store, id, reason);
            }
            Ok(Some(_)) => return Ok(Delivered::Confirmed(id)),
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(200)),
            // Still running: the turn goes on without Lunchroom waiting for it (D1.3).
            Ok(None) | Err(_) => return Ok(Delivered::Unconfirmed(id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = crate::test_target()
            .join("openclaw-unit")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        assert!(dir.is_dir());
        dir
    }

    // §10::1 (OpenClaw): the program run for a gateway is the `openclaw.mjs` of the
    // `openclaw` package the gateway's script sits in, found by walking up from the script.
    #[test]
    fn the_bin_is_the_openclaw_package_root() {
        let root = scratch("pkg");
        let _ = std::fs::create_dir_all(root.join("dist"));
        let _ = std::fs::write(root.join("package.json"), "{\"name\": \"openclaw\"}");
        let _ = std::fs::write(root.join("openclaw.mjs"), "");
        let script = root.join("dist/entry.js");
        assert_eq!(package_bin(&script), Ok(root.join("openclaw.mjs")));
        let _ = std::fs::remove_dir_all(&root);
    }

    // §10::1 (OpenClaw): a script outside an `openclaw` package, or a package without
    // its bin, gives no program, so the hand-over is refused rather than run from somewhere else.
    #[test]
    fn no_bin_outside_the_openclaw_package() {
        let other = scratch("other");
        let _ = std::fs::write(other.join("package.json"), "{\"name\": \"not-openclaw\"}");
        assert!(package_bin(&other.join("entry.js")).is_err());
        let nobin = scratch("nobin");
        let _ = std::fs::write(nobin.join("package.json"), "{\"name\": \"openclaw\"}");
        assert!(package_bin(&nobin.join("entry.js")).is_err());
        let _ = std::fs::remove_dir_all(&other);
        let _ = std::fs::remove_dir_all(&nobin);
    }

    // §10::14 (OpenClaw): `npm install -g` lays the package out with a link in `bin`,
    // and the plugin reports the script the gateway runs: the program is the package's
    // `openclaw.mjs`, reached through the link; a script outside the package gives none.
    #[test]
    fn a_package_reached_through_the_npm_link_is_found() {
        let root = std::fs::canonicalize(scratch("npm")).unwrap_or_default();
        let package = root.join("lib/node_modules/openclaw");
        let _ = std::fs::create_dir_all(&package);
        let _ = std::fs::create_dir_all(root.join("bin"));
        let _ = std::fs::write(package.join("package.json"), "{\"name\": \"openclaw\"}");
        let _ = std::fs::write(package.join("openclaw.mjs"), "");
        let linked = std::os::unix::fs::symlink(
            "../lib/node_modules/openclaw/openclaw.mjs",
            root.join("bin/openclaw"),
        );
        assert!(linked.is_ok(), "{linked:?}");
        let _ = std::fs::write(root.join("x.js"), "");
        let bin = Ok(package.join("openclaw.mjs"));
        assert_eq!(gateway_bin(&root.join("bin/openclaw")), bin);
        assert_eq!(gateway_bin(&package.join("openclaw.mjs")), bin);
        assert!(gateway_bin(&root.join("gone.mjs")).is_err());
        assert!(gateway_bin(&root.join("x.js")).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    // §10::1 (OpenClaw): a process that is not the recorded one gives no command, and
    // nor does a gateway under a runtime whose plugin reported no script.
    #[test]
    fn no_command_for_a_process_that_is_not_recorded() {
        for process in [format!("{}:1", std::process::id()), String::new()] {
            assert!(openclaw_command(&process, None).is_err(), "{process}");
        }
        let key = crate::process::key(std::process::id()).unwrap_or_default();
        assert!(openclaw_command(&key, None).is_err());
    }

    // §10::1 (OpenClaw): the child's environment is the gateway's: the sender's
    // variables are not passed, and a process that is not running gives none.
    #[test]
    fn the_gateway_environment_has_only_listed_variables() {
        let key = crate::process::key(std::process::id());
        assert!(key.is_some());
        let env = gateway_env(&key.unwrap_or_default()).unwrap_or_default();
        assert_ne!(env, Vec::new());
        assert!(
            env.iter()
                .all(|(k, _)| k.starts_with("OPENCLAW_") || CHILD_ENV.contains(&k.as_str()))
        );
        assert_eq!(gateway_env(&format!("{}:1", u32::MAX)), None);
    }
}
