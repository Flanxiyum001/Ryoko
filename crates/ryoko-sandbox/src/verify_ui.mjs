#!/usr/bin/env node
// Deterministic UI verification harness, baked into ryoko sandbox images at
// /opt/ryoko/verify-ui.mjs. Agents call this instead of writing their own
// browser scripts, so every run captures the same artifacts the same way.
//
// It starts the project's dev/preview server, waits for it to answer, then for
// each route records desktop and mobile screenshots, an accessibility tree and a
// runtime error log, and finally kills the whole server process group.
//
// Offline by construction: only loopback URLs are accepted.

import { spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { realpathSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const DESKTOP = { width: 1440, height: 900 };
const MOBILE = { width: 390, height: 844 };
const MAX_ROUTE_LENGTH = 200;
const MAX_ROUTES = 25;
const MAX_LOG_ENTRIES = 200;
const MAX_TEXT = 4000;

const USAGE = `ryoko verify-ui

  --dev-command <cmd>       command that serves the app (required)
  --url <url>               base URL to poll, default http://127.0.0.1:4173
  --routes <a,b>            comma separated routes, default /
  --out <dir>               artifact directory, default /ryoko-artifacts
  --ready-timeout-ms <n>    how long to wait for the server, default 60000
  --settle-ms <n>           wait after load before capturing, default 1500
  --allow-issues            exit 0 even when issues are captured
  --check-config            validate arguments and print the plan; no browser
  --help                    print this text

Artifacts per route, under <out>/<route>/:
  desktop.png, mobile.png, aria-tree.json, console-errors.json

Exit codes: 0 clean (or --allow-issues), 1 captured issues, 2 harness/launch
failure, 3 the server never became ready.`;

function fail(message) {
  throw new Error(message);
}

function bounded(value, limit) {
  const text = String(value ?? "");
  return text.length > limit ? `${text.slice(0, limit)}…` : text;
}

function parseArgs(argv) {
  const options = {
    devCommand: "",
    url: "http://127.0.0.1:4173",
    routes: ["/"],
    out: "/ryoko-artifacts",
    readyTimeoutMs: 60_000,
    settleMs: 1_500,
    allowIssues: false,
    checkConfig: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    const value = () => {
      const next = argv[index + 1];
      if (next === undefined) fail(`${arg} needs a value`);
      index += 1;
      return next;
    };
    switch (arg) {
      case "--dev-command":
        options.devCommand = value();
        break;
      case "--url":
        options.url = value();
        break;
      case "--routes":
        options.routes = value()
          .split(",")
          .map((route) => route.trim())
          .filter(Boolean);
        break;
      case "--out":
        options.out = value();
        break;
      case "--ready-timeout-ms":
        options.readyTimeoutMs = Number(value());
        break;
      case "--settle-ms":
        options.settleMs = Number(value());
        break;
      case "--allow-issues":
        options.allowIssues = true;
        break;
      case "--check-config":
        options.checkConfig = true;
        break;
      case "--help":
      case "-h":
        console.log(USAGE);
        process.exit(0);
        break;
      default:
        fail(`unknown argument: ${arg}`);
    }
  }

  if (argv.includes("--help") || argv.includes("-h")) return options;
  if (!options.devCommand.trim()) fail("--dev-command is required");
  if (options.devCommand.length > MAX_TEXT) fail("--dev-command is too long");

  let url;
  try {
    url = new URL(options.url);
  } catch {
    fail("--url must be a valid URL");
  }
  const loopback = ["127.0.0.1", "localhost", "[::1]", "0.0.0.0"];
  if (!loopback.includes(url.hostname)) {
    fail(`--url must point at the local server, not ${url.hostname}`);
  }
  if (!["http:", "https:"].includes(url.protocol)) fail("--url must be http or https");
  options.url = url.toString();

  if (!options.routes.length) fail("--routes cannot be empty");
  if (options.routes.length > MAX_ROUTES) fail(`--routes accepts at most ${MAX_ROUTES} routes`);
  options.routes = options.routes.map((route) => {
    if (!route.startsWith("/")) fail(`route must start with /: ${route}`);
    if (route.length > MAX_ROUTE_LENGTH) fail(`route is too long: ${route}`);
    return route;
  });

  if (!path.isAbsolute(options.out)) fail("--out must be an absolute path");
  for (const key of ["readyTimeoutMs", "settleMs"]) {
    const number = options[key];
    if (!Number.isFinite(number) || number < 0 || number > 600_000) {
      fail(`${key} must be between 0 and 600000`);
    }
  }
  return options;
}

function routeSlug(route) {
  const cleaned = route.replace(/[^a-zA-Z0-9]+/g, "-").replace(/^-+|-+$/g, "");
  return cleaned || "root";
}

function plan(options) {
  return {
    devCommand: options.devCommand,
    url: options.url,
    out: options.out,
    routes: options.routes.map((route) => ({
      route,
      directory: path.join(options.out, routeSlug(route)),
      artifacts: ["desktop.png", "mobile.png", "aria-tree.json", "console-errors.json"],
    })),
    viewports: { desktop: DESKTOP, mobile: MOBILE },
  };
}

function isLocal(url, base) {
  try {
    const target = new URL(url, base);
    return target.origin === base.origin;
  } catch {
    return false;
  }
}

/**
 * Whether any process in a group is still around. A group cannot be given a
 * new leader while a member lives, so a live group with this id is always the
 * one this harness started. EPERM means it exists but is not ours.
 */
function groupAlive(pgid) {
  try {
    process.kill(-pgid, 0);
    return true;
  } catch (error) {
    return error?.code === "EPERM";
  }
}

/**
 * Waits for a group to drain. Signal delivery and reaping are asynchronous, so
 * a single liveness check right after a kill races the scheduler; polling is
 * what makes "the group is gone" a fact rather than a hope.
 */
async function waitForGroupDrain(pgid, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    if (!groupAlive(pgid)) return true;
    if (Date.now() >= deadline) return false;
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
}

async function stopServer(child) {
  if (!child || !child.pid) return;
  const pgid = child.pid;
  const leaderExited = child.exitCode !== null || child.signalCode !== null;
  // A leader that exited cleanly can still leave a backgrounded grandchild
  // holding the port, so cleanup looks at the group, not just the leader.
  if (leaderExited && !groupAlive(pgid)) return;
  const signalGroup = (signal) => {
    try {
      // Negative pid targets the whole process group: `npm run preview` leaves
      // grandchildren behind that a plain child.kill() would orphan.
      process.kill(-pgid, signal);
    } catch {
      try {
        child.kill(signal);
      } catch {
        /* already gone */
      }
    }
  };
  const exited = leaderExited
    ? Promise.resolve()
    : new Promise((resolve) => child.once("exit", resolve));
  signalGroup("SIGTERM");
  const timer = setTimeout(() => signalGroup("SIGKILL"), 5_000);
  try {
    await Promise.race([exited, waitForGroupDrain(pgid, 6_000)]);
  } finally {
    clearTimeout(timer);
  }
  // The leader exiting is not the group exiting: escalate, then wait for the
  // group itself so the caller resumes with the port already free instead of
  // racing a kill that the kernel has not delivered yet.
  signalGroup("SIGKILL");
  await Promise.race([exited, waitForGroupDrain(pgid, 5_000)]);
  await waitForGroupDrain(pgid, 5_000);
}

async function waitForServer(url, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let lastError = "no response";
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url, {
        redirect: "follow",
        signal: AbortSignal.timeout(3_000),
      });
      if (response.status < 500) return { ready: true, status: response.status };
      lastError = `HTTP ${response.status}`;
    } catch (error) {
      lastError = String(error?.message ?? error);
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  return { ready: false, status: 0, error: lastError };
}

async function ariaTree(page) {
  // page.accessibility was deprecated and removed in newer Playwright builds, so
  // prefer the ARIA snapshot when it exists and degrade to the accessibility
  // API, then to a pruned DOM outline. A model still gets a semantic tree.
  try {
    if (typeof page.locator === "function") {
      const snapshot = await page.locator("body").ariaSnapshot();
      if (typeof snapshot === "string" && snapshot.trim()) {
        return { source: "ariaSnapshot", tree: snapshot };
      }
    }
  } catch {
    /* fall through */
  }
  try {
    if (page.accessibility?.snapshot) {
      const snapshot = await page.accessibility.snapshot({ interestingOnly: true });
      if (snapshot) return { source: "accessibility.snapshot", tree: snapshot };
    }
  } catch {
    /* fall through */
  }
  const outline = await page.evaluate(() => {
    const walk = (node, depth) => {
      if (depth > 24 || !node) return null;
      if (node.nodeType === 3) {
        const text = node.textContent.trim();
        return text ? `"${text.slice(0, 120)}"` : null;
      }
      if (node.nodeType !== 1) return null;
      const tag = node.tagName.toLowerCase();
      if (["script", "style", "svg", "path"].includes(tag)) return null;
      const attributes = [];
      for (const name of ["role", "aria-label", "aria-hidden", "alt", "title", "href", "type"]) {
        const value = node.getAttribute?.(name);
        if (value) attributes.push(`${name}="${value.slice(0, 80)}"`);
      }
      const children = [...node.childNodes].map((child) => walk(child, depth + 1)).filter(Boolean);
      return `${tag}${attributes.length ? `[${attributes.join(" ")}]` : ""}${children.length ? `(${children.join(" ")})` : ""}`;
    };
    return walk(document.body, 0) ?? "";
  });
  return { source: "dom-outline", tree: bounded(outline, 200_000) };
}

async function captureRoute(browser, options, route) {
  const directory = path.join(options.out, routeSlug(route));
  await mkdir(directory, { recursive: true });
  const url = new URL(route, options.url).toString();
  const consoleErrors = [];
  const pageErrors = [];
  const failedRequests = [];
  const page = await browser.newPage({ viewport: DESKTOP });
  try {
    page.on("console", (message) => {
      if (message.type() === "error" && consoleErrors.length < MAX_LOG_ENTRIES) {
        consoleErrors.push(bounded(message.text(), 2_000));
      }
    });
    page.on("pageerror", (error) => {
      if (pageErrors.length < MAX_LOG_ENTRIES) pageErrors.push(bounded(error?.message ?? error, 2_000));
    });
    page.on("requestfailed", (request) => {
      if (!isLocal(request.url(), options.url) || failedRequests.length >= MAX_LOG_ENTRIES) return;
      failedRequests.push({
        url: bounded(request.url(), 2_000),
        reason: bounded(request.failure()?.errorText ?? "failed", 200),
      });
    });
    page.on("response", (response) => {
      if (!isLocal(response.url(), options.url) || failedRequests.length >= MAX_LOG_ENTRIES) return;
      if (response.status() >= 400) {
        failedRequests.push({ url: bounded(response.url(), 2_000), status: response.status() });
      }
    });

    await page.goto(url, { waitUntil: "load", timeout: 30_000 });
    await page.waitForTimeout(options.settleMs);

    const measure = () =>
      page.evaluate(() => ({
        scrollWidth: document.documentElement.scrollWidth,
        innerWidth: window.innerWidth,
        overflowing: document.documentElement.scrollWidth > window.innerWidth,
      }));
    const desktopLayout = await measure();
    await page.screenshot({ path: path.join(directory, "desktop.png"), fullPage: true });

    await page.setViewportSize(MOBILE);
    await page.waitForTimeout(200);
    const mobileLayout = await measure();
    await page.screenshot({ path: path.join(directory, "mobile.png"), fullPage: true });

    const accessibility = await ariaTree(page);
    await writeFile(path.join(directory, "aria-tree.json"), `${JSON.stringify(accessibility, null, 2)}\n`);

    const report = {
      route,
      url,
      consoleErrors,
      pageErrors,
      failedRequests,
      overflow: { desktop: desktopLayout, mobile: mobileLayout },
      horizontalOverflow: Boolean(desktopLayout.overflowing || mobileLayout.overflowing),
    };
    await writeFile(
      path.join(directory, "console-errors.json"),
      `${JSON.stringify(report, null, 2)}\n`,
    );
    return {
      route,
      directory,
      issues:
        consoleErrors.length + pageErrors.length + failedRequests.length + (report.horizontalOverflow ? 1 : 0),
    };
  } finally {
    await page.close().catch(() => {});
  }
}

async function launchBrowser() {
  const module = await import("playwright");
  return module.chromium.launch({
    headless: true,
    // /dev/shm is small in some runtimes; this keeps renderers stable there.
    args: ["--disable-dev-shm-usage", "--no-sandbox", "--disable-gpu"],
  });
}

async function main() {
  let options;
  try {
    options = parseArgs(process.argv.slice(2));
  } catch (error) {
    console.error(`verify-ui: ${error.message}`);
    console.error(USAGE);
    process.exit(2);
  }
  if (options.checkConfig) {
    console.log(JSON.stringify(plan(options), null, 2));
    return 0;
  }

  await mkdir(options.out, { recursive: true });
  const server = spawn("/bin/sh", ["-c", options.devCommand], {
    detached: true,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let serverLog = "";
  const remember = (chunk) => {
    serverLog = bounded(serverLog + chunk.toString(), 8_000);
  };
  server.stdout.on("data", remember);
  server.stderr.on("data", remember);

  let browser;
  let exitCode = 0;
  try {
    const ready = await waitForServer(options.url, options.readyTimeoutMs);
    if (!ready.ready) {
      console.error(`verify-ui: ${options.url} never became ready (${ready.error})`);
      console.error(serverLog);
      exitCode = 3;
      return exitCode;
    }
    try {
      browser = await launchBrowser();
    } catch (error) {
      console.error(`verify-ui: could not launch Chromium: ${error.message}`);
      console.error(serverLog);
      exitCode = 2;
      return exitCode;
    }

    const routes = [];
    for (const route of options.routes) {
      routes.push(await captureRoute(browser, options, route));
    }
    const summary = {
      url: options.url,
      out: options.out,
      routes,
      issues: routes.reduce((total, entry) => total + entry.issues, 0),
      serverLog: bounded(serverLog, 2_000),
    };
    await writeFile(path.join(options.out, "summary.json"), `${JSON.stringify(summary, null, 2)}\n`);
    console.log(JSON.stringify(summary, null, 2));
    if (summary.issues > 0 && !options.allowIssues) exitCode = 1;
    return exitCode;
  } finally {
    if (browser) await browser.close().catch(() => {});
    await stopServer(server);
  }
}

// On macOS /var is commonly a symlink to /private/var. Node resolves the
// imported module to its physical location, while argv[1] can retain the
// alias; a lexical comparison silently skips main() and exits successfully.
const invokedDirectly = (() => {
  if (!process.argv[1]) return false;
  try {
    return realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url));
  } catch {
    return false;
  }
})();

if (invokedDirectly) {
  main()
    .then((code) => process.exit(code))
    .catch((error) => {
      console.error(`verify-ui: ${error?.stack ?? error}`);
      process.exit(2);
    });
}

export { parseArgs, plan, routeSlug, waitForServer, stopServer, isLocal };
