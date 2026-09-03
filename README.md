# nativepkg

Build native Linux packages: `.deb`, `.rpm` and `.pkg.tar.zst`, from a single
command.

You have an app, maybe a daemon, maybe a CLI, and you want to ship it the way
people actually install software on Linux: `apt install`, `dnf install`,
`pacman -U`. Normally that means learning `dpkg-deb`, `rpmbuild` and `makepkg`,
gluing them together with `fpm`, and keeping a build box per distribution.
nativepkg writes the archives itself, in Rust, so you build all three formats
from one machine (even one that has never had Debian tooling installed).

It is not a wrapper around the distro tools. There is no `dpkg-deb`,
`rpmbuild`, `makepkg` or `fakeroot` under the hood; the package files are
produced directly.

Configuration lives in one place — a `.nativepkg` file beside your project, or
the command line. `package.json` is not read, so there is a single source of
truth and no divergence between two spellings of the same key.

The app can be anything. The entry point runs by its own shebang, so a Node
service, a shell script, a Python program or a compiled Go or Rust binary is
packaged just the same, and the service integration is generated for you.

Runtime dependencies are declared, never assumed. Pass `--nodejs` to depend on
`nodejs` for a Node application, or `--deps` for a comma-separated list of
whatever your program actually needs; with neither, the package depends on
nothing but what its own maintainer scripts require.

A `.nativepkg` is a small JSON file — `package_name`, `version`, `maintainer`,
an entry point, and whatever else you want to set. You need not write one at
all: pass those same values as flags and package a project that has no config
file. A flag always wins over the file.

## What you get

One run produces, for every format you ask for:

- the app installed under a prefix you choose (default `/usr/lib/<name>`),
- a `/usr/bin/<name>` wrapper so the command is on `PATH`,
- a systemd unit (or sysv / upstart), a dedicated system user and group, and a
  log directory, with the service enabled and started on install using each
  distribution's own conventions,
- maintainer scripts that create the account, wire up the service and clean up
  on removal: `postinst`/`prerm` for deb, `%pre`/`%post`/`%preun` for rpm, and
  an `.INSTALL` for Arch.

The output is byte-for-byte reproducible. The timestamp comes from
`SOURCE_DATE_EPOCH`, the git commit, or the newest source file, so two builds of
the same input are identical.

## Install

nativepkg is a single self-contained binary. It is written in Rust and needs no
Node.js or npm to run; it reads a `.nativepkg` file, or takes everything from
flags.

Install it from crates.io:

```bash
cargo install nativepkg
```

Or build from source (needs Rust 1.88 or newer):

```bash
cargo install --path crates/nativepkg-cli
```

That gives you a `nativepkg` command.

nativepkg runs on **Linux and macOS**, on both x86-64 and arm64. Wherever it
runs it writes the same Linux packages — the host is only where the build
happens; Windows is not supported. The binary is self-contained: `zstd` and `xz`
are compiled in, so there is no system `liblzma`, `dpkg`, `rpmbuild` or Node.js
to install. Drop a prebuilt one anywhere on `PATH`, or install it system-wide as
its own `.deb`, `.rpm` or Arch package.

npm is one distribution channel, not a requirement. A small launcher package
lets Node developers pull the matching prebuilt binary — Linux or macOS, x64 or
arm64 — with `npx nativepkg`; it runs the same standalone binary either way.

## Quick start

A Node service to run under systemd. Put its settings in a `.nativepkg` beside
the project:

```jsonc
// .nativepkg
{
  "package_name": "hello-svc",
  "version": "1.0.0",
  "description": "greets the log every second",
  "maintainer": "You <you@example.com>",
  "init": "systemd",
  "install_dir": "/opt/acme",
  "entrypoints": { "daemon": "index.js" },
  "install_strategy": "npm-install"
}
```

Then build all three formats at once:

```bash
nativepkg --format deb,rpm,arch --nodejs -- index.js package.json lib/
```

`--nodejs` makes the package depend on `nodejs`; add `--deps` for anything else
it needs, for example `--deps "redis-server, ca-certificates"`. Every key in
`.nativepkg` also has an equivalent flag, so a project can be packaged with no
config file at all. A complete, annotated example lives in
[`examples/`](examples/).

(`package.json` is listed there only as a file to ship — the `npm-install`
strategy runs at install time and needs it. nativepkg still never reads it for
configuration.)

A project that is not Node looks the same, only without `--nodejs`. Here a
single Go or Rust binary, with its metadata and one real runtime dependency
supplied on the command line:

```bash
nativepkg \
  --format deb,rpm,arch \
  --init none \
  --pkg-name mytool --version 1.4.0 \
  --maintainer "You <you@example.com>" \
  --deps "libpq5, ca-certificates" \
  -- mytool
```

`nativepkg --help` lists every flag. `--dry-run --json` prints the full plan
without writing a single file.

## Options and configuration

Every option has a default; most projects set only a few. A value comes from a
command-line flag, or from `.nativepkg` when the flag is absent — a flag always
wins. Nothing is read from `package.json`.

`nativepkg --help` is the authoritative flag list and
`nativepkg --list-json-overrides` prints every config key.

In the tables below, the **config key** is what you set in `.nativepkg`; a dash
there means the option is command-line only. The **Default** column is what the
option falls back to when neither the flag nor the config key is set.

### Identity

| Flag | Config key | Default | Description |
|---|---|---|---|
| `-n, --pkg-name` | `package_name` | required | Package name |
| `-v, --version` | `version` | required | Package version |
| `--epoch` | `epoch` | none | Version epoch, for when upstream versioning goes backwards |
| `-d, --description` | `description` | required | One-line description |
| `-m, --maintainer` | `maintainer` | required | Maintainer, as `Name <email>` |
| `-a, --architecture` | `architecture` | `all` / `noarch` | Target architecture |
| — | `homepage` | none | Homepage URL |
| — | `license` | none | License |

`--arch` is a deprecated spelling of `--architecture`; it still works but warns.

### Payload

| Flag | Config key | Default | Description |
|---|---|---|---|
| `--daemon` | `entrypoints.daemon` | — | File the service runs (e.g. `index.js`) |
| `--cli` | `entrypoints.cli` | daemon entry point | File the `/usr/bin` wrapper runs |
| `-e, --exec-name` | `executable_name` | package name | Name of the command placed on `PATH` |
| `--install-dir` | `install_dir` | `/usr/lib/<name>` | Directory the app is installed into |
| `--extra-files` | `extra_files` | none | Extra files copied verbatim to the filesystem root |
| `--triggers-file` | `triggers_file` | none | A dpkg `triggers` control file (deb only) |
| `[INPUTS]…` | — | — | Files and directories to package (positional, after `--`) |

### Service

| Flag | Config key | Default | Description |
|---|---|---|---|
| `-i, --init` | `init` | `auto` | Init system: `auto`, `systemd`, `upstart`, `sysv`, `none` |
| `-u, --user` | `user` | derived from name | User the service runs as |
| `-g, --group` | `group` | derived from name | Group the service runs as |

### Dependencies

| Flag | Config key | Default | Description |
|---|---|---|---|
| `--nodejs` | put `nodejs` in `dependencies` | off | Add `nodejs` as a runtime dependency |
| `--deps` | `dependencies` | none | Comma-separated runtime dependencies |
| `--install-strategy` | `install_strategy` | `auto` | How `node_modules` gets in: `auto`, `copy`, `npm-install` |
| `--install-command` | `install_command` | plain `npm install` | The command run at install time (`npm-install` strategy) |
| `--install-binary` | `install_binary` | first word of the command | Binary the install command is guarded on |

### Output

| Flag | Config key | Default | Description |
|---|---|---|---|
| `--format` | — | `deb` | Formats to build: `deb`, `rpm`, `arch`, comma-separated |
| `--output-dir` | — | `.` | Directory the packages are written to |
| `-o, --output-name` | `output_deb_name` | package name | Base name of the output file (version and architecture are always appended) |

### Templates

Replace a generated file with your own; `nativepkg --list-tmps` names them.
The config equivalents live under a `templates` object (e.g.
`"templates": { "systemd_service": "my-unit.service" }`).

| Flag | Config key |
|---|---|
| `--tmp-exec` | `templates.executable` |
| `--tmp-systemd-service` | `templates.systemd_service` |
| `--tmp-sysv-init` | `templates.sysv_init` |
| `--tmp-upstart-cnf` | `templates.upstart_conf` |
| `--tmp-default-variables` | `templates.default_variables` |

### Behaviour and introspection (command-line only)

| Flag | Description |
|---|---|
| `--dry-run` | Resolve and plan, but write nothing |
| `--json` | Emit machine-readable output (pairs with `--dry-run`) |
| `--verbose` / `--quiet` | More output / errors only |
| `--tool-version` | Print nativepkg's own version |
| `--list-json-overrides` | List every config key |
| `--list-tmps`, `--list-tmp-vars`, `--cat-tmp <name>` | Inspect the built-in templates and the variables they use |
| `--show-readme`, `--show-changelog` | Print this document, or the changelog |

## Bundling node_modules

This is about how a Node project's `node_modules` gets into the package, which is
separate from the runtime dependencies you declare with `--nodejs` and `--deps`.
Two strategies, chosen with `--install-strategy`:

- **copy** (the default when `node_modules` is present): the tree is vendored
  into the package as it sits on disk. Works with any package manager and needs
  no network when the package is installed.
- **npm-install**: nothing is vendored; the package installs dependencies at
  install time. The generated maintainer script runs `npm install --omit=dev` by
  default. There is no package-manager detection — point `--install-command` at
  any other command (`pnpm install --prod`, `yarn install --production`, …) and
  it runs exactly that, guarding on the command's own binary. Ship the matching
  lock file by listing it among the inputs.

## Choosing formats

`--format` selects which packages to build; it defaults to `deb`. Pass a
comma-separated list to build several at once — one binary produces all of them,
so `--format deb,rpm,arch` writes a `.deb`, an `.rpm` and a `.pkg.tar.zst` from a
single run, on any host:

- `deb` — Debian, Ubuntu and derivatives (`apt`, `dpkg`)
- `rpm` — Fedora, RHEL and family, openSUSE (`dnf`, `rpm`, `zypper`)
- `arch` — Arch Linux and derivatives (`pacman`)

## Building from source

```bash
cargo build --release
cargo test --workspace
```

The workspace is five crates: a format-agnostic core that reads the config,
resolves it, plans the file set and renders templates, plus one backend each for
Debian, RPM and Arch that write the archives directly, and the CLI on top.

## License

MIT.
