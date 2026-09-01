//! Fitness functions for the shape of the workspace: every place the core names a package
//! format is registered below with a reason, and a new mention fails the build until someone
//! adds it deliberately — an enum, a string literal, a field name, or shell in a template.
//!
//! A register rather than a blacklist: the first attempt grepped for `enum Format`, and a
//! `match` over `"deb"`/`"rpm"` literals slipped straight past it.

use std::path::{Path, PathBuf};

/// Why a given mention of a format is allowed to exist in the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    /// A per-format rendering of one closed value. Exhaustive matches, so adding an
    /// architecture is a compile error in every format at once — why the tables are central
    /// rather than devolved to the backends.
    Spelling,

    /// A name that carries "deb" for historical reasons rather than because it names a
    /// format, such as `output_deb_name`. Kept as a compatibility spelling.
    LegacyName,

    /// A place where the core silently assumes Debian: a defect, listed with the change that
    /// removes it.
    DebianAssumption,
}

/// Every mention of a package format in the core, and why. Matched as a substring of one
/// line's code, so one entry can cover a run of lines.
const REGISTER: &[(&str, &str, Reason)] = &[
    // -- per-format spellings of closed values --------------------------------------------
    ("arch.rs", "pub fn deb", Reason::Spelling),
    ("arch.rs", "pub fn rpm", Reason::Spelling),
    ("arch.rs", "pub fn arch_linux", Reason::Spelling),
    (
        "build.rs",
        "version_deb: config.version.deb()",
        Reason::Spelling,
    ),
    (
        "build.rs",
        "version_rpm: config.version.rpm_version()",
        Reason::Spelling,
    ),
    (
        "build.rs",
        "release_rpm: config.version.rpm_release()",
        Reason::Spelling,
    ),
    ("plan.rs", "pub version_deb", Reason::Spelling),
    ("plan.rs", "pub version_rpm", Reason::Spelling),
    ("plan.rs", "pub release_rpm", Reason::Spelling),
    ("version.rs", "deb: String", Reason::Spelling),
    ("version.rs", "rpm_version", Reason::Spelling),
    ("version.rs", "rpm_release", Reason::Spelling),
    ("version.rs", "DEFAULT_RPM_RELEASE", Reason::Spelling),
    ("version.rs", "let deb = match epoch", Reason::Spelling),
    ("version.rs", "            deb,", Reason::Spelling),
    ("version.rs", "pub fn deb(&self)", Reason::Spelling),
    ("version.rs", "&self.deb", Reason::Spelling),
    // -- the old tool's own name ------------------------------------------------------------
    ("npm.rs", "output_deb_name", Reason::LegacyName),
    ("resolve.rs", "output_deb_name", Reason::LegacyName),
    // -- known Debian assumptions ----------------------------------------------------------
    // None left. Two were fixed by `cli-and-compat` (the caller supplies the spelling; the
    // core chooses nothing); eighteen relocated to `nativepkg-deb` with maintainer-script
    // composition.
];

/// Format coupling that names no format, so no token scan can find it (review once found
/// `package_architecture` bound to the raw user string). Entries are asserted to still be
/// *present*, so fixing one forces an edit here. Maintained by hand, so a floor, never a proof.
const UNDETECTABLE_COUPLING: &[(&str, &str, &str)] = &[];

const FORMAT_TOKENS: &[&str] = &["deb", "rpm", "dpkg", "pacman", "debian", "lintian"];

/// Spellings that survive tokenisation intact.
const FORMAT_PHRASES: &[&str] = &[
    "arch_linux",
    "pkg.tar",
    "enum Format",
    "enum TargetFormat",
    // Debian-only programs whose names contain no format word; without them a script
    // invoking one would count as portable.
    "adduser",
    "deluser",
    "invoke-rc.d",
    "update-rc.d",
    "policy-rc.d",
    "dh_",
];

/// Which token gives a line away. Splitting on every non-alphanumeric character is what makes
/// this resistant to the *shape* of a violation: `deb-systemd-helper`, `version_deb`, a `"deb"`
/// literal and an `enum Format` all surface the same way. It cannot see a name assembled by a
/// macro, or coupling that never spells a format — see [`UNDETECTABLE_COUPLING`].
fn names_a_format(line: &str) -> Option<String> {
    if let Some(phrase) = FORMAT_PHRASES.iter().find(|p| line.contains(**p)) {
        return Some((*phrase).to_owned());
    }
    line.split(|c: char| !c.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .find(|token| FORMAT_TOKENS.contains(&token.as_str()))
}

/// Lexer state that survives across lines. [`Lex::Str`] really does: a plain string literal
/// may contain a raw line break, and resetting at every line end re-lexed the continuation as
/// code and desynchronised the brace depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lex {
    Code,
    Str,
    /// Raw string, carrying the number of `#` that must precede its closing quote.
    Raw(usize),
    /// Block comment with its nesting depth: `/* /* */ */` nests, and exiting on the first
    /// `*/` read the rest of the file as a comment.
    Block(usize),
}

/// Strips comments from one line; reports the net brace depth, the next line's state, and the
/// code **with string contents intact**. Braces are counted only in code, because a brace in a
/// literal desynchronises the depth (a multi-line `r#"{ ... }"#` fixture in `resolve.rs` once
/// exposed that file's whole test module). String contents stay, because
/// `match t { "deb" => ... }` hides a format name in a literal.
fn scrub(line: &str, mut state: Lex) -> Scrubbed {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut delta = 0_i32;
    let mut i = 0;

    while i < chars.len() {
        match state {
            Lex::Code => {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'/') {
                    break;
                }
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    state = Lex::Block(1);
                    i += 2;
                    continue;
                }
                // A raw string opener: `r` then zero or more `#` then `"`.
                if chars[i] == 'r' {
                    let mut j = i + 1;
                    while chars.get(j) == Some(&'#') {
                        j += 1;
                    }
                    if chars.get(j) == Some(&'"') {
                        state = Lex::Raw(j - i - 1);
                        i = j + 1;
                        continue;
                    }
                }
                if chars[i] == '"' {
                    state = Lex::Str;
                    i += 1;
                    continue;
                }
                // A char literal, which may legitimately contain a brace or a quote.
                if chars[i] == '\'' {
                    let mut j = i + 1;
                    if chars.get(j) == Some(&'\\') {
                        j += 1;
                    }
                    if chars.get(j + 1) == Some(&'\'') {
                        i = j + 2;
                        continue;
                    }
                }
                match chars[i] {
                    '{' => delta += 1,
                    '}' => delta -= 1,
                    _ => {}
                }
                out.push(chars[i]);
                i += 1;
            }
            Lex::Str => {
                if chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == '"' {
                    state = Lex::Code;
                } else {
                    out.push(chars[i]);
                }
                i += 1;
            }
            Lex::Raw(hashes) => {
                // `.take(n).all(..)` is vacuously true when fewer than `n` characters remain,
                // so a bare `"` at the end of a line was read as a closer.
                let closing = chars[i + 1..]
                    .iter()
                    .take(hashes)
                    .filter(|c| **c == '#')
                    .count();
                if chars[i] == '"' && closing == hashes {
                    state = Lex::Code;
                    i += hashes + 1;
                    continue;
                }
                out.push(chars[i]);
                i += 1;
            }
            Lex::Block(nesting) => {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    state = Lex::Block(nesting + 1);
                    i += 2;
                    continue;
                }
                if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    state = if nesting == 1 {
                        Lex::Code
                    } else {
                        Lex::Block(nesting - 1)
                    };
                    i += 2;
                    continue;
                }
                i += 1;
            }
        }
    }

    Scrubbed {
        text: out,
        delta,
        state,
    }
}

struct Scrubbed {
    text: String,
    delta: i32,
    state: Lex,
}

/// Blanks `#[cfg(test)]` modules, keeping line numbers. An earlier version truncated at the
/// first one, so code appended after a test module was invisible — where three injected
/// violations hid.
fn strip_test_modules(text: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut lex = Lex::Code;
    let mut pending = false;
    let mut depth = 0_i32;

    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let continues_a_literal = matches!(lex, Lex::Raw(_) | Lex::Str);
        let Scrubbed {
            text: scrubbed,
            delta,
            state,
        } = scrub(line, lex);
        lex = state;

        if depth > 0 {
            depth += delta;
            out.push((number, String::new()));
            continue;
        }

        // A string literal spanning lines is one lexical unit: a token split across the break
        // — `dp` then `kg` in a raw string — is invisible to line-by-line matching.
        if continues_a_literal && let Some(last) = out.last_mut() {
            last.1.push_str(&scrubbed);
            continue;
        }
        if !pending && scrubbed.trim_start().starts_with("#[cfg(test)]") {
            pending = true;
        }

        if pending {
            out.push((number, String::new()));
            depth += delta;
            if depth > 0 {
                // A braced item. Skipping now continues until the depth returns to zero.
                pending = false;
            } else if scrubbed.contains('{') || scrubbed.trim_end().ends_with(';') {
                // The item began and ended on this line (`use ...;`, `mod name;`, a one-line
                // fn). Clearing `pending` only when a line *opened* a brace left it stuck, and
                // every following line was blanked out of the scan.
                pending = false;
                depth = 0;
            }
            continue;
        }

        out.push((number, scrubbed));
    }
    out
}

/// Source files minus their test modules: a test may name a format freely.
fn scannable() -> Vec<(String, Vec<(usize, String)>)> {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR")));
    let mut out = Vec::new();

    let mut stack = vec![root.join("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("source directory readable") {
            let path: PathBuf = entry.expect("directory entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).expect("source readable");
                let lines = strip_test_modules(&text);
                let name = path
                    .file_name()
                    .expect("file name")
                    .to_string_lossy()
                    .into_owned();
                out.push((name, lines));
            }
        }
    }

    // `templates/` is compiled in by `include_str!`, so it ships format knowledge like source;
    // it was missed at first. Scanned whole, with no comment stripping: `#` opens a shell
    // comment but also a template conditional, so cutting at the first would hide the
    // construct most worth finding.
    for directory in ["templates"] {
        for entry in std::fs::read_dir(root.join(directory)).expect("directory readable") {
            let path = entry.expect("directory entry").path();
            let text = std::fs::read_to_string(&path).expect("file readable");
            let name = format!(
                "{directory}/{}",
                path.file_name().expect("file name").to_string_lossy()
            );
            let lines = text
                .lines()
                .enumerate()
                .map(|(i, l)| (i + 1, l.to_owned()))
                .collect();
            out.push((name, lines));
        }
    }

    out
}

/// Replaces a test that grepped for `enum Format`, defeated in one edit by a `match` over
/// string literals. Two later versions fell to a stuck `pending` flag and a token split across
/// a line break; this test is only as good as the last violation someone tried to sneak past.
#[test]
fn every_format_mention_in_the_core_is_registered() {
    let mut unregistered = Vec::new();

    for (file, lines) in scannable() {
        // In shell and templates the dot belongs to a tool name (`update-rc.d`); in Rust it
        // opens a method call.
        let dotted = file.starts_with("snippets/") || file.starts_with("templates/");

        for (number, line) in &lines {
            // Cheap pre-filter: most lines name no format at all.
            if names_a_format(line).is_none() {
                continue;
            }
            // Excise every registered needle, then look again. Asking whether the line
            // contains *some* needle would bless anything sharing it: review appended
            // `dpkg-trigger` to a registered `update-rc.d` call and the test stayed green.
            let mut residue = line.clone();
            for (f, needle, _) in REGISTER {
                if *f == file {
                    residue = excise(&residue, needle, dotted);
                }
            }
            if let Some(token) = names_a_format(&residue) {
                unregistered.push(format!("{file}:{number} names `{token}`: {}", line.trim()));
            }
        }
    }

    assert!(
        unregistered.is_empty(),
        "the core gained {} unregistered mention(s) of a package format.\n\n{}\n\n\
         Either remove the format knowledge, or add it to REGISTER with a reason. A \
         `DebianAssumption` entry must name the change that removes it.",
        unregistered.len(),
        unregistered.join("\n")
    );
}

/// Every process the workspace may spawn, and why. An allowlist: the blacklist it replaced was
/// defeated nine times (an absolute path, an owned `String`, an aliased import, `concat!`, a
/// wrapper through `env`, a line break, extra spaces, `sh -c`, a binding). A process can only
/// start by naming `Command::new`, so every call is visible even when the program is not.
/// Deliberately weaker than "never shells out" — cargo-deb itself runs `dpkg-shlibdeps` and
/// `strip`; what matters is that *building a package* needs no distribution tooling.
const REGISTERED_SPAWNS: &[(&str, &str)] = &[(
    "timestamp.rs",
    "`git log -1 --format=%ct`, to derive a reproducible build timestamp from the commit. \
     Failure is not an error — not being in a checkout is ordinary and the caller falls back to \
     the newest file's modification time — so a machine without git still builds packages.",
)];

/// Ways to start a process without naming `Command`, which the register could not see.
const FORBIDDEN_SPAWN_ROUTES: &[&str] = &["execve", "execvp", "posix_spawn", "libc::fork"];

/// What this does not prove: that a registered spawn is harmless, or that a dependency does
/// not hide one.
#[test]
fn every_process_the_workspace_spawns_is_registered() {
    let crates = Path::new(concat!(env!("CARGO_MANIFEST_DIR")))
        .parent()
        .expect("the crates directory");

    let mut unregistered = Vec::new();
    let mut forbidden = Vec::new();
    let mut matched = 0_usize;
    let mut scanned = 0_usize;

    for entry in std::fs::read_dir(crates).expect("crates directory") {
        let src = entry.expect("entry").path().join("src");
        if !src.is_dir() {
            continue;
        }
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("source directory") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }

                let text = std::fs::read_to_string(&path).expect("source");
                let stripped = strip_test_modules(&text);
                scanned += stripped.len();
                let body: String = stripped
                    .iter()
                    .map(|(_, line)| line.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");

                let name = path
                    .file_name()
                    .expect("a file name")
                    .to_string_lossy()
                    .into_owned();

                // Both spellings: `Command::new` misses an aliased import (`Cmd::new`), and
                // `process::Command` misses a renamed module (`p::Command::new`). A process
                // cannot start without one of the two appearing in the file.
                if body.contains("Command::new") || body.contains("process::Command") {
                    if REGISTERED_SPAWNS.iter().any(|(file, _)| *file == name) {
                        matched += 1;
                    } else {
                        unregistered.push(path.display().to_string());
                    }
                }
                for route in FORBIDDEN_SPAWN_ROUTES {
                    if body.contains(route) {
                        forbidden.push(format!("{}: {route}", path.display()));
                    }
                }
            }
        }
    }

    assert!(scanned > 1000, "only {scanned} lines were scanned");

    // A positive control: the previous version read every file, reported a healthy count, and
    // recognised nothing. An empty walk and a clean workspace look identical.
    assert_eq!(
        matched,
        REGISTERED_SPAWNS.len(),
        "the scan found {matched} of {} registered spawns, so it is not looking where it \
         claims to",
        REGISTERED_SPAWNS.len()
    );

    assert!(
        unregistered.is_empty(),
        "these files start a process without an entry in REGISTERED_SPAWNS. Building a package \
         must not need a distribution's tooling; if the spawn is deliberate, register it with \
         the reason:\n{}",
        unregistered.join("\n")
    );
    assert!(
        forbidden.is_empty(),
        "these files start a process by a route the register cannot see:\n{}",
        forbidden.join("\n")
    );
}

/// A single edge the other way would let one format's quirks reach the plan every other format
/// reads.
#[test]
fn the_core_depends_on_no_backend() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("manifest readable");
    for backend in ["nativepkg-deb", "nativepkg-rpm", "nativepkg-arch"] {
        assert!(
            !manifest.contains(backend),
            "the core must not depend on `{backend}`:\n{manifest}"
        );
    }
}

/// A debt ledger: the count is asserted so it cannot drift upward unnoticed. It proves the
/// table agrees with itself, not that it is complete — see [`UNDETECTABLE_COUPLING`].
#[test]
fn the_debian_assumptions_are_a_known_and_bounded_set() {
    let assumptions = REGISTER
        .iter()
        .filter(|(_, _, reason)| *reason == Reason::DebianAssumption)
        .count();
    assert_eq!(
        assumptions, 0,
        "the core's Debian assumptions changed; if one was fixed, lower this number"
    );

    // The assertion stays at zero: noticing a new assumption is cheaper than rediscovering
    // twenty, which is how this register started.
}

/// Removes whole-token occurrences of `needle`, leaving partial ones. Plain `replace` was not
/// enough: a needle applies to every line of its file, so one that is a *prefix* of a new
/// mention strips what made it distinct — review's `command -v update-rc.d-legacy` was
/// swallowed by the registered `command -v update-rc.d`. A second identical mention in the
/// same file still goes unreported.
fn excise(line: &str, needle: &str, dotted_names: bool) -> String {
    // Whether `.` continues a token depends on the domain. In Rust a trailing `.` is a method
    // call and nearly every needle sits before one (`config.version.deb().to_owned()`), so
    // treating it as part of the token would stop excising them. In templates `update-rc.d`
    // carries a real dot, and ignoring it lets a needle swallow `update-rc.d.real`.
    let is_token_char =
        |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_' || (dotted_names && c == '.');

    let mut out = String::with_capacity(line.len());
    let mut rest = line;

    while let Some(at) = rest.find(needle) {
        let before_ok = rest[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !is_token_char(c));
        let after = &rest[at + needle.len()..];
        let after_ok = after.chars().next().is_none_or(|c| !is_token_char(c));

        out.push_str(&rest[..at]);
        if before_ok && after_ok {
            out.push(' ');
        } else {
            // A partial match: keep it, so whatever made it different stays visible.
            out.push_str(needle);
        }
        rest = after;
    }

    out.push_str(rest);
    out
}

/// Without this a fixed defect leaves its entry behind and the register becomes a description
/// of the past — as happened when task 4b.4 removed a binding two entries described.
#[test]
fn every_registered_needle_still_exists() {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR")));
    let mut missing = Vec::new();

    for (file, needle, _) in REGISTER {
        let path = if file.contains('/') {
            root.join(file)
        } else {
            root.join("src").join(file)
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            missing.push(format!("{file} (no such file)"));
            continue;
        };
        if !text.contains(needle) {
            missing.push(format!("{file}: `{needle}`"));
        }
    }

    assert!(
        missing.is_empty(),
        "the register describes text that is no longer there. If it was fixed, remove the \
         entry; if it moved, update it.\n{}",
        missing.join("\n")
    );
}

/// A needle equal to a bare token is excised from every line of its file and swallows every
/// other mention there. Review proved it with `"dpkg"`: a new `dpkg-trigger` line elsewhere in
/// the file went unreported.
#[test]
fn every_needle_carries_context_beyond_the_bare_token() {
    for (file, needle, _) in REGISTER {
        let bare = FORMAT_TOKENS
            .iter()
            .chain(FORMAT_PHRASES.iter())
            .find(|t| needle.eq_ignore_ascii_case(t));
        assert!(
            bare.is_none(),
            "`{file}`'s needle `{needle}` is a bare format token, so it would excise every \
             other mention of it in that file. Widen it to the phrase it actually excuses."
        );
    }
}

/// Coupling that no token scan can find must at least be prevented from vanishing silently.
#[test]
fn the_undetectable_coupling_is_still_where_the_register_says() {
    for (file, needle, why) in UNDETECTABLE_COUPLING {
        let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR")));
        let path = if file.contains('/') {
            root.join(file)
        } else {
            root.join("src").join(file)
        };
        let text = std::fs::read_to_string(&path).expect("source readable");
        assert!(
            text.contains(needle),
            "`{file}` no longer contains the coupling the register describes ({why}).\n\
             If it was fixed, remove the entry; if it moved, update it."
        );
    }
}
