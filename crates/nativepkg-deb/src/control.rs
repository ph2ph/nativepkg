//! The Debian `control` file.
//!
//! `Installed-Size` must be present or apt reports 0 bytes; `Section` must be in the current
//! list; a description body is folded with a leading space per line and ` .` for blank lines.

use core::fmt::Write as _;

use nativepkg_core::plan::BuildPlan;

/// `misc` is in the current section list; `base`, which the bash template used, is not.
const DEFAULT_SECTION: &str = "misc";

const DEFAULT_PRIORITY: &str = "optional";

/// Fields in the order `dpkg-deb` emits them, so a diff against a reference package reads well.
#[must_use]
pub fn render(plan: &BuildPlan, section: Option<&str>) -> String {
    let identity = &plan.identity;
    let mut out = String::with_capacity(512);

    // `writeln!` into a String cannot fail.
    let _ = writeln!(out, "Package: {}", identity.package_name);
    let _ = writeln!(out, "Version: {}", identity.version_deb);
    let _ = writeln!(out, "Architecture: {}", identity.architecture.deb());
    let _ = writeln!(out, "Maintainer: {}", identity.maintainer);
    let _ = writeln!(out, "Installed-Size: {}", plan.installed_size_kib());

    if let Some(homepage) = &identity.homepage {
        let _ = writeln!(out, "Homepage: {homepage}");
    }
    let _ = writeln!(out, "Section: {}", section.unwrap_or(DEFAULT_SECTION));
    let _ = writeln!(out, "Priority: {DEFAULT_PRIORITY}");

    if let Some(depends) = &identity.dependencies {
        let trimmed = depends.trim();
        if !trimmed.is_empty() {
            let _ = writeln!(out, "Depends: {trimmed}");
        }
    }

    let _ = write!(out, "{}", description_field(plan));
    out
}

/// Synopsis on the field line; body lines indented one space; a blank body line becomes ` .`,
/// or the parser treats it as the end of the paragraph.
fn description_field(plan: &BuildPlan) -> String {
    let description = &plan.identity.description;
    let mut out = String::with_capacity(description.body.len() + 64);
    let _ = writeln!(out, "Description: {}", description.synopsis);

    if description.body.trim().is_empty() {
        // Lintian reports a missing extended description as an error, so a one-line manifest
        // description gets a factual paragraph rather than nothing. Folded by hand under 80
        // columns (policy 5.6.13, `extended-description-line-too-long`).
        let _ = writeln!(
            out,
            " This package installs the {} application",
            plan.identity.package_name
        );
        let _ = writeln!(
            out,
            " together with the service integration for the target's"
        );
        let _ = writeln!(
            out,
            " init system. Add a longer `description` to the project"
        );
        let _ = writeln!(out, " manifest to replace this text.");
        return out;
    }

    for line in description.body.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            let _ = writeln!(out, " .");
        } else {
            let _ = writeln!(out, " {trimmed}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{plan_with, sample_plan};

    #[test]
    fn required_fields_are_present() {
        let control = render(&sample_plan(), None);
        for field in [
            "Package:",
            "Version:",
            "Architecture:",
            "Maintainer:",
            "Description:",
            "Priority:",
            "Section:",
            "Installed-Size:",
        ] {
            assert!(control.contains(field), "missing `{field}` in:\n{control}");
        }
    }

    #[test]
    fn the_default_section_is_a_recognised_one() {
        let control = render(&sample_plan(), None);
        assert!(control.contains("Section: misc"), "{control}");
        assert!(
            !control.contains("Section: base"),
            "`base` is not in the current section list"
        );
    }

    #[test]
    fn a_configured_section_wins() {
        let control = render(&sample_plan(), Some("javascript"));
        assert!(control.contains("Section: javascript"), "{control}");
    }

    #[test]
    fn installed_size_is_greater_than_zero_for_a_non_empty_package() {
        let control = render(&sample_plan(), None);
        let line = control
            .lines()
            .find(|l| l.starts_with("Installed-Size:"))
            .expect("field present");
        let value: u64 = line
            .trim_start_matches("Installed-Size:")
            .trim()
            .parse()
            .expect("a number");
        assert!(value > 0, "bash reported every package as 0: {line}");
    }

    #[test]
    fn homepage_is_emitted_only_when_known() {
        assert!(render(&sample_plan(), None).contains("Homepage: https://example.com"));

        let mut plan = sample_plan();
        plan.identity.homepage = None;
        assert!(!render(&plan, None).contains("Homepage:"));
    }

    #[test]
    fn a_single_line_description_still_gets_an_extended_paragraph() {
        let plan = plan_with("a tool", "");
        let control = render(&plan, None);
        assert!(control.contains("Description: a tool"));

        // This test once asserted the opposite — no continuation line — which was exactly the
        // behaviour lintian flagged as an error.
        let body: Vec<&str> = control
            .lines()
            .filter(|line| line.starts_with(' '))
            .collect();
        assert!(!body.is_empty(), "no extended description:\n{control}");
        assert!(
            body.iter().any(|line| line.contains("probe-app")),
            "the paragraph should name the package it describes:\n{control}"
        );
        assert!(
            body.iter().any(|line| line.contains("description")),
            "and should say how to replace it:\n{control}"
        );
    }

    #[test]
    fn a_supplied_extended_description_is_left_alone() {
        let plan = plan_with("a tool", "It does a specific thing.\n\nAnd another.");
        let control = render(&plan, None);
        assert!(control.contains(" It does a specific thing."), "{control}");
        assert!(
            control.contains(" ."),
            "a blank body line becomes ` .`:\n{control}"
        );
        assert!(
            !control.contains("Add a longer `description`"),
            "the generated paragraph must not appear when one was supplied:\n{control}"
        );
    }

    #[test]
    fn a_body_is_folded_with_a_leading_space() {
        let plan = plan_with("a tool", "does things\nand more things");
        let control = render(&plan, None);
        assert!(control.contains("\n does things\n"), "{control}");
        assert!(control.contains("\n and more things\n"), "{control}");
    }

    /// The shape that made a multi-line description unrepresentable in the bash templates.
    #[test]
    fn a_blank_body_line_becomes_a_space_and_a_full_stop() {
        let plan = plan_with("a tool", "first paragraph\n\nsecond paragraph");
        let control = render(&plan, None);
        assert!(
            control.contains("\n .\n"),
            "a blank line must not end the paragraph:\n{control}"
        );
    }

    #[test]
    fn every_body_line_is_indented() {
        let plan = plan_with("a tool", "one\n\ntwo\nthree");
        let control = render(&plan, None);
        let body: Vec<&str> = control
            .lines()
            .skip_while(|l| !l.starts_with("Description:"))
            .skip(1)
            .collect();
        assert!(!body.is_empty());
        for line in body {
            assert!(
                line.starts_with(' '),
                "body line `{line}` is not indented:\n{control}"
            );
        }
    }

    #[test]
    fn trailing_whitespace_in_a_body_line_is_trimmed() {
        let plan = plan_with("a tool", "padded   ");
        assert!(render(&plan, None).contains("\n padded\n"));
    }

    #[test]
    fn an_empty_dependency_field_is_omitted() {
        let mut plan = sample_plan();
        plan.identity.dependencies = Some("   ".to_owned());
        assert!(!render(&plan, None).contains("Depends:"));

        plan.identity.dependencies = None;
        assert!(!render(&plan, None).contains("Depends:"));
    }

    #[test]
    fn the_file_ends_with_a_newline() {
        // A control file whose last field lacks a terminator is malformed.
        assert!(render(&sample_plan(), None).ends_with('\n'));
    }
}
