//! Tests of GATED (build unit C2; spec §6 P1.4, P1.6, §3 R2.4, §7 A1.2): the recognition of a
//! merge, a deploy and a migration by words, the exemptions, what the guard cannot read to the
//! end, and the decision of every kind in every state. Failure classes: `FAILURES.md` of the unit
//! (rows A to K; ids are in the comments).

use std::os::unix::fs::PermissionsExt;

use super::*;
use crate::classify::gated::{known_programs, scan_code, scan_text};

const MODES: [HoldMode; 3] = [HoldMode::Strict, HoldMode::Balanced, HoldMode::Relaxed];
const YOLOS: [YoloSource; 3] = [
    YoloSource::Off,
    YoloSource::HarnessOnly,
    YoloSource::Lunchroom,
];

/// The words of `cmd` alone raise a GATED kind (no file system is read).
fn words(cmd: &str) -> bool {
    scan_text(cmd).words()
}

/// The HELD call raised GATED.
fn gated(h: &Fixture, cmd: &str) -> bool {
    h.call(true, cmd).kinds.contains(ProtectedKind::Gated)
}

/// Writes an executable file that is not a script.
fn compiled(h: &Fixture, rel: &str) {
    let p = h.home.join("project").join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap_or_default();
    }
    std::fs::write(&p, b"\x7fELF\x02\x01\x01\0\0\0\0\0\0\0\0\0").unwrap_or_default();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap_or_default();
}

/// Each row is `(bare form, form with an option and its separate value before the subcommand)`;
/// both are GATED (P1.6 tests: "each form has its own test row, and so does each form with an
/// option and its separate value before the subcommand").
const FORMS: &[(&str, &str)] = &[
    // C2-A01..A07 merge
    ("git merge main", "git -C dir merge main"),
    ("git merge main", "git -c user.name=x merge main"),
    ("git merge x", "git --git-dir d merge x"),
    ("git merge x", "git --work-tree w merge x"),
    ("git cherry-pick abc", "git -C d cherry-pick abc"),
    ("git am p.mbox", "git -C d am p.mbox"),
    ("git rebase main", "git -C d rebase main"),
    ("git pull", "git -C d pull"),
    ("git pull --ff-only", "git -C d pull --ff-only"),
    ("git pull --rebase", "git -C d pull --rebase"),
    ("git pull origin main", "git -C d pull origin main"),
    ("gh pr merge 1", "gh -R o/r pr merge 1"),
    ("gh pr merge 1", "gh --repo o/r pr merge 1"),
    ("glab mr merge 1", "glab -R g/p mr merge 1"),
    // K01: hub is read as git and as gh; a dashed git program is git with its subcommand
    ("hub merge x", "hub -C d merge x"),
    ("hub pr merge 1", "hub -R o/r pr merge 1"),
    ("git-merge x", "git-merge -s ours x"),
    ("git-pull", "git-pull --rebase"),
    // C2-A09..A12 deploy
    ("terraform apply", "terraform -chdir=d apply"),
    ("terraform destroy", "terraform -chdir d destroy"),
    ("pulumi up", "pulumi --cwd d up"),
    ("pulumi destroy", "pulumi -C d destroy"),
    (
        "kubectl apply -f x",
        "kubectl --request-timeout 5s apply -f x",
    ),
    ("kubectl create -f x", "kubectl -n prod create -f x"),
    ("kubectl replace -f x", "kubectl --context c replace -f x"),
    ("kubectl patch x", "kubectl --kubeconfig k patch x"),
    ("kubectl delete x", "kubectl -n prod delete x"),
    ("kubectl scale x", "kubectl -n prod scale x"),
    ("kubectl set image x", "kubectl -n prod set image x"),
    (
        "kubectl rollout restart x",
        "kubectl -n prod rollout restart x",
    ),
    ("helm install r c", "helm -n ns install r c"),
    ("helm upgrade r c", "helm --kube-context c upgrade r c"),
    ("helm rollback r 1", "helm --kube-context c rollback r 1"),
    ("helm uninstall r", "helm --kube-context c uninstall r"),
    ("git push origin main", "git -C d push origin main"),
    ("git push origin master", "git -C d push origin master"),
    (
        "git push origin production",
        "git -C d push origin production",
    ),
    ("git push origin prod", "git -C d push origin prod"),
    ("git push origin release", "git -C d push origin release"),
    (
        "git push origin release-1.2",
        "git -C d push origin release-1.2",
    ),
    ("git push --force", "git -C d push --force"),
    ("git push -f", "git -C d push -f"),
    (
        "git push --force-with-lease",
        "git -C d push --force-with-lease",
    ),
    (
        "ansible-playbook site.yml",
        "ansible-playbook -i inv site.yml",
    ),
    ("docker push img", "docker --context c push img"),
    // C2-A17..A23 migration
    ("alembic upgrade head", "alembic -c a.ini upgrade head"),
    (
        "prisma migrate deploy",
        "prisma --schema s.prisma migrate dev",
    ),
    ("sqlx migrate run", "sqlx -D url migrate run"),
    (
        "diesel migration run",
        "diesel --database-url u migration run",
    ),
    ("liquibase update", "liquibase --defaults-file f update"),
    ("goose up", "goose -dir d postgres url up"),
    ("dbmate up", "dbmate -d d up"),
];

// C2-A01..A23, K01: every form P1.4 lists is GATED by its words, bare and with an option and its
// separate value before the subcommand, and each is the kind it names.
#[test]
fn every_listed_form_is_gated_bare_and_with_an_option_before_the_subcommand() {
    for (bare, optioned) in FORMS {
        assert!(words(bare), "bare: {bare}");
        assert!(words(optioned), "with an option: {optioned}");
    }
    let kinds = |c: &str| {
        let g = scan_text(c);
        (g.merge, g.deploy, g.migrate)
    };
    assert_eq!(kinds("git -C d merge x"), (true, false, false));
    assert_eq!(kinds("gh -R o/r pr merge 1"), (true, false, false));
    assert_eq!(kinds("kubectl -n p apply -f x"), (false, true, false));
    assert_eq!(kinds("git push origin main"), (false, true, false));
    assert_eq!(kinds("alembic -c a upgrade head"), (false, false, true));
}

// C2-A13, A34, K05: a `git push` word as P1.4 compares it: an option with an attached value is the
// option, a cluster of short options is each of its letters, a refspec is its destination.
#[test]
fn a_push_is_compared_by_its_normalised_words() {
    for cmd in [
        "git push --force-with-lease=origin/x",
        "git push --force-with-lease=ref origin x",
        "git push -fu origin x",
        "git push -uf origin x",
        "git push origin +main",
        "git push origin HEAD:main",
        "git push origin main:main",
        "git push origin x:master",
        "git push origin refs/heads/main",
        "git push origin HEAD:refs/heads/production",
        "git push origin HEAD:release-2",
        "git push origin +refs/heads/prod",
    ] {
        assert!(words(cmd), "{cmd}");
    }
    for cmd in [
        "git push origin feature",
        "git push origin +feature",
        "git push origin HEAD:feature",
        "git push origin refs/heads/feature",
        "git push -u origin feature",
        "git push --delete origin x",
        "git push --force-if-includes origin x",
        "git push --tags",
        "git push origin mainline",
    ] {
        assert!(!words(cmd), "{cmd}");
    }
}

// C2-A25, K03, K02: near misses that are not GATED, and the exception words only as the only word.
#[test]
fn near_misses_are_not_gated() {
    for cmd in [
        "git merge --abort",
        "git cherry-pick --abort",
        "git cherry-pick --quit",
        "git am --abort",
        "git am --quit",
        "git rebase --abort",
        "git rebase --quit",
        "git rebase --skip",
        "git -C d merge --abort",
        "git pull --help",
        "git pull -h",
        "helm list",
        "helm -n ns list",
        "kubectl get pods",
        "kubectl -n prod get pods",
        "git status",
        "git log",
        "terraform plan",
        "pulumi preview",
        "docker ps",
        "alembic current",
        "prisma generate",
        "git push origin feature",
        "git -C d push origin feature",
        "diesel migration list",
        "gh pr view 1",
        "gh merge pr 1",
        "git commit -m \"deploy fix\"",
        "git commit -m \"migrate users\"",
        "echo migrate",
        "echo deploy",
    ] {
        assert!(!words(cmd), "{cmd}");
    }
    // K03: the exception is only the only word after the subcommand.
    for cmd in [
        "git merge --abort x --no-ff main",
        "git rebase --skip --onto x",
        "git am --skip",
        "git cherry-pick --skip",
        "git rebase --abort x",
    ] {
        assert!(words(cmd), "{cmd}");
    }
    // K02: two words count when both appear in order, not necessarily next to each other.
    assert!(words("gh pr view 1 --json merge"));
    assert!(words("gh pr x merge"));
    assert!(!words("gh merge pr 1"));
}

// C2-B01..B09, K06: the generic words, after quote removal, not a path, compared whole.
#[test]
fn the_generic_words_for_any_program() {
    for cmd in [
        "fly deploy",
        "serverless deploy --stage x",
        "cap production deploy",
        "npm run deploy",
        "npm run --silent deploy",
        "deploy",
        "git commit -m deploy",
        "rake db:migrate",
        "knex migrate:latest",
        "knex migration:run",
        "mix ecto.migrate",
        "x migrate",
        "x migrate:",
        "x migration:",
        "x rake.db.migrate",
        // Quote removal, every spelling.
        "git 'merge' main",
        "git \"merge\" main",
        "fly 'deploy'",
        "fly \"deploy\"",
        "fly de'ploy'",
        "fly dep\"\"loy",
        "fly d\\eploy",
        "fly \\deploy",
        "fly $'deploy'",
        "fly $'\\x64eploy'",
        "fly $'\\144eploy'",
        "fly \"de\"ploy",
        "fly d'e'p\"l\"oy",
        // K06: the removal of each expansion the guard cannot resolve leaves the word.
        "fly d${E}eploy",
        "fly dep${X}loy",
        "x mig${E}rate",
        "x ${E}migrate:latest",
    ] {
        assert!(words(cmd), "{cmd}");
    }
    for cmd in [
        "./deploy",
        "../deploy",
        "~deploy",
        ".deploy",
        "bin/deploy",
        "deploy/",
        "a/deploy",
        "fly deploy.sh",
        "fly deployment",
        "fly redeploy",
        "fly Deploy",
        "fly DEPLOY",
        "fly deploy-prod",
        "fly 'deploy fix'",
        "fly \"deploy \"",
        "fly ' deploy'",
        "fly deploy\\ x",
        "x migrations",
        "x migrated",
        "x migration",
        "x migrate_users",
        "x db:migrations",
        "x migrate-up",
        "x remigrate",
        "x 'migrate users'",
        "x .migrate",
        "x d${E}ployx",
        // The assignment's whole word is `X=deploy`.
        "X=deploy ls",
        "deploy=1 ls",
    ] {
        assert!(!words(cmd), "{cmd}");
    }
}

// C2-B11..B14, K04: a bare `--` of the command's own program ends its words; a wrapper's does
// not; a redirection operator and its operand are not words.
#[test]
fn the_bare_double_dash_and_redirections() {
    assert!(words("git merge -- x"));
    assert!(words("kubectl apply -- x"));
    assert!(!words("git -- merge"));
    assert!(!words("fly -- deploy"));
    assert!(!words("rm -- deploy"));
    assert!(!words("git x -- y -- merge"));
    for wrapper in ["sudo", "env", "nohup", "timeout 5", "xargs"] {
        assert!(words(&format!("{wrapper} -- git merge x")), "{wrapper}");
        // The wrapper's `--` ends its own words; the wrapped command's `--` ends its own.
        assert!(
            !words(&format!("{wrapper} -- fly x -- deploy")),
            "{wrapper}"
        );
        assert!(words(&format!("{wrapper} -- fly deploy -- x")), "{wrapper}");
    }
    assert!(!words("kubectl exec p -- fly x -- deploy"));
    assert!(words("kubectl exec p -- helm upgrade r c"));
    for cmd in [
        "git status > deploy",
        "git status >> migrate",
        "git status 2>deploy",
        "git status < deploy",
        "git status &> migrate",
        "git status > deploy 2> migrate",
    ] {
        assert!(!words(cmd), "{cmd}");
    }
}

// C2-D01..D19, K29: the exemptions belong to one command and to their positions.
#[test]
fn the_exemptions_are_per_command_and_by_position() {
    for cmd in [
        "kubectl apply --help",
        "kubectl apply -h",
        "helm upgrade --help",
        "helm upgrade -h",
        "git pull --help",
        "terraform apply -h",
        "helm --version",
        "git --version",
        "fly deploy --help",
        "deploy --help",
        "sudo -h",
        "env --help",
        "xargs --version",
        "echo deploy",
        "printf 'kubectl apply'",
        "cat deploy",
        "grep migrate x",
        "rg deploy",
        "man deploy",
        "less deploy",
        "head deploy",
        "tail deploy",
        "ls deploy",
        "which terraform apply",
        "type -a deploy",
        "/bin/echo deploy",
        "/usr/bin/cat deploy",
        "/usr/local/bin/ls deploy",
        "kubectl apply '--help'",
        "kubectl apply \"-h\"",
    ] {
        assert!(!words(cmd), "{cmd}");
    }
    for cmd in [
        // `--help` anywhere else is a word like any other.
        "helm upgrade r c --version 1.2.3",
        "helm install r c --version 1",
        "gh pr merge 1 --body -h",
        "git merge --version",
        "git push origin main --help",
        "kubectl apply -f x --server-side --field-manager -h",
        "kubectl apply -- --help",
        // Not the whole word.
        "kubectl apply --help=x",
        "kubectl apply --hel",
        "kubectl apply --versions",
        "kubectl apply -help",
        "kubectl apply -hv",
        "kubectl apply --help-all",
        "kubectl apply -hh",
        "kubectl apply --no-help",
        // The command has a subcommand and `--help` is not the first word after it.
        "helm --help upgrade r c",
        "deploy --version x",
        "helm --version 1 upgrade r c",
        "git -h merge x",
        "deploy x --help",
        // Per command: any command of the call can raise it.
        "kubectl apply -f x; ls --help",
        "ls --help; kubectl apply -f x",
        "kubectl apply --help && kubectl apply -f x",
        "gh pr merge 1; echo --version",
        // An exempt outer wrapper does not exempt the command it runs.
        "sudo --help git merge x",
        "sh -c 'kubectl apply -f x' --help",
        "bash -c 'x' -h; git merge y",
        // A path to a printing program outside the system folders is not that program.
        "./echo deploy",
        "/tmp/x/echo deploy",
        "~/bin/ls deploy",
        // The word of a wrapper's option value.
        "sudo -u deploy ls",
    ] {
        assert!(words(cmd), "{cmd}");
    }
}

// C2-C01..C08, C12, C13, K11..K13: each command of a compound call is read on its own, through
// the wrappers, runners, shells and substitutions P1.4 names, and the strings of code.
#[test]
fn compound_wrapped_and_handed_on_commands_are_read_on_their_own() {
    for cmd in [
        "echo a; gh pr merge 1",
        "gh pr merge 1 && echo ok",
        "ls | kubectl apply -f -",
        "true || terraform apply",
        "echo a\nhelm upgrade r c",
        "git merge x &",
        "nohup git merge x &",
        "sudo -u bob -g staff git merge x",
        "timeout 5 kubectl apply -f x",
        "timeout -s KILL 5 git merge x",
        "nice -n 5 git merge x",
        "env A=1 git merge x",
        "env -C d git merge x",
        "xargs -n1 terraform apply",
        "exec helm upgrade r c",
        "/usr/bin/git merge x",
        "cd d && git merge x",
        "sh -c 'git merge x'",
        "bash -lc 'kubectl apply -f x'",
        "env -S 'git merge x'",
        "su -c 'git merge x' bob",
        "echo $(kubectl apply -f x)",
        "echo `terraform apply`",
        "echo \"$(gh pr merge 1)\"",
        // K13: ssh, docker exec and run, kubectl exec, find -exec, watch, at.
        "ssh host 'kubectl apply -f x'",
        "ssh -p 22 host kubectl apply -f x",
        "docker exec c kubectl apply -f x",
        "docker run --rm img terraform apply",
        "kubectl exec p -- helm upgrade r c",
        "find . -exec git merge {} \\;",
        "watch -n 5 git merge x",
        // K14: an alias in the same call is the command it names.
        "alias m='git merge'; m x",
    ] {
        assert!(words(cmd), "{cmd}");
    }
    // Arithmetic is not a substitution.
    assert!(!scan_text("git commit -m \"$((1+1))\"").unread);
    assert!(scan_text("git commit -m \"$(date)\"").unread);
    // K12: the string literals of one list or call that starts a program are one command.
    for code in [
        "subprocess.run([\"git\", \"merge\", \"x\"])",
        "execFile('kubectl', ['apply', '-f', 'x'])",
        "Command::new(\"git\").arg(\"merge\")",
        "system(\"git\", \"merge\")",
        "os.system(\"terraform apply\")",
        "subprocess.run(\"git merge x\", shell=True)",
    ] {
        assert!(scan_code(code).words(), "{code}");
    }
    for code in [
        "subprocess.run([\"git\", \"status\"])",
        "print(\"merge\")",
        "x = [\"git\", \"log\"]; y = [\"merge\"]",
    ] {
        assert!(!scan_code(code).words(), "{code}");
    }
}

// C2-E12..E16, K10, K11, K13: something the guard cannot read to the end, as the words show it.
#[test]
fn what_is_built_at_run_time_cannot_be_read() {
    for cmd in [
        "$(echo deploy)",
        "echo $(date)",
        "ls `pwd`",
        "git commit -m \"$(date)\"",
        "eval \"$cmd\"",
        "eval ls",
        "command eval x",
        "builtin eval x",
        "env eval x",
        "$cmd",
        "${cmd}",
        "\"$cmd\" x",
        "$CMD merge",
        // A word of a command of a listed program.
        "git \"$sub\"",
        "kubectl \"$verb\" -f x",
        "git merge $BRANCH",
    ] {
        assert!(scan_text(cmd).unread, "{cmd}");
    }
    for cmd in ["ls $d", "cat \"$x\"", "echo $PATH", "git merge main", "ls"] {
        assert!(!scan_text(cmd).unread, "{cmd}");
    }
}

// C2-A, J01, J02, G22: the HELD call carries every kind, GATED among them; a push to `main` is
// both GATED and PROTECTED, a push to a feature branch only PROTECTED, a merge only GATED.
#[test]
fn the_call_carries_gated_with_every_other_kind() {
    let h = Fixture::new();
    let kinds = |cmd: &str| {
        let c = h.call(true, cmd);
        (
            c.kinds.contains(ProtectedKind::Gated),
            c.kinds.contains(ProtectedKind::PushOrPublish),
            c.kinds.contains(ProtectedKind::Delete),
        )
    };
    assert_eq!(kinds("git push origin main"), (true, true, false));
    assert_eq!(kinds("docker push img"), (true, true, false));
    assert_eq!(kinds("git push origin feature"), (false, true, false));
    assert_eq!(kinds("git merge x"), (true, false, false));
    assert_eq!(kinds("rm deploy"), (true, false, true));
    assert_eq!(kinds("rm -rf ./x"), (false, false, true));
    assert_eq!(kinds("git merge --abort"), (false, false, false));
    // A call GATED is never in the slip.
    assert_eq!(h.call(true, "git merge x").class, CallClass::Ordinary);
}

// C2-K19, P1.1: a reply and a read stay inside the slip whatever words they hold.
#[test]
fn a_word_in_a_reply_or_a_read_does_not_make_it_gated() {
    let h = Fixture::new();
    let program = h.path(".lunchroom/bin/lunchroom");
    for cmd in [
        format!("{program} send deploy hi"),
        format!("{program} send migrate hi"),
        format!("{program} send kimi deploy"),
        format!("{program} send kimi migrate"),
        format!("{program} read lr-00000001"),
    ] {
        let call = h.call(true, &cmd);
        assert!(!call.kinds.contains(ProtectedKind::Gated), "{cmd}");
    }
    // Any other call that holds the word is GATED: it is not a reply.
    assert!(gated(&h, "cat deploy; git merge x"));
}

// C2-E01..E02, E22, E26, K07, K08, K15: a compiled program not on the list, and a program of a
// listed name outside a system folder, cannot be read to the end while HELD, with or without a
// help word; not HELD, nothing is added.
#[test]
fn a_compiled_program_that_is_not_on_the_list_is_gated_while_held() {
    let h = Fixture::new();
    compiled(&h, "ship");
    compiled(&h, "git");
    compiled(&h, "echo");
    for cmd in [
        "./ship",
        "./ship --help",
        "./ship -h",
        "./ship --version",
        "sudo ./ship",
        "env ./ship",
        "timeout 5 ./ship",
        "nohup ./ship",
        "xargs ./ship",
        // K07: a program of a listed name, not in a system folder.
        "./git status",
        "./echo hi",
        // K10: a name that is no known program and no file.
        "frobnicate",
        "g merge",
    ] {
        assert!(gated(&h, cmd), "{cmd}");
        assert!(
            !h.call(false, cmd).kinds.contains(ProtectedKind::Gated),
            "{cmd} not held"
        );
        assert_eq!(h.bash(false, cmd), h.bash(false, cmd));
    }
    // Known programs, and Lunchroom's own program, are read by name.
    for cmd in [
        "ls",
        "git status",
        "cat x",
        "echo hi",
        "grep x y",
        "true",
        "cd /tmp && ls",
    ] {
        assert!(!gated(&h, cmd), "{cmd}");
    }
    // K15: a program that runs code it fetches or builds.
    for cmd in [
        "npx x",
        "bunx x",
        "npm exec x",
        "pnpm dlx x",
        "yarn dlx x",
        "cargo run",
        "cargo build",
        "go run x.go",
        "npm install",
        "npm ci",
        "yarn install",
        "pnpm install",
        "yarn",
    ] {
        assert!(gated(&h, cmd), "{cmd}");
    }
    assert!(!gated(&h, "cargo test"));
    // The same call, not HELD, runs whatever it holds.
    for cmd in ["./ship", "npx x", "git merge x"] {
        let call = h.call(false, cmd);
        assert_eq!(
            decide_call(
                call,
                Situation {
                    held: false,
                    yolo: YoloSource::Off,
                    approval_channel: true,
                    mode: HoldMode::Strict,
                }
            ),
            Decision::Run,
            "{cmd}"
        );
    }
}

// C2-E23, K09: a shell script is read; one that cannot be found or read is DENIED in every
// session, not GATED.
#[test]
fn a_script_is_read_and_a_missing_one_is_denied() {
    let h = Fixture::new();
    write(&h.home.join("project/ok.sh"), "#!/bin/sh\nls\n");
    write(
        &h.home.join("project/ship.sh"),
        "#!/bin/sh\nkubectl apply -f x\n",
    );
    write(
        &h.home.join("project/run.sh"),
        "#!/bin/sh\nexport C=ship\n$C\n",
    );
    for held in [false, true] {
        assert_eq!(
            h.bash(held, "sh nope.sh"),
            CallClass::Denied(DenyReason::Unparsed),
            "{held}"
        );
    }
    assert!(!gated(&h, "sh ok.sh"));
    assert!(gated(&h, "sh ship.sh"));
    assert!(gated(&h, "bash ship.sh"));
    assert!(gated(&h, "source ship.sh"));
}

// C2-F01..F20, K16..K18: the recipe of `make`, the script `npm`, `yarn` and `pnpm` run, and the
// targets of `just` are read, and the GATED words of the text decide; one that cannot be read, or
// that builds a command at run time, is something the guard cannot read to the end.
#[test]
fn make_npm_and_just_are_read_to_the_end() {
    let h = Fixture::new();
    let p = |rel: &str| h.home.join("project").join(rel);
    write(
        &p("mk/Makefile"),
        "default: build\nbuild:\n\techo hi\nrelease: build\n\t@kubectl apply -f x\nrt:\n\t$(CMD) x\n\
         sh:\n\techo $(shell date)\nsub:\n\t$(MAKE) -C d release\ninc:\n\techo ok\ninclude more.mk\n\
         rel2: pre\n\techo done\npre:\n\t-terraform apply\nc1: c2\n\techo 1\nc2: c1\n\techo 2\n",
    );
    write(&p("mk/more.mk"), "inc2:\n\t./nope\nmig:\n\tdbmate up\n");
    write(&p("mk/d/Makefile"), "release:\n\thelm upgrade r c\n");
    write(&p("mk2/Makefile"), "build:\n\techo hi\n");
    for (cmd, want) in [
        ("cd mk && make release", true),
        ("cd mk && make build", false),
        ("cd mk && make", false),
        ("cd mk && make rt", true),
        ("cd mk && make sh", true),
        ("cd mk && make sub", true),
        ("cd mk && make rel2", true),
        ("cd mk && make c1", false),
        ("cd mk && make mig", true),
        ("cd mk && make -C d release", true),
        ("make -C mk release", true),
        // `include more.mk` is the folder of the call's, where it is not: unreadable.
        ("make -f mk/Makefile build", true),
        ("make -f mk/Makefile release", true),
        ("cd mk && make nope", false),
        ("cd mk2 && make build", false),
        ("cd mk && make --help", false),
        ("cd mk && make --version", false),
        // No Makefile in the folder: it cannot be read.
        ("make build", true),
        (
            "cd mk/d && make nothing && cd .. && make x -f nothing.mk",
            true,
        ),
    ] {
        assert_eq!(gated(&h, cmd), want, "{cmd}");
    }
    write(
        &p("pk/package.json"),
        "{\"scripts\":{\"deploy\":\"kubectl apply -f x\",\"build\":\"echo hi\",\
         \"postbuild\":\"kubectl apply -f y\",\"test\":\"echo t\",\"a\":\"npm run b\",\"b\":\"echo ok\",\
         \"c\":\"npm run c\",\"v\":\"$CMD x\",\"d\":\"npm run deploy\",\"prelint\":\"helm upgrade r c\",\"lint\":\"echo l\"}}",
    );
    write(&p("pk2/package.json"), "not json");
    write(&p("pk3/package.json"), "{\"name\":\"x\"}");
    write(&p("pk4/package.json"), "{\"scripts\":{\"x\":5}}");
    for (cmd, want) in [
        ("cd pk && npm run deploy", true),
        ("cd pk && npm run --silent deploy", true),
        ("cd pk && npm run build", true),
        ("cd pk && npm run b", false),
        ("cd pk && npm run a", false),
        ("cd pk && npm run c", false),
        ("cd pk && npm run v", true),
        ("cd pk && npm run d", true),
        ("cd pk && npm run lint", true),
        ("cd pk && npm run-script b", false),
        ("cd pk && npm test", false),
        ("cd pk && pnpm run b", false),
        ("cd pk && pnpm run deploy", true),
        ("cd pk && yarn run b", false),
        ("cd pk && yarn b", false),
        ("cd pk && yarn deploy", true),
        ("npm run b --prefix pk", false),
        ("npm run build --prefix pk", true),
        // `pnpm <script>` is not a form the guard reads as a script.
        ("cd pk && pnpm b", false),
        // A package.json it cannot read to the end.
        ("cd pk2 && npm run x", true),
        ("cd pk3 && npm run x", true),
        ("cd pk4 && npm run x", true),
        ("cd mk2 && npm run x", true),
    ] {
        assert_eq!(gated(&h, cmd), want, "{cmd}");
    }
    write(
        &p("jf/justfile"),
        "default:\n    echo hi\n\nrelease: default\n    kubectl apply -f x\nrt:\n    {{x}} y\n\
         bt:\n    echo `date`\nsb:\n    #!/bin/sh\n    terraform apply\n",
    );
    for (cmd, want) in [
        ("cd jf && just", false),
        ("cd jf && just default", false),
        ("cd jf && just release", true),
        ("cd jf && just rt", true),
        ("cd jf && just bt", true),
        ("cd jf && just sb", true),
        ("cd jf && just --list", false),
        ("cd jf && just --version", false),
        ("just --justfile jf/justfile release", true),
        ("cd mk2 && just x", true),
    ] {
        assert_eq!(gated(&h, cmd), want, "{cmd}");
    }
}

// C2-C28, E10, F10, K28: hostile text is read in bounded time and never panics; what the reader
// cannot follow is something the guard cannot read to the end.
#[test]
fn hostile_text_is_bounded() {
    let deep = format!("{}{}", "echo $(".repeat(5_000), ")".repeat(5_000));
    assert!(scan_text(&deep).unread);
    let long_word = format!("fly {}", "a".repeat(1 << 20));
    assert!(!scan_text(&long_word).words());
    let many = vec!["echo a"; 10_000].join(" && ");
    assert!(!scan_text(&many).words());
    let last = format!("{many} && git merge x");
    assert!(scan_text(&last).words());
    let words = vec!["x"; 100_000].join(" ");
    assert!(!scan_text(&words).words());
    for odd in [
        "git merge \0 x",
        "git \u{202e}merge x",
        "\u{feff}git merge x",
        "fly deploy\0",
    ] {
        let _ = scan_text(odd);
    }
    assert!(scan_text("git 'merge").unread);
    assert!(scan_text("echo \"$(date").unread);
    let h = Fixture::new();
    assert!(gated(&h, &deep));
}

// C2-E03, E04, K-list: the table of known programs holds each category, and is published.
#[test]
fn the_list_of_known_programs_has_each_category() {
    let known = known_programs();
    for name in [
        // Shells, interpreters, wrappers and runners.
        "sh",
        "bash",
        "zsh",
        "python3",
        "node",
        "perl",
        "awk",
        "sudo",
        "env",
        "nohup",
        "timeout",
        "xargs",
        "busybox",
        "exec",
        "watch",
        "ssh",
        // Programs that run none of their arguments.
        "ls",
        "cat",
        "grep",
        "rg",
        "echo",
        "printf",
        "head",
        "tail",
        "which",
        "type",
        "man",
        "less",
        // Programs P1.4 and L1.1 name.
        "git",
        "gh",
        "glab",
        "hub",
        "kubectl",
        "helm",
        "terraform",
        "pulumi",
        "liquibase",
        "goose",
        "dbmate",
        "alembic",
        "prisma",
        "sqlx",
        "diesel",
        "docker",
        "make",
        "npm",
        "pnpm",
        "yarn",
        "just",
        "lunchroom",
        "ansible-playbook",
    ] {
        assert!(known.contains(&name), "{name}");
    }
    for name in ["ship", "frobnicate", "mywrap", "deploy", "migrate"] {
        assert!(!known.contains(&name), "{name}");
    }
    // No name twice, and none empty.
    let mut seen = std::collections::HashSet::new();
    for name in &known {
        assert!(!name.is_empty() && seen.insert(*name), "{name}");
    }
}

/// What the policy does with a call that is `kinds`, shown as `class`, written independently of
/// [`decide_call`]: the rules of P1.3, P1.4, P1.6, R2.4 and A1.2 as the spec words them.
fn expected(class: CallClass, kinds: &[ProtectedKind], s: Situation) -> Decision {
    use ProtectedKind::{Gated, GuardConfig, Reply, Unparsed};
    if let CallClass::Denied(r) = class {
        return Decision::Deny(r);
    }
    if !s.held {
        return Decision::Run;
    }
    let has = |k| kinds.contains(&k);
    let protected = kinds.iter().any(|k| !matches!(k, Gated | Reply));
    let covered = matches!(s.yolo, YoloSource::Lunchroom);
    let user = if has(GuardConfig)
        || has(Unparsed)
        || (has(Gated) && protected)
        || (has(Gated) && !covered)
        || (has(Reply) && matches!(s.mode, HoldMode::Strict))
    {
        true
    } else if protected {
        !matches!(s.mode, HoldMode::Relaxed)
    } else if matches!(class, CallClass::InSlip | CallClass::Protected(Reply)) {
        false
    } else {
        matches!(s.mode, HoldMode::Strict) && !covered
    };
    match (user, s.approval_channel) {
        (false, _) => Decision::Run,
        (true, true) => Decision::AskUser,
        (true, false) => Decision::Deny(DenyReason::NoApprovalChannel),
    }
}

// C2-G01..G24, K20, K21, K24: the decision for every kind of call in every state: HELD or not, each
// hold mode, no YOLO or the harness's own or a `lunchroom yolo`, with and without an approval
// channel. A GATED call goes to the user in every mode unless a `lunchroom yolo` covers the agent;
// a call both GATED and PROTECTED, and a command that cannot be parsed, go to the user whatever
// covers the agent; with no channel a call for the user is DENIED.
#[test]
#[allow(clippy::too_many_lines)] // one row per cell of the decision matrix
fn the_decision_matrix_of_gated_calls() {
    use ProtectedKind::{Delete, Gated, GuardConfig, Network, PushOrPublish, Reply, Unparsed};
    let calls: Vec<(&str, CallClass, Vec<ProtectedKind>)> = vec![
        ("in-slip read", CallClass::InSlip, vec![]),
        ("ordinary", CallClass::Ordinary, vec![]),
        ("GATED", CallClass::Ordinary, vec![Gated]),
        (
            "GATED and PROTECTED push",
            CallClass::Protected(PushOrPublish),
            vec![PushOrPublish, Gated],
        ),
        (
            "GATED and PROTECTED network",
            CallClass::Protected(Network),
            vec![Network, Gated],
        ),
        (
            "GATED and PROTECTED delete",
            CallClass::Protected(Delete),
            vec![Delete, Gated],
        ),
        (
            "GATED and guard configuration",
            CallClass::Protected(GuardConfig),
            vec![GuardConfig, Gated],
        ),
        (
            "GATED and DENIED",
            CallClass::Denied(DenyReason::Fence),
            vec![Gated],
        ),
        ("PROTECTED only", CallClass::Protected(Delete), vec![Delete]),
        ("unparsed", CallClass::Protected(Unparsed), vec![Unparsed]),
        ("reply", CallClass::InSlip, vec![Reply]),
        ("GATED reply", CallClass::Ordinary, vec![Reply, Gated]),
    ];
    let mut checked = 0;
    for (name, class, kinds) in &calls {
        let mut set = crate::policy::ProtectedKinds::NONE;
        for k in kinds {
            set = set.with(*k);
        }
        let call = Call {
            class: *class,
            kinds: set,
        };
        for held in [false, true] {
            for mode in MODES {
                for yolo in YOLOS {
                    for approval_channel in [true, false] {
                        let s = Situation {
                            held,
                            yolo,
                            approval_channel,
                            mode,
                        };
                        assert_eq!(
                            decide_call(call, s),
                            expected(*class, kinds, s),
                            "{name} {s:?}"
                        );
                        checked += 1;
                    }
                }
            }
        }
    }
    assert_eq!(checked, 12 * 2 * 3 * 3 * 2);
    // The spec's own examples, stated outright.
    let at = |mode, yolo, channel| Situation {
        held: true,
        yolo,
        approval_channel: channel,
        mode,
    };
    let gated_call = Call {
        class: CallClass::Ordinary,
        kinds: crate::policy::ProtectedKinds::NONE.with(Gated),
    };
    for mode in MODES {
        assert_eq!(
            decide_call(gated_call, at(mode, YoloSource::Off, true)),
            Decision::AskUser
        );
        assert_eq!(
            decide_call(gated_call, at(mode, YoloSource::HarnessOnly, true)),
            Decision::AskUser
        );
        assert_eq!(
            decide_call(gated_call, at(mode, YoloSource::Lunchroom, true)),
            Decision::Run
        );
        assert_eq!(
            decide_call(gated_call, at(mode, YoloSource::Off, false)),
            Decision::Deny(DenyReason::NoApprovalChannel)
        );
    }
    let both = Call {
        class: CallClass::Protected(PushOrPublish),
        kinds: crate::policy::ProtectedKinds::NONE
            .with(PushOrPublish)
            .with(Gated),
    };
    for mode in MODES {
        assert_eq!(
            decide_call(both, at(mode, YoloSource::Lunchroom, true)),
            Decision::AskUser
        );
    }
}

// C2-J04, K27, P1.4: a command the guard cannot parse is PROTECTED and, since Lunchroom cannot read
// it to the end, also GATED: it goes to the user in every hold mode and under every YOLO. A command
// that parses and is built at run time is GATED only, and runs under a covering `lunchroom yolo`.
#[test]
fn an_unparsable_command_is_gated_and_protected_and_a_built_one_gated_only() {
    let h = Fixture::new();
    for cmd in [
        "echo $PATH",
        "for f in a; do echo $f; done",
        "echo {a,b}",
        "cat <<E\nx\nE",
    ] {
        let call = h.call(true, cmd);
        assert!(call.kinds.contains(ProtectedKind::Unparsed), "{cmd}");
        assert!(call.kinds.contains(ProtectedKind::Gated), "{cmd}");
        for mode in MODES {
            for yolo in YOLOS {
                let s = Situation {
                    held: true,
                    yolo,
                    approval_channel: true,
                    mode,
                };
                assert_eq!(
                    decide_call(call, s),
                    Decision::AskUser,
                    "{cmd} {mode:?} {yolo:?}"
                );
            }
        }
        // Not HELD: nothing is added, whatever the call is.
        let free = Situation {
            held: false,
            yolo: YoloSource::Off,
            approval_channel: true,
            mode: HoldMode::Strict,
        };
        assert_eq!(
            decide_call(h.call(false, cmd), free),
            Decision::Run,
            "{cmd}"
        );
    }
    for cmd in ["$(echo deploy)", "eval \"$cmd\"", "$cmd", "ls `pwd`"] {
        let call = h.call(true, cmd);
        assert!(call.kinds.contains(ProtectedKind::Gated), "{cmd}");
        assert!(!call.kinds.contains(ProtectedKind::Unparsed), "{cmd}");
        for mode in MODES {
            let held = |yolo| Situation {
                held: true,
                yolo,
                approval_channel: true,
                mode,
            };
            assert_eq!(
                decide_call(call, held(YoloSource::Off)),
                Decision::AskUser,
                "{cmd}"
            );
            assert_eq!(
                decide_call(call, held(YoloSource::HarnessOnly)),
                Decision::AskUser,
                "{cmd}"
            );
            assert_eq!(
                decide_call(call, held(YoloSource::Lunchroom)),
                Decision::Run,
                "{cmd}"
            );
        }
    }
    // PROTECTED kinds of the commands read inside text built at run time still stand.
    let call = h.call(true, "rm -rf $(echo x)");
    assert!(call.kinds.contains(ProtectedKind::Delete));
    assert!(call.kinds.contains(ProtectedKind::Gated));
}

// C2-G22, J03, J01: the classifier's result carries GATED among the kinds, so the policy reads it.
#[test]
fn the_classified_calls_are_decided_with_gated() {
    let h = Fixture::new();
    compiled(&h, "ship");
    let cases: &[(&str, [Decision; 3], [Decision; 3])] = &[
        // (call, no YOLO in Strict, Balanced, Relaxed; `lunchroom yolo` in the same order)
        ("git merge x", [Decision::AskUser; 3], [Decision::Run; 3]),
        ("./ship", [Decision::AskUser; 3], [Decision::Run; 3]),
        ("$(echo deploy)", [Decision::AskUser; 3], [Decision::Run; 3]),
        ("eval \"$cmd\"", [Decision::AskUser; 3], [Decision::Run; 3]),
        (
            "terraform apply",
            [Decision::AskUser; 3],
            [Decision::Run; 3],
        ),
        ("dbmate up", [Decision::AskUser; 3], [Decision::Run; 3]),
        (
            "git push origin main",
            [Decision::AskUser; 3],
            [Decision::AskUser; 3],
        ),
        (
            "docker push img",
            [Decision::AskUser; 3],
            [Decision::AskUser; 3],
        ),
        ("git pull", [Decision::AskUser; 3], [Decision::AskUser; 3]),
        (
            "git merge --abort",
            [Decision::AskUser, Decision::Run, Decision::Run],
            [Decision::Run; 3],
        ),
    ];
    for (cmd, plain, yolo) in cases {
        let call = h.call(true, cmd);
        let under = |y: YoloSource| {
            MODES.map(|mode| {
                decide_call(
                    call,
                    Situation {
                        held: true,
                        yolo: y,
                        approval_channel: true,
                        mode,
                    },
                )
            })
        };
        assert_eq!(under(YoloSource::Off), *plain, "{cmd}");
        assert_eq!(under(YoloSource::HarnessOnly), *plain, "{cmd} harness yolo");
        if *cmd != "git merge --abort" {
            assert_eq!(under(YoloSource::Lunchroom), *yolo, "{cmd} lunchroom yolo");
        }
    }
}
