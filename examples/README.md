# Examples

Each `.nativepkg` here is a complete configuration you can copy to the root of
your own project, adjust, and build. Metadata (name, version, maintainer,
dependencies, …) comes entirely from the file — `package.json` is never read.
Only the files to package are named on the command line.

| Language | File | Entry point |
|---|---|---|
| Node.js | [`.nativepkg`](./.nativepkg) | `index.js` (runs under `node` by its shebang) |
| Python | [`python/.nativepkg`](./python/.nativepkg) | `app.py` (`#!/usr/bin/env python3`) |
| Java (Spring Boot) | [`java-spring-boot/.nativepkg`](./java-spring-boot/.nativepkg) | `run.sh` launching the fat jar |

## Node.js

```bash
nativepkg --format deb,rpm,arch -- index.js cli.js node_modules
```

`"install_strategy": "copy"` vendors `node_modules` into the package as it sits
on disk. Switch to `"npm-install"` to install at unpack time instead; then name
`package.json` (and your lock file) among the inputs so the install can run.
`nodejs` is declared in `dependencies` — there is no config key for `--nodejs`.

## Python

```bash
nativepkg --format deb,rpm,arch -- app.py
```

The entry point is a script with a `#!/usr/bin/env python3` shebang, so it runs
by itself — nothing about the tool is Node-specific. Declare the interpreter and
any libraries as **distribution** packages in `dependencies` (`python3`,
`python3-flask`), not pip names. `install_strategy` is `copy`: there is no
`node_modules`, so it simply ships the files you list.

## Java (Spring Boot)

```bash
nativepkg --format deb,rpm,arch -- run.sh orders-service.jar
```

A Spring Boot fat jar is not runnable by a shebang, so the entry point is a tiny
launcher shipped beside it:

```sh
#!/bin/sh
exec java -jar "$(dirname "$0")/orders-service.jar" "$@"
```

The systemd unit runs `run.sh`, which execs the JVM. `dependencies` names the
JRE (`openjdk-17-jre-headless`). (Alternatively, build a Spring Boot *fully
executable* jar — one with a shell launch-script preamble — and point
`entrypoints.daemon` straight at the jar.)

## Notes

- **`install_dir` is a parent directory** — the package name is appended. `/opt`
  installs the app under `/opt/<name>/`. Omit it for the default, `/usr/lib`.
- **Every key has a flag.** Anything in a `.nativepkg` can instead be passed on
  the command line, so a project needs no config file at all.
  `nativepkg --list-json-overrides` prints every accepted key; `nativepkg --help`
  lists every flag.
