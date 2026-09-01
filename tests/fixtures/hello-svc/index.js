#!/usr/bin/env node
// A daemon that proves it is running: appends a line to its log every second, and says on
// startup where its configuration came from, so a packaged install can be verified from the
// outside with nothing but `tail`.
const fs = require("node:fs");
const path = require("node:path");

const logDir = process.env.LOG_DIR || "/var/log/hello-svc";
const greeting = process.env.GREETING || "hello";
const logFile = path.join(logDir, "hello.log");

fs.mkdirSync(logDir, { recursive: true });
const write = (msg) => fs.appendFileSync(logFile, `${new Date().toISOString()} ${msg}\n`);

write(`started pid=${process.pid} cwd=${process.cwd()} greeting=${greeting}`);
setInterval(() => write(`${greeting} from ${process.cwd()}`), 1000);
process.on("SIGTERM", () => { write("stopping"); process.exit(0); });
