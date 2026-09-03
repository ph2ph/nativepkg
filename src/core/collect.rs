//! Turning the paths a user named into planned entries.
//!
//! Nothing here writes anything. The bash implementation copied every input into a staging
//! tree before archiving it (24 s and 341 MB of writes on a realistic payload); this produces
//! a list of records the backend streams from.
//!
//! Containment: review found three separate escape routes here, each a different shape (a
//! symlink target checked lexically, a `..` count compared against the wrong prefix, a symlink
//! in an intermediate component of a named input that the OS resolves before the walk ever
//! sees it), so the rule is structural rather than per-case. Every path entering this module
//! is canonicalised and proved to be inside the canonical project root by
//! [`canonical_within`], and canonical form is then used for both the boundary check and
//! destination mapping.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::core::npm::InstallStrategy;
use crate::core::plan::{Destination, PlannedFile};
use crate::core::resolve::{ResolvedConfig, Warning};
use crate::core::{Error, Result};

const DEPENDENCY_DIR: &str = "node_modules";

/// Always included when present, on top of the detected manager's lock files.
const ALWAYS_INCLUDED: &[&str] = &["package.json"];

const BIN_DIR: &str = "/usr/bin";

/// Anything landing here is a configuration file.
const CONFIG_PREFIX: &str = "/etc/";

/// Destinations claimed so far, each remembering which source claimed it: the same file
/// collected twice (named by the user and always included) is a no-op, two different files
/// claiming one destination is an error.
type Claims = BTreeMap<Destination, PathBuf>;

/// How a walk treats inert oddities: a dangling symlink, a socket, a fifo.
///
/// A live symlink resolving outside authorised bounds is an error in every tree regardless.
/// Four review rounds here were variations of "a warning would have said so, and nobody would
/// have read it before shipping".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Strictness {
    /// The application's own tree: oddities are the user's mistake.
    Refuse,
    /// A dependency tree: dangling `.bin` links are ordinary, skip them.
    Tolerate,
}

/// Maps source paths under one canonical root onto destinations under another.
#[derive(Clone)]
struct Mapping {
    source_root: PathBuf,
    /// Without a trailing separator; empty means the filesystem root.
    dest_root: String,
}

impl Mapping {
    /// The destination for a source path, or `None` when the path lies outside this tree.
    ///
    /// Both sides are canonical. Comparing a canonical target against a raw root refused
    /// legitimate links whenever the project root was itself reached through a symlink.
    fn destination_for(&self, path: &Path) -> Result<Option<Destination>> {
        let Ok(relative) = path.strip_prefix(&self.source_root) else {
            return Ok(None);
        };
        if relative.as_os_str().is_empty() {
            // The tree's own root maps to the destination prefix itself.
            return Ok(Some(Destination::new(&self.dest_root)?));
        }
        Ok(Some(Destination::new(format!(
            "{}/{}",
            self.dest_root,
            relative.display()
        ))?))
    }
}

/// Canonicalises `path` and proves it lies inside `root`: the containment gate.
///
/// Resolves every component, so a symlink in the middle of a path, which never appears as a
/// walked entry, is caught here. `label` is what the user typed, for the error message.
fn canonical_within(root: &Path, path: &Path, label: &Path) -> Result<PathBuf> {
    let resolved = std::fs::canonicalize(path).map_err(|e| Error::io(path.to_path_buf(), e))?;
    if resolved.starts_with(root) {
        return Ok(resolved);
    }
    Err(Error::manifest(format!(
        "`{}` resolves to `{}`, which is outside the project; it escapes the package. \
         Use the extra-files directory to place content from elsewhere",
        label.display(),
        resolved.display()
    )))
}

/// Collects planned entries for a project. `inputs` are relative to `project_root`.
///
/// # Errors
///
/// Refuses an absolute or missing input, anything resolving outside the project root (directly
/// or through a symlinked component), sockets, fifos and device nodes, and two different
/// sources claiming one destination.
pub fn collect(
    config: &ResolvedConfig,
    project_root: &Path,
    inputs: &[PathBuf],
) -> Result<(Vec<PlannedFile>, Vec<Warning>)> {
    // Canonical from here on.
    let project_root = std::fs::canonicalize(project_root)
        .map_err(|e| Error::io(project_root.to_path_buf(), e))?;

    let mut walker = Walker {
        files: Vec::new(),
        claims: Claims::new(),
        visited: BTreeSet::new(),
        warnings: Vec::new(),
        workspace_roots: canonical_workspace_roots(&project_root, &config.workspace_roots),
        executed: executed_destinations(config),
    };

    let app_mapping = Mapping {
        source_root: project_root.clone(),
        dest_root: app_root(config),
    };

    for input in inputs {
        if input.is_absolute() {
            return Err(Error::manifest(format!(
                "`{}` is an absolute path; inputs must be relative to the project root so the \
                 package layout does not depend on where the project happens to live",
                input.display()
            )));
        }
        if names_dependency_dir(input) {
            walker.warnings.push(Warning::DependenciesExcluded {
                reason: "the install strategy decides whether dependencies are vendored, not the \
                         command line"
                    .to_owned(),
            });
            continue;
        }
        let joined = project_root.join(input);
        if !joined.exists() {
            return Err(Error::manifest(format!(
                "input `{}` does not exist",
                input.display()
            )));
        }
        let start = canonical_within(&project_root, &joined, input)?;
        walker.walk(
            &start,
            &app_mapping,
            &project_root,
            Strictness::Refuse,
            true,
        )?;
    }

    for name in ALWAYS_INCLUDED.iter().copied() {
        let joined = project_root.join(name);
        if joined.is_file() {
            let start = canonical_within(&project_root, &joined, Path::new(name))?;
            walker.walk(
                &start,
                &app_mapping,
                &project_root,
                Strictness::Refuse,
                true,
            )?;
        }
    }

    walker.collect_dependencies(config, &project_root, &app_mapping.dest_root)?;

    if let Some(extra) = &config.extra_files {
        let joined = project_root.join(extra);
        if !joined.is_dir() {
            return Err(Error::manifest(format!(
                "extra-files directory `{}` does not exist",
                extra.display()
            )));
        }
        let root = canonical_within(&project_root, &joined, extra)?;
        let mapping = Mapping {
            source_root: root.clone(),
            dest_root: String::new(),
        };
        walker.walk(&root, &mapping, &root, Strictness::Refuse, false)?;
    }

    let link = executable_symlink(config)?;
    walker.claim(&link, Path::new("<generated>"))?;
    walker.files.push(link);

    Ok((walker.files, walker.warnings))
}

/// Accumulates entries while walking, carrying the state shared across trees.
struct Walker {
    files: Vec<PlannedFile>,
    claims: Claims,
    /// Canonical roots being materialised, so a link cycle terminates.
    visited: BTreeSet<PathBuf>,
    warnings: Vec<Warning>,
    /// Canonical directories a dependency link may point at. Empty means none.
    workspace_roots: Vec<PathBuf>,
    /// Destinations executed directly (the unit's `ExecStart=`, the `/usr/bin` wrapper), with
    /// what executes each. Neither goes through an interpreter, so a file the kernel cannot
    /// exec makes a package whose command or service can never run. The wrapper exists for
    /// every build, `init: none` included: review built a `644` cli entry point under
    /// `init: none` and got `Permission denied`.
    executed: Vec<(String, &'static str)>,
}

impl Walker {
    /// Walks one canonical tree. `boundary` is where resolved symlink targets must stay;
    /// `skip_dependency_dir` keeps the general walk out of `node_modules`.
    fn walk(
        &mut self,
        start: &Path,
        mapping: &Mapping,
        boundary: &Path,
        strictness: Strictness,
        skip_dependency_dir: bool,
    ) -> Result<()> {
        for entry in WalkDir::new(start).sort_by_file_name() {
            let entry = entry.map_err(|e| {
                let path = e.path().unwrap_or(start).to_path_buf();
                Error::io(path, e.into())
            })?;
            let path = entry.path();

            if skip_dependency_dir && path.components().any(|c| c.as_os_str() == DEPENDENCY_DIR) {
                continue;
            }

            let file_type = entry.file_type();
            if file_type.is_dir() {
                continue;
            }

            let Some(destination) = mapping.destination_for(path)? else {
                return Err(Error::manifest(format!(
                    "`{}` resolved outside the tree being collected",
                    path.display()
                )));
            };

            if file_type.is_symlink() {
                self.plan_symlink(path, &destination, mapping, boundary, strictness)?;
                continue;
            }

            if !file_type.is_file() {
                match strictness {
                    Strictness::Refuse => {
                        return Err(Error::manifest(format!(
                            "`{}` is neither a regular file, a directory nor a symlink; \
                             sockets, fifos and device nodes cannot be packaged",
                            path.display()
                        )));
                    }
                    Strictness::Tolerate => continue,
                }
            }

            let metadata = entry
                .metadata()
                .map_err(|e| Error::io(path.to_path_buf(), e.into()))?;
            let runnable = is_executable(&metadata) && has_shebang(path);
            self.refuse_unrunnable_entrypoint(&destination, path, runnable)?;

            let planned =
                PlannedFile::from_source(destination, path.to_path_buf(), metadata.len(), runnable);
            if self.claim(&planned, path)? {
                self.files.push(mark_config(planned));
            }
        }
        Ok(())
    }

    /// Plans a symlink, materialising it when its target lies outside the current tree.
    ///
    /// A target inside the tree becomes a symlink entry. A target elsewhere in the project
    /// (npm workspaces: `node_modules/<name>` -> `packages/<name>`) has its contents planned
    /// under the link's destination. Skipping it instead built cleanly and failed at runtime
    /// with module-not-found; the bash implementation's `cp -rfL` got this right.
    fn plan_symlink(
        &mut self,
        link: &Path,
        destination: &Destination,
        mapping: &Mapping,
        boundary: &Path,
        strictness: Strictness,
    ) -> Result<()> {
        let Some(resolved) = Self::resolve_link_target(link, boundary, strictness)? else {
            return Ok(());
        };

        if let Some(target_destination) = mapping.destination_for(&resolved)? {
            // A symlinked entry point executes its target, so check the resolved file, not
            // the link. Review built `app.js -> real.js` with `real.js` shebang-less: it
            // shipped, and exec'ing it gave `Permission denied`.
            if let Ok(metadata) = std::fs::metadata(&resolved) {
                let runnable = is_executable(&metadata) && has_shebang(&resolved);
                self.refuse_unrunnable_entrypoint(destination, &resolved, runnable)?;
            }
            let planned = PlannedFile::symlink(destination.clone(), &target_destination);
            if self.claim(&planned, link)? {
                self.files.push(planned);
            }
            return Ok(());
        }

        // Inside the boundary but outside this tree: only a declared workspace package may be
        // materialised. `node_modules` is untrusted input, and one symlink in a transitive
        // dependency would otherwise pull `.env` or a key file into the shipped artifact.
        if !self.may_materialise(&resolved) {
            return Err(Error::manifest(format!(
                "`{}` links to `{}`, which is outside its own tree and is not inside a declared \
                 workspace root. If this is a workspace package, declare it in the `workspaces` \
                 field of package.json; otherwise this dependency is reaching for a file it has \
                 no business packaging",
                link.display(),
                resolved.display()
            )));
        }

        if !self.visited.insert(resolved.clone()) {
            // Already being materialised further up the stack: a cycle.
            return Err(Error::manifest(format!(
                "`{}` is a symlink to `{}`, which is already being packaged through another \
                 link; this is a cycle",
                link.display(),
                resolved.display()
            )));
        }

        let nested = Mapping {
            source_root: resolved.clone(),
            dest_root: destination.as_str().to_owned(),
        };
        let result = self.walk(&resolved, &nested, boundary, strictness, false);
        self.visited.remove(&resolved);
        result
    }

    /// Resolves a symlink against the real filesystem, refusing or skipping one that leaves
    /// `boundary`. `canonicalize` follows the whole chain, so a relative target with enough
    /// `..` segments is caught like an absolute one.
    fn resolve_link_target(
        link: &Path,
        boundary: &Path,
        strictness: Strictness,
    ) -> Result<Option<PathBuf>> {
        if !link.exists() {
            return match strictness {
                Strictness::Refuse => Err(Error::manifest(format!(
                    "`{}` is a dangling symlink; it points at something that does not exist",
                    link.display()
                ))),
                Strictness::Tolerate => Ok(None),
            };
        }

        let resolved = std::fs::canonicalize(link).map_err(|e| Error::io(link.to_path_buf(), e))?;
        if resolved.starts_with(boundary) {
            return Ok(Some(resolved));
        }
        // Not governed by `strictness`: a link escaping the project entirely is at least as
        // serious as one reaching an undeclared spot inside it.
        Err(Error::manifest(format!(
            "`{}` is a symlink to `{}`, which escapes the package. Use the extra-files \
             directory to place content from elsewhere",
            link.display(),
            resolved.display()
        )))
    }

    /// True only inside a declared workspace root. A workspace root is never the project root
    /// itself: [`Workspaces::directory_prefixes`] drops such a pattern, since admitting the
    /// root re-opens the reach-through this gate closes.
    ///
    /// [`Workspaces::directory_prefixes`]: crate::core::npm::Workspaces::directory_prefixes
    fn may_materialise(&self, resolved: &Path) -> bool {
        self.workspace_roots
            .iter()
            .any(|root| resolved.starts_with(root))
    }

    /// Records a claim on a destination; `false` when the same source already holds it. Two
    /// different sources is an error: the bash implementation resolved that by walk order, so
    /// whichever file was visited second silently disappeared.
    fn claim(&mut self, planned: &PlannedFile, source: &Path) -> Result<bool> {
        match self.claims.get(&planned.destination) {
            Some(existing) if existing == source => Ok(false),
            Some(existing) => Err(Error::manifest(format!(
                "`{}` and `{}` both install to `{}`; refusing to guess which should win",
                existing.display(),
                source.display(),
                planned.destination
            ))),
            None => {
                self.claims
                    .insert(planned.destination.clone(), source.to_path_buf());
                Ok(true)
            }
        }
    }

    /// Applies the install strategy to the dependency directory.
    ///
    /// The boundary is the project root, not `node_modules`: a link leaving `node_modules`
    /// (npm workspaces) is materialised, a link leaving the project is still refused.
    fn collect_dependencies(
        &mut self,
        config: &ResolvedConfig,
        project_root: &Path,
        app_root: &str,
    ) -> Result<()> {
        let dependency_root = project_root.join(DEPENDENCY_DIR);

        match config.install_strategy {
            InstallStrategy::NpmInstall => {
                self.warnings
                    .push(Warning::DependenciesInstalledAtInstallTime);
                if dependency_root.is_dir() {
                    self.warnings.push(Warning::DependenciesExcluded {
                        reason: "the install strategy installs them during package installation"
                            .to_owned(),
                    });
                }
                Ok(())
            }
            InstallStrategy::Auto | InstallStrategy::Copy => {
                if !dependency_root.is_dir() {
                    return Ok(());
                }
                self.warnings
                    .push(Warning::DependenciesMayIncludeDevelopmentPackages);
                if config.architecture_parsed()?.is_any()
                    && let Some(addon) = find_compiled_addon(&dependency_root)
                {
                    self.warnings
                        .push(Warning::CompiledAddonsInArchitectureIndependentPackage {
                            example: addon,
                        });
                }
                let root =
                    canonical_within(project_root, &dependency_root, Path::new(DEPENDENCY_DIR))?;
                let mapping = Mapping {
                    source_root: root.clone(),
                    dest_root: format!("{app_root}/{DEPENDENCY_DIR}"),
                };
                self.walk(&root, &mapping, project_root, Strictness::Tolerate, false)
            }
        }
    }
}

/// Canonicalises the declared workspace roots. A missing directory is not an error: workspace
/// globs routinely cover directories a given checkout does not have.
fn canonical_workspace_roots(project_root: &Path, declared: &[String]) -> Vec<PathBuf> {
    declared
        .iter()
        .filter_map(|relative| {
            let candidate = project_root.join(relative);
            let canonical = std::fs::canonicalize(candidate).ok()?;
            // A root outside the project, or the project root itself, is not something a
            // manifest may grant.
            if canonical.starts_with(project_root) && canonical != project_root {
                Some(canonical)
            } else {
                None
            }
        })
        .collect()
}

/// The first compiled Node.js addon (`.node`, built for one architecture and ABI) in a
/// dependency tree. Vendoring one into an `Architecture: all` package installs everywhere
/// and works in one place, which the bash implementation did on every build.
fn find_compiled_addon(dependency_root: &Path) -> Option<String> {
    WalkDir::new(dependency_root)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .find(|entry| {
            entry.file_type().is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("node"))
        })
        .map(|entry| entry.path().display().to_string())
}

/// Every destination something in the package executes directly, with what executes it. The
/// `/usr/bin` wrapper exists for every build; the service unit only with a service.
pub(crate) fn executed_destinations(config: &ResolvedConfig) -> Vec<(String, &'static str)> {
    use crate::core::npm::InitSystem;
    let root = app_root(config);

    // Through `Destination::new`, because the comparison is string equality against planned
    // destinations that went through it: a bare `format!` compared `/usr/lib/app/app/./app.js`
    // with `/usr/lib/app/app/app.js` and refused a good `"cli": "./app.js"`. An entry point
    // that cannot form a destination is skipped; collection reports it on its own.
    let normalised = |entry: &str| {
        Destination::new(format!("{root}/{entry}"))
            .ok()
            .map(|d| d.as_str().to_owned())
    };

    let mut executed = Vec::new();
    if let Some(dest) = config
        .cli_entrypoint
        .as_ref()
        .and_then(|e| normalised(e.as_str()))
    {
        executed.push((dest, "the `/usr/bin` wrapper"));
    }
    if config.init != InitSystem::None
        && let Some(dest) = config
            .daemon_entrypoint
            .as_ref()
            .and_then(|e| normalised(e.as_str()))
    {
        executed.push((dest, "the service unit"));
    }
    executed
}

fn app_root(config: &ResolvedConfig) -> String {
    format!("{}/{}/app", config.install_dir, config.package_name)
}

fn names_dependency_dir(input: &Path) -> bool {
    input.components().any(|c| c.as_os_str() == DEPENDENCY_DIR)
}

fn mark_config(planned: PlannedFile) -> PlannedFile {
    if planned.destination.as_str().starts_with(CONFIG_PREFIX) && !planned.is_symlink() {
        planned.as_config()
    } else {
        planned
    }
}

fn executable_symlink(config: &ResolvedConfig) -> Result<PlannedFile> {
    let link = Destination::new(format!("{BIN_DIR}/{}", config.executable_name))?;
    let target = Destination::new(format!(
        "{}/{}/bin/{}",
        config.install_dir, config.package_name, config.executable_name
    ))?;
    Ok(PlannedFile::symlink(link, &target))
}

impl Walker {
    /// Refuses a file something in the package will execute directly but the kernel cannot.
    ///
    /// systemd has no ENOEXEC fallback to a shell, so a missing shebang or executable bit gives
    /// a package that installs cleanly and whose service or command can never run. `path` is
    /// the file actually read: the entry itself, or a symlink's resolved target.
    fn refuse_unrunnable_entrypoint(
        &self,
        destination: &Destination,
        path: &Path,
        runnable: bool,
    ) -> Result<()> {
        if runnable {
            return Ok(());
        }
        if let Some((_, executor)) = self
            .executed
            .iter()
            .find(|(dest, _)| dest == destination.as_str())
        {
            return Err(Error::manifest(format!(
                "`{}` is executed directly by {executor} but cannot be: it needs a \
                 `#!/usr/bin/env node` first line and the executable bit",
                path.display()
            )));
        }
        Ok(())
    }
}

/// Whether a file begins with `#!` or ELF magic, which is what makes an executable bit mean
/// anything. A checkout made with a permissive umask marks every file `775`, `package.json`
/// included, and lintian reports each one as `executable-not-elf-or-script`.
fn has_shebang(path: &Path) -> bool {
    use std::io::Read as _;
    let mut head = [0_u8; 4];
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(n) = file.read(&mut head) else {
        return false;
    };
    (n >= 2 && &head[..2] == b"#!") || (n == 4 && head == *b"\x7fELF")
}

/// Whether the owner-executable bit is set. Group and other bits are an accident of the
/// developer's umask and must not reach a package.
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o100 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}
