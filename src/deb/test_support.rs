//! Fixtures shared by this crate's unit tests.

use crate::core::arch::Architecture;
use crate::core::plan::{BuildPlan, Description, Destination, Identity, PlanMetadata, PlannedFile};
use crate::core::timestamp::Timestamp;

/// Fixed timestamp, so every fixture is deterministic.
pub const TIMESTAMP: u64 = 1_700_000_000;

/// A destination that is known to normalise.
pub fn dest(path: &str) -> Destination {
    Destination::new(path).expect("fixture path should normalise")
}

/// A plan with one inline file, a symlink, and a configuration file.
pub fn sample_plan() -> BuildPlan {
    plan_with("a probe", "with a body")
}

/// A plan whose description is built from the given synopsis and body.
pub fn plan_with(synopsis: &str, body: &str) -> BuildPlan {
    let raw = if body.is_empty() {
        synopsis.to_owned()
    } else {
        format!("{synopsis}\n{body}")
    };
    let identity = Identity {
        package_name: "probe-app".into(),
        version_deb: "1.2.3".into(),
        version_rpm: "1.2.3".into(),
        release_rpm: "1".into(),
        epoch: None,
        description: Description::split(&raw).expect("fixture description should split"),
        maintainer: "A <a@example.com>".into(),
        architecture: Architecture::Any,
        dependencies: Some("nodejs".into()),
        homepage: Some("https://example.com".into()),
        license: Some("MIT".into()),
    };

    let target = dest("/usr/share/probe-app/bin/probe-app");
    let files = vec![
        PlannedFile::inline(
            dest("/usr/share/probe-app/app/app.js"),
            b"console.log(1)\n".to_vec(),
            PlannedFile::MODE_REGULAR,
        ),
        PlannedFile::inline(
            target.clone(),
            b"#!/bin/sh\nexec node app.js\n".to_vec(),
            PlannedFile::MODE_EXECUTABLE,
        ),
        PlannedFile::inline(
            dest("/etc/probe-app/config.json"),
            b"{}\n".to_vec(),
            PlannedFile::MODE_REGULAR,
        )
        .as_config(),
        PlannedFile::symlink(dest("/usr/bin/probe-app"), &target),
    ];

    BuildPlan::new(
        identity,
        files,
        Timestamp::from_secs(TIMESTAMP),
        PlanMetadata {
            generator: "nativepkg".into(),
            generator_version: "0.1.0".into(),
        },
    )
    .expect("fixture plan should assemble")
}
