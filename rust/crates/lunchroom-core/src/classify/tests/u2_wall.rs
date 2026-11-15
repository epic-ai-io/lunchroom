//! Codex classes G (Lunchroom's paths), H (other processes) and I (terminals) of build unit U2.

use super::u2_enum::{bash, collect, guard_class, is_denied, report, strs, wall};
use super::*;

const ROOTS: [&str; 6] = [
    "/var/lib/lunchroom",
    "/var/lib/lunchroom-approvals",
    "/usr/local/lib/lunchroom",
    "/etc/lunchroom",
    "/etc/lunchroom/1000.verifier",
    "/etc/sudoers.d/zz-lunchroom-1000",
];

// G6, G7 — §17::58: a write to a path of Lunchroom's is DENIED in every session,
// whichever program writes it and whether the path is the root or a file below it.
#[test]
fn g6_g7_every_writer_is_denied_at_every_path_of_lunchroom() {
    let h = Fixture::new();
    let layout = wall(&h);
    let mut rows = Vec::new();
    for root in ROOTS {
        // The verifier and the rule file are files: nothing lies below them.
        let below = !root.contains("verifier") && !root.contains("zz-lunchroom");
        let mut paths = vec![root.to_owned()];
        if below {
            paths.push(format!("{root}/x"));
        }
        for p in paths {
            for w in [
                "echo x > {p}",
                "echo x >> {p}",
                "cat > {p}",
                "tee {p}",
                "printf x | tee -a {p}",
                "sudo tee {p}",
                "cp /tmp/a {p}",
                "mv /tmp/a {p}",
                "install /tmp/a {p}",
                "chmod 777 {p}",
                "chown bob {p}",
                "rm {p}",
                "rm -rf {p}",
                "truncate -s 0 {p}",
                "sed -i s/a/b/ {p}",
                "perl -i -pe 1 {p}",
                "ln -s /tmp/a {p}",
                "touch {p}",
                "mkdir {p}",
                "dd of={p}",
                "rsync -a /tmp/a {p}",
                "python3 -c \"open('{p}','w').write('x')\"",
                "perl -e 'open(F, \">{p}\")'",
                "sh -c 'echo x > {p}'",
            ] {
                rows.push(w.replace("{p}", &p));
            }
        }
    }
    let mut bad = Vec::new();
    collect(&h, &layout, "G6/G7", &rows, true, &mut bad);
    report(&bad);
}

// G8 — a path written with dot parts, doubled separators, a trailing slash, `..`, or relative to
// a folder the call changed to, is the path it names.
#[test]
fn g8_paths_of_lunchroom_are_read_by_the_file_they_name() {
    let h = Fixture::new();
    let layout = wall(&h);
    let mut rows = strs(&[
        "echo x > /etc/./lunchroom/x",
        "echo x > /etc/a/../lunchroom/x",
        "echo x > //etc//lunchroom//x",
        "echo x > /etc/lunchroom/",
        "echo x > /etc/lunchroom/.",
        "echo x > /etc/lunchroom/./x",
        "echo x > /var/lib/../lib/lunchroom/x",
        "echo x > /usr/local/lib/lunchroom/../lunchroom/x",
        "echo x > /etc/sudoers.d/./zz-lunchroom-1000",
        "echo x > /etc/sudoers.d/../sudoers.d/zz-lunchroom-1000",
        "cd /etc && echo x > lunchroom/x",
        "cd /etc/lunchroom && echo x > f",
        "cd /var/lib && echo x > lunchroom/f",
        "cd /var/lib/lunchroom && rm -rf .",
        "cd /etc && cp /tmp/a ./lunchroom/x",
        "pushd /etc/lunchroom && touch f",
        "cd /etc; echo x > lunchroom/1000.verifier",
        // A wildcard is read by the files it matches (L1.1; the shell leaves a pattern that
        // matches nothing as it is, so the file is one that exists: the fixture home holds a
        // store file); a brace is expanded before the path is looked at.
        "echo x > ~/.lunchro*/registry.json",
        "echo x > ~/.lunchroo[m]/registry.json",
        "echo x > ~/.lunchroo?/registry.json",
        "tee ~/.l*/registry.json",
        "echo x > ~/.{lunchroom,y}/f",
        "echo x > ~/{.lunchroom,y}/f",
        "echo x > ~/.lunch{room,x}/f",
        "cp /tmp/a ~/.lunchroom{,.bak}",
        "echo x > /etc/{lunchroom,x}/y",
        "echo x > /var/lib/{lunchroom,lunchroom-approvals}/y",
        "echo x > /etc/{a,b,lunchroom}/y",
        "echo x > /etc/{a,{b,lunchroom}}/y",
        "echo x > /etc/{x..z,lunchroom}/y",
    ]);
    // A path of Lunchroom's named with a dot before the write only.
    rows.extend(strs(&["tee /etc/lunchroom/../lunchroom/x < /dev/null"]));
    let mut bad = Vec::new();
    collect(&h, &layout, "G8", &rows, true, &mut bad);
    report(&bad);
}

// G9 — a link in the project that leads to a path of Lunchroom's makes a write through it a write
// of that path.
#[test]
fn g9_a_link_to_a_path_of_lunchroom_is_the_path() {
    let h = Fixture::new();
    let layout = wall(&h);
    for (name, target) in [
        ("lnk-etc", "/etc/lunchroom"),
        ("lnk-var", "/var/lib/lunchroom"),
        ("lnk-appr", "/var/lib/lunchroom-approvals"),
        ("lnk-lib", "/usr/local/lib/lunchroom"),
        ("lnk-ver", "/etc/lunchroom/1000.verifier"),
        ("lnk-rule", "/etc/sudoers.d/zz-lunchroom-1000"),
    ] {
        std::os::unix::fs::symlink(target, h.home.join("project").join(name)).unwrap_or_default();
    }
    // A link to a link.
    std::os::unix::fs::symlink("lnk-etc", h.home.join("project/lnk-2")).unwrap_or_default();
    let mut rows = Vec::new();
    for l in ["lnk-etc", "lnk-var", "lnk-appr", "lnk-lib", "lnk-2"] {
        rows.push(format!("echo x > {l}/f"));
        rows.push(format!("cp /tmp/a {l}/f"));
        rows.push(format!("rm -rf {l}"));
        rows.push(format!("cd {l} && touch f"));
    }
    for l in ["lnk-ver", "lnk-rule"] {
        rows.push(format!("echo x > {l}"));
        rows.push(format!("tee {l}"));
        rows.push(format!("rm {l}"));
    }
    let mut bad = Vec::new();
    collect(&h, &layout, "G9", &rows, true, &mut bad);
    report(&bad);
}

// G11 — a path that only begins with the name of one of Lunchroom's is not one of them.
#[test]
fn g11_a_path_that_only_begins_like_lunchroom_s_is_not_one() {
    let h = Fixture::new();
    let layout = wall(&h);
    let mut rows = Vec::new();
    for p in [
        "/etc/lunchroom.old/x",
        "/etc/lunchroom2",
        "/etc/lunchrooms/x",
        "/var/lib/lunchroomer/x",
        "/var/lib/lunchroom-other/x",
        "/var/lib/lunchrooms",
        "/usr/local/lib/lunchroom-other/x",
        "/etc/sudoers.d/90-wheel",
        "/etc/sudoers.d/lunchroom-1000",
    ] {
        rows.push(format!("echo x > {p}"));
        rows.push(format!("cat {p}"));
    }
    let mut bad = Vec::new();
    collect(&h, &layout, "G11", &rows, false, &mut bad);
    report(&bad);
}

// G12 — a path with a NUL, an empty path and a missing one are judged without a panic, and a NUL
// after a path of Lunchroom's does not hide it.
#[test]
fn g12_odd_paths_are_judged_without_a_panic_and_without_a_bypass() {
    let h = Fixture::new();
    let layout = wall(&h);
    let mut bad = Vec::new();
    for held in [false, true] {
        for path in [
            "/etc/lunchroom/\u{0}x",
            "/etc/lunchroom\u{0}",
            "/var/lib/lunchroom/\u{0}/x",
            "/usr/local/lib/lunchroom/lunchroom\u{0}.sh",
        ] {
            for (name, input) in [
                ("Write", json!({ "file_path": path, "content": "" })),
                (
                    "Edit",
                    json!({ "file_path": path, "old_string": "a", "new_string": "b" }),
                ),
            ] {
                let class = guard_class(&h, &layout, held, name, &input);
                if !is_denied(class) {
                    bad.push(format!("G12 held={held} {name} {path:?}: {class:?}"));
                }
            }
            let class = bash(&h, &layout, held, &format!("echo x > '{path}'"));
            if !is_denied(class) {
                bad.push(format!("G12 held={held} echo > {path:?}: {class:?}"));
            }
        }
        for path in ["", "/", "/nonexistent/deep/path/file", "relative", "."] {
            for name in ["Write", "Edit", "Read", "LS", "Grep"] {
                // Only that none of these panics.
                let _ = guard_class(
                    &h,
                    &layout,
                    held,
                    name,
                    &json!({ "file_path": path, "path": path, "content": "", "old_string": "a", "new_string": "b" }),
                );
            }
            let _ = bash(&h, &layout, held, &format!("echo x > '{path}'"));
        }
    }
    report(&bad);
}
