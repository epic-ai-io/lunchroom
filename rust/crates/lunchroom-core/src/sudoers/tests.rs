//! Tests of the `sudo -l` reader and the wide/narrow classifier. Node ids are
//! `§17::<n>` (fixed2-LUNCHROOM-17.json) and `§7::<n>`.

use super::*;

/// The `sudo -n -l` this host (sudo 1.9.15p5) prints for the account `alice` once its
/// sudoers had been changed on 2026-11-05 (about 23:00 UTC), byte for byte.
const HOST_OWN: &str = "Matching Defaults entries for alice on devhost:\n    env_reset, mail_badpass, secure_path=/usr/local/sbin\\:/usr/local/bin\\:/usr/sbin\\:/usr/bin\\:/sbin\\:/bin\\:/snap/bin, use_pty\n\nUser alice may run the following commands on devhost:\n    (ALL : ALL) ALL\n    (ALL) ALL\n    (svc) NOPASSWD: ALL\n    (root) NOPASSWD: /usr/local/sbin/send-report\n";

/// The `sudo -n -l` this host (sudo 1.9.15p5) printed for `alice` on 2026-11-05 before that change
/// (`(ALL) NOPASSWD: ALL`), byte for byte.
const HOST_OWN_EARLY: &str = "Matching Defaults entries for alice on devhost:\n    env_reset, mail_badpass, secure_path=/usr/local/sbin\\:/usr/local/bin\\:/usr/sbin\\:/usr/bin\\:/sbin\\:/bin\\:/snap/bin, use_pty\n\nUser alice may run the following commands on devhost:\n    (ALL : ALL) ALL\n    (ALL) NOPASSWD: ALL\n";

/// The `sudo -l -U svc` this host prints for the service account `svc`.
const HOST_SERVICE: &str = "Matching Defaults entries for svc on devhost:\n    env_reset, mail_badpass, secure_path=/usr/local/sbin\\:/usr/local/bin\\:/usr/sbin\\:/usr/bin\\:/sbin\\:/bin\\:/snap/bin, use_pty\n\nUser svc may run the following commands on devhost:\n    (svc) NOPASSWD: /opt/svc/node_modules/pm2/bin/pm2\n    (root) NOPASSWD: /usr/bin/systemctl restart nginx\n    (root) NOPASSWD: /usr/bin/systemctl restart pm2-svc.service\n    (root) NOPASSWD: /usr/bin/systemctl restart support.service\n    (root) NOPASSWD: /usr/bin/systemctl restart jobs.service\n    (root) NOPASSWD: /usr/bin/systemctl is-active *\n    (root) NOPASSWD: /usr/bin/systemctl start disk-pressure-prune.service\n";

fn root_file() -> Entry {
    Entry {
        owner: 0,
        mode: 0o755,
    }
}

/// Facts for a root-owned file under root-only folders.
fn tidy() -> PathFacts {
    PathFacts {
        file: Some(root_file()),
        above: vec![root_file(), root_file()],
        names: Vec::new(),
    }
}

fn listing_of(rules: &str) -> String {
    format!(
        "Matching Defaults entries for dev on host:\n    env_reset, use_pty\n\nUser dev may run the following commands on host:\n{rules}\n"
    )
}

fn verdict_with(
    text: &str,
    facts: &dyn Fn(&str) -> Option<PathFacts>,
    others: &dyn Fn(&str) -> Option<String>,
) -> Verdict {
    classify(
        &parse(text),
        "dev",
        &Sources {
            facts,
            others,
            lunchroom_ids: LunchroomIds::known(None, None),
        },
    )
}

/// The verdict of `text` where every file is root's and tidy and no other user is readable.
fn plain(text: &str) -> Verdict {
    verdict_with(text, &|_| Some(tidy()), &|_| None)
}

fn reasons(v: &Verdict) -> Vec<Reason> {
    v.findings.iter().flat_map(|f| f.reasons.clone()).collect()
}

// §7::58, §7::67 — the host's real `sudo -n -l`: the Defaults line and the two rules are
// read, and `(ALL) NOPASSWD: ALL` is the one password-free rule.
#[test]
fn the_host_listing_is_read_as_sudo_prints_it() {
    let l = parse(HOST_OWN_EARLY);
    assert_eq!(l.status, Status::Listed);
    assert_eq!(l.unparsed.len(), 0);
    assert_eq!(l.rules.len(), 2);
    assert_eq!(l.rules[0].run_as, ["ALL"]);
    assert_eq!(l.rules[0].groups, ["ALL"]);
    assert_eq!(l.rules[0].command, "ALL");
    assert!(!l.rules[0].password_free);
    assert!(l.rules[1].password_free);
    assert_eq!(l.rules[1].describe(), "(ALL) NOPASSWD: ALL");
    assert!(l.has_password_free());
}

// §7::58 — an account whose rules all ask for a password has no password-free rule.
#[test]
fn a_listing_with_only_password_rules_shows_no_password_free_rule() {
    let l = parse(&listing_of("    (ALL : ALL) ALL"));
    assert!(!l.has_password_free());
    assert_eq!(plain(&listing_of("    (ALL : ALL) ALL")).findings.len(), 0);
}

// §7::58 — `(root)` with several commands, a `!` negation and a tag that changes in the
// line: each command is its own rule; NOPASSWD holds from its tag to the next tag.
#[test]
fn tags_hold_until_the_next_tag_and_negations_grant_nothing() {
    let text = listing_of(
        "    (root) NOPASSWD: /usr/bin/systemctl restart nginx, /usr/bin/systemctl status nginx, !/usr/bin/systemctl restart sshd, PASSWD: /usr/bin/apt, NOPASSWD: /usr/bin/journalctl",
    );
    let l = parse(&text);
    let pairs: Vec<(String, bool, bool)> = l
        .rules
        .iter()
        .map(|r| (r.command.clone(), r.password_free, r.denies))
        .collect();
    assert_eq!(
        pairs,
        [
            ("/usr/bin/systemctl restart nginx".to_owned(), true, false),
            ("/usr/bin/systemctl status nginx".to_owned(), true, false),
            ("/usr/bin/systemctl restart sshd".to_owned(), true, true),
            ("/usr/bin/apt".to_owned(), false, false),
            ("/usr/bin/journalctl".to_owned(), true, false),
        ]
    );
    // The negated command is not a rule that grants: it is not judged.
    let described: Vec<String> = l.password_free().map(Rule::describe).collect();
    assert_eq!(described.len(), 3);
    assert_eq!(plain(&text).findings.len(), 3);
    assert!(!plain(&text).wide());
}

// §7::58 — `PASSWD:` and `NOPASSWD:` can be stacked with other tags.
#[test]
fn stacked_tags_and_options_are_not_part_of_the_command() {
    let l = parse(&listing_of(
        "    (root) NOPASSWD: SETENV: NOEXEC: CWD=/srv sha256:abcdef /usr/local/sbin/job",
    ));
    assert_eq!(l.rules.len(), 1);
    assert_eq!(l.rules[0].command, "/usr/local/sbin/job");
    assert!(l.rules[0].password_free);
}

// §7::58 — runas lists: users, groups, negations and `#0`.
#[test]
fn runas_lists_are_split_in_users_negations_and_groups() {
    let l = parse(&listing_of(
        "    (root, svc : staff, !wheel) NOPASSWD: /usr/local/bin/x\n    (ALL, !root) NOPASSWD: /usr/local/bin/y\n    (#0) NOPASSWD: /usr/local/bin/z",
    ));
    assert_eq!(l.rules[0].run_as, ["root", "svc"]);
    assert_eq!(l.rules[0].groups, ["staff", "!wheel"]);
    assert_eq!(l.rules[1].run_as, ["ALL"]);
    assert_eq!(l.rules[1].run_as_not, ["root"]);
    assert_eq!(l.rules[2].run_as, ["#0"]);
}

// §7::58 — an unescaped comma ends a command; an escaped one does not; a long line sudo
// wrapped continues the line above.
#[test]
fn escaped_commas_and_wrapped_lines_are_one_command_list() {
    let l = parse(&listing_of(
        "    (root) NOPASSWD: /usr/bin/echo a\\,b, /usr/bin/id,\n        /usr/bin/uptime",
    ));
    let commands: Vec<&str> = l.rules.iter().map(|r| r.command.as_str()).collect();
    assert_eq!(
        commands,
        ["/usr/bin/echo a\\,b", "/usr/bin/id", "/usr/bin/uptime"]
    );
    assert!(l.rules.iter().all(|r| r.password_free));
}

// §7::58 — `!authenticate` in the Defaults line makes every rule password-free; a later
// `authenticate` puts it back; a command-specific default does so for that command only.
#[test]
fn defaults_that_turn_authentication_off_make_rules_password_free() {
    let off = "Matching Defaults entries for dev on host:\n    env_reset, !authenticate\n\nUser dev may run the following commands on host:\n    (ALL : ALL) ALL\n";
    let l = parse(off);
    assert!(l.rules[0].password_free);
    assert!(l.has_password_free());
    let back = off.replace("!authenticate", "!authenticate, authenticate");
    assert!(!parse(&back).rules[0].password_free);
    let specific = "Matching Defaults entries for dev on host:\n    env_reset\n\nRunas and Command-specific defaults for dev:\n    Defaults!/usr/local/bin/x !authenticate\n\nUser dev may run the following commands on host:\n    (root) /usr/local/bin/x, /usr/local/bin/y\n";
    let l = parse(specific);
    assert!(l.rules[0].password_free);
    assert!(!l.rules[1].password_free);
}

// §7::58 — a refusal and a text that is no listing are told apart.
#[test]
fn refusals_and_unreadable_text_have_a_status_of_their_own() {
    assert_eq!(
        parse("sudo: a password is required\n").status,
        Status::PasswordRequired
    );
    assert_eq!(
        parse("Sorry, user dev may not run sudo on host.\n").status,
        Status::NotAllowed
    );
    assert_eq!(
        parse(&["User dev is not all", "owed to run sudo on host.\n"].concat()).status,
        Status::NotAllowed
    );
    assert!(matches!(
        parse("sudo: boom\n").status,
        Status::Unreadable(_)
    ));
    assert!(!parse("sudo: boom\n").known());
    assert!(parse(HOST_OWN_EARLY).known());
    assert!(!parse("sudo: a password is required\n").has_password_free());
}

// §17::68 (RootOrAny) — root with `ALL`, and any user with `ALL`, are each wide; root with
// one root-owned file nobody else can write is not wide by this condition.
#[test]
fn root_or_any_user_with_all_is_wide() {
    for rule in [
        "    (root) NOPASSWD: ALL",
        "    (ALL) NOPASSWD: ALL",
        "    (ALL : ALL) NOPASSWD: ALL",
        "    (ALL, !root) NOPASSWD: ALL",
        "    (#0) NOPASSWD: ALL",
        "    (root, svc) NOPASSWD: ALL",
    ] {
        let v = plain(&listing_of(rule));
        assert!(v.wide(), "{rule}");
        assert!(
            reasons(&v).contains(&Reason::RootOrAnyWithAll),
            "{rule}: {:?}",
            reasons(&v)
        );
    }
    let v = plain(&listing_of(
        "    (root) NOPASSWD: /usr/local/sbin/send-lead",
    ));
    assert!(!v.wide());
    assert!(!reasons(&v).contains(&Reason::RootOrAnyWithAll));
}

// §17::69 — a rule running as `lunchroom` is wide, with any command, also with the group
// part and in a list; `ALL` takes in `lunchroom` unless the rule takes it out.
#[test]
fn a_rule_running_as_lunchroom_is_wide() {
    for rule in [
        "    (lunchroom) NOPASSWD: /usr/local/sbin/job",
        "    (lunchroom : lunchroom) NOPASSWD: /usr/bin/id",
        "    (root, lunchroom) NOPASSWD: /usr/local/sbin/job",
        "    (ALL) NOPASSWD: /usr/local/sbin/job",
    ] {
        let v = plain(&listing_of(rule));
        assert!(v.wide(), "{rule}");
        assert!(reasons(&v).contains(&Reason::RunsAsLunchroom), "{rule}");
    }
    let v = plain(&listing_of(
        "    (ALL, !lunchroom) NOPASSWD: /usr/local/sbin/job",
    ));
    assert!(!v.wide(), "{:?}", v.wide_lines());
}

// §17::70 — a command with a wildcard is wide: in the arguments, in the path, with
// `?` and with `[`; an escaped wildcard is a literal character.
#[test]
fn a_command_with_a_wildcard_is_wide() {
    for rule in [
        "    (root) NOPASSWD: /usr/bin/systemctl is-active *",
        "    (root) NOPASSWD: /usr/local/sbin/*",
        "    (root) NOPASSWD: /usr/local/sbin/job-?",
        "    (root) NOPASSWD: /usr/local/sbin/job-[ab]",
        "    (root) NOPASSWD: /usr/bin/journalctl -u nginx*",
    ] {
        let v = plain(&listing_of(rule));
        assert!(reasons(&v).contains(&Reason::Wildcard), "{rule}");
        assert!(v.wide(), "{rule}");
    }
    let v = plain(&listing_of("    (root) NOPASSWD: /usr/bin/echo a\\*b"));
    assert!(!reasons(&v).contains(&Reason::Wildcard));
    assert!(!v.wide());
}

// §17::71 — each program the spec lists is wide when named by path, whatever its
// directory; `python3.11` and `vim.basic` are the same programs under their versioned names.
#[test]
fn each_listed_shell_and_runner_is_wide() {
    for name in RUNNERS {
        for dir in ["/usr/bin", "/bin", "/usr/local/bin"] {
            let rule = format!("    (root) NOPASSWD: {dir}/{name}");
            let v = plain(&listing_of(&rule));
            assert!(
                reasons(&v).contains(&Reason::Runner(name.to_owned())),
                "{rule}: {:?}",
                reasons(&v)
            );
            assert!(v.wide(), "{rule}");
        }
    }
    assert_eq!(RUNNERS.len(), 35);
    // The nine added in the review of 2026-11-06 are in the list by name, and each is wide.
    for name in [
        "gawk", "mawk", "busybox", "xargs", "nohup", "timeout", "chroot", "setpriv", "tee",
    ] {
        assert!(RUNNERS.contains(&name), "{name}");
        let v = plain(&listing_of(&format!(
            "    (root) NOPASSWD: /usr/bin/{name}"
        )));
        assert_eq!(reasons(&v), [Reason::Runner(name.to_owned())], "{name}");
    }
    for name in ["python3.11", "perl5.38.2", "vim.basic", "bash-5"] {
        let v = plain(&listing_of(&format!(
            "    (root) NOPASSWD: /usr/bin/{name}"
        )));
        assert!(v.wide(), "{name}");
    }
    // A program whose name only begins with a listed one is not that program.
    for name in ["lessecho", "envsubst", "viewer", "mandb", "findmnt"] {
        let v = plain(&listing_of(&format!(
            "    (root) NOPASSWD: /usr/bin/{name}"
        )));
        assert!(!v.wide(), "{name}: {:?}", v.wide_lines());
    }
}

// §17::71 — a link to a listed program is found through its name on disk
// (`/usr/bin/editor` -> `vim.basic`).
#[test]
fn a_link_to_a_listed_program_is_wide() {
    let facts = |_: &str| {
        Some(PathFacts {
            names: vec!["editor".to_owned(), "vim.basic".to_owned()],
            ..tidy()
        })
    };
    let v = verdict_with(
        &listing_of("    (root) NOPASSWD: /usr/bin/editor"),
        &facts,
        &|_| None,
    );
    assert!(reasons(&v).contains(&Reason::Runner("vim".to_owned())));
}

// §17::72 — the three kinds: a file not owned by root; a file or a folder above it that
// a user other than root can write; a path that is not absolute.
#[test]
fn a_file_not_owned_by_root_is_wide() {
    let facts = |_: &str| {
        Some(PathFacts {
            file: Some(Entry {
                owner: 1000,
                mode: 0o755,
            }),
            ..tidy()
        })
    };
    let v = verdict_with(
        &listing_of("    (root) NOPASSWD: /opt/app/run"),
        &facts,
        &|_| None,
    );
    assert!(reasons(&v).contains(&Reason::NotOwnedByRoot));
    assert!(v.wide());
}

#[test]
fn a_file_or_folder_a_user_other_than_root_can_write_is_wide() {
    let cases: [(Entry, Vec<Entry>); 7] = [
        // The file is group-writable.
        (
            Entry {
                owner: 0,
                mode: 0o775,
            },
            vec![root_file()],
        ),
        // The file is world-writable.
        (
            Entry {
                owner: 0,
                mode: 0o757,
            },
            vec![root_file()],
        ),
        // A folder above is group-writable.
        (
            root_file(),
            vec![
                root_file(),
                Entry {
                    owner: 0,
                    mode: 0o775,
                },
            ],
        ),
        // A folder above is world-writable (a sticky bit does not stop a user creating files).
        (
            root_file(),
            vec![Entry {
                owner: 0,
                mode: 0o1777,
            }],
        ),
        // A folder above is owned by a user.
        (
            root_file(),
            vec![Entry {
                owner: 1000,
                mode: 0o755,
            }],
        ),
        // The folder nearest the root is.
        (
            root_file(),
            vec![
                Entry {
                    owner: 5,
                    mode: 0o700,
                },
                root_file(),
            ],
        ),
        // The mode carries the file-type bits.
        (
            Entry {
                owner: 0,
                mode: 0o100_664,
            },
            vec![root_file()],
        ),
    ];
    for (i, (file, above)) in cases.into_iter().enumerate() {
        let facts = move |_: &str| {
            Some(PathFacts {
                file: Some(file),
                above: above.clone(),
                names: Vec::new(),
            })
        };
        let v = verdict_with(
            &listing_of("    (root) NOPASSWD: /usr/local/sbin/job"),
            &facts,
            &|_| None,
        );
        assert!(reasons(&v).contains(&Reason::WritableAbove), "case {i}");
        assert!(v.wide(), "case {i}");
    }
}

#[test]
fn a_path_that_is_not_absolute_is_wide() {
    for rule in [
        "    (root) NOPASSWD: backup",
        "    (root) NOPASSWD: ./backup",
        "    (root) NOPASSWD: bin/backup",
        "    (root) NOPASSWD: sudoedit /etc/hosts",
    ] {
        let v = plain(&listing_of(rule));
        assert!(reasons(&v).contains(&Reason::NotAbsolute), "{rule}");
        assert!(v.wide(), "{rule}");
    }
}

// §17::72 — a file that is not there is judged by the folders above it: root's alone,
// nobody can put a file there; a folder a user can write, anybody can.
#[test]
fn a_file_that_is_not_there_is_judged_by_its_folders() {
    let none_tidy = |_: &str| {
        Some(PathFacts {
            file: None,
            ..tidy()
        })
    };
    let v = verdict_with(
        &listing_of("    (root) NOPASSWD: /usr/local/sbin/later"),
        &none_tidy,
        &|_| None,
    );
    assert!(!v.wide());
    let none_open = |_: &str| {
        Some(PathFacts {
            file: None,
            above: vec![Entry {
                owner: 1000,
                mode: 0o755,
            }],
            names: Vec::new(),
        })
    };
    let v = verdict_with(
        &listing_of("    (root) NOPASSWD: /home/u/later"),
        &none_open,
        &|_| None,
    );
    assert!(v.wide());
}

// A path that cannot be examined, and a folder named as a command, cannot be called narrow.
#[test]
fn what_cannot_be_examined_is_not_narrow() {
    let v = verdict_with(
        &listing_of("    (root) NOPASSWD: /usr/local/sbin/job"),
        &|_| None,
        &|_| None,
    );
    assert!(v.wide());
    assert!(matches!(reasons(&v)[0], Reason::Unjudged(_)));
    let v = plain(&listing_of("    (root) NOPASSWD: /usr/local/sbin/"));
    assert!(reasons(&v).contains(&Reason::Folder));
    let v = plain(&listing_of("    (root) NOPASSWD: ???"));
    assert!(v.wide());
    // A line this parser does not understand is a rule it cannot judge.
    let text = "User dev may run the following commands on host:\n    (root NOPASSWD: /x\n";
    let l = parse(text);
    assert_eq!(l.unparsed.len(), 1);
    assert!(l.has_password_free());
    let v = classify(
        &l,
        "dev",
        &Sources {
            facts: &|_| Some(tidy()),
            others: &|_| None,
            lunchroom_ids: LunchroomIds::known(None, None),
        },
    );
    assert!(v.wide());
}

// §17::67 — the two narrow examples of the spec. `alice ALL=(root) NOPASSWD: <a file root
// owns that no other user can write>` is narrow; `alice ALL=(svc) NOPASSWD: ALL` is narrow where
// `svc` holds only narrow rules.
#[test]
fn the_narrow_examples_are_narrow() {
    let v = verdict_with(
        "User alice may run the following commands on h:\n    (root) NOPASSWD: /usr/local/sbin/send-report\n",
        &|path| {
            assert_eq!(path, "/usr/local/sbin/send-report");
            Some(tidy())
        },
        &|_| None,
    );
    assert!(v.password_free());
    assert!(!v.wide(), "{:?}", v.wide_lines());
    let narrow_service = "User svc may run the following commands on h:\n    (root) NOPASSWD: /usr/bin/systemctl restart nginx\n    (svc) NOPASSWD: /opt/svc/node_modules/pm2/bin/pm2\n    (ALL : ALL) ALL\n";
    let asked = std::cell::RefCell::new(Vec::new());
    {
        let others = |user: &str| {
            asked.borrow_mut().push(user.to_owned());
            Some(narrow_service.to_owned())
        };
        let v = verdict_with(
            "User alice may run the following commands on h:\n    (svc) NOPASSWD: ALL\n",
            &|_| Some(tidy()),
            &others,
        );
        assert!(v.password_free());
        assert!(!v.wide(), "{:?}", v.wide_lines());
    }
    assert_eq!(asked.into_inner(), ["svc"]);
}

// §17::73 — a rule running as a service account that holds a wide rule is wide: with this
// host's real service-account listing (`systemctl is-active *`), and for each kind of wide rule.
#[test]
fn a_service_account_holding_a_wide_rule_makes_the_rule_wide() {
    let own = "User alice may run the following commands on h:\n    (svc) NOPASSWD: ALL\n";
    let v = verdict_with(own, &|_| Some(tidy()), &|u| {
        (u == "svc").then(|| HOST_SERVICE.to_owned())
    });
    assert!(v.wide());
    assert_eq!(reasons(&v), [Reason::OtherUserWide("svc".to_owned())]);
    for wide_rule in [
        "(root) NOPASSWD: ALL",
        "(lunchroom) NOPASSWD: /usr/bin/id",
        "(root) NOPASSWD: /usr/bin/bash",
        "(root) NOPASSWD: /usr/bin/cat *",
    ] {
        let service = format!("User svc may run the following commands on h:\n    {wide_rule}\n");
        let v = verdict_with(
            "User alice may run the following commands on h:\n    (svc) NOPASSWD: /usr/local/bin/x\n",
            &|_| Some(tidy()),
            &|_| Some(service.clone()),
        );
        assert!(v.wide(), "{wide_rule}");
    }
    // The service account's rules that ask for a password are not wide ones.
    let service = "User svc may run the following commands on h:\n    (ALL : ALL) ALL\n";
    let v = verdict_with(
        "User alice may run the following commands on h:\n    (svc) NOPASSWD: ALL\n",
        &|_| Some(tidy()),
        &|_| Some(service.to_owned()),
    );
    assert!(!v.wide());
}

// §17::73 — a service account whose rules were not read, a group of users, and a chain
// of accounts that loops or runs too deep, are not called narrow.
#[test]
fn another_user_whose_rules_are_not_read_is_wide() {
    let own = "User alice may run the following commands on h:\n    (svc) NOPASSWD: ALL\n";
    let v = verdict_with(own, &|_| Some(tidy()), &|_| None);
    assert_eq!(reasons(&v), [Reason::OtherUserUnread("svc".to_owned())]);
    let v = plain("User alice may run the following commands on h:\n    (%ops) NOPASSWD: ALL\n");
    assert_eq!(reasons(&v), [Reason::OtherUserUnread("%ops".to_owned())]);
    // a -> b -> a: the loop adds nothing; both narrow, so narrow.
    let a = "User a may run the following commands on h:\n    (b) NOPASSWD: ALL\n";
    let b = "User b may run the following commands on h:\n    (a) NOPASSWD: ALL\n";
    let v = verdict_with(
        "User alice may run the following commands on h:\n    (a) NOPASSWD: ALL\n",
        &|_| Some(tidy()),
        &|u| Some(if u == "a" { a } else { b }.to_owned()),
    );
    assert!(!v.wide(), "{:?}", v.wide_lines());
    // a -> b, b holds a wide rule: wide, through two steps.
    let b_wide = "User b may run the following commands on h:\n    (root) NOPASSWD: ALL\n";
    let v = verdict_with(
        "User alice may run the following commands on h:\n    (a) NOPASSWD: ALL\n",
        &|_| Some(tidy()),
        &|u| Some(if u == "a" { a } else { b_wide }.to_owned()),
    );
    assert!(v.wide());
}

// §17::74 — the root step reads `sudo -l -U <user>` for each plain user name a
// password-free rule runs as, and for no other.
#[test]
fn the_users_to_read_are_the_plain_names_other_than_root_and_lunchroom() {
    let l = parse(&listing_of(
        "    (root) NOPASSWD: /usr/bin/id\n    (svc, www-data) NOPASSWD: /usr/bin/id\n    (lunchroom) NOPASSWD: ALL\n    (%ops) NOPASSWD: ALL\n    (ALL) NOPASSWD: /usr/bin/id\n    (dev) NOPASSWD: /usr/bin/id\n    (zed) PASSWD: ALL",
    ));
    assert_eq!(users_to_read(&l, "dev"), ["svc", "www-data"]);
}

// §7::68 — the answer is kept: what was read survives a round trip through the store's
// text, and the kept answer classifies as the fresh one does, the other users' text with it.
#[test]
fn the_kept_answer_round_trips_and_classifies() {
    let mut kept = Kept {
        read_at: 1_793_000_000,
        own: "User alice may run the following commands on h:\n    (svc) NOPASSWD: ALL\n"
            .to_owned(),
        others: BTreeMap::new(),
    };
    kept.others
        .insert("svc".to_owned(), HOST_SERVICE.to_owned());
    let back = Kept::from_json(&kept.to_json());
    assert_eq!(back.as_ref(), Some(&kept));
    assert!(kept.verdict("alice", &|_| Some(tidy())).wide());
    kept.others.insert(
        "svc".to_owned(),
        HOST_SERVICE.replace("is-active *", "is-active ssh"),
    );
    assert!(!kept.verdict("alice", &|_| Some(tidy())).wide());
    assert_eq!(Kept::from_json("not json"), None);
    assert_eq!(Kept::from_json("{\"read_at\": 1}"), None);
}

// §17.8 (lead's decision, 2026-11-05) — doctor prints a FAIL when the account's `sudo -n -l` differs
// from the kept one: the password-free rules decide, the Defaults wording and the order do not.
#[test]
fn a_changed_set_of_password_free_rules_differs_from_the_kept_answer() {
    let kept = Kept {
        read_at: 1,
        own: HOST_OWN_EARLY.to_owned(),
        others: BTreeMap::new(),
    };
    assert!(kept.same_rules_as(HOST_OWN_EARLY));
    assert!(kept.same_rules_as(&HOST_OWN_EARLY.replace("env_reset", "env_reset, lecture=never")));
    let swapped = HOST_OWN_EARLY.replace(
        "    (ALL : ALL) ALL\n    (ALL) NOPASSWD: ALL\n",
        "    (ALL) NOPASSWD: ALL\n    (ALL : ALL) ALL\n",
    );
    assert!(kept.same_rules_as(&swapped));
    let added = format!("{HOST_OWN_EARLY}    (root) NOPASSWD: /usr/bin/id\n");
    assert!(!kept.same_rules_as(&added));
    let removed = HOST_OWN_EARLY.replace("    (ALL) NOPASSWD: ALL\n", "");
    assert!(!kept.same_rules_as(&removed));
    assert!(!kept.same_rules_as("sudo: a password is required\n"));
}

// Cold first-time install, 2026-11-15: kept while sudo's credentials were cached, `sudo -n -l`
// listed rules none of which runs without a password; read later it says "a password is
// required". Those are the same rules; a password-free rule on either side is not.
#[test]
fn a_listing_with_no_password_free_rule_and_a_password_required_answer_are_the_same() {
    let listed = "Matching Defaults entries for dev on h:\n    env_reset\n\nUser dev may run the following commands on h:\n    (ALL : ALL) ALL\n    (lunchroom) /usr/local/lib/lunchroom/lunchroom console\n";
    let kept = Kept {
        read_at: 1,
        own: listed.to_owned(),
        others: BTreeMap::new(),
    };
    assert!(kept.same_rules_as("sudo: a password is required\n"));
    let asked = Kept {
        read_at: 1,
        own: "sudo: a password is required\n".to_owned(),
        others: BTreeMap::new(),
    };
    assert!(asked.same_rules_as(listed));
    let free = format!("{listed}    (root) NOPASSWD: /usr/bin/id\n");
    assert!(!asked.same_rules_as(&free));
}

// The host's own account: `(ALL) NOPASSWD: ALL` is wide; with the real service-account text the
// `svc` rule on this host holds `systemctl is-active *`.
#[test]
fn this_hosts_own_listing_is_wide() {
    let v = plain(HOST_OWN_EARLY);
    assert!(v.password_free());
    assert!(v.wide());
    let lines = v.wide_lines();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].starts_with("(ALL) NOPASSWD: ALL: "));
    let service = classify(
        &parse(HOST_SERVICE),
        "svc",
        &Sources {
            facts: &|_| Some(tidy()),
            others: &|_| None,
            lunchroom_ids: LunchroomIds::known(None, None),
        },
    );
    assert_eq!(service.findings.len(), 7);
    let wide: Vec<&str> = service
        .findings
        .iter()
        .filter(|f| !f.reasons.is_empty())
        .map(|f| f.rule.as_str())
        .collect();
    assert_eq!(wide, ["(root) NOPASSWD: /usr/bin/systemctl is-active *"]);
}

// facts_from_disk reads the owner and modes of a file, the folders above it and the links on
// the way; a missing file has no entry of its own.
#[test]
fn facts_are_read_from_the_disk_through_links() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = crate::test_target().join(format!("sudoers-facts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    assert!(std::fs::create_dir_all(root.join("real")).is_ok());
    assert!(
        std::fs::set_permissions(root.join("real"), std::fs::Permissions::from_mode(0o777)).is_ok()
    );
    assert!(std::fs::write(root.join("real/vim.basic"), "x").is_ok());
    assert!(std::os::unix::fs::symlink(root.join("real"), root.join("linked")).is_ok());
    assert!(std::os::unix::fs::symlink("vim.basic", root.join("real/editor")).is_ok());
    let path = root.join("linked/editor").display().to_string();
    let facts = facts_from_disk(&path);
    assert!(facts.is_some());
    let facts = facts.unwrap_or_default();
    assert!(facts.file.is_some());
    assert_eq!(facts.names[0], "editor");
    assert!(facts.names.contains(&"vim.basic".to_owned()));
    assert!(facts.above.iter().any(|e| e.mode & 0o777 == 0o777));
    // Nothing there: the folders are still read.
    let missing = facts_from_disk(&root.join("real/absent").display().to_string());
    assert!(
        missing
            .as_ref()
            .is_some_and(|f| f.file.is_none() && !f.above.is_empty())
    );
    // A loop of links is no facts.
    assert!(std::os::unix::fs::symlink(root.join("b"), root.join("a")).is_ok());
    assert!(std::os::unix::fs::symlink(root.join("a"), root.join("b")).is_ok());
    assert_eq!(facts_from_disk(&root.join("a").display().to_string()), None);
    // The classifier, run on the real facts, finds the world-writable folder.
    let v = verdict_with(
        &listing_of(&format!("    (root) NOPASSWD: {path}")),
        &facts_from_disk,
        &|_| None,
    );
    assert!(reasons(&v).contains(&Reason::WritableAbove));
    assert!(reasons(&v).contains(&Reason::Runner("vim".to_owned())));
    let _ = std::fs::remove_dir_all(&root);
}

// This host's `sudo -n -l` since 2026-11-05 (~23:00 UTC): `(ALL) ALL` asks for a password; the two
// password-free rules are `(svc) NOPASSWD: ALL` and `(root) NOPASSWD:` one root-owned file.
// With the service account's real listing (`systemctl is-active *`) the first is wide and the
// second narrow; with the service account narrow, neither is wide.
#[test]
fn this_hosts_listing_since_the_sudoers_change() {
    let l = parse(HOST_OWN);
    assert_eq!(l.status, Status::Listed);
    assert_eq!(l.rules.len(), 4);
    let free: Vec<String> = l.password_free().map(Rule::describe).collect();
    assert_eq!(
        free,
        [
            "(svc) NOPASSWD: ALL",
            "(root) NOPASSWD: /usr/local/sbin/send-report"
        ]
    );
    assert_eq!(users_to_read(&l, "alice"), ["svc"]);
    let facts = |_: &str| Some(tidy());
    let real = |u: &str| (u == "svc").then(|| HOST_SERVICE.to_owned());
    let v = verdict_with(HOST_OWN, &facts, &real);
    assert_eq!(v.findings.len(), 2);
    assert_eq!(
        v.findings[0].reasons,
        [Reason::OtherUserWide("svc".to_owned())]
    );
    assert_eq!(v.findings[1].reasons, []);
    let narrow_service = HOST_SERVICE.replace("is-active *", "is-active ssh");
    let narrow = |u: &str| (u == "svc").then(|| narrow_service.clone());
    assert!(!verdict_with(HOST_OWN, &facts, &narrow).wide());
}

// §17.8 — a rule is wide when it runs as user `lunchroom` or as group `lunchroom`: the three forms
// of the developer's `sudo -n -l`, each with a command that is narrow in every other way.
#[test]
fn a_rule_running_as_user_or_group_lunchroom_is_wide() {
    for (text, user, group) in [
        ("(lunchroom) NOPASSWD: /usr/bin/true", "lunchroom", false),
        ("(alice : lunchroom) NOPASSWD: /usr/bin/true", "alice", true),
        ("(: lunchroom) NOPASSWD: /usr/bin/true", "", true),
    ] {
        let listing = format!("User alice may run the following commands on h:\n    {text}\n");
        let l = parse(&listing);
        assert_eq!(l.rules.len(), 1, "{text}");
        assert_eq!(
            l.rules[0].groups.iter().any(|g| g == "lunchroom"),
            group,
            "{text}"
        );
        assert_eq!(
            l.rules[0].run_as.iter().any(|u| u == user),
            !user.is_empty(),
            "{text}"
        );
        let sources = Sources {
            facts: &|_| Some(tidy()),
            others: &|_| None,
            lunchroom_ids: LunchroomIds::known(None, None),
        };
        let v = classify(&l, "alice", &sources);
        assert_eq!(reasons(&v), [Reason::RunsAsLunchroom], "{text}");
        assert!(v.wide(), "{text}");
    }
    // The same command as another user and group is narrow.
    let narrow = "User alice may run the following commands on h:\n    (root : staff) NOPASSWD: /usr/bin/true\n";
    assert!(!verdict_with(narrow, &|_| Some(tidy()), &|_| None).wide());
}

// Fable M4 — sudoers(5) gives `Runas_Spec? Option_Spec* (Tag_Spec ':')* Cmnd`: options before tags.
// A rule written that way is password-free, with the option stripped from the command, and a shell
// under it is wide; options after tags, and options between tags, read the same; a tag this parser
// does not know makes the line one it cannot judge.
#[test]
fn options_before_tags_and_in_any_order_leave_the_tags_with_the_command() {
    for rule in [
        "    (root) CWD=/srv NOPASSWD: /usr/bin/sh",
        "    (root) TIMEOUT=60 NOTBEFORE=20260101 NOPASSWD: SETENV: /usr/bin/sh",
        "    (root) APPARMOR_PROFILE=p NOPASSWD: /usr/bin/sh",
        "    (root) NOPASSWD: CWD=/srv /usr/bin/sh",
        "    (root) NOPASSWD: CWD=/srv SETENV: /usr/bin/sh",
    ] {
        let l = parse(&listing_of(rule));
        assert_eq!(l.rules.len(), 1, "{rule}");
        assert_eq!(l.rules[0].command, "/usr/bin/sh", "{rule}");
        assert!(l.rules[0].password_free, "{rule}");
        let v = plain(&listing_of(rule));
        assert!(
            reasons(&v).contains(&Reason::Runner("sh".to_owned())),
            "{rule}"
        );
    }
    // A password-required rule with an option stays password-required.
    let l = parse(&listing_of("    (root) CWD=/srv PASSWD: /usr/bin/sh"));
    assert!(!l.rules[0].password_free);
    // An unknown tag is not skipped over silently.
    let v = plain(&listing_of("    (root) FUTURE_TAG: /usr/local/sbin/job"));
    assert!(v.wide(), "{:?}", v.findings);
}

// Fable H3 — a listing in a translated locale is not English text the parser knows: the account
// is not narrow. (The caller also runs sudo with `LC_ALL=C`.)
#[test]
fn a_translated_listing_is_wide_not_narrow() {
    let german = "Passende Vorgaben für alice auf h:\n    env_reset\n\nBenutzer alice darf die folgenden Befehle auf h ausführen:\n    (ALL) NOPASSWD: ALL\n";
    let l = parse(german);
    assert!(!l.known());
    let v = plain(german);
    assert!(v.password_free());
    assert!(v.wide());
    // Another user whose listing is such text makes the rule wide as well.
    let own = "User alice may run the following commands on h:\n    (svc) NOPASSWD: ALL\n";
    let v = verdict_with(own, &|_| Some(tidy()), &|_| Some(german.to_owned()));
    assert!(v.wide());
    // Empty output, and an error, are not listings either.
    assert!(plain("").wide());
    assert!(plain("sudo: unknown user alice\n").wide());
    // The two refusals sudo makes in English are listings of no rule.
    assert!(!plain("sudo: a password is required\n").wide());
}

// Fable M3 — group `ALL` and the numbers of the user and the group count as `lunchroom`, unless the
// rule takes `lunchroom` out.
#[test]
fn group_all_and_numeric_ids_are_lunchroom() {
    let with = |text: &str, ids: LunchroomIds| {
        let listing = format!("User dev may run the following commands on h:\n    {text}\n");
        let sources = Sources {
            facts: &|_| Some(tidy()),
            others: &|_| {
                Some(
                    "User x may run the following commands on h:\n    (ALL : ALL) ALL\n".to_owned(),
                )
            },
            lunchroom_ids: ids,
        };
        classify(&parse(&listing), "dev", &sources)
    };
    let ids = LunchroomIds::known(Some(990), Some(985));
    for text in [
        "(root : ALL) NOPASSWD: /usr/bin/true",
        "(root : lunchroom) NOPASSWD: /usr/bin/true",
        "(#990) NOPASSWD: /usr/bin/true",
        "(root : #985) NOPASSWD: /usr/bin/true",
        "(: #985) NOPASSWD: /usr/bin/true",
    ] {
        let v = with(text, ids);
        assert_eq!(reasons(&v), [Reason::RunsAsLunchroom], "{text}");
    }
    for text in [
        "(root : ALL, !lunchroom) NOPASSWD: /usr/bin/true",
        "(root : ALL, !#985) NOPASSWD: /usr/bin/true",
        "(root : #986) NOPASSWD: /usr/bin/true",
    ] {
        assert!(!with(text, ids).wide(), "{text}");
    }
    // Numbers that are not those of `lunchroom` on this system, or a system without the user, do
    // not match it.
    assert!(
        !with(
            "(root : #985) NOPASSWD: /usr/bin/true",
            LunchroomIds::known(Some(990), Some(1))
        )
        .wide()
    );
    assert!(
        !with(
            "(root : #985) NOPASSWD: /usr/bin/true",
            LunchroomIds::known(None, None)
        )
        .wide()
    );
}

// Fable U8-2 — every path sees an unreadable listing as the classifier does: it may hold a
// password-free rule, so `has_password_free` says yes, the statement of the install says what could
// not be read, and the canonical text carries a marker, so a kept unreadable answer is not the
// same as a kept answer with no rules.
#[test]
fn an_unreadable_listing_is_seen_as_possibly_password_free_everywhere() {
    let german = "Benutzer alice darf die folgenden Befehle auf h ausf\u{fc}hren:\n    (ALL) NOPASSWD: ALL\n";
    let l = parse(german);
    assert!(!l.known());
    assert!(l.has_password_free());
    assert!(
        l.canonical().contains("not a listing"),
        "{:?}",
        l.canonical()
    );
    let said = statement(&l);
    assert!(said.contains("was not a listing it can read"), "{said}");
    assert!(said.contains("K1.10"), "{said}");
    // A listing with no rules is not the same as one that could not be read.
    let none = parse(&listing_of("    (ALL : ALL) ALL"));
    assert!(none.known());
    assert!(!none.has_password_free());
    assert_ne!(none.canonical(), l.canonical());
    // The refusals sudo makes in English are listings of no rule, and stay so.
    assert!(!parse("sudo: a password is required\n").has_password_free());
}

// Fable U8-3 / M4 — options in a rule line, in the order sudoers(5) gives. `man sudoers` of sudo
// 1.9.15p5 (this host, `man sudoers | col -b`): the grammar, lines 651 and 655-669, is
// `Cmnd_Spec ::= Runas_Spec? Option_Spec* (Tag_Spec ':')* Cmnd` with `Chdir_Spec ::= 'CWD=directory'`
// and `AppArmor_Spec ::= 'APPARMOR_PROFILE=profile'`; the example rule lines, lines 786-793, are
// `alice ALL = (root) APPARMOR_PROFILE=my-profile ALL` and `bob ALL = (root)
// APPARMOR_PROFILE=foo//&bar /usr/bin/vi`. sudo prints a rule as its runas, options, tags and
// command, so the listing below is built from that grammar; it is NOT a capture. The container
// proof captures the real text, with this command, run as root with `u` an account in the
// container:
//   printf 'u ALL=(root) CWD=/srv APPARMOR_PROFILE=unconfined NOPASSWD: /usr/bin/vi\nu ALL=(root)
//   TIMEOUT=60 NOTBEFORE=20260101000000Z NOPASSWD: /bin/ls\n' > /etc/sudoers.d/capture &&
//   visudo -c -f /etc/sudoers.d/capture && su -s /bin/sh u -c 'LC_ALL=C sudo -n -l'
// and the text it prints replaces this fixture.
#[test]
fn a_listing_built_from_the_grammar_with_two_options_reads_as_password_free() {
    let text = "Matching Defaults entries for alice on devhost:\n    env_reset\n\nUser alice may run the following commands on devhost:\n    (root) CWD=/srv APPARMOR_PROFILE=unconfined NOPASSWD: /usr/bin/vi\n    (root) TIMEOUT=60 NOTBEFORE=20260101000000Z NOPASSWD: /bin/ls\n    (root) CWD=/srv /usr/bin/id\n";
    let l = parse(text);
    assert_eq!(l.unparsed.len(), 0);
    let rules: Vec<(String, bool)> = l
        .rules
        .iter()
        .map(|r| (r.command.clone(), r.password_free))
        .collect();
    assert_eq!(
        rules,
        [
            ("/usr/bin/vi".to_owned(), true),
            ("/bin/ls".to_owned(), true),
            ("/usr/bin/id".to_owned(), false),
        ]
    );
    // The editor is wide; the listing program is narrow; the rule that asks for a password is no
    // password-free rule at all.
    let v = plain(text);
    assert_eq!(v.findings.len(), 2);
    assert_eq!(v.findings[0].reasons, [Reason::Runner("vi".to_owned())]);
    assert_eq!(v.findings[1].reasons, []);
}

// §17.8 and A1.3 (review of 2026-11-06) — every branch of the statement says what its clause
// says, in its words, and none of them softens it: A1.3 "this confirmation proves no person is
// present and ... the passphrase check does not hold on that account (K1.10)"; §17.8 "the console's
// wall does not hold on this account". Text that is not a listing is counted as a wide rule, so its
// branch says the wall does not hold and that the install of the console can make sudo ask.
#[test]
fn each_branch_of_the_statement_uses_the_clauses_words() {
    let readable = statement(&parse(HOST_OWN));
    let unreadable = statement(&parse("sudo: boom\n"));
    for (branch, text) in [("listing", &readable), ("unreadable", &unreadable)] {
        for words in [
            "proves no person is present",
            "the passphrase check does not hold on this account (K1.10)",
        ] {
            assert!(text.contains(words), "{branch}: {words}: {text}");
        }
        for soft in [" may ", "might", "perhaps", "possibly"] {
            assert!(
                !text.contains(soft),
                "{branch}: softened by {soft:?}: {text}"
            );
        }
    }
    assert!(
        readable.contains("shows a rule that lets your account run a command without a password")
    );
    assert!(readable.contains("(root) NOPASSWD: /usr/local/sbin/send-report"));
    for words in [
        "was not a listing it can read",
        "counts this account as one with a rule that runs a command without a password",
        "The console's wall does not hold on this account",
        "`lunchroom install console` can make it ask",
    ] {
        assert!(unreadable.contains(words), "{words}: {unreadable}");
    }
}

/// A listing of exactly `size` bytes with one valid rule whose command has a long argument list.
fn listing_of_size(size: usize) -> String {
    let head = "User alice may run the following commands on h:\n    (root) NOPASSWD: /usr/bin/id ";
    let mut text = String::from(head);
    text.push_str(&"a".repeat(size - head.len() - 1));
    text.push('\n');
    assert_eq!(text.len(), size);
    text
}

// Codex ruling, item 5 — a listing of exactly the limit is parsed in full; one byte over is not read
// and counts as a listing that could not be read, so as wide; and hostile text of the limit's size,
// built to make the parser keep and copy as much as it can, keeps at most `MAX_UNPARSED` short lines
// and is parsed within a time bound.
#[test]
fn a_listing_is_read_up_to_the_limit_and_hostile_text_is_bounded() {
    let at_limit = listing_of_size(MAX_LISTING_BYTES);
    let l = parse(&at_limit);
    assert!(l.known());
    assert_eq!(l.rules.len(), 1);
    // Read in full, so judged by its rules: `/usr/bin/id` as root is narrow.
    let v = plain(&at_limit);
    assert!(v.password_free() && !v.wide());
    let over = listing_of_size(MAX_LISTING_BYTES + 1);
    let l = parse(&over);
    assert!(!l.known());
    assert_eq!(l.rules.len(), 0);
    let v = plain(&over);
    assert!(v.password_free() && v.wide());
    assert_eq!(
        parse(OVER_LIMIT_TEXT).status,
        Status::Unreadable(OVER_LIMIT_TEXT.trim().to_owned())
    );
    // Hostile: one rule line of unknown tags to the limit; many lines no rule starts; one very long
    // line of tags that each leave the rest of the line to be copied.
    let started = std::time::Instant::now();
    let head = "User alice may run the following commands on h:\n";
    let tags = format!(
        "{head}    (root) {}\n",
        "FOO: ".repeat((MAX_LISTING_BYTES - 200) / 5)
    );
    let lines = format!(
        "{head}{}",
        "    stray words\n".repeat((MAX_LISTING_BYTES - 200) / 16)
    );
    let long = format!(
        "{head}    (root) {}\n",
        "A: ".repeat((MAX_LISTING_BYTES - 200) / 3)
    );
    for hostile in [tags, lines, long] {
        assert!(hostile.len() <= MAX_LISTING_BYTES);
        let l = parse(&hostile);
        assert!(l.unparsed.len() <= MAX_UNPARSED, "{}", l.unparsed.len());
        assert!(l.unparsed.iter().all(|u| u.chars().count() <= 256));
        assert!(l.has_password_free());
        assert!(plain(&hostile).wide());
        assert!(l.canonical().len() <= MAX_UNPARSED * 262);
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
}

// Codex ruling, item 6 — the canonical line of an unreadable listing cannot be the canonical form of
// a readable one: a line the parser could not read that says what the marker says is a different
// state.
#[test]
fn the_unreadable_marker_cannot_collide_with_an_unparsed_line() {
    let readable = parse(
        "User alice may run the following commands on h:\n    the output was not a listing\n",
    );
    assert!(readable.known());
    assert_eq!(readable.unparsed, ["the output was not a listing"]);
    let unreadable = parse("sudo: boom\n");
    assert!(!unreadable.known());
    assert_ne!(readable.canonical(), unreadable.canonical());
}

// Codex ruling 3 — the `APPARMOR_PROFILE=` option has a test of its own. `man sudoers` of sudo
// 1.9.15p5 (line 659, `AppArmor_Spec ::= 'APPARMOR_PROFILE=profile'`; lines 786-793 give
// `APPARMOR_PROFILE=my-profile`, `=foo//&bar` and `=unconfined`). The option is taken off the
// command wherever it stands: before the tags, between them, after them, beside another option.
#[test]
fn the_apparmor_profile_option_is_taken_off_the_command_wherever_it_stands() {
    for (rule, command) in [
        (
            "(root) APPARMOR_PROFILE=my-profile NOPASSWD: /usr/bin/sh",
            "/usr/bin/sh",
        ),
        (
            "(root) APPARMOR_PROFILE=foo//&bar NOPASSWD: /usr/bin/vi",
            "/usr/bin/vi",
        ),
        (
            "(root) APPARMOR_PROFILE=unconfined NOPASSWD: SETENV: /bin/ls",
            "/bin/ls",
        ),
        (
            "(root) NOPASSWD: APPARMOR_PROFILE=unconfined /bin/ls",
            "/bin/ls",
        ),
        (
            "(root) NOPASSWD: SETENV: APPARMOR_PROFILE=p /bin/ls",
            "/bin/ls",
        ),
        (
            "(root) CWD=/srv APPARMOR_PROFILE=p NOPASSWD: /bin/ls",
            "/bin/ls",
        ),
        (
            "(root) APPARMOR_PROFILE=p CWD=/srv NOPASSWD: /bin/ls",
            "/bin/ls",
        ),
    ] {
        let l = parse(&listing_of(&format!("    {rule}")));
        assert_eq!(l.unparsed.len(), 0, "{rule}");
        assert_eq!(l.rules.len(), 1, "{rule}");
        assert_eq!(l.rules[0].command, command, "{rule}");
        assert!(l.rules[0].password_free, "{rule}");
    }
    // Where it is followed by `PASSWD:` the rule asks for a password.
    let l = parse(&listing_of(
        "    (root) APPARMOR_PROFILE=p PASSWD: /usr/bin/sh",
    ));
    assert!(!l.rules[0].password_free);
}

// Codex round 4, item 2 — the retention caps, at the cap and above it: 100 unparsed lines are all
// kept and a 101st is not, the first 100 stay in order; a line of 256 characters is kept whole and
// one of 257 is cut to 256; a cut falls on a character, not in the middle of a multi-byte one.
#[test]
fn the_unparsed_lines_kept_are_at_most_100_of_at_most_256_characters() {
    use std::fmt::Write as _;
    let head = "User alice may run the following commands on h:\n";
    let numbered = |n: usize| -> String {
        let mut lines = String::new();
        for i in 0..n {
            let _ = writeln!(lines, "    stray {i}");
        }
        format!("{head}{lines}")
    };
    let at_cap = parse(&numbered(MAX_UNPARSED));
    assert_eq!(at_cap.unparsed.len(), MAX_UNPARSED);
    let over = parse(&numbered(MAX_UNPARSED + 1));
    assert_eq!(over.unparsed.len(), MAX_UNPARSED);
    assert_eq!(over.unparsed[0], "stray 0");
    assert_eq!(
        over.unparsed[MAX_UNPARSED - 1],
        format!("stray {}", MAX_UNPARSED - 1)
    );
    assert_eq!(over.unparsed, at_cap.unparsed);
    for (len, kept) in [(255, 255), (256, 256), (257, 256), (1000, 256)] {
        let l = parse(&format!("{head}    {}\n", "x".repeat(len)));
        assert_eq!(l.unparsed.len(), 1);
        assert_eq!(l.unparsed[0].chars().count(), kept, "{len}");
    }
    // Two-byte characters: 300 of them are cut to 256 characters (512 bytes), valid text.
    let l = parse(&format!("{head}    {}\n", "\u{e9}".repeat(300)));
    assert_eq!(l.unparsed[0].chars().count(), 256);
    assert_eq!(l.unparsed[0].len(), 512);
    // The unreadable listing and the over-cap listing both count as password-free.
    assert!(over.has_password_free());
}

// Codex round 4, item 3 — the parse time on hostile text at the byte cap: the cases that would
// make a loop grow as a product (many scoped Defaults times many rules; many rules; long lines of
// tags) are cut off by the counts (`MAX_SCOPED_DEFAULTS`, `MAX_RULES`, `MAX_UNPARSED`), and each
// finishes within a time bound; text with more rules than `MAX_RULES` is not read.
#[test]
fn parse_time_is_bounded_on_hostile_text_at_the_byte_cap() {
    use std::fmt::Write as _;
    let head = "User alice may run the following commands on h:\n";
    let fill = |unit: &str, prefix: &str| -> String {
        let mut text = String::from(prefix);
        while text.len() + unit.len() < MAX_LISTING_BYTES {
            text.push_str(unit);
        }
        text
    };
    // 1: very many scoped defaults, then rules.
    let defaults = fill(
        "    Defaults!/usr/bin/x !authenticate\n",
        "Runas and Command-specific defaults for alice:\n",
    );
    let mut hostile: Vec<String> = vec![format!(
        "{}\n{head}    (root) NOPASSWD: /usr/bin/id\n",
        &defaults[..defaults.len() / 2]
    )];
    // 2: as many rules as fit.
    hostile.push(fill("    (root) /usr/bin/id\n", head));
    // 3: one very long line of ever-shorter rests.
    hostile.push(fill("A: ", &format!("{head}    (root) ")));
    // 4: many comma-separated commands on one line.
    hostile.push(fill("/bin/a, ", &format!("{head}    (root) NOPASSWD: ")));
    let started = std::time::Instant::now();
    for (i, text) in hostile.iter().enumerate() {
        assert!(text.len() <= MAX_LISTING_BYTES, "{i}");
        let one = std::time::Instant::now();
        let l = parse(text);
        assert!(
            one.elapsed() < std::time::Duration::from_secs(10),
            "{i}: {:?}",
            one.elapsed()
        );
        assert!(l.unparsed.len() <= MAX_UNPARSED, "{i}");
        assert!(l.rules.len() <= MAX_RULES, "{i}");
        // Classifying what was read is bounded too (the rules are at most MAX_RULES).
        let two = std::time::Instant::now();
        let _ = plain(text);
        assert!(
            two.elapsed() < std::time::Duration::from_secs(10),
            "{i}: {:?}",
            two.elapsed()
        );
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
    // More rules than the limit is a listing that could not be read; exactly the limit is read.
    let rules = |n: usize| -> String { format!("{head}{}", "    (root) /usr/bin/id\n".repeat(n)) };
    assert!(parse(&rules(MAX_RULES)).known());
    assert_eq!(parse(&rules(MAX_RULES)).rules.len(), MAX_RULES);
    let over = parse(&rules(MAX_RULES + 1));
    assert!(!over.known());
    assert_eq!(over.rules.len(), 0);
    assert!(plain(&rules(MAX_RULES + 1)).wide());
    // More scoped defaults than the limit make every rule password-free.
    let scoped = |n: usize| -> String {
        let mut defaults = String::new();
        for i in 0..n {
            let _ = writeln!(defaults, "    Defaults!/usr/bin/x{i} !authenticate");
        }
        format!(
            "Runas and Command-specific defaults for alice:\n{defaults}\n{head}    (root) /usr/bin/id\n"
        )
    };
    assert!(!parse(&scoped(1000)).rules[0].password_free);
    assert!(parse(&scoped(1001)).rules[0].password_free);
}

// Codex round 4, item 5 — the user names handed to `sudo -l -U` in the root step are login names or
// numbers: an empty name, white space, a leading `-` or `.`, shell characters, quotes, a backslash,
// a newline, a non-ASCII letter and a name over 64 characters are never passed on, and a rule that
// runs as one is taken as wide; duplicates are read once.
#[test]
fn only_valid_user_names_are_handed_to_the_root_step() {
    for good in [
        "svc",
        "www-data",
        "john.smith",
        "_apt",
        "a",
        "#1000",
        "#0",
        "svc$",
        "A1",
    ] {
        assert!(valid_user_name(good), "{good}");
    }
    let long = "a".repeat(65);
    for bad in [
        "",
        " ",
        "a b",
        " svc",
        "svc ",
        "-x",
        "-U",
        "--",
        ".hidden",
        "a;b",
        "a&b",
        "a|b",
        "$(id)",
        "`id`",
        "a'b",
        "a\"b",
        "a\\b",
        "a\nb",
        "a\tb",
        "a<b",
        "a>b",
        "a*b",
        "a?b",
        "a[b",
        "#",
        "#x",
        "#12345678901",
        "\u{e9}",
        "a/b",
        "a:b",
        "%grp",
        "+net",
        long.as_str(),
    ] {
        assert!(!valid_user_name(bad), "{bad:?}");
    }
    // users_to_read keeps the valid ones once each; the rule that runs as an invalid one is wide.
    let text = "User alice may run the following commands on h:\n    (svc, svc, -x, a b, www-data) NOPASSWD: /usr/bin/id\n    (svc) NOPASSWD: /usr/bin/id\n";
    let l = parse(text);
    assert_eq!(users_to_read(&l, "alice"), ["svc", "www-data"]);
    let asked = std::cell::RefCell::new(Vec::new());
    let others = |u: &str| {
        asked.borrow_mut().push(u.to_owned());
        Some("User x may run the following commands on h:\n    (ALL : ALL) ALL\n".to_owned())
    };
    let v = verdict_with(text, &|_| Some(tidy()), &others);
    assert!(v.wide());
    assert!(reasons(&v).contains(&Reason::OtherUserUnread("-x".to_owned())));
    assert!(reasons(&v).contains(&Reason::OtherUserUnread("a b".to_owned())));
    assert!(
        !asked.borrow().iter().any(|u| u == "-x" || u == "a b"),
        "{:?}",
        asked.borrow()
    );
}

// Codex round 4, limit D7 — the ids of `lunchroom` come from one state of `/etc/passwd` and
// `/etc/group`: each is read once per try between two looks at both files; where a file changed in
// between, the try is made again; where none held still the ids are not settled and a number in a
// rule counts as `lunchroom`'s.
#[test]
fn the_ids_of_lunchroom_are_read_from_one_state_of_the_two_files() {
    use std::cell::Cell;
    let passwd = "root:x:0:0:root:/root:/bin/sh\nlunchroom:x:990:985::/var/lib/lunchroom:/usr/sbin/nologin\n";
    let group = "root:x:0:\nlunchroom:x:985:\n";
    let read = |path: &str| {
        (if path.ends_with("passwd") {
            passwd
        } else {
            group
        })
        .to_owned()
    };
    // Still files: known ids, one try.
    let calls = Cell::new(0);
    let still = |_: &str| {
        calls.set(calls.get() + 1);
        Some((1, 2, 3, 4))
    };
    let ids = lunchroom_ids_with(&still, &read, 5);
    assert_eq!(ids, LunchroomIds::known(Some(990), Some(985)));
    assert_eq!(calls.get(), 4, "two looks at two files, once");
    // A file that changes during the first try only: the second try's reading stands.
    let looks = Cell::new(0);
    let changing_once = |_: &str| {
        looks.set(looks.get() + 1);
        Some((1, 2, i64::from(looks.get() == 3), 4))
    };
    let ids = lunchroom_ids_with(&changing_once, &read, 5);
    assert!(ids.settled);
    assert_eq!(ids.uid, Some(990));
    // Files that never hold still: not settled, no ids.
    let n = Cell::new(0);
    let moving = |_: &str| {
        n.set(n.get() + 1);
        Some((1, 2, n.get(), 4))
    };
    let ids = lunchroom_ids_with(&moving, &read, 3);
    assert!(!ids.settled);
    assert_eq!((ids.uid, ids.gid), (None, None));
    // Not settled: a number in a rule is `lunchroom`'s, by user and by group.
    let sources = Sources {
        facts: &|_| Some(tidy()),
        others: &|_| {
            Some("User x may run the following commands on h:\n    (ALL : ALL) ALL\n".to_owned())
        },
        lunchroom_ids: ids,
    };
    for rule in [
        "(#1234) NOPASSWD: /usr/bin/true",
        "(root : #4321) NOPASSWD: /usr/bin/true",
    ] {
        let text = format!("User alice may run the following commands on h:\n    {rule}\n");
        let v = classify(&parse(&text), "alice", &sources);
        assert!(reasons(&v).contains(&Reason::RunsAsLunchroom), "{rule}");
    }
    // Settled with other numbers: not.
    let sources = Sources {
        lunchroom_ids: LunchroomIds::known(Some(990), Some(985)),
        ..sources
    };
    let text = "User alice may run the following commands on h:\n    (root : #4321) NOPASSWD: /usr/bin/true\n";
    assert!(!classify(&parse(text), "alice", &sources).wide());
}
