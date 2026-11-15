//! The YOLO test of every harness adapter (spec §3 R2.2, R2.4, R2.5, R2.7; §13 H1.1): under a
//! `lunchroom yolo` the guard frees a covered, HELD agent's calls in Claude Code, Codex, OpenClaw
//! and Hermes (a freed call runs: Claude Code is answered `allow`, the others with nothing the
//! harness would refuse, and no `ask`), and it asks again at the end time. Each test drives the real
//! `lunchroom guard pretool` with the input the harness (or Lunchroom's plugin for it) sends.
//!
//! GATED calls (P1.4) belong to unit C2's classifier; this file has no GATED row until that unit
//! is merged (reported, not guessed).

mod common;

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use lunchroom_core::identity::{Harness, Identity};
use lunchroom_core::mail::Origin;
use lunchroom_core::policy::HoldMode;

use common::Run;

const ALL: [Harness; 4] = [
    Harness::ClaudeCode,
    Harness::Codex,
    Harness::OpenClaw,
    Harness::Hermes,
];
const MODES: [HoldMode; 3] = [HoldMode::Strict, HoldMode::Balanced, HoldMode::Relaxed];

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What a call came to: it ran (nothing in the way), went to the user (`ask`), or was DENIED
/// (exit 2: a rule, or no channel to ask through).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Out {
    Run,
    Ask,
    Denied,
}

/// Which YOLO is on for one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Yolo {
    None,
    /// The harness's own switch alone (R2.1).
    Own,
    /// A `lunchroom yolo` the user set.
    Lunchroom,
}

struct Sb(common::Sandbox);

impl Sb {
    fn new() -> Self {
        let sb = common::Sandbox::new("yolo-tests");
        let _ = std::fs::create_dir_all(sb.home().join("project"));
        Self(sb)
    }

    fn session(h: Harness) -> &'static str {
        match h {
            Harness::ClaudeCode => "c1",
            Harness::Codex => "x1",
            Harness::OpenClaw => "agent:main:a",
            Harness::Hermes => "h1",
        }
    }

    fn id(h: Harness) -> Identity {
        Identity::new(h, Self::session(h))
    }

    fn update(&self, f: impl FnOnce(&mut lunchroom_core::mail::Mail)) {
        let store = self.0.store();
        assert!(store.is_some());
        assert!(store.is_some_and(|s| s.update(f).is_ok()));
    }

    fn hold(&self, who: &Identity) {
        let who = who.clone();
        self.update(move |m| m.hold_unrecorded(&who, "uds", "m1"));
    }

    fn mode(&self, mode: HoldMode) {
        self.update(move |m| m.set_hold_mode(mode));
    }

    fn set_yolo(&self, who: &Identity, end: u64) {
        let who = who.clone();
        self.update(move |m| m.set_yolo(&who, end));
    }

    /// The input of one `PreToolUse`, as the harness sends it (Lunchroom's plugin for OpenClaw and
    /// Hermes), for `tool` and its input.
    fn input_of(
        &self,
        who: &Identity,
        tool: &(&str, Value),
        own: bool,
        channel: bool,
        mark: &str,
    ) -> (String, Vec<(&'static str, String)>) {
        let cwd = self.0.home().join("project");
        let h = who.harness;
        let mut v = json!({
            "session_id": who.session, "cwd": cwd, "hook_event_name": "PreToolUse",
            "tool_name": tool.0, "tool_input": tool.1.clone(), "tool_use_id": mark,
        });
        let mut env = Vec::new();
        let Some(o) = v.as_object_mut() else {
            return (String::new(), env);
        };
        match h {
            Harness::ClaudeCode => {
                o.insert(
                    "permission_mode".into(),
                    json!(if own { "bypassPermissions" } else { "default" }),
                );
                env.push(("CLAUDE_PID", std::process::id().to_string()));
                env.push((
                    "CLAUDE_CODE_SESSION_ATTENDED",
                    if channel { "1" } else { "0" }.to_owned(),
                ));
                env.push((
                    "CLAUDE_CODE_ENTRYPOINT",
                    if channel { "cli" } else { "sdk-cli" }.to_owned(),
                ));
            }
            Harness::Codex => {
                o.insert("turn_id".into(), json!("turn-1"));
                o.insert(
                    "permission_mode".into(),
                    json!(if own { "bypassPermissions" } else { "default" }),
                );
            }
            Harness::OpenClaw | Harness::Hermes => {
                o.insert(
                    "harness".into(),
                    json!(if h == Harness::OpenClaw {
                        "openclaw"
                    } else {
                        "hermes"
                    }),
                );
                o.insert("approval_channel".into(), json!(channel));
                o.insert("harness_yolo".into(), json!(own));
            }
        }
        (v.to_string(), env)
    }

    /// The tool a shell command is, for each harness.
    fn shell(h: Harness, cmd: &str) -> (&'static str, Value) {
        match h {
            Harness::ClaudeCode | Harness::Codex => ("Bash", json!({ "command": cmd })),
            Harness::OpenClaw => ("exec", json!({ "command": cmd })),
            Harness::Hermes => ("terminal", json!({ "command": cmd })),
        }
    }

    fn run_tool(
        &self,
        who: &Identity,
        tool: &(&str, Value),
        own: bool,
        channel: bool,
        mark: &str,
    ) -> Run {
        let (input, env) = self.input_of(who, tool, own, channel, mark);
        self.0.run(&["guard", "pretool"], &input, &env)
    }

    fn call(&self, who: &Identity, cmd: &str, own: bool, channel: bool) -> (Out, Run) {
        let run = self.run_tool(who, &Self::shell(who.harness, cmd), own, channel, "t1");
        (out_of(&run), run)
    }

    fn out(&self, who: &Identity, cmd: &str, own: bool, channel: bool) -> Out {
        self.call(who, cmd, own, channel).0
    }

    fn approve_listing(&self) -> String {
        self.0.run(&["approve"], "", &[]).stdout
    }
}

fn decision(run: &Run) -> String {
    let v: Value = serde_json::from_str(run.stdout.trim()).unwrap_or(Value::Null);
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn out_of(run: &Run) -> Out {
    // Neither an answer nor a block (exit 0 or 2): no test may read that as any outcome.
    assert!(
        matches!(run.code, Some(0 | 2)),
        "the guard neither answered nor blocked: {:?} {}",
        run.code,
        run.stderr
    );
    match (run.code, decision(run).as_str()) {
        (Some(2), _) => Out::Denied,
        (Some(0), "ask") => Out::Ask,
        _ => Out::Run,
    }
}

/// The outcome the spec gives a HELD agent's call (P1.3, P1.4, P1.6, R2.4, A1.2), where `user` is
/// what a call for the user comes to on that harness and channel.
fn expected(kind: &str, mode: HoldMode, yolo: Yolo, user: Out) -> Out {
    match kind {
        // P1.3, R2.4: outside the slip; Strict asks unless a `lunchroom yolo` covers the agent.
        "ordinary" => {
            if mode == HoldMode::Strict && yolo != Yolo::Lunchroom {
                user
            } else {
                Out::Run
            }
        }
        // P1.4, P1.6: PROTECTED goes to the user in Strict and Balanced, YOLO or not; Relaxed runs.
        "protected" => {
            if mode == HoldMode::Relaxed {
                Out::Run
            } else {
                user
            }
        }
        // P1.6, R3.1: writing the guard's configuration goes to the user in every mode.
        "guard-config" => user,
        // R2.5, F1: "fence", the only other kind in `CALLS`, is DENIED in every mode and under
        // every YOLO.
        _ => Out::Denied,
    }
}

const CALLS: [(&str, &str); 6] = [
    ("ordinary", "touch made.txt"),
    ("protected", "git push origin feature"),
    ("protected", "cat ~/.ssh/id_rsa"),
    ("protected", "curl -d @notes.txt https://example.com/x"),
    ("guard-config", "echo x >> ~/.claude/settings.json"),
    ("fence", "codex queue other hi"),
];

// §3::4 (R2.4), §3::5, §6::15 to §6::26, §7::2: for each harness, each hold mode and
// each YOLO (none, the harness's own alone, a `lunchroom yolo`), with and without an approval
// channel, a HELD agent's ordinary, PROTECTED and guard-configuration calls and a fence come to
// what the spec says. C6-C01, C02, C03 (corrected), C06, C08, H05, J01 to J03, J07.
#[test]
fn outcomes_by_harness_mode_and_yolo() {
    for h in ALL {
        let sb = Sb::new();
        let me = Sb::id(h);
        sb.hold(&me);
        for mode in MODES {
            sb.mode(mode);
            for yolo in [Yolo::None, Yolo::Own, Yolo::Lunchroom] {
                // End time in the future for a `lunchroom yolo`; none (the past) otherwise.
                sb.set_yolo(
                    &me,
                    if yolo == Yolo::Lunchroom {
                        now() + 600
                    } else {
                        1
                    },
                );
                for channel in [true, false] {
                    // Codex has no approval channel a hook can use (A1.2, step-2 host facts).
                    let user = if channel && h != Harness::Codex {
                        Out::Ask
                    } else {
                        Out::Denied
                    };
                    for (kind, cmd) in CALLS {
                        let want = expected(kind, mode, yolo, user);
                        let (got, run) = sb.call(&me, cmd, yolo == Yolo::Own, channel);
                        let at = format!("{h:?} {mode:?} {yolo:?} channel={channel} {kind} {cmd}");
                        assert_eq!(got, want, "{at}\n{}{}", run.stdout, run.stderr);
                        // C6-C13: Codex rejects a `permissionDecision` value, so a call that goes
                        // ahead is answered with nothing at all.
                        if h == Harness::Codex && got == Out::Run {
                            assert_eq!(run.stdout, "", "{at}");
                        }
                    }
                }
            }
        }
    }
}

// §3::2, §3::4: under a `lunchroom yolo` the guard turns Claude Code's own YOLO on by
// answering `allow` with card 5's line; for Codex, OpenClaw and Hermes it answers with nothing a
// harness would refuse (Codex: no output; the plugins read no `ask` and no block). The line is not
// printed into Codex's output. C6-D13, D14, C13.
#[test]
fn a_freed_call_is_answered_as_each_harness_reads_it() {
    for h in ALL {
        let sb = Sb::new();
        let me = Sb::id(h);
        sb.hold(&me);
        sb.mode(HoldMode::Strict);
        sb.set_yolo(&me, now() + 600);
        let (got, run) = sb.call(&me, "touch made.txt", false, true);
        assert_eq!(got, Out::Run, "{h:?}: {}", run.stderr);
        match h {
            Harness::ClaudeCode => {
                assert_eq!(decision(&run), "allow");
                assert!(
                    run.stdout.contains("YOLO authorized until"),
                    "{}",
                    run.stdout
                );
            }
            Harness::Codex => assert_eq!(run.stdout, "", "Codex rejects a decision value"),
            Harness::OpenClaw | Harness::Hermes => {
                assert_ne!(decision(&run), "ask", "{}", run.stdout);
                assert!(!run.stdout.contains("block"), "{}", run.stdout);
            }
        }
    }
}

// §3::2, §3::7: at the end time the YOLO is off and the guard asks again: one second
// before it the call runs, at it and after it the call goes to the user. Stored times at the
// extremes: 0 is no YOLO, `u64::MAX` covers and never overflows. C6-D01, B06, B08.
#[test]
fn yolo_ends_at_its_end_time_on_each_harness() {
    for h in ALL {
        let sb = Sb::new();
        let me = Sb::id(h);
        sb.hold(&me);
        sb.mode(HoldMode::Strict);
        let user = if h == Harness::Codex {
            Out::Denied
        } else {
            Out::Ask
        };
        sb.set_yolo(&me, now() + 3);
        assert_eq!(
            sb.out(&me, "touch made.txt", false, true),
            Out::Run,
            "{h:?} before"
        );
        std::thread::sleep(std::time::Duration::from_secs(4));
        assert_eq!(
            sb.out(&me, "touch made.txt", false, true),
            user,
            "{h:?} after"
        );
        sb.set_yolo(&me, 0);
        assert_eq!(
            sb.out(&me, "touch made.txt", false, true),
            user,
            "{h:?} zero"
        );
        sb.set_yolo(&me, now().saturating_sub(1));
        assert_eq!(
            sb.out(&me, "touch made.txt", false, true),
            user,
            "{h:?} one second ago"
        );
        sb.set_yolo(&me, u64::MAX);
        assert_eq!(
            sb.out(&me, "touch made.txt", false, true),
            Out::Run,
            "{h:?} max"
        );
        // Ended by the user (`till now` sets the end to this instant, so it is no longer ahead).
        sb.set_yolo(&me, now());
        assert_eq!(
            sb.out(&me, "touch made.txt", false, true),
            user,
            "{h:?} ended"
        );
    }
}

// §3::2 (I1): a `lunchroom yolo` covers the session it names and no other: not another
// session of the harness, not a new session id after a restart, not the same session id on
// another harness; a restart of the same session (a new process, the same store) is still covered.
// C6-C11, C12, D10, D11, K05.
#[test]
fn yolo_covers_the_session_it_names_only() {
    for h in ALL {
        let sb = Sb::new();
        let me = Sb::id(h);
        sb.mode(HoldMode::Strict);
        let others = [
            Identity::new(h, "other"),
            Identity::new(h, &format!("{}x", Sb::session(h))),
            Identity::new(h, &Sb::session(h).to_uppercase()),
        ];
        for other in &others {
            sb.hold(other);
        }
        sb.hold(&me);
        // The same session id on each other harness is another identity.
        let twins: Vec<Identity> = ALL
            .into_iter()
            .filter(|o| *o != h)
            .map(|o| Identity::new(o, Sb::session(h)))
            .collect();
        for twin in &twins {
            sb.hold(twin);
        }
        sb.set_yolo(&me, now() + 600);
        for _restart in 0..2 {
            // Each guard run is a new process on the same store.
            assert_eq!(
                sb.out(&me, "touch made.txt", false, true),
                Out::Run,
                "{h:?}"
            );
        }
        for other in others.iter().chain(&twins) {
            // Not covered: what a call for the user comes to on the other's own harness.
            let want = if other.harness == Harness::Codex {
                Out::Denied
            } else {
                Out::Ask
            };
            let got = sb.out(other, "touch made.txt", false, true);
            assert_eq!(got, want, "{h:?} covers {other:?}");
        }
    }
}

// §3::2, §3::5, §3::7 (A1.3): an agent's `lunchroom yolo` is DENIED on each harness,
// HELD or not, under a `lunchroom yolo` or not, in every spelling; the store does not change.
// C6-E01, E02, I04.
#[test]
fn an_agents_lunchroom_yolo_is_denied_on_each_harness() {
    let program = env!("CARGO_BIN_EXE_lunchroom");
    let spellings = [
        "lunchroom yolo all till 14:00".to_owned(),
        "lunchroom yolo all till now".to_owned(),
        "env lunchroom yolo all till 14:00".to_owned(),
        "sudo lunchroom yolo all till 14:00".to_owned(),
        "timeout 5 lunchroom yolo all till 14:00".to_owned(),
        "sh -c 'lunchroom yolo all till 14:00'".to_owned(),
        "/usr/local/bin/lunchroom yolo all till 14:00".to_owned(),
        "\"lunchroom\" yolo all till 14:00".to_owned(),
        "lunchroom   yolo   all till 14:00".to_owned(),
        "cd /tmp && lunchroom yolo all till 14:00".to_owned(),
        format!("{program} yolo all till 14:00"),
    ];
    for h in ALL {
        for (held, covered) in [(false, false), (true, false), (true, true)] {
            let sb = Sb::new();
            let me = Sb::id(h);
            if held {
                sb.hold(&me);
            }
            if covered {
                sb.set_yolo(&me, now() + 600);
            }
            for cmd in &spellings {
                for channel in [true, false] {
                    let (got, run) = sb.call(&me, cmd, false, channel);
                    let at = format!("{h:?} held={held} covered={covered} channel={channel} {cmd}");
                    assert_eq!(got, Out::Denied, "{at}\n{}", run.stdout);
                    assert!(run.stderr.contains("DENIED"), "{at}: {}", run.stderr);
                }
            }
            let kept = sb.0.store().and_then(|s| s.read().ok());
            let covers = kept.as_ref().and_then(|m| m.yolo_until(&me, now()));
            assert_eq!(covers.is_some(), covered, "{h:?} the store is unchanged");
        }
    }
}

// §3::4, D1.8: a call waiting for the user is not changed by a YOLO set or ended while
// it waits: it still waits (still listed by `lunchroom approve`), its retry is judged under the
// YOLO then in force; an ended YOLO approves nothing by itself. C6-D03, D04.
#[test]
fn a_yolo_set_or_ended_while_a_call_waits_changes_nothing_for_it() {
    for h in ALL {
        let sb = Sb::new();
        let me = Sb::id(h);
        // With no approval channel the guard keeps the call for `lunchroom approve` (A1.2); a call
        // a harness's own prompt answers has no record of Lunchroom's to keep (D1.8).
        let user = Out::Denied;
        sb.hold(&me);
        sb.mode(HoldMode::Strict);
        assert_eq!(sb.out(&me, "touch waits.txt", false, false), user, "{h:?}");
        let key = me.key();
        assert!(
            sb.approve_listing().contains(&key),
            "{h:?}: {}",
            sb.approve_listing()
        );
        // A YOLO begins: the waiting call still waits.
        sb.set_yolo(&me, now() + 600);
        assert!(sb.approve_listing().contains(&key), "{h:?} still waits");
        // Its retry is judged under the YOLO and runs; a PROTECTED one still goes to the user.
        assert_eq!(
            sb.out(&me, "touch waits.txt", false, false),
            Out::Run,
            "{h:?} retry"
        );
        assert_eq!(
            sb.out(&me, "git push origin feature", false, false),
            user,
            "{h:?}"
        );
        assert!(
            sb.approve_listing().contains(&key),
            "{h:?} still waits after the retry"
        );
        // The YOLO ends: nothing was approved by it, the retry asks again.
        sb.set_yolo(&me, 1);
        assert!(
            sb.approve_listing().contains(&key),
            "{h:?} still waits at the end"
        );
        assert_eq!(
            sb.out(&me, "touch waits.txt", false, false),
            user,
            "{h:?} after the end"
        );
    }
}

// §3::4, §8 A2.1: the user's approval of a PROTECTED call that waited is the user's own:
// given after the YOLO ended it runs that call once; an identical retry asks again. C6-C09, K02.
#[test]
fn an_approval_runs_its_call_once_under_and_after_yolo() {
    for h in ALL {
        let sb = Sb::new();
        let me = Sb::id(h);
        let user = if h == Harness::Codex {
            Out::Denied
        } else {
            Out::Ask
        };
        sb.hold(&me);
        sb.mode(HoldMode::Strict);
        sb.set_yolo(&me, now() + 600);
        let push = "git push origin feature";
        assert_eq!(sb.out(&me, push, false, true), user, "{h:?} under YOLO");
        sb.set_yolo(&me, 1);
        let (tool, input) = Sb::shell(h, push);
        let dir = sb.0.home().join("project").to_string_lossy().into_owned();
        let me2 = me.clone();
        sb.update(move |m| {
            m.approve(&me2, tool, &input, &dir, now());
        });
        assert_eq!(sb.out(&me, push, false, true), Out::Run, "{h:?} once");
        assert_eq!(sb.out(&me, push, false, true), user, "{h:?} not twice");
    }
}

// §3::3: while an agent is not HELD, YOLO changes nothing for it: ordinary and PROTECTED
// calls run, as without; the fence and `lunchroom yolo` stay DENIED. C6-C10.
#[test]
fn yolo_changes_nothing_for_an_agent_that_is_not_held() {
    for h in ALL {
        let sb = Sb::new();
        let me = Sb::id(h);
        sb.mode(HoldMode::Strict);
        for covered in [false, true] {
            sb.set_yolo(&me, if covered { now() + 600 } else { 1 });
            for (_, cmd) in CALLS {
                let want = if cmd.starts_with("codex queue") {
                    Out::Denied
                } else {
                    Out::Run
                };
                assert_eq!(
                    sb.out(&me, cmd, false, true),
                    want,
                    "{h:?} covered={covered} {cmd}"
                );
            }
        }
    }
}

// §3::6: status shows the end time of a `lunchroom yolo` and, when the harness reports
// it to the guard, the harness's own YOLO; each follows the next call's report. C6-C15, H06, H07.
#[test]
fn status_shows_each_yolo_for_each_harness() {
    for h in ALL {
        let sb = Sb::new();
        let me = Sb::id(h);
        let name = format!("agent-{h:?}").to_lowercase();
        let reg = (name.clone(), me.clone());
        sb.update(move |m| {
            assert!(m.register(&reg.0, &reg.1, Origin::User, None).is_ok());
        });
        sb.hold(&me);
        sb.mode(HoldMode::Strict);
        let status = |sb: &Sb| sb.0.run(&["status"], "", &[]).stdout;
        let line = |s: &str| {
            s.lines()
                .find(|l| l.starts_with(&name))
                .unwrap_or_default()
                .to_owned()
        };
        let _ = sb.out(&me, "touch a", false, true);
        assert!(
            !line(&status(&sb)).contains("YOLO"),
            "{h:?}: {}",
            status(&sb)
        );
        let _ = sb.out(&me, "touch a", true, true);
        assert!(
            line(&status(&sb)).contains("YOLO (the harness's own)"),
            "{h:?}: {}",
            status(&sb)
        );
        let end = now() + 600;
        sb.set_yolo(&me, end);
        let shown = format!(
            "YOLO until {:02}:{:02} UTC",
            end % 86_400 / 3600,
            end % 3600 / 60
        );
        let _ = sb.out(&me, "touch a", true, true);
        let both = line(&status(&sb));
        assert!(
            both.contains(&shown) && both.contains("(the harness's own)"),
            "{h:?}: {both}"
        );
        // The harness reports its own YOLO off: only the `lunchroom yolo` stays; ended, none.
        let _ = sb.out(&me, "touch a", false, true);
        let only = line(&status(&sb));
        assert!(
            only.contains(&shown) && !only.contains("(the harness's own)"),
            "{h:?}: {only}"
        );
        sb.set_yolo(&me, 1);
        assert!(
            !line(&status(&sb)).contains("YOLO"),
            "{h:?}: {}",
            status(&sb)
        );
    }
}

// §6::8, §6::9, §3::4 (P1.1): a HELD agent's one reply is inside its slip in the
// Balanced and Relaxed modes and goes to the user in the Strict mode, also under a `lunchroom
// yolo`, on each harness. Which agents it may reply to is `lunchroom send`'s rule, not the
// hook's (tests/guard.rs `held_reply_runs_and_reply_to_non_holder_is_denied`), and is not read
// from the YOLO. C6-J06.
#[test]
fn a_reply_is_judged_by_mode_and_not_by_yolo() {
    for h in ALL {
        let sb = Sb::new();
        let me = Sb::id(h);
        sb.hold(&me);
        sb.set_yolo(&me, now() + 600);
        let cmd = format!("{} send glm hello", env!("CARGO_BIN_EXE_lunchroom"));
        for mode in MODES {
            sb.mode(mode);
            let user = if h == Harness::Codex {
                Out::Denied
            } else {
                Out::Ask
            };
            let want = if mode == HoldMode::Strict {
                user
            } else {
                Out::Run
            };
            assert_eq!(sb.out(&me, &cmd, false, true), want, "{h:?} {mode:?}");
        }
    }
}

// §3::7: a store the guard cannot read frees nothing, on each harness (fails closed):
// the call is not answered as freed by a YOLO that cannot be read. C6-K06.
#[test]
fn an_unreadable_store_frees_nothing() {
    for h in ALL {
        let sb = Sb::new();
        let me = Sb::id(h);
        sb.hold(&me);
        sb.mode(HoldMode::Strict);
        sb.set_yolo(&me, now() + 600);
        assert_eq!(
            sb.out(&me, "touch a", false, true),
            Out::Run,
            "{h:?} before"
        );
        let file = sb.0.lunchroom().join("store/state.json");
        assert!(std::fs::write(&file, b"{ not json").is_ok());
        let (input, env) = sb.input_of(&me, &Sb::shell(h, "touch a"), false, true, "t1");
        let run = sb.0.run(&["guard", "pretool"], &input, &env);
        assert!(
            run.code == Some(2) || decision(&run) == "ask",
            "{h:?}: a garbled store frees nothing: {:?} {}{}",
            run.code,
            run.stdout,
            run.stderr
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Codex's complete list (FAILURES-CODEX.md): the classes it marked [N], and the corrected rows.
// ---------------------------------------------------------------------------------------------

impl Sb {
    /// A HELD agent of `h` in the Strict hold mode, covered by a `lunchroom yolo` or not.
    fn held(h: Harness, covered: bool, mode: HoldMode) -> (Self, Identity) {
        let sb = Self::new();
        let me = Self::id(h);
        sb.hold(&me);
        sb.mode(mode);
        sb.set_yolo(&me, if covered { now() + 600 } else { 1 });
        (sb, me)
    }

    /// One `PreToolUse` input of `who` for `tool`, changed by `edit`, run as bytes.
    fn edited(&self, who: &Identity, tool: &(&str, Value), edit: impl Fn(&mut Value)) -> Run {
        let (input, env) = self.input_of(who, tool, false, true, "t1");
        let mut v: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
        edit(&mut v);
        self.0
            .run_bytes(&["guard", "pretool"], v.to_string().as_bytes(), &env)
    }
}

/// What a freed call looks like: it ran (exit 0, no `ask`).
fn ran(run: &Run) -> bool {
    run.code == Some(0) && decision(run) != "ask"
}

// C6-H08: a harness's own YOLO report of the wrong type is never read as true: the plugin's
// `harness_yolo` as null, a string, a number, an object, an array frees nothing in Strict.
#[test]
fn wrong_type_harness_yolo_is_not_authorization() {
    for h in [Harness::OpenClaw, Harness::Hermes] {
        let (sb, me) = Sb::held(h, false, HoldMode::Strict);
        let tool = Sb::shell(h, "touch made.txt");
        // Named, so that status lists the agent and shows what its harness reported.
        let named = me.clone();
        sb.update(move |m| {
            assert!(m.register("wrongtype", &named, Origin::User, None).is_ok());
        });
        for wrong in [
            json!(null),
            json!("true"),
            json!(1),
            json!({}),
            json!([true]),
        ] {
            let run = sb.edited(&me, &tool, |v| v["harness_yolo"] = wrong.clone());
            assert_eq!(out_of(&run), Out::Ask, "{h:?} {wrong}");
            let status = sb.0.run(&["status"], "", &[]).stdout;
            assert!(!status.contains("harness's own"), "{h:?} {wrong}: {status}");
        }
    }
}

// C6-X05, X06: a missing, empty or wrong-type session id is no identity: the guard does not answer
// the call as run, under any YOLO, on any harness.
#[test]
fn identity_wrong_types_fail_closed() {
    for h in ALL {
        let (sb, me) = Sb::held(h, true, HoldMode::Strict);
        let tool = Sb::shell(h, "touch made.txt");
        for wrong in [
            json!(null),
            json!(""),
            json!(0),
            json!(false),
            json!([]),
            json!({}),
        ] {
            let run = sb.edited(&me, &tool, |v| v["session_id"] = wrong.clone());
            // H1.5: a call the guard cannot check goes to the user (or is blocked): never freed.
            assert!(!ran(&run), "{h:?} session_id {wrong}: {}", run.stdout);
        }
        let run = sb.edited(&me, &tool, |v| {
            if let Some(o) = v.as_object_mut() {
                o.remove("session_id");
            }
        });
        assert!(!ran(&run), "{h:?} no session_id: {}", run.stdout);
        let run = sb.0.run(&["guard", "pretool"], "{}", &[]);
        assert!(!ran(&run), "{h:?} empty object: {}", run.stdout);
    }
}

fn patch(path: &str) -> String {
    format!("*** Begin Patch\n*** Add File: {path}\n+x\n*** End Patch\n")
}

fn patch_tool(h: Harness, text: &str) -> (&'static str, Value) {
    match h {
        Harness::OpenClaw => ("apply_patch", json!({ "input": text })),
        _ => ("apply_patch", json!({ "command": text })),
    }
}

// C6-P17: the paths of a patch are its writes: a patch adding an ordinary file is an ordinary call
// (asks in Strict, runs under a covering YOLO); one writing the guard's configuration goes to the
// user in every mode, YOLO included; the patch body is not read as paths.
#[test]
fn patch_paths_are_classified() {
    for h in [Harness::Codex, Harness::OpenClaw] {
        for mode in MODES {
            for covered in [false, true] {
                let (sb, me) = Sb::held(h, covered, mode);
                let user = if h == Harness::Codex {
                    Out::Denied
                } else {
                    Out::Ask
                };
                let at = format!("{h:?} {mode:?} covered={covered}");
                let plain = patch_tool(h, &patch("/tmp/a-c6.txt"));
                let want = if mode == HoldMode::Strict && !covered {
                    user
                } else {
                    Out::Run
                };
                let got = out_of(&sb.run_tool(&me, &plain, false, true, "t1"));
                assert_eq!(got, want, "{at} plain");
                let guard = format!("{}/.claude/settings.json", sb.0.home().display());
                let cfg = patch_tool(h, &patch(&guard));
                let got = out_of(&sb.run_tool(&me, &cfg, false, true, "t2"));
                assert_eq!(got, user, "{at} guard configuration");
                // A body line that looks like a header is body, not a second path.
                let body = "*** Begin Patch\n*** Add File: /tmp/b-c6.txt\n+*** Add File: /etc/passwd\n*** End Patch\n";
                let got = out_of(&sb.run_tool(&me, &patch_tool(h, body), false, true, "t3"));
                assert_eq!(got, want, "{at} body line");
            }
        }
    }
}

// C6-P18: a file-write tool with no path or patch the guard can read is DENIED in every session:
// held or not, every mode, covered or not.
#[test]
fn pathless_file_write_is_denied() {
    for h in [Harness::Codex, Harness::OpenClaw] {
        for held in [false, true] {
            for mode in MODES {
                let sb = Sb::new();
                let me = Sb::id(h);
                if held {
                    sb.hold(&me);
                }
                sb.mode(mode);
                sb.set_yolo(&me, now() + 600);
                for text in [
                    "",
                    "just prose, no patch",
                    "*** Begin Patch\n*** End Patch\n",
                ] {
                    let run = sb.run_tool(&me, &patch_tool(h, text), false, true, "t1");
                    assert_eq!(
                        out_of(&run),
                        Out::Denied,
                        "{h:?} held={held} {mode:?} {text:?}"
                    );
                }
            }
        }
    }
}

// C6-P19, M07, X07, X08, X09, X10, S07: an agent's `lunchroom yolo` is DENIED in every spelling the
// shell reads as that command, also inside a script that has a PROTECTED part (the stricter rule
// wins), in the process-tree wrappers, with control characters, through a symlink or a hard link or
// a path alias; a spelling the shell reads as another word is not that command and is not a panic.
#[test]
fn lunchroom_yolo_in_every_shell_spelling_is_denied() {
    let program = env!("CARGO_BIN_EXE_lunchroom");
    for h in ALL {
        for (held, covered) in [(false, false), (true, false), (true, true)] {
            let sb = Sb::new();
            let me = Sb::id(h);
            if held {
                sb.hold(&me);
            }
            sb.mode(HoldMode::Strict);
            sb.set_yolo(&me, if covered { now() + 600 } else { 1 });
            let bin = sb.0.root.join("bin2");
            assert!(std::fs::create_dir_all(&bin).is_ok());
            let link = bin.join("lr");
            assert!(std::os::unix::fs::symlink(program, &link).is_ok());
            let spellings = vec![
                "sh -c 'cat ~/.ssh/id_rsa; lunchroom yolo x till now'".to_owned(),
                "sudo sh -c 'lunchroom yolo x till now'".to_owned(),
                "timeout 5 env lunchroom yolo x till now".to_owned(),
                "(lunchroom yolo x till now)".to_owned(),
                "{ lunchroom yolo x till now; }".to_owned(),
                "lunchroom\tyolo x till now".to_owned(),
                "true\nlunchroom yolo x till now".to_owned(),
                "true\r\nlunchroom yolo x till now".to_owned(),
                "lunchroom 'yolo' x till now".to_owned(),
                "lunchroom y\\olo x till now".to_owned(),
                "lunchroom $'yolo' x till now".to_owned(),
                "lunchroom \"yolo\" x till now".to_owned(),
                format!("{program} yolo x till now"),
                format!(
                    "{}/./lunchroom yolo x till now",
                    program.trim_end_matches("/lunchroom")
                ),
                format!("{} yolo x till now", link.display()),
                "env -i lunchroom yolo x till now".to_owned(),
                "nohup lunchroom yolo x till now".to_owned(),
                "xargs lunchroom yolo x till now".to_owned(),
                "bash -c \"lunchroom yolo x till now\"".to_owned(),
            ];
            for cmd in &spellings {
                let (got, run) = sb.call(&me, cmd, false, true);
                let at = format!("{h:?} held={held} covered={covered} {cmd:?}");
                assert_eq!(got, Out::Denied, "{at}\n{}{}", run.stdout, run.stderr);
            }
            // Other words: not that command; the guard answers (exit 0 or 2) and does not panic.
            for cmd in [
                "lunchroom\u{200b}yolo x till now",
                "lunchroom\u{a0}yolo x till now",
                "lunchroom y\u{202e}olo x till now",
                "lunchroom yo\u{200d}lo x till now",
            ] {
                let (_, run) = sb.call(&me, cmd, false, true);
                assert!(
                    matches!(run.code, Some(0 | 2)),
                    "{h:?} {cmd:?}: {}",
                    run.stderr
                );
            }
        }
    }
}

// C6-G13: a shell script the guard cannot find or read is DENIED in every session, every mode and
// under a covering YOLO (F1.2).
#[test]
fn unreadable_script_stays_denied() {
    for h in ALL {
        for mode in MODES {
            for covered in [false, true] {
                let (sb, me) = Sb::held(h, covered, mode);
                let hidden = sb.0.home().join("project/hidden.sh");
                assert!(std::fs::write(&hidden, "touch x\n").is_ok());
                assert!(
                    std::fs::set_permissions(
                        &hidden,
                        std::os::unix::fs::PermissionsExt::from_mode(0o000)
                    )
                    .is_ok()
                );
                for cmd in ["./missing.sh", "bash ./missing.sh", "sh ./hidden.sh"] {
                    let (got, run) = sb.call(&me, cmd, false, true);
                    assert_eq!(
                        got,
                        Out::Denied,
                        "{h:?} {mode:?} {covered} {cmd}: {}",
                        run.stdout
                    );
                }
            }
        }
    }
}

// C6-S08: a hostile command (a fork bomb, a detached never-ending child, a 100,000-character
// word) is judged as text: the guard answers within 10 seconds with exit 0 or 2, and sets no YOLO.
#[test]
fn hostile_commands_are_judged_as_text_and_set_nothing() {
    let long = format!("echo {}", "a".repeat(100_000));
    for h in ALL {
        let (sb, me) = Sb::held(h, true, HoldMode::Strict);
        for cmd in [
            ":(){ :|:& };:",
            "nohup sleep 1000000 >/dev/null 2>&1 &",
            "yes | head -c 10000000",
            long.as_str(),
        ] {
            let began = std::time::Instant::now();
            let (_, run) = sb.call(&me, cmd, false, true);
            assert!(began.elapsed().as_secs() < 10, "{h:?} {cmd:.30}");
            assert!(
                matches!(run.code, Some(0 | 2)),
                "{h:?} {:.30}: {}",
                cmd,
                run.stderr
            );
        }
        let other = Identity::new(h, "someone-else");
        let kept = sb.0.store().and_then(|s| s.read().ok());
        assert!(
            kept.is_some_and(|m| m.yolo_until(&other, now()).is_none()),
            "{h:?}"
        );
    }
}

// C6-S09: input in an encoding the guard cannot read (invalid UTF-8, an overlong sequence, a lone
// JSON surrogate) is not answered as a freed call, under a covering YOLO, on any harness.
#[test]
fn invalid_encoding_fails_closed() {
    for h in ALL {
        let (sb, me) = Sb::held(h, true, HoldMode::Strict);
        let (input, env) = sb.input_of(&me, &Sb::shell(h, "touch ZZZ"), false, true, "t1");
        let cases: Vec<Vec<u8>> = vec![
            input
                .replace("ZZZ", "\u{1}")
                .into_bytes()
                .iter()
                .map(|b| if *b == 1 { 0xff } else { *b })
                .collect(),
            input
                .replace("touch ZZZ", "touch \u{1}\u{1}")
                .into_bytes()
                .iter()
                .map(|b| if *b == 1 { 0xc0 } else { *b })
                .collect(),
            input.replace("ZZZ", "\\ud800").into_bytes(),
        ];
        for (i, bytes) in cases.iter().enumerate() {
            let run = sb.0.run_bytes(&["guard", "pretool"], bytes, &env);
            assert!(
                !ran(&run),
                "{h:?} case {i} was freed: {} {}",
                run.stdout,
                run.stderr
            );
        }
    }
}

// C6-U01: Claude Code's adapter contract for YOLO as a whole: a freed call is answered `allow`
// with card 5's line; a PROTECTED call asks; a headless session's PROTECTED call is DENIED; after
// the end time it asks again.
#[test]
fn claude_code_adapter_contract() {
    let (sb, me) = Sb::held(Harness::ClaudeCode, true, HoldMode::Strict);
    let (got, run) = sb.call(&me, "touch made.txt", false, true);
    assert_eq!(got, Out::Run);
    assert_eq!(decision(&run), "allow");
    assert!(
        run.stdout.contains("YOLO authorized until"),
        "{}",
        run.stdout
    );
    assert_eq!(
        sb.out(&me, "git push origin feature", false, true),
        Out::Ask
    );
    assert_eq!(
        sb.out(&me, "git push origin feature", false, false),
        Out::Denied
    );
    sb.set_yolo(&me, 1);
    assert_eq!(sb.out(&me, "touch made.txt", false, true), Out::Ask);
}

// C6-S01: a `lunchroom yolo` written while guard calls run: each answer is the whole old state or
// the whole new one (run, or the harness's user route), never an error.
#[test]
fn yolo_set_while_the_guard_reads_the_store() {
    for h in ALL {
        let (sb, me) = Sb::held(h, false, HoldMode::Strict);
        let user = if h == Harness::Codex {
            Out::Denied
        } else {
            Out::Ask
        };
        let stop = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let mut on = false;
                while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                    on = !on;
                    sb.set_yolo(&me, if on { now() + 600 } else { 1 });
                }
            });
            for i in 0..6 {
                let got = sb.out(&me, "touch made.txt", false, true);
                assert!(got == Out::Run || got == user, "{h:?} call {i}: {got:?}");
            }
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
        });
    }
}

// C6-X09 (hard link): a hard link to Lunchroom's program, under another name and folder, is
// Lunchroom's program (classify.rs `is_lunchroom` compares the file's device and inode with the
// program's), so an agent's `lunchroom yolo` through it is DENIED as the program's own is.
#[test]
fn lunchroom_yolo_through_a_hard_link_is_denied() {
    let program = env!("CARGO_BIN_EXE_lunchroom");
    for h in ALL {
        let sb = Sb::new();
        let me = Sb::id(h);
        let hard = sb.0.root.join("lh");
        assert!(std::fs::hard_link(program, &hard).is_ok());
        let (got, run) = sb.call(
            &me,
            &format!("{} yolo x till now", hard.display()),
            false,
            true,
        );
        assert_eq!(got, Out::Denied, "{h:?}: {}", run.stdout);
    }
}
