//! What the tool does, as opposed to what it accepts: where output goes, what a dry run
//! writes, and what happens when one format of three fails.

use std::io::Write;
use std::sync::{Arc, Mutex};

use nativepkg::format::Format;
use nativepkg::report::{Reporter, Verbosity};

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("not poisoned")).into_owned()
    }
}

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("not poisoned").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn reporter(verbosity: Verbosity) -> (Reporter, Captured, Captured) {
    let out = Captured::default();
    let err = Captured::default();
    let reporter = Reporter::new(verbosity, Box::new(out.clone()), Box::new(err.clone()));
    (reporter, out, err)
}

/// Or `--json` stops being parseable the moment anything warns; the bash tool mixed the two.
#[test]
fn diagnostics_go_to_standard_error_and_results_to_standard_output() {
    let (mut reporter, out, err) = reporter(Verbosity::Normal);

    reporter.warn("something to note");
    reporter.produced(Format::Deb, std::path::Path::new("/tmp/out/app.deb"));

    assert_eq!(out.text().trim(), "/tmp/out/app.deb");
    assert!(err.text().contains("something to note"), "{}", err.text());
    assert!(
        !out.text().contains("something to note"),
        "a warning reached standard output: {}",
        out.text()
    );
}

#[test]
fn quiet_suppresses_warnings_but_never_errors() {
    let (mut reporter, _, err) = reporter(Verbosity::Quiet);

    reporter.warn("suppressed");
    reporter.detail("also suppressed");
    reporter.failed(
        Format::Rpm,
        &anyhow::anyhow!("the payload could not be written"),
    );

    let text = err.text();
    assert!(!text.contains("suppressed"), "{text}");
    assert!(text.contains("the payload could not be written"), "{text}");
}

#[test]
fn verbose_adds_detail_that_normal_leaves_out() {
    let (mut normal, _, normal_err) = reporter(Verbosity::Normal);
    normal.detail("how the timestamp was chosen");
    assert_eq!(normal_err.text(), "");

    let (mut verbose, _, verbose_err) = reporter(Verbosity::Verbose);
    verbose.detail("how the timestamp was chosen");
    assert!(
        verbose_err.text().contains("how the timestamp was chosen"),
        "{}",
        verbose_err.text()
    );
}

#[test]
fn a_failure_renders_its_whole_cause_chain() {
    let (mut reporter, _, err) = reporter(Verbosity::Normal);

    let error = anyhow::anyhow!("permission denied")
        .context("opening /var/lib/app/state")
        .context("writing the .rpm");
    reporter.failed(Format::Rpm, &error);

    let text = err.text();
    assert!(text.contains("writing the .rpm"), "{text}");
    assert!(text.contains("opening /var/lib/app/state"), "{text}");
    assert!(text.contains("permission denied"), "{text}");
    assert!(
        text.matches("caused by").count() >= 2,
        "each link in the chain should be shown: {text}"
    );
}

#[test]
fn quiet_prints_no_paths() {
    let (mut reporter, out, _) = reporter(Verbosity::Quiet);
    reporter.produced(Format::Deb, std::path::Path::new("/tmp/app.deb"));
    assert_eq!(out.text(), "");
}

/// Stopping at the first failure would withhold the packages that were fine.
#[test]
fn a_partial_failure_reports_every_outcome_and_exits_non_zero() {
    use nativepkg::run::{Built, summarise};

    let (mut reporter, out, err) = reporter(Verbosity::Normal);

    let built = vec![
        Built {
            format: Format::Deb,
            outcome: Ok(std::path::PathBuf::from("/tmp/out/app.deb")),
        },
        Built {
            format: Format::Rpm,
            outcome: Err(anyhow::anyhow!("the payload could not be assembled")),
        },
        Built {
            format: Format::Arch,
            outcome: Ok(std::path::PathBuf::from("/tmp/out/app.pkg.tar.zst")),
        },
    ];

    for entry in &built {
        match &entry.outcome {
            Ok(path) => reporter.produced(entry.format, path),
            Err(error) => reporter.failed(entry.format, error),
        }
    }
    let code = summarise(&built, &mut reporter);

    assert_eq!(code, 1, "one failure must not be reported as success");

    let printed = out.text();
    assert!(printed.contains("app.deb"), "{printed}");
    assert!(printed.contains("app.pkg.tar.zst"), "{printed}");

    let diagnostics = err.text();
    assert!(
        diagnostics.contains("the payload could not be assembled"),
        "{diagnostics}"
    );
    assert!(
        diagnostics.contains("1 of 3"),
        "the summary should say how much failed: {diagnostics}"
    );
}

#[test]
fn a_run_where_everything_worked_exits_zero() {
    use nativepkg::run::{Built, summarise};

    let (mut reporter, _, _) = reporter(Verbosity::Normal);
    let built = vec![Built {
        format: Format::Deb,
        outcome: Ok(std::path::PathBuf::from("/tmp/out/app.deb")),
    }];

    assert_eq!(summarise(&built, &mut reporter), 0);
}
