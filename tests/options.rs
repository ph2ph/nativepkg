//! The command line surface: what parses, what warns, what refuses. Asserted against the bash
//! tool's actual surface — 36 branches, catalogued in `openspec/cli-surface.md` — rather than
//! a remembered subset.

use clap::Parser as _;
use nativepkg::cli::{Cli, InitChoice, StrategyChoice};
use nativepkg::format::Format;
use nativepkg::introspect;

fn parse(args: &[&str]) -> Cli {
    let mut full = vec!["nativepkg"];
    full.extend_from_slice(args);
    Cli::try_parse_from(full).expect("should parse")
}

#[test]
fn every_option_the_bash_tool_accepted_still_parses() {
    let cli = parse(&[
        "--pkg-name",
        "app",
        "--version",
        "1.2.3",
        "--description",
        "a thing",
        "--maintainer",
        "A <a@example.com>",
        "--architecture",
        "amd64",
        "--exec-name",
        "app",
        "--install-dir",
        "/usr/lib",
        "--install-strategy",
        "copy",
        "--extra-files",
        "etc",
        "--init",
        "systemd",
        "--user",
        "app",
        "--group",
        "app",
        "--deps",
        "redis-server",
        "--nodejs",
        "--verbose",
    ]);

    assert_eq!(cli.package_name.as_deref(), Some("app"));
    assert_eq!(cli.version.as_deref(), Some("1.2.3"));
    assert_eq!(cli.description.as_deref(), Some("a thing"));
    assert_eq!(cli.maintainer.as_deref(), Some("A <a@example.com>"));
    assert_eq!(cli.architecture.as_deref(), Some("amd64"));
    assert_eq!(cli.executable_name.as_deref(), Some("app"));
    assert_eq!(cli.install_dir.as_deref(), Some("/usr/lib"));
    assert_eq!(cli.install_strategy, Some(StrategyChoice::Copy));
    assert_eq!(cli.extra_files.as_deref(), Some("etc"));
    assert_eq!(cli.init, Some(InitChoice::Systemd));
    assert_eq!(cli.user.as_deref(), Some("app"));
    assert_eq!(cli.group.as_deref(), Some("app"));
    assert_eq!(cli.deps.as_deref(), Some("redis-server"));
    assert!(cli.nodejs);
    assert!(cli.verbose);
}

/// The short forms are what a naive audit of the bash source misses.
#[test]
fn the_short_forms_parse_too() {
    let cli = parse(&[
        "-n",
        "app",
        "-v",
        "1.0.0",
        "-d",
        "desc",
        "-m",
        "M <m@e.com>",
        "-a",
        "amd64",
        "-e",
        "bin",
        "-i",
        "none",
        "-u",
        "u",
        "-g",
        "g",
        "-o",
        "out",
    ]);

    assert_eq!(cli.package_name.as_deref(), Some("app"));
    assert_eq!(cli.version.as_deref(), Some("1.0.0"));
    assert_eq!(cli.description.as_deref(), Some("desc"));
    assert_eq!(cli.maintainer.as_deref(), Some("M <m@e.com>"));
    assert_eq!(cli.architecture.as_deref(), Some("amd64"));
    assert_eq!(cli.executable_name.as_deref(), Some("bin"));
    assert_eq!(cli.init, Some(InitChoice::None));
    assert_eq!(cli.user.as_deref(), Some("u"));
    assert_eq!(cli.group.as_deref(), Some("g"));
    assert_eq!(cli.output_name.as_deref(), Some("out"));
}

#[test]
fn the_legacy_output_option_is_still_accepted() {
    let cli = parse(&["--output-deb-name", "custom"]);
    assert_eq!(cli.output_name.as_deref(), Some("custom"));
}

/// `--arch` was deprecated in the bash source years ago; the alias stays rather than break
/// scripts whose authors never saw the message.
#[test]
fn the_deprecated_architecture_alias_parses_separately() {
    let cli = parse(&["--arch", "arm64"]);
    assert_eq!(cli.arch_deprecated.as_deref(), Some("arm64"));
    assert_eq!(cli.architecture, None, "the two must stay distinguishable");
}

/// Accepting silently would let a build script keep working while losing what it asked for.
#[test]
fn options_that_cannot_be_honoured_are_refused_with_a_reason() {
    for (args, expected) in [
        (vec!["--no-delete-temp"], "--no-delete-temp"),
        (vec!["--no-md5sums"], "--no-md5sums"),
        (vec!["--no-rebuild"], "--no-rebuild"),
        (vec!["--template-control", "x"], "--template-control"),
        (vec!["--template-preinst", "x"], "--template-preinst"),
        (vec!["--template-postinst", "x"], "--template-postinst"),
        (vec!["--template-prerm", "x"], "--template-prerm"),
        (vec!["--template-postrm", "x"], "--template-postrm"),
    ] {
        let cli = parse(&args);
        let removed = cli.removed_options();
        assert_eq!(removed.len(), 1, "{args:?}");
        assert_eq!(removed[0].option, expected);
        assert!(
            !removed[0].reason.is_empty(),
            "a refusal without a reason is not actionable"
        );
    }
}

#[test]
fn a_normal_invocation_refuses_nothing() {
    let cli = parse(&["--pkg-name", "app"]);
    assert!(cli.removed_options().is_empty());
}

#[test]
fn formats_are_a_comma_separated_list_defaulting_to_deb() {
    assert_eq!(parse(&[]).format, vec![Format::Deb]);
    assert_eq!(
        parse(&["--format", "deb,rpm,arch"]).format,
        vec![Format::Deb, Format::Rpm, Format::Arch]
    );
    assert_eq!(parse(&["--format", "arch"]).format, vec![Format::Arch]);
}

#[test]
fn an_unknown_format_is_rejected() {
    let result = Cli::try_parse_from(["nativepkg", "--format", "msi"]);
    assert!(result.is_err(), "an unknown format must not be accepted");
}

#[test]
fn quiet_and_verbose_cannot_both_be_asked_for() {
    let result = Cli::try_parse_from(["nativepkg", "--quiet", "--verbose"]);
    assert!(result.is_err(), "these ask for opposite things");
}

#[test]
fn inputs_are_collected_after_the_options() {
    let cli = parse(&["--pkg-name", "app", "lib", "package.json"]);
    assert_eq!(
        cli.inputs,
        vec![
            std::path::PathBuf::from("lib"),
            std::path::PathBuf::from("package.json")
        ]
    );
}

/// The bash tool answered by grepping its own source, so its listing could disagree with what
/// the code honoured.
#[test]
fn introspection_answers_match_the_data_it_describes() {
    let variables = introspect::template_variables();
    assert!(variables.contains(&"package_name"), "{variables:?}");
    assert!(variables.contains(&"package_architecture"), "{variables:?}");

    let overrides = introspect::json_overrides();
    assert!(overrides.contains(&"install_dir"), "{overrides:?}");
    assert!(overrides.contains(&"entrypoints"), "{overrides:?}");

    let templates = introspect::templates();
    assert!(templates.contains(&"systemd.service"), "{templates:?}");
    // `control` and the maintainer scripts became data and snippets; listing them would
    // advertise something that cannot be overridden.
    for gone in ["control", "preinst", "postinst", "prerm", "postrm"] {
        assert!(!templates.contains(&gone), "{gone} should not be listed");
    }
}

#[test]
fn a_template_can_be_printed_and_an_unknown_one_names_the_alternatives() {
    let text = introspect::cat_template("systemd.service").expect("exists");
    assert!(text.contains("[Unit]"), "{text}");

    let error = introspect::cat_template("nope").expect_err("should fail");
    let message = format!("{error:#}");
    assert!(message.contains("systemd.service"), "{message}");
}

#[test]
fn introspection_is_recognised_as_such() {
    assert!(parse(&["--list-tmps"]).is_introspection());
    assert!(parse(&["--list-tmp-vars"]).is_introspection());
    assert!(parse(&["--list-json-overrides"]).is_introspection());
    assert!(parse(&["--cat-tmp", "default"]).is_introspection());
    assert!(!parse(&["--pkg-name", "app"]).is_introspection());
}

/// Whether the parser answers this option at all. clap answers `--help` by *failing* to parse
/// with a kind meaning "here is the help"; treating that as a rejection would report it missing.
fn accepted(option: &str) -> bool {
    for args in [
        vec![
            "nativepkg".to_owned(),
            format!("--{option}"),
            "x".to_owned(),
        ],
        vec!["nativepkg".to_owned(), format!("--{option}")],
    ] {
        match Cli::try_parse_from(args) {
            Ok(_) => return true,
            Err(error) => {
                use clap::error::ErrorKind;
                // `InvalidValue` means the option exists and `x` is not among its values: a
                // pass. `--init` and `--install-strategy` answer this way as `ValueEnum`s, and
                // an earlier helper reported both as missing.
                if matches!(
                    error.kind(),
                    ErrorKind::DisplayHelp
                        | ErrorKind::DisplayVersion
                        | ErrorKind::InvalidValue
                        | ErrorKind::ValueValidation
                ) {
                    return true;
                }
            }
        }
    }
    false
}

/// Or the compatibility check would pass for any input at all.
#[test]
fn the_acceptance_helper_rejects_an_option_that_does_not_exist() {
    assert!(!accepted("no-such-option-at-all"));
    assert!(accepted("pkg-name"));
    assert!(
        accepted("init"),
        "a ValueEnum option must count as accepted"
    );
    assert!(accepted("help"));
}

/// `--init` and `--install-strategy` once matched their value by hand and returned `None` for
/// anything unknown, so `--init Systemd` — the wrong case — was discarded in silence and the
/// manifest's value won.
#[test]
fn an_unrecognised_value_is_refused_rather_than_discarded() {
    for (option, bad) in [
        ("--init", "Systemd"),
        ("--init", "systemdd"),
        ("--install-strategy", "npm_install"),
        ("--install-strategy", "bogus"),
    ] {
        let result = Cli::try_parse_from(["nativepkg", option, bad]);
        let error = result.expect_err(&format!("`{option} {bad}` must be refused"));

        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::InvalidValue,
            "`{option} {bad}` failed for the wrong reason"
        );
        let message = error.to_string();
        assert!(
            message.contains(bad),
            "the refusal must name the value: {message}"
        );
        assert!(
            message.contains("possible values"),
            "the refusal must list what is accepted: {message}"
        );
    }
}

/// Or the check above would pass by refusing everything.
#[test]
fn the_accepted_spellings_still_parse() {
    for value in ["auto", "systemd", "upstart", "sysv", "none"] {
        assert!(
            Cli::try_parse_from(["nativepkg", "--init", value]).is_ok(),
            "`--init {value}` should parse"
        );
    }
    for value in ["auto", "copy", "npm-install"] {
        assert!(
            Cli::try_parse_from(["nativepkg", "--install-strategy", value]).is_ok(),
            "`--install-strategy {value}` should parse"
        );
    }
}
