//! The text `make`, `npm`, `yarn`, `pnpm` and `just` run (spec §6 P1.4): the guard reads, to the
//! end, the recipe of `make` (the Makefile it takes, `-f` included, and every file that Makefile
//! includes), the script that `npm`, `yarn` or `pnpm` `run` (or `yarn <script>`) name in
//! `package.json` with that script's `pre` and `post` scripts, and the targets of `just`, and
//! applies the GATED words to that text. A recipe, script or target it reads to the end is not
//! unreadable for the rule; a file it cannot read is, and so is a command built at run time
//! inside one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::gated::{Gate, Seen, scan_code, subcommand};
use super::{Guard, SHELLS, basename};
use crate::fs::Script;
use crate::paths::absolute;

/// What a program that runs a recipe, a script or a target runs.
pub(super) enum Recipe {
    /// The program is not one of those.
    Not,
    /// What it runs cannot be read to the end.
    Unread,
    /// What it runs, read: the kinds its text holds.
    Read(Gate),
}

/// The most recipes, scripts and targets one call is followed into, and files it reads.
const MAX_FOLLOWED: usize = 200;

/// The longest chain of recipes, scripts and targets that call one another.
const MAX_CHAIN: usize = 8;

/// P1.4: a program that runs code it fetches or builds: `npx`, `npm exec`, `pnpm dlx`, `yarn dlx`,
/// `bunx`, `cargo run`, `cargo build`, `go run`, and `npm install` and `npm ci` with their `yarn`
/// and `pnpm` forms, which run package scripts.
pub(super) fn fetches(t: &[&str], p: usize) -> bool {
    let name = basename(t[p]);
    let args = &t[p + 1..];
    let sub = subcommand(args, 0).map(|(_, s)| s);
    match name {
        "npx" | "bunx" => true,
        "npm" | "pnpm" | "yarn" | "bun" => match sub {
            // `yarn` alone installs.
            None => {
                name == "yarn"
                    && args.iter().all(|a| {
                        a.starts_with('-') && *a != "--help" && *a != "-h" && *a != "--version"
                    })
            }
            Some(s) => {
                matches!(s, "exec" | "dlx" | "install" | "i" | "ci") || (name == "bun" && s == "x")
            }
        },
        "cargo" => matches!(sub, Some("run" | "build" | "r" | "b")),
        "go" => sub == Some("run"),
        _ => false,
    }
}

/// `yarn`'s own subcommands: any other word after `yarn` is a script.
const YARN_COMMANDS: [&str; 30] = [
    "add",
    "remove",
    "install",
    "init",
    "publish",
    "run",
    "workspace",
    "workspaces",
    "global",
    "cache",
    "config",
    "link",
    "unlink",
    "upgrade",
    "up",
    "version",
    "info",
    "list",
    "outdated",
    "why",
    "audit",
    "exec",
    "dlx",
    "bin",
    "pack",
    "login",
    "logout",
    "owner",
    "tag",
    "team",
];

impl Guard<'_> {
    /// What `make`, `npm run`, `yarn <script>`, `pnpm run` or `just` at `p` runs, read.
    pub(super) fn recipe(
        &self,
        t: &[&str],
        p: usize,
        here: &Path,
        depth: usize,
        seen: &mut Seen,
    ) -> Recipe {
        let name = basename(t[p]);
        let args = &t[p + 1..];
        let read = |g: Option<Gate>| g.map_or(Recipe::Unread, Recipe::Read);
        if !matches!(name, "make" | "gmake" | "just" | "npm" | "pnpm" | "yarn") {
            return Recipe::Not;
        }
        // Nothing runs: `--help`, `--version`.
        if args
            .iter()
            .any(|a| matches!(*a, "--help" | "-h" | "--version"))
            && name != "npm"
        {
            return Recipe::Read(Gate::default());
        }
        if depth >= MAX_CHAIN || seen.files > MAX_FOLLOWED {
            return Recipe::Unread;
        }
        match name {
            "make" | "gmake" => read(self.make(args, here, depth, seen)),
            "just" => read(self.just(args, here, depth, seen)),
            _ => match Self::script_names(name, args) {
                None => Recipe::Not,
                Some(scripts) if scripts.is_empty() => Recipe::Read(Gate::default()),
                Some(scripts) => read(self.package_scripts(args, &scripts, here, depth, seen)),
            },
        }
    }

    /// The scripts `npm`, `yarn` or `pnpm` run for `args`; `None` when they run none (another
    /// subcommand, or a shorthand the guard does not read: `pnpm <script>`).
    fn script_names(name: &str, args: &[&str]) -> Option<Vec<String>> {
        let (i, sub) = subcommand(args, 0)?;
        let after = |from: usize| subcommand(args, from).map(|(_, s)| s.to_owned());
        match (name, sub) {
            ("npm", "run" | "run-script" | "rum" | "urn") | ("pnpm" | "yarn", "run") => {
                Some(after(i + 1).into_iter().collect())
            }
            ("npm", "test" | "t" | "tst") | ("pnpm" | "yarn", "test") => Some(vec!["test".into()]),
            ("npm" | "pnpm", "start" | "stop" | "restart") => Some(vec![sub.to_owned()]),
            ("yarn", s) if !YARN_COMMANDS.contains(&s) => Some(vec![s.to_owned()]),
            _ => None,
        }
    }

    /// The folder a package runner works in: `--prefix`, `-C`, `--cwd` or `--dir`, else `here`.
    fn runner_dir(args: &[&str], here: &Path, home: &Path) -> PathBuf {
        for (i, a) in args.iter().enumerate() {
            if matches!(*a, "--prefix" | "-C" | "--cwd" | "--dir")
                && let Some(d) = args.get(i + 1)
            {
                return absolute(d, here, home);
            }
            if let Some(d) = a
                .strip_prefix("--prefix=")
                .or_else(|| a.strip_prefix("--cwd="))
                .or_else(|| a.strip_prefix("--dir="))
            {
                return absolute(d, here, home);
            }
        }
        here.to_path_buf()
    }

    fn package_scripts(
        &self,
        args: &[&str],
        scripts: &[String],
        here: &Path,
        depth: usize,
        seen: &mut Seen,
    ) -> Option<Gate> {
        let dir = Self::runner_dir(args, here, &self.layout.home);
        // The nearest `package.json` upward from the folder.
        let file = dir
            .ancestors()
            .map(|d| d.join("package.json"))
            .find(|f| self.fs.exists(f))?;
        seen.files += 1;
        let Script::Text(text) = self.fs.script(&file) else {
            return None;
        };
        let json: serde_json::Value =
            serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
        let table = json.get("scripts")?.as_object()?;
        let base = file.parent().unwrap_or(&dir).to_path_buf();
        let mut g = Gate::default();
        for s in scripts {
            let key = format!("npm:{}:{s}", file.display());
            if !seen.keys.insert(key) {
                continue;
            }
            for n in [format!("pre{s}"), s.clone(), format!("post{s}")] {
                let Some(body) = table.get(&n) else {
                    continue;
                };
                // A script value that is not a string cannot be read.
                let body = body.as_str()?;
                g = g.or(self.gated_judge(body, &base, depth + 1, seen));
            }
        }
        Some(g)
    }

    fn make(&self, args: &[&str], here: &Path, depth: usize, seen: &mut Seen) -> Option<Gate> {
        let home = &self.layout.home;
        let mut dir = here.to_path_buf();
        let mut file: Option<PathBuf> = None;
        let mut targets: Vec<String> = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let a = args[i];
            let value = |k: usize| args.get(k).map(|v| (*v).to_owned());
            if a == "--" {
                break;
            }
            if matches!(a, "-C" | "--directory") {
                dir = absolute(&value(i + 1)?, &dir, home);
                i += 2;
            } else if let Some(d) = a.strip_prefix("--directory=") {
                dir = absolute(d, &dir, home);
                i += 1;
            } else if matches!(a, "-f" | "--file" | "--makefile") {
                file = Some(PathBuf::from(value(i + 1)?));
                i += 2;
            } else if let Some(f) = a
                .strip_prefix("--file=")
                .or_else(|| a.strip_prefix("--makefile="))
            {
                file = Some(PathBuf::from(f));
                i += 1;
            } else if matches!(a, "-I" | "-j" | "-l" | "-o" | "-W" | "-E" | "--include-dir") {
                i += 2;
            } else if a.starts_with('-') || a.contains('=') {
                i += 1;
            } else {
                targets.push(a.to_owned());
                i += 1;
            }
        }
        let path = match file {
            Some(f) => absolute(&f.to_string_lossy(), &dir, home),
            None => ["GNUmakefile", "makefile", "Makefile"]
                .iter()
                .map(|n| dir.join(n))
                .find(|f| self.fs.exists(f))?,
        };
        let mut rules: Vec<Rule> = Vec::new();
        let mut default: Option<String> = None;
        self.parse_make(&path, &dir, 0, seen, &mut rules, &mut default)?;
        let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
        for (k, r) in rules.iter().enumerate() {
            for t in &r.targets {
                by_name.entry(t.as_str()).or_default().push(k);
            }
        }
        if targets.is_empty() {
            targets.extend(default);
        }
        let mut g = Gate::default();
        let mut visited: Vec<String> = Vec::new();
        let mut todo: Vec<String> = targets;
        while let Some(t) = todo.pop() {
            if visited.contains(&t) {
                continue;
            }
            visited.push(t.clone());
            let mut found = by_name.get(t.as_str()).cloned().unwrap_or_default();
            // A pattern rule `%.o: %.c` matches by its fixed parts.
            found.extend(rules.iter().enumerate().filter_map(|(k, r)| {
                r.targets
                    .iter()
                    .any(|p| p.contains('%') && pattern_matches(p, &t))
                    .then_some(k)
            }));
            for k in found {
                todo.extend(rules[k].deps.iter().cloned());
                for line in &rules[k].recipe {
                    let key = format!("make:{}:{line}", path.display());
                    if !seen.keys.insert(key) {
                        continue;
                    }
                    let shell = make_to_shell(line);
                    g = g.or(self.gated_judge(&shell, &dir, depth + 1, seen));
                }
            }
        }
        Some(g)
    }

    /// Reads a Makefile and, recursively, the files it includes. `None`: one cannot be read.
    #[allow(clippy::too_many_lines)] // one pass over the Makefile's lines, one arm per line kind
    fn parse_make(
        &self,
        path: &Path,
        dir: &Path,
        level: usize,
        seen: &mut Seen,
        rules: &mut Vec<Rule>,
        default: &mut Option<String>,
    ) -> Option<()> {
        seen.files += 1;
        if level > MAX_CHAIN * 4 || seen.files > MAX_FOLLOWED {
            return None;
        }
        let Script::Text(text) = self.fs.script(path) else {
            return None;
        };
        let mut current: Option<usize> = None;
        for line in &logical_lines(&text) {
            if line.starts_with('\t') {
                if let Some(k) = current {
                    let l = line.trim_start_matches(['\t', ' ', '@', '-', '+']);
                    if !l.is_empty() {
                        rules[k].recipe.push(l.to_owned());
                    }
                }
                continue;
            }
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            current = None;
            let word = l.split_whitespace().next().unwrap_or("");
            if matches!(word, "include" | "-include" | "sinclude") {
                for f in l.split_whitespace().skip(1) {
                    let inc = absolute(f, dir, &self.layout.home);
                    if self.fs.exists(&inc) {
                        self.parse_make(&inc, dir, level + 1, seen, rules, default)?;
                    } else if word == "include" {
                        return None;
                    }
                }
                continue;
            }
            if matches!(
                word,
                "define"
                    | "endef"
                    | "ifeq"
                    | "ifneq"
                    | "ifdef"
                    | "ifndef"
                    | "else"
                    | "endif"
                    | "export"
                    | "unexport"
                    | "override"
                    | "vpath"
                    | "private"
                    | "undefine"
            ) {
                continue;
            }
            let colon = l.find(':');
            let equals = l.find('=');
            let assignment = match (colon, equals) {
                (_, None) => false,
                (None, Some(_)) => true,
                (Some(c), Some(e)) => e < c || (e == c + 1) || l[c..].starts_with("::="),
            };
            if assignment {
                if let Some(v) = l.strip_prefix(".DEFAULT_GOAL")
                    && let Some((_, v)) = v.split_once('=')
                {
                    *default = Some(v.trim().to_owned());
                }
                continue;
            }
            let Some(c) = colon else {
                continue;
            };
            let targets: Vec<String> = l[..c].split_whitespace().map(str::to_owned).collect();
            let mut rest = l[c + 1..].trim_start_matches(':');
            // A static pattern rule `targets: pattern: deps` has the dependencies after the
            // second colon.
            if let Some(c2) = rest.find(':')
                && !rest[..c2].contains(';')
            {
                rest = &rest[c2 + 1..];
            }
            let (deps_part, inline) = rest.split_once(';').unwrap_or((rest, ""));
            let deps: Vec<String> = deps_part
                .split(['|', ' ', '\t'])
                .filter(|d| !d.is_empty())
                .map(str::to_owned)
                .collect();
            if default.is_none()
                && let Some(t) = targets
                    .iter()
                    .find(|t| !t.starts_with('.') && !t.contains('%'))
            {
                *default = Some(t.clone());
            }
            let mut recipe = Vec::new();
            let inline = inline.trim_start_matches(['@', '-', '+', ' ']);
            if !inline.is_empty() {
                recipe.push(inline.to_owned());
            }
            rules.push(Rule {
                targets,
                deps,
                recipe,
            });
            current = Some(rules.len() - 1);
        }
        Some(())
    }

    fn just(&self, args: &[&str], here: &Path, depth: usize, seen: &mut Seen) -> Option<Gate> {
        let home = &self.layout.home;
        let mut dir = here.to_path_buf();
        let mut file: Option<PathBuf> = None;
        let mut wanted: Vec<String> = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let a = args[i];
            if matches!(
                a,
                "--list"
                    | "-l"
                    | "--summary"
                    | "--dump"
                    | "--fmt"
                    | "--init"
                    | "--evaluate"
                    | "--show"
                    | "-s"
                    | "--choose"
                    | "--variables"
                    | "--completions"
            ) {
                // Nothing runs.
                return Some(Gate::default());
            }
            if matches!(a, "--justfile" | "-f") {
                file = Some(PathBuf::from(*args.get(i + 1)?));
                i += 2;
            } else if matches!(a, "--working-directory" | "-d" | "--dir") {
                dir = absolute(args.get(i + 1)?, &dir, home);
                i += 2;
            } else if a == "--set" {
                i += 3;
            } else if a.starts_with('-') {
                i += 1;
            } else {
                wanted.push(a.to_owned());
                i += 1;
            }
        }
        let path = match file {
            Some(f) => absolute(&f.to_string_lossy(), &dir, home),
            None => dir
                .ancestors()
                .flat_map(|d| ["justfile", "Justfile", ".justfile"].map(|n| d.join(n)))
                .find(|f| self.fs.exists(f))?,
        };
        seen.files += 1;
        let Script::Text(text) = self.fs.script(&path) else {
            return None;
        };
        let recipes = parse_just(&text);
        if wanted.is_empty() {
            wanted.extend(recipes.first().map(|r| r.name.clone()));
        }
        let mut g = Gate::default();
        let base = path.parent().unwrap_or(&dir).to_path_buf();
        let mut visited: Vec<String> = Vec::new();
        let mut todo: Vec<String> = wanted.into_iter().take(1).collect();
        while let Some(n) = todo.pop() {
            if visited.contains(&n) {
                continue;
            }
            visited.push(n.clone());
            // A name that is no recipe is a parameter value or an error: nothing to read.
            let Some(r) = recipes.iter().find(|r| r.name == n) else {
                continue;
            };
            todo.extend(r.deps.iter().cloned());
            if r.body.is_empty() {
                continue;
            }
            let shebang = r.body.first().is_some_and(|l| l.starts_with("#!"));
            let text = r.body.join("\n");
            if shebang
                && !r.body[0]
                    .split_whitespace()
                    .any(|w| SHELLS.contains(&basename(w)))
            {
                g = g.or(Gate {
                    unread: false,
                    ..scan_code(&text)
                });
                continue;
            }
            for line in if shebang { vec![text] } else { r.body.clone() } {
                g = g.or(self.gated_judge(&just_to_shell(&line), &base, depth + 1, seen));
            }
        }
        Some(g)
    }
}

/// The lines of a Makefile, each backslash continuation joined to the line it continues.
fn logical_lines(text: &str) -> Vec<String> {
    let mut logical: Vec<String> = Vec::new();
    let mut pending = String::new();
    for raw in text.lines() {
        if let Some(stripped) = raw.strip_suffix('\\') {
            pending.push_str(stripped);
            pending.push(' ');
            continue;
        }
        pending.push_str(raw);
        logical.push(std::mem::take(&mut pending));
    }
    if !pending.is_empty() {
        logical.push(pending);
    }
    logical
}

/// One rule of a Makefile.
struct Rule {
    targets: Vec<String>,
    deps: Vec<String>,
    recipe: Vec<String>,
}

/// `pattern` (holding one `%`) matches `name`.
fn pattern_matches(pattern: &str, name: &str) -> bool {
    pattern.split_once('%').is_some_and(|(pre, post)| {
        name.len() >= pre.len() + post.len() && name.starts_with(pre) && name.ends_with(post)
    })
}

/// The make functions: a call of one is a value computed when make runs.
const MAKE_FUNCTIONS: [&str; 5] = ["call", "eval", "foreach", "file", "let"];

/// A recipe line as the shell sees it: make's `$$` is a shell `$`, `$(MAKE)` is `make`,
/// `$(shell cmd)` is a command substitution, the make functions that run something are
/// substitutions, and every other make variable or automatic variable is data.
fn make_to_shell(line: &str) -> String {
    let cs: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < cs.len() {
        if cs[i] != '$' {
            out.push(cs[i]);
            i += 1;
            continue;
        }
        match cs.get(i + 1) {
            Some('$') => {
                out.push('$');
                i += 2;
            }
            Some(&open @ ('(' | '{')) => {
                let close = if open == '(' { ')' } else { '}' };
                let mut depth = 1_usize;
                let mut j = i + 2;
                while j < cs.len() && depth > 0 {
                    if cs[j] == open {
                        depth += 1;
                    } else if cs[j] == close {
                        depth -= 1;
                    }
                    j += 1;
                }
                let end = if depth == 0 { j - 1 } else { cs.len() };
                let inner: String = cs[i + 2..end.min(cs.len())].iter().collect();
                let first = inner.split_whitespace().next().unwrap_or("");
                if inner == "MAKE" {
                    out.push_str("make");
                } else if first == "shell" && inner.len() > 5 {
                    out.push_str("$(");
                    out.push_str(&inner[5..]);
                    out.push(')');
                } else if MAKE_FUNCTIONS.contains(&first) {
                    out.push_str("$( )");
                } else {
                    // A variable, or a make function that only computes text: data.
                    out.push_str("$_MK");
                }
                i = if depth == 0 { j } else { cs.len() };
            }
            Some('@' | '<' | '^' | '?' | '*' | '%' | '+' | '|') => {
                out.push_str("$_MK");
                i += 2;
            }
            _ => {
                out.push('$');
                i += 1;
            }
        }
    }
    out
}

/// One recipe of a justfile.
struct JustRecipe {
    name: String,
    deps: Vec<String>,
    body: Vec<String>,
}

/// The recipes of a justfile, in order.
fn parse_just(text: &str) -> Vec<JustRecipe> {
    let mut out: Vec<JustRecipe> = Vec::new();
    let mut in_body = false;
    for raw in text.lines() {
        if raw.starts_with([' ', '\t']) {
            if in_body
                && !raw.trim().is_empty()
                && let Some(r) = out.last_mut()
            {
                r.body.push(raw.trim().to_owned());
            }
            continue;
        }
        let l = raw.trim();
        if l.is_empty() {
            continue;
        }
        in_body = false;
        if l.starts_with('#') || l.starts_with('[') {
            continue;
        }
        let word = l.split_whitespace().next().unwrap_or("");
        if matches!(
            word,
            "set" | "import" | "mod" | "alias" | "export" | "unexport"
        ) || l.contains(":=")
        {
            continue;
        }
        let Some((head, deps)) = l.split_once(':') else {
            continue;
        };
        let name = head
            .trim_start_matches('@')
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_owned();
        if name.is_empty() {
            continue;
        }
        let deps = deps
            .split_whitespace()
            .map(|d| d.trim_matches(['(', ')']).to_owned())
            .filter(|d| d != "&&" && !d.is_empty())
            .collect();
        out.push(JustRecipe {
            name,
            deps,
            body: Vec::new(),
        });
        in_body = true;
    }
    out
}

/// A justfile recipe line as the shell sees it: `{{...}}` is a value computed when just runs, a
/// leading `@` or `-` is just's own.
fn just_to_shell(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line.trim_start_matches(['@', '-']);
    while let Some(k) = rest.find("{{") {
        out.push_str(&rest[..k]);
        out.push_str("$_MK");
        rest = match rest[k..].find("}}") {
            Some(e) => &rest[k + e + 2..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}
